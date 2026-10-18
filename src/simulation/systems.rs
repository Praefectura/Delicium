//! Schedule and systems that advance the simulation world one fixed step.
//!
//! One tick runs six systems, in this order:
//!
//! 1. the social step pairs available participants before either can wander;
//! 2. [`wander_step`]: every instance advances its idle/walk state machine
//!    inside the current wander bounds and may move horizontally;
//! 3. [`interaction_snapshot`]: an exclusive system that snapshots every
//!    position in instance-id order, resolves the nearest neighbour inside
//!    [`INTERACTION_RADIUS`](super::INTERACTION_RADIUS) for each instance and
//!    eases the reaction weights; it runs after the walk so the snapshot and
//!    the same frame's render agree on where every instance is;
//! 4. [`reaction_step`]: ages every poke reaction by one step and turns 2D
//!    instances toward their poke point while one is active;
//! 5. the attention step: an opt-in, deterministic presentation pass that is
//!    a no-op while attention stays disabled;
//! 6. [`advance_poses`]: a parallel-query system that samples animation
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

use crate::model::ModelKind;

use super::actions::{self, ActionEvents, ActionState};
use super::components::{
    InstanceTag, InteractionState, InteractionsRes, ModelHandle, PokeReaction, PoseState, Position,
    TickStatus, TimeState, WanderBoundsRes, WanderEventsRes, WanderPhase, WanderRegionsRes,
    WanderState,
};
use super::pose::DriveInput;
use super::{
    FIXED_DT_SECONDS, INTERACTION_EASE_PER_SECOND, INTERACTION_RADIUS, InstanceId, InteractionEdge,
    MAX_SIM_THREADS, POKE_HOP_AMPLITUDE, POKE_HOP_SECONDS, POKE_REACTION_SECONDS,
    POKE_RISE_FRACTION, SimError, WANDER_ARRIVE_EPSILON, WANDER_EDGE_MARGIN, WANDER_EVENT_CAP,
    WANDER_IDLE_MAX_SECONDS, WANDER_IDLE_MIN_SECONDS, WANDER_MIN_TARGET_DISTANCE, WANDER_SEED_STEP,
    WANDER_SPEED, WanderEvent,
};

/// Weight fraction gained per fixed step while a neighbour is in range.
const EASE_STEP: f32 = INTERACTION_EASE_PER_SECOND / 60.0;

/// Fraction of the remaining 3D body turn gained per fixed step, and the
/// residual below which it snaps exactly onto its target.
///
/// A turn spans twice the authored yaw (50 degrees for the bundled
/// character), and the same tick usually starts the walk clip's own 25-degree
/// cross-fade. Keeping the first step under a third of that span keeps the
/// visible root yaw changing by less than the 0.2 rad per-tick bound the
/// walk tests assert, while still settling in about half a second.
const TURN_EASE_STEP: f32 = 0.16;
const TURN_SNAP: f32 = 1e-3;

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
        turn: 1.0,
        turn_target: 1.0,
        rng,
        suspended: false,
        moved_this_tick: false,
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

/// Builds the fixed-step schedule: social, wander, snapshot, poke reactions,
/// attention, poses.
///
/// The walk runs before the interaction snapshot so both the snapshot and the
/// same frame's render describe the same (already-walked) position; the
/// reaction step ages the poke reactions before the attention pass reads them
/// for the streak, and the attention pass runs before the pose pass so its
/// drive and presentation pair are the same tick's.
pub fn build_schedule() -> Schedule {
    let mut schedule = Schedule::default();
    schedule.add_systems(
        (
            super::social::step,
            wander_step,
            interaction_snapshot,
            reaction_step,
            super::attention::step,
            advance_poses,
        )
            .chain(),
    );
    schedule
}

/// Advances the idle/walk state machine of every instance by one fixed step.
///
/// Deterministic by construction: every instance owns its state and its
/// random stream, and each phase change draws exactly one value, so neither
/// the query order nor the bounds geometry can change the sequence. A
/// suspended instance is completely frozen; an instance with an active poke
/// reaction stands still too (it looks at the poke point for
/// [`POKE_REACTION_SECONDS`](super::POKE_REACTION_SECONDS)) and resumes
/// exactly where it left off; without bounds nothing moves.
///
/// An instance found outside the walkable range (an external placement, or
/// bounds that shrank) walks back in immediately instead of waiting out its
/// idle pause, so it can never sit off-screen for a whole idle period.
///
/// The walk direction is expressed differently per kind, because a 3D root
/// must never mirror: a mirror would flip asymmetric details such as a scarf
/// print. A 2D sprite flips its mirror `facing`; a 3D skeleton eases a body
/// `turn` that reflects the authored three-quarter stance about the model's
/// front axis, so it faces the way it walks as a rotation rather than a
/// reversed copy of its own geometry.
pub fn wander_step(
    mut query: Query<(
        &InstanceTag,
        &ModelHandle,
        &mut Position,
        &mut WanderState,
        &PokeReaction,
        &super::social::SocialState,
    )>,
    bounds: Res<WanderBoundsRes>,
    regions: Res<WanderRegionsRes>,
    mut events: ResMut<WanderEventsRes>,
) {
    // This is a tick output, including when bounds are disabled. Never let a
    // previous walk survive an early return as an in-place walk animation.
    // The body turn eases here as well, so a turn still finishes settling on
    // ticks that do not walk (idle pauses, a suspended instance, no bounds).
    for (_, _, _, mut wander, _, _) in query.iter_mut() {
        wander.moved_this_tick = false;
        ease_turn(&mut wander);
    }
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

    for (tag, handle, mut position, mut wander, reaction, social) in query.iter_mut() {
        if wander.suspended || reaction.elapsed < POKE_REACTION_SECONDS || social.active.is_some() {
            continue;
        }
        let id = tag.0;
        let x = position.0.x;
        let (lo, hi) = if let Some(regions) = &regions.0 {
            let Some(segment) = visible_segment(regions, x, position.0.y, bounds) else {
                continue;
            };
            let left = segment.left + WANDER_EDGE_MARGIN;
            let right = segment.right - WANDER_EDGE_MARGIN;
            if left > right {
                let middle = segment.left + (segment.right - segment.left) * 0.5;
                (middle, middle)
            } else {
                (left, right)
            }
        } else {
            (lo, hi)
        };
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
            wander.moved_this_tick = position.0.x != x;
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
        wander.moved_this_tick = position.0.x != x;
        // A 3D skeleton never mirrors: a mirror would flip asymmetric details
        // such as the scarf print. Its walk direction is expressed as a body
        // turn instead, which faces the authored three-quarter stance along
        // the step without reversing any geometry; see `apply_root_turn`.
        // Only 2D sprites turn by flipping the mirror.
        if handle.model.kind() == ModelKind::Sprite2d {
            wander.facing = if step >= 0.0 { 1.0 } else { -1.0 };
        } else {
            wander.turn_target = if step >= 0.0 { 1.0 } else { -1.0 };
        }
        wander.phase = WanderPhase::Walking { target_x: target };
    }
}

/// Merge the visible horizontal slice before applying the character margin:
/// adjacent monitors form one walkable span, disconnected monitors do not.
fn visible_segment(
    regions: &[super::WorldRect],
    x: f32,
    y: f32,
    bounds: super::WorldXBounds,
) -> Option<super::WorldXBounds> {
    let mut intervals: Vec<_> = regions
        .iter()
        .filter(|rect| rect.bottom < y && y <= rect.top)
        .map(|rect| (rect.left, rect.right))
        .collect();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Option<(f32, f32)> = None;
    let mut selected = None;
    for (left, right) in intervals {
        match merged {
            Some((lo, hi)) if left <= hi => merged = Some((lo, hi.max(right))),
            Some((lo, hi)) => {
                if lo <= x && x <= hi {
                    selected = Some((lo, hi));
                    break;
                }
                merged = Some((left, right));
            }
            None => merged = Some((left, right)),
        }
    }
    let (left, right) = selected.or_else(|| merged.filter(|&(lo, hi)| lo <= x && x <= hi))?;
    let left = left.max(bounds.left);
    let right = right.min(bounds.right);
    (left <= right && left <= x && x <= right).then_some(super::WorldXBounds { left, right })
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

/// Eases the 3D body turn one step toward the walk direction it targets.
///
/// Exponential approach with an exact snap, so a settled instance carries
/// the exact `+1.0`/`-1.0` endpoints and never a permanent 1e-7 residue that
/// would keep a tiny rotation alive forever. Sprites keep `turn = +1.0`.
fn ease_turn(wander: &mut WanderState) {
    if wander.turn == wander.turn_target {
        return;
    }
    wander.turn += (wander.turn_target - wander.turn) * TURN_EASE_STEP;
    if (wander.turn_target - wander.turn).abs() <= TURN_SNAP {
        wander.turn = wander.turn_target;
    }
}

/// Appends one event, honouring [`WANDER_EVENT_CAP`].
fn push_event(events: &mut WanderEventsRes, event: WanderEvent) {
    debug_assert!(events.0.len() <= WANDER_EVENT_CAP);
    if events.0.len() < WANDER_EVENT_CAP {
        events.0.push(event);
    }
}

/// Ages every poke reaction by one fixed step.
///
/// While the reaction is active and the poke direction is not dead-zone'd, a
/// 2D instance is turned to face the poke point by flipping its mirror
/// `facing`; 3D instances turn through the pose drive instead. Once
/// `elapsed` reaches [`POKE_REACTION_SECONDS`](super::POKE_REACTION_SECONDS)
/// the direction is cleared, so the pose pass falls back to the interaction
/// drive on the very next step.
pub fn reaction_step(mut query: Query<(&ModelHandle, &mut WanderState, &mut PokeReaction)>) {
    for (handle, mut wander, mut reaction) in query.iter_mut() {
        if reaction.elapsed >= POKE_REACTION_SECONDS {
            continue;
        }
        reaction.elapsed = (reaction.elapsed + FIXED_DT_SECONDS as f32).min(POKE_REACTION_SECONDS);
        if reaction.elapsed >= POKE_REACTION_SECONDS {
            reaction.direction = Vec3::ZERO;
            continue;
        }
        if reaction.direction.x != 0.0 && handle.model.kind() == ModelKind::Sprite2d {
            wander.facing = if reaction.direction.x >= 0.0 {
                1.0
            } else {
                -1.0
            };
        }
    }
}

/// Turn strength of a poke reaction at `elapsed` seconds: a fast linear rise
/// over the first [`POKE_RISE_FRACTION`](super::POKE_RISE_FRACTION) of the
/// reaction, then a quadratic fall to zero at
/// [`POKE_REACTION_SECONDS`](super::POKE_REACTION_SECONDS).
pub(crate) fn poke_envelope(elapsed: f32) -> f32 {
    if elapsed < 0.0 || elapsed >= POKE_REACTION_SECONDS {
        return 0.0;
    }
    let rise = POKE_REACTION_SECONDS * POKE_RISE_FRACTION;
    if elapsed < rise {
        elapsed / rise
    } else {
        let remaining = 1.0 - (elapsed - rise) / (POKE_REACTION_SECONDS - rise);
        remaining * remaining
    }
}

/// Vertical offset of the poke hop at `elapsed` seconds: one parabola
/// peaking at [`POKE_HOP_AMPLITUDE`](super::POKE_HOP_AMPLITUDE) halfway
/// through [`POKE_HOP_SECONDS`](super::POKE_HOP_SECONDS), zero everywhere
/// else. Presentation only; the instance position never moves.
pub(crate) fn poke_hop(elapsed: f32) -> f32 {
    if elapsed < 0.0 || elapsed >= POKE_HOP_SECONDS {
        return 0.0;
    }
    let progress = elapsed / POKE_HOP_SECONDS;
    4.0 * progress * (1.0 - progress) * POKE_HOP_AMPLITUDE
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
#[allow(clippy::type_complexity)]
pub fn advance_poses(
    mut query: Query<(
        &InstanceTag,
        &ModelHandle,
        &InteractionState,
        &WanderState,
        &PokeReaction,
        &mut TimeState,
        &mut PoseState,
        &mut ActionState,
        &super::social::SocialState,
        Option<&super::attention::AttentionState>,
    )>,
    mut status: ResMut<TickStatus>,
    mut events: ResMut<ActionEvents>,
) {
    let slot = Arc::new(Mutex::new((None::<(InstanceId, SimError)>, Vec::new())));
    let worker_slot = Arc::clone(&slot);

    query.par_iter_mut().for_each(
        move |(
            tag,
            handle,
            interaction,
            wander,
            reaction,
            mut time,
            mut pose,
            mut action,
            social,
            attention,
        )| {
            let id = tag.0;
            let next_time = time.time + FIXED_DT_SECONDS;
            // Priority: an active poke reaction wins outright, then an active
            // social lock, then the opt-in attention drive (pointer, edge or
            // idle), then the existing nearest-neighbour reaction. The
            // attention drive is `None` whenever attention is disabled or
            // neutral, so the default path is unchanged.
            let drive = if reaction.elapsed < POKE_REACTION_SECONDS {
                DriveInput::from_reaction(reaction, wander.facing, wander.turn)
            } else if let Some(active) = social.active {
                // A paired pet looks at its partner, not an unrelated nearest neighbour.
                DriveInput::from_state(
                    &InteractionState {
                        neighbor: Some(active.partner),
                        offset: social.direction,
                        weight: interaction.weight,
                    },
                    wander.facing,
                    wander.turn,
                )
            } else if let Some(drive) =
                attention.and_then(|attention| attention.drive(wander.facing, wander.turn))
            {
                drive
            } else {
                DriveInput::from_state(interaction, wander.facing, wander.turn)
            };
            match actions::advance(
                &handle.model,
                &action,
                wander.moved_this_tick,
                social.active,
                reaction,
                &drive,
                next_time,
                id,
            ) {
                Ok((next_action, next_pose, event)) => {
                    time.time = next_time;
                    pose.prev = std::mem::replace(&mut pose.curr, next_pose);
                    *action = next_action;
                    if let Some(event) = event {
                        let mut slot = match worker_slot.lock() {
                            Ok(slot) => slot,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        slot.1.push(event);
                    }
                }
                Err(error) => {
                    let mut slot = match worker_slot.lock() {
                        Ok(slot) => slot,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    let dominated = slot.0.as_ref().is_some_and(|(existing, _)| *existing <= id);
                    if !dominated {
                        slot.0 = Some((id, error));
                    }
                }
            }
        },
    );

    let (recorded, mut committed) = match slot.lock() {
        Ok(mut slot) => (slot.0.take(), std::mem::take(&mut slot.1)),
        Err(poisoned) => {
            let mut slot = poisoned.into_inner();
            (slot.0.take(), std::mem::take(&mut slot.1))
        }
    };
    committed.sort_by_key(|event| event.instance);
    for event in committed {
        events.push(event);
    }
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
    fn visible_slice_merges_before_inset_and_keeps_half_open_vertical_edges() {
        use super::super::{WorldRect, WorldXBounds};
        let bounds = WorldXBounds {
            left: -10.0,
            right: 10.0,
        };
        let regions = [
            WorldRect {
                left: 0.0,
                right: 4.0,
                bottom: -2.0,
                top: 2.0,
            },
            WorldRect {
                left: -4.0,
                right: 0.0,
                bottom: 0.0,
                top: 2.0,
            },
            WorldRect {
                left: 6.0,
                right: 9.0,
                bottom: -2.0,
                top: 2.0,
            },
        ];
        assert_eq!(
            visible_segment(&regions, -1.0, 2.0, bounds),
            Some(WorldXBounds {
                left: -4.0,
                right: 4.0
            })
        );
        assert_eq!(visible_segment(&regions, -1.0, 0.0, bounds), None);
        assert_eq!(
            visible_segment(&regions, 1.0, 0.0, bounds),
            Some(WorldXBounds {
                left: 0.0,
                right: 4.0
            })
        );
        assert_eq!(visible_segment(&regions, 1.0, -2.0, bounds), None);
        assert_eq!(visible_segment(&regions, 5.0, 1.0, bounds), None);
        assert_eq!(
            visible_segment(&regions, 7.0, 1.0, bounds),
            Some(WorldXBounds {
                left: 6.0,
                right: 9.0
            })
        );
        assert_eq!(
            visible_segment(
                &regions,
                1.0,
                1.0,
                WorldXBounds {
                    left: -2.0,
                    right: 2.0
                }
            ),
            Some(WorldXBounds {
                left: -2.0,
                right: 2.0
            })
        );
        assert_eq!(visible_segment(&[], 0.0, 1.0, bounds), None);
    }

    #[test]
    fn poke_envelope_rises_and_decays() {
        // Outside the window the envelope is exactly zero.
        assert_eq!(poke_envelope(-0.01), 0.0);
        assert_eq!(poke_envelope(POKE_REACTION_SECONDS), 0.0);
        assert_eq!(poke_envelope(POKE_REACTION_SECONDS + 0.5), 0.0);
        // Start at zero, reach the peak at the end of the rise, decay to
        // zero at the end of the reaction.
        assert_eq!(poke_envelope(0.0), 0.0);
        let rise = POKE_REACTION_SECONDS * POKE_RISE_FRACTION;
        assert!((poke_envelope(rise) - 1.0).abs() < 1e-6);
        assert!(poke_envelope(rise + 1e-4) < 1.0);
        let midpoint = rise + (POKE_REACTION_SECONDS - rise) * 0.5;
        let expected = 0.25; // (1 - 0.5)^2
        assert!((poke_envelope(midpoint) - expected).abs() < 1e-6);
        // Monotone rise, then monotone decay.
        let mut previous = 0.0;
        for step in 1..=10 {
            let value = poke_envelope(rise * step as f32 / 10.0);
            assert!(value >= previous, "envelope is not rising");
            previous = value;
        }
        let mut previous = poke_envelope(rise);
        for step in 1..=10 {
            let t = rise + (POKE_REACTION_SECONDS - rise) * step as f32 / 10.0;
            let value = poke_envelope(t);
            assert!(value <= previous, "envelope is not decaying");
            previous = value;
        }
    }

    #[test]
    fn poke_hop_is_a_single_parabola() {
        assert_eq!(poke_hop(-0.01), 0.0);
        assert_eq!(poke_hop(0.0), 0.0);
        assert_eq!(poke_hop(POKE_HOP_SECONDS), 0.0);
        assert_eq!(poke_hop(POKE_HOP_SECONDS + 1.0), 0.0);
        // Peak at half duration, symmetric around it.
        let peak = poke_hop(POKE_HOP_SECONDS * 0.5);
        assert!((peak - POKE_HOP_AMPLITUDE).abs() < 1e-6);
        let quarter = poke_hop(POKE_HOP_SECONDS * 0.25);
        let three_quarters = poke_hop(POKE_HOP_SECONDS * 0.75);
        // 4*s*(1-s) at s = 0.25 and 0.75 both give 0.75 * amplitude.
        assert!((quarter - 0.75 * POKE_HOP_AMPLITUDE).abs() < 1e-6);
        assert!((three_quarters - quarter).abs() < 1e-6);
        // Never negative and never above the amplitude.
        for step in 0..=20 {
            let value = poke_hop(POKE_HOP_SECONDS * step as f32 / 20.0);
            assert!((0.0..=POKE_HOP_AMPLITUDE + 1e-6).contains(&value));
        }
    }

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
