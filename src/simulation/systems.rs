//! Schedule and systems that advance the simulation world one fixed step.
//!
//! One tick runs three systems, in this order:
//!
//! 1. [`wander_step`]: every instance advances its idle/walk state machine
//!    inside the current wander bounds and may move horizontally;
//! 2. [`interaction_snapshot`]: an exclusive system that snapshots every
//!    position in instance-id order, resolves the nearest neighbour inside
//!    [`INTERACTION_RADIUS`](super::INTERACTION_RADIUS) for each instance and
//!    eases the reaction weights; it runs after the walk so the snapshot and
//!    the same frame's render agree on where every instance is;
//! 3. [`advance_poses`]: a parallel-query system that samples animation
//!    parameters or joint locals for every instance and advances the pose
//!    ring (`prev <- curr <- sampled`).
//!
//! All systems are deterministic: the wander state machine draws exactly one
//! random value per phase change from a per-instance stream, the snapshot
//! works on a sorted copy of the positions, and every instance owns its time
//! and pose, so neither thread scheduling nor query order can change the
//! observable result of a tick.

use std::sync::{Arc, Mutex};

use bevy_ecs::{
    schedule::{IntoScheduleConfigs, Schedule},
    system::{Query, Res, ResMut},
    world::World,
};
use bevy_tasks::{ComputeTaskPool, TaskPoolBuilder};
use glam::Vec3;

use super::components::{
    InstanceTag, InteractionState, InteractionsRes, ModelHandle, PoseState, Position, TickStatus,
    TimeState, WanderBoundsRes, WanderEventsRes, WanderPhase, WanderState,
};
use super::pose::{DriveInput, sample_pose};
use super::{
    FIXED_DT_SECONDS, INTERACTION_EASE_PER_SECOND, INTERACTION_RADIUS, InstanceId, InteractionEdge,
    MAX_SIM_THREADS, SimError, WANDER_ARRIVE_EPSILON, WANDER_EDGE_MARGIN, WANDER_EVENT_CAP,
    WANDER_IDLE_MAX_SECONDS, WANDER_IDLE_MIN_SECONDS, WANDER_MIN_TARGET_DISTANCE, WANDER_SEED_STEP,
    WANDER_SPEED, WanderEvent,
};

/// Weight fraction gained per fixed step while a neighbour is in range.
const EASE_STEP: f32 = INTERACTION_EASE_PER_SECOND / 60.0;

/// Per-step travel of a walking instance. Must stay below
/// [`WANDER_ARRIVE_EPSILON`] for a walk to converge onto its target.
const WANDER_STEP: f32 = WANDER_SPEED * (FIXED_DT_SECONDS as f32);

/// splitmix64: no dependencies, pure integer arithmetic, identical on every
/// platform. The state is advanced first, so two instances seeded apart can
/// never emit correlated sequences.
fn next_random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform `value in [0, 1)`. Uses the top 24 bits, which are exactly
/// representable in an `f32`.
fn random_unit(state: &mut u64) -> f32 {
    (next_random(state) >> 40) as f32 / (1u32 << 24) as f32
}

/// Draws one idle duration from the instance's own stream.
fn random_idle(state: &mut u64) -> f32 {
    WANDER_IDLE_MIN_SECONDS
        + random_unit(state) * (WANDER_IDLE_MAX_SECONDS - WANDER_IDLE_MIN_SECONDS)
}

/// The wander state a freshly spawned instance starts with.
///
/// The first idle duration is drawn unconditionally at spawn, whether or not
/// bounds exist yet, so enabling wandering later cannot shift the stream and
/// two identically created instances always agree tick for tick.
pub(super) fn initial_wander_state(id: InstanceId) -> WanderState {
    let mut rng = id.wrapping_mul(WANDER_SEED_STEP).wrapping_add(1);
    let remaining = random_idle(&mut rng);
    WanderState {
        phase: WanderPhase::Idle { remaining },
        facing: 1.0,
        rng,
        suspended: false,
    }
}

/// Abandons an ongoing walk and starts a fresh idle countdown; the facing and
/// the suspension flag are untouched. An instance already idling keeps its
/// remaining time.
pub(super) fn cancel_walk(wander: &mut WanderState) {
    if matches!(wander.phase, WanderPhase::Idle { .. }) {
        return;
    }
    let remaining = random_idle(&mut wander.rng);
    wander.phase = WanderPhase::Idle { remaining };
}

/// Unconditionally starts a fresh idle countdown, even for an already idling
/// instance. Used when a suspension is lifted, so a character released after
/// a long hold pauses before walking off instead of leaving immediately.
pub(super) fn reset_to_idle(wander: &mut WanderState) {
    let remaining = random_idle(&mut wander.rng);
    wander.phase = WanderPhase::Idle { remaining };
}

/// Initialises the bounded `ComputeTaskPool` the parallel pose pass runs on.
///
/// `bevy_tasks` keeps a process-global pool; the first caller wins, later
/// calls are no-ops. The pool is capped at
/// `min(available_parallelism, MAX_SIM_THREADS)` worker threads so the
/// simulation cannot flood the machine. Safe to call repeatedly.
pub fn initialize_task_pool() {
    ComputeTaskPool::get_or_init(|| {
        let threads = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
            .min(MAX_SIM_THREADS);
        TaskPoolBuilder::new().num_threads(threads).build()
    });
}

/// Builds the fixed-step schedule: wander first, snapshot second, poses last.
///
/// The walk runs before the interaction snapshot so both the snapshot and the
/// same frame's render describe the same (already-walked) position.
pub fn build_schedule() -> Schedule {
    let mut schedule = Schedule::default();
    schedule.add_systems((wander_step, interaction_snapshot, advance_poses).chain());
    schedule
}

/// Advances the idle/walk state machine of every instance by one fixed step.
///
/// Deterministic by construction: every instance owns its state and its
/// random stream, and each phase change draws exactly one value, so neither
/// the query order nor the bounds geometry can change the sequence. A
/// suspended instance is completely frozen; without bounds nothing moves.
///
/// An instance found outside the walkable range (an external placement, or
/// bounds that shrank) walks back in immediately instead of waiting out its
/// idle pause, so it can never sit off-screen for a whole idle period.
pub fn wander_step(
    mut query: Query<(&InstanceTag, &mut Position, &mut WanderState)>,
    bounds: Res<WanderBoundsRes>,
    mut events: ResMut<WanderEventsRes>,
) {
    let Some(bounds) = bounds.0 else {
        return;
    };
    // Collapse a too-narrow range onto its midpoint, so the clamps below can
    // never panic and the target arithmetic stays finite.
    let lo = bounds.left + WANDER_EDGE_MARGIN;
    let hi = bounds.right - WANDER_EDGE_MARGIN;
    let (lo, hi) = if lo > hi {
        let middle = bounds.left + (bounds.right - bounds.left) * 0.5;
        (middle, middle)
    } else {
        (lo, hi)
    };

    for (tag, mut position, mut wander) in query.iter_mut() {
        if wander.suspended {
            continue;
        }
        let id = tag.0;
        let x = position.0.x;
        let outside = x < lo || x > hi;

        let (target, started) = match wander.phase {
            WanderPhase::Idle { remaining } => {
                if outside {
                    // Walk straight back from wherever the instance landed;
                    // a recovery never waits out the idle pause.
                    (x.clamp(lo, hi), true)
                } else {
                    let remaining = remaining - FIXED_DT_SECONDS as f32;
                    if remaining > 0.0 {
                        wander.phase = WanderPhase::Idle { remaining };
                        continue;
                    }
                    (pick_target(&mut wander.rng, x, lo, hi), true)
                }
            }
            WanderPhase::Walking { target_x } => {
                // Bounds that shrank may leave the target out of range; the
                // retarget is deterministic and draws no random value.
                let target = target_x.clamp(lo, hi);
                (target, target != target_x)
            }
        };

        let delta = target - x;
        if delta.abs() <= WANDER_ARRIVE_EPSILON {
            position.0.x = target;
            wander.phase = WanderPhase::Idle {
                remaining: random_idle(&mut wander.rng),
            };
            push_event(
                &mut events,
                WanderEvent::Arrive {
                    instance: id,
                    x: target,
                    y: position.0.y,
                },
            );
            continue;
        }

        if started {
            push_event(
                &mut events,
                WanderEvent::Start {
                    instance: id,
                    x,
                    y: position.0.y,
                    target_x: target,
                },
            );
        }
        let step = delta.clamp(-WANDER_STEP, WANDER_STEP);
        position.0.x += step;
        wander.facing = if step >= 0.0 { 1.0 } else { -1.0 };
        wander.phase = WanderPhase::Walking { target_x: target };
    }
}

/// Chooses a walk target inside `[lo, hi]`: one uniform draw, replaced by the
/// far end when it lands too close to the current position, and by the
/// midpoint when the range is too narrow for a full-length walk. The number
/// of draws depends only on the (deterministic) bounds, never on the
/// instance's previous position, so the stream cannot be perturbed by an
/// external placement.
fn pick_target(rng: &mut u64, x: f32, lo: f32, hi: f32) -> f32 {
    if hi - lo < WANDER_MIN_TARGET_DISTANCE {
        return lo + (hi - lo) * 0.5;
    }
    let target = lo + random_unit(rng) * (hi - lo);
    if (target - x).abs() < WANDER_MIN_TARGET_DISTANCE {
        if x < lo + (hi - lo) * 0.5 { hi } else { lo }
    } else {
        target
    }
}

/// Appends one event, honouring [`WANDER_EVENT_CAP`].
fn push_event(events: &mut WanderEventsRes, event: WanderEvent) {
    debug_assert!(events.0.len() <= WANDER_EVENT_CAP);
    if events.0.len() < WANDER_EVENT_CAP {
        events.0.push(event);
    }
}

/// Resolves the nearest neighbour of every instance and eases reactions.
///
/// Exclusive on purpose: the pass borrows the whole world so the position
/// snapshot is taken before any state is written, exactly matching the
/// documented tick order.
pub fn interaction_snapshot(world: &mut World) {
    // 1. Snapshot positions, sorted by instance id so thread or archetype
    //    order can never change the resolved links.
    let mut points: Vec<(InstanceId, Vec3)> = {
        let mut query = world.query::<(&InstanceTag, &Position)>();
        query
            .iter(world)
            .map(|(tag, position)| (tag.0, position.0))
            .collect()
    };
    points.sort_by_key(|(id, _)| *id);

    // 2. O(N^2) nearest-neighbour resolution (N <= 64). Ties break toward the
    //    smaller instance id, so the result is deterministic.
    struct Link {
        to: InstanceId,
        distance: f32,
        direction: Vec3,
    }
    let mut links: Vec<Option<Link>> = Vec::with_capacity(points.len());
    for (index, (_, position)) in points.iter().enumerate() {
        let mut best: Option<Link> = None;
        for (other_index, (other_id, other_position)) in points.iter().enumerate() {
            if index == other_index {
                continue;
            }
            let delta = *other_position - *position;
            let distance = delta.length();
            if distance > INTERACTION_RADIUS {
                continue;
            }
            let better = match &best {
                None => true,
                Some(link) => (distance, *other_id) < (link.distance, link.to),
            };
            if better {
                best = Some(Link {
                    to: *other_id,
                    distance,
                    direction: delta.normalize_or_zero(),
                });
            }
        }
        links.push(best);
    }

    // 3. Write the reaction state of every instance.
    {
        let mut states = world.query::<(&InstanceTag, &mut InteractionState)>();
        for (tag, mut state) in states.iter_mut(world) {
            let link = points
                .iter()
                .position(|(id, _)| *id == tag.0)
                .and_then(|index| links[index].as_ref());
            match link {
                Some(link) => {
                    state.neighbor = Some(link.to);
                    state.offset = link.direction;
                    state.weight = advanced_weight(state.weight, 1.0);
                }
                None => {
                    state.neighbor = None;
                    state.offset = Vec3::ZERO;
                    state.weight = advanced_weight(state.weight, 0.0);
                }
            }
        }
    }

    // 4. Publish the directed edge list, ordered by (from, to).
    let edges: Vec<InteractionEdge> = points
        .iter()
        .zip(links.iter())
        .filter_map(|((from, _), link)| {
            link.as_ref().map(|link| InteractionEdge {
                from: *from,
                to: link.to,
                distance: link.distance,
            })
        })
        .collect();
    if let Some(mut interactions) = world.get_resource_mut::<InteractionsRes>() {
        interactions.0 = edges;
    }
}

/// Eases a reaction weight one step toward `target`.
fn advanced_weight(weight: f32, target: f32) -> f32 {
    let step = EASE_STEP.clamp(0.0, 1.0);
    (weight + (target - weight) * step).clamp(0.0, 1.0)
}

/// Advances the animation time and pose of every instance by one fixed step.
///
/// The per-instance evaluation runs through `Query::par_iter_mut` on the
/// bounded [`ComputeTaskPool`]; each instance only writes its own components,
/// so the parallel pass is deterministic. A pose that cannot be evaluated
/// leaves the instance's time, pose and model untouched and is recorded for
/// [`Simulation::advance`](super::Simulation::advance) to surface as a
/// visible error; the deterministic failure repeats on the next tick instead
/// of silently degrading.
pub fn advance_poses(
    mut query: Query<(
        &InstanceTag,
        &ModelHandle,
        &InteractionState,
        &WanderState,
        &mut TimeState,
        &mut PoseState,
    )>,
    mut status: ResMut<TickStatus>,
) {
    let slot = Arc::new(Mutex::new(None::<(InstanceId, SimError)>));
    let worker_slot = Arc::clone(&slot);

    query.par_iter_mut().for_each(
        move |(tag, handle, interaction, wander, mut time, mut pose)| {
            let id = tag.0;
            let next_time = time.time + FIXED_DT_SECONDS;
            let drive = DriveInput::from_state(interaction, wander.facing);
            match sample_pose(&handle.model, next_time, &drive, id) {
                Ok(next) => {
                    time.time = next_time;
                    pose.prev = std::mem::replace(&mut pose.curr, next);
                }
                Err(error) => {
                    let mut slot = match worker_slot.lock() {
                        Ok(slot) => slot,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    let dominated = slot.as_ref().is_some_and(|(existing, _)| *existing <= id);
                    if !dominated {
                        *slot = Some((id, error));
                    }
                }
            }
        },
    );

    let recorded = match slot.lock() {
        Ok(mut slot) => slot.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    if let Some((_, error)) = recorded {
        status.error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FIXED_DT, Simulation, pose::Pose};
    use super::*;
    use crate::model::{LoadLimits, ModelData, load_model_bytes, try_sample_parameters};
    use bevy_ecs::entity::Entity;

    #[test]
    fn long_running_ticks_keep_full_precision_until_clip_sampling() {
        let model = load_model_bytes(
            include_bytes!("../../assets/examples/puppet2d.dmodel.json"),
            &LoadLimits::default(),
        )
        .unwrap();
        let ModelData::Sprite2d(sprite) = &model.data else {
            panic!("expected 2D model")
        };
        let mut simulation = Simulation::new();
        let asset = simulation.insert_asset(model.clone()).unwrap();
        let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
        let mut query = simulation.world.query::<(Entity, &InstanceTag)>();
        let entity = query
            .iter(&simulation.world)
            .find(|(_, tag)| tag.0 == id)
            .unwrap()
            .0;
        let mut previous_time = 262_144.125;
        simulation.world.get_mut::<TimeState>(entity).unwrap().time = previous_time;
        for _ in 0..12 {
            simulation.advance(FIXED_DT).unwrap();
            let time = simulation.instance(id).unwrap().animation_time;
            assert!(time > previous_time);
            assert!((time - previous_time - FIXED_DT_SECONDS).abs() < 1e-8);
            let expected = try_sample_parameters(sprite, time).unwrap();
            let state = simulation.world.get::<PoseState>(entity).unwrap();
            let Pose::Sprite2d(actual) = &state.curr else {
                panic!("expected 2D pose")
            };
            assert_eq!(actual, &expected);
            previous_time = time;
        }
    }
}
