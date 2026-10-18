//! Deterministic reciprocal greetings and play for explicitly opted-in models.
//! Social locks never mutate a user's wander suspension or consume its RNG.

use bevy_ecs::{component::Component, resource::Resource, world::World};

use super::components::{InstanceTag, ModelHandle, PokeReaction, Position, WanderState};
use super::{ActionKind, FIXED_DT_SECONDS, InstanceId, POKE_REACTION_SECONDS};

pub const SOCIAL_RADIUS: f32 = 1.6;
pub const GREET_SECONDS: f64 = 1.6;
pub const PLAY_SECONDS: f64 = 2.2;
pub const COOLDOWN_SECONDS: f64 = 12.0;
const EVENT_CAP: usize = 4096;

/// Both participants report the same phase and phase-local time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SocialInfo {
    pub partner: InstanceId,
    pub action: ActionKind,
    pub elapsed: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocialPhase {
    Greet,
    Play,
    End,
    Cancel,
}

impl SocialPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Greet => "greet",
            Self::Play => "play",
            Self::End => "end",
            Self::Cancel => "cancel",
        }
    }
}

/// One event per pair, with its ids in ascending order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocialEvent {
    pub first: InstanceId,
    pub second: InstanceId,
    pub phase: SocialPhase,
}

#[derive(Component, Default)]
pub(super) struct SocialState {
    pub active: Option<SocialInfo>,
    pub direction: glam::Vec3,
    cooldown: f64,
}

#[derive(Resource, Default)]
pub(super) struct SocialEvents(pub Vec<SocialEvent>);

fn event(world: &mut World, a: InstanceId, b: InstanceId, phase: SocialPhase) {
    let mut events = world.resource_mut::<SocialEvents>();
    if events.0.len() < EVENT_CAP {
        events.0.push(SocialEvent {
            first: a.min(b),
            second: a.max(b),
            phase,
        });
    }
}

/// Cancel only the pair, never the independently owned drag suspension.
pub(super) fn cancel(world: &mut World, id: InstanceId) {
    let partner = world
        .query::<(&InstanceTag, &SocialState)>()
        .iter(world)
        .find(|(tag, _)| tag.0 == id)
        .and_then(|(_, state)| state.active.map(|active| active.partner));
    let Some(partner) = partner else { return };
    let mut query = world.query::<(&InstanceTag, &mut SocialState)>();
    for (tag, mut state) in query.iter_mut(world) {
        if tag.0 == id || (tag.0 == partner && state.active.is_some_and(|s| s.partner == id)) {
            state.active = None;
            state.cooldown = COOLDOWN_SECONDS;
        }
    }
    event(world, id, partner, SocialPhase::Cancel);
}

#[derive(Clone, Copy)]
struct Candidate {
    id: InstanceId,
    entity: bevy_ecs::entity::Entity,
    position: glam::Vec3,
    available: bool,
    active: Option<SocialInfo>,
}

/// Runs before wandering: a newly formed pair is stationary on its first tick.
pub(super) fn step(world: &mut World) {
    let mut query = world.query::<(
        bevy_ecs::entity::Entity,
        &InstanceTag,
        &ModelHandle,
        &Position,
        &WanderState,
        &PokeReaction,
        &mut SocialState,
    )>();
    let mut candidates = Vec::new();
    for (entity, tag, handle, position, wander, reaction, mut state) in query.iter_mut(world) {
        state.cooldown = (state.cooldown - FIXED_DT_SECONDS).max(0.0);
        let opted_in = handle
            .model
            .actions
            .is_some_and(|a| a.greet.is_some() && a.play.is_some());
        candidates.push(Candidate {
            id: tag.0,
            entity,
            position: position.0,
            available: opted_in
                && !wander.suspended
                && reaction.elapsed >= POKE_REACTION_SECONDS
                && state.cooldown <= 0.0,
            active: state.active,
        });
    }
    candidates.sort_by_key(|c| c.id);
    for a in &candidates {
        let Some(active) = a.active else { continue };
        if a.id >= active.partner {
            continue;
        }
        let b = candidates.iter().find(|b| b.id == active.partner);
        let Some(b) = b.filter(|b| {
            a.available
                && b.available
                && b.active.is_some_and(|s| {
                    s.partner == a.id && s.action == active.action && s.elapsed == active.elapsed
                })
        }) else {
            cancel(world, a.id);
            continue;
        };
        let elapsed = active.elapsed + FIXED_DT_SECONDS;
        let (next, phase) = match active.action {
            ActionKind::Greet if elapsed + 1e-9 >= GREET_SECONDS => {
                (Some((ActionKind::Play, 0.0)), Some(SocialPhase::Play))
            }
            ActionKind::Play if elapsed + 1e-9 >= PLAY_SECONDS => (None, Some(SocialPhase::End)),
            _ => (Some((active.action, elapsed)), None),
        };
        for (own, other) in [(a, b), (b, a)] {
            let mut state = world.get_mut::<SocialState>(own.entity).unwrap();
            state.active = next.map(|(action, elapsed)| SocialInfo {
                partner: other.id,
                action,
                elapsed,
            });
            if next.is_none() {
                state.cooldown = COOLDOWN_SECONDS;
            }
        }
        if let Some(phase) = phase {
            event(world, a.id, b.id, phase);
        }
    }
    // Sort the edges by distance, then ids: symmetric nearest available pairs,
    // independent of ECS traversal order. No wander random numbers are consumed.
    let mut edges = Vec::new();
    for (index, a) in candidates.iter().enumerate() {
        if !a.available || a.active.is_some() {
            continue;
        }
        for b in &candidates[index + 1..] {
            if !b.available || b.active.is_some() {
                continue;
            }
            let delta = b.position - a.position;
            let distance = delta.length_squared();
            if delta.y.abs() <= 0.15
                && delta.z.abs() <= 0.2
                && (0.8 * 0.8..=SOCIAL_RADIUS * SOCIAL_RADIUS).contains(&distance)
            {
                edges.push((distance, a, b));
            }
        }
    }
    edges.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then(a.1.id.cmp(&b.1.id))
            .then(a.2.id.cmp(&b.2.id))
    });
    for (_, a, b) in edges {
        let free = [a, b].iter().all(|c| {
            world
                .get::<SocialState>(c.entity)
                .is_some_and(|s| s.active.is_none() && s.cooldown <= 0.0)
        });
        if !free {
            continue;
        }
        for (own, other) in [(a, b), (b, a)] {
            let mut state = world.get_mut::<SocialState>(own.entity).unwrap();
            state.active = Some(SocialInfo {
                partner: other.id,
                action: ActionKind::Greet,
                elapsed: 0.0,
            });
            state.direction = (other.position - own.position).normalize();
        }
        event(world, a.id, b.id, SocialPhase::Greet);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CompiledModel, LoadLimits, load_model_bytes};
    use crate::simulation::{FIXED_DT, Simulation, WorldXBounds, pose::Pose};
    use glam::Vec3;
    use serde_json::json;
    use std::sync::Arc;

    fn model(social: bool) -> Arc<CompiledModel> {
        let mut doc: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../assets/examples/puppet2d.dmodel.json"))
                .unwrap();
        doc["format_version"] = json!("0.2");
        doc["actions"] = json!({"idle":"idle","walk":"walk","reaction":"reaction"});
        if social {
            doc["actions"]["greet"] = json!("greet");
            doc["actions"]["play"] = json!("play");
        }
        doc["sprite2d"]["animations"] = json!([
            {"name":"idle","duration":2.0,"channels":[{"parameter":"breath","keys":[{"time":0.0,"value":0.0},{"time":2.0,"value":0.0}]}]},
            {"name":"walk","duration":2.0,"channels":[{"parameter":"breath","keys":[{"time":0.0,"value":0.25},{"time":2.0,"value":0.25}]}]},
            {"name":"reaction","duration":1.0,"channels":[{"parameter":"breath","keys":[{"time":0.0,"value":0.5},{"time":1.0,"value":0.5}]}]},
            {"name":"greet","duration":1.6,"channels":[{"parameter":"breath","keys":[{"time":0.0,"value":0.75},{"time":1.6,"value":0.75}]}]},
            {"name":"play","duration":2.2,"channels":[{"parameter":"breath","keys":[{"time":0.0,"value":1.0},{"time":2.2,"value":1.0}]}]}
        ]);
        load_model_bytes(&serde_json::to_vec(&doc).unwrap(), &LoadLimits::default()).unwrap()
    }

    fn pair() -> (Simulation, u64, u64) {
        let mut sim = Simulation::new();
        let asset = sim.insert_asset(model(true)).unwrap();
        let a = sim.spawn(asset, Vec3::new(-0.65, 0.15, 0.0)).unwrap();
        let b = sim.spawn(asset, Vec3::new(0.65, 0.15, 0.0)).unwrap();
        (sim, a, b)
    }

    fn tick(sim: &mut Simulation, n: usize) {
        for _ in 0..n {
            sim.advance(FIXED_DT).unwrap();
        }
    }

    fn reciprocal(sim: &Simulation, a: u64, b: u64, kind: ActionKind) {
        let x = sim.instance(a).unwrap();
        let y = sim.instance(b).unwrap();
        assert_eq!(x.social.unwrap().partner, b);
        assert_eq!(y.social.unwrap().partner, a);
        assert_eq!(x.social.unwrap().elapsed, y.social.unwrap().elapsed);
        assert_eq!(x.action, Some(kind));
        assert_eq!(y.action, Some(kind));
    }

    #[test]
    fn reciprocal_greet_play_end_freezes_wander_without_consuming_rng() {
        let (mut sim, a, b) = pair();
        sim.set_wander_bounds(Some(WorldXBounds {
            left: -5.0,
            right: 5.0,
        }))
        .unwrap();
        let before: Vec<_> = [a, b]
            .map(|id| {
                let w = sim.world.get::<WanderState>(sim.entities[&id]).unwrap();
                (w.phase, w.rng, sim.instance(id).unwrap().position)
            })
            .into();
        tick(&mut sim, 1);
        reciprocal(&sim, a, b, ActionKind::Greet);
        tick(&mut sim, 20);
        // Distinct clip values prove actual two-way pose playback, not just logs.
        for id in [a, b] {
            let Pose::Sprite2d(values) = &sim
                .world
                .get::<super::super::components::PoseState>(sim.entities[&id])
                .unwrap()
                .curr
            else {
                panic!()
            };
            assert!((values[0] - 0.75).abs() < 1e-5);
        }
        // The nanosecond step is slightly shorter than 1/60 second.
        let greet_steps = (GREET_SECONDS / FIXED_DT_SECONDS).ceil() as usize;
        tick(&mut sim, greet_steps - 20 - 1);
        reciprocal(&sim, a, b, ActionKind::Greet);
        tick(&mut sim, 1);
        reciprocal(&sim, a, b, ActionKind::Play);
        tick(&mut sim, 20);
        for (index, id) in [a, b].into_iter().enumerate() {
            let w = sim.world.get::<WanderState>(sim.entities[&id]).unwrap();
            assert_eq!(
                (w.phase, w.rng, sim.instance(id).unwrap().position),
                before[index]
            );
            assert!(!w.suspended && !w.moved_this_tick);
        }
        let play_steps = (PLAY_SECONDS / FIXED_DT_SECONDS).ceil() as usize;
        tick(&mut sim, play_steps - 20 - 1);
        reciprocal(&sim, a, b, ActionKind::Play);
        tick(&mut sim, 1);
        assert!(sim.instance(a).unwrap().social.is_none());
        assert!(sim.instance(b).unwrap().social.is_none());
        assert_eq!(
            sim.drain_social_events()
                .iter()
                .map(|e| e.phase)
                .collect::<Vec<_>>(),
            [SocialPhase::Greet, SocialPhase::Play, SocialPhase::End]
        );
        assert!(sim.drain_social_events().is_empty());
        tick(&mut sim, 400);
        assert!(sim.drain_social_events().is_empty());
        assert!(
            [a, b]
                .iter()
                .any(|id| sim.instance(*id).unwrap().position != before[(*id - 1) as usize].2)
        );
    }

    #[test]
    fn pairing_is_deterministic_exclusive_and_requires_both_optional_clips() {
        let mut sim = Simulation::new();
        let asset = sim.insert_asset(model(true)).unwrap();
        for x in [0.0, 1.0, 4.0, 5.0] {
            sim.spawn(asset, Vec3::new(x, 0.0, 0.0)).unwrap();
        }
        tick(&mut sim, 1);
        reciprocal(&sim, 1, 2, ActionKind::Greet);
        reciprocal(&sim, 3, 4, ActionKind::Greet);
        assert_eq!(
            sim.drain_social_events()
                .iter()
                .map(|e| (e.first, e.second))
                .collect::<Vec<_>>(),
            [(1, 2), (3, 4)]
        );
        let (mut sim, a, b) = pair();
        let mut incomplete = model(true);
        Arc::get_mut(&mut incomplete)
            .unwrap()
            .actions
            .as_mut()
            .unwrap()
            .play = None;
        let asset = sim.insert_asset(incomplete).unwrap();
        sim.replace_model(b, asset).unwrap();
        tick(&mut sim, 200);
        assert!(sim.instance(a).unwrap().social.is_none());
        assert!(sim.drain_social_events().is_empty());
    }

    #[test]
    fn social_drive_targets_partner_even_when_another_pet_is_nearer() {
        let mut sim = Simulation::new();
        let asset = sim.insert_asset(model(true)).unwrap();
        for x in [0.0, 0.9, 1.9, 3.0] {
            sim.spawn(asset, Vec3::new(x, 0.0, 0.0)).unwrap();
        }
        tick(&mut sim, 60);
        reciprocal(&sim, 1, 2, ActionKind::Greet);
        reciprocal(&sim, 3, 4, ActionKind::Greet);
        assert_eq!(sim.instance(3).unwrap().neighbor, Some(2));
        let entity = sim.entities[&3];
        let handle = sim.world.get::<ModelHandle>(entity).unwrap();
        let crate::model::ModelData::Sprite2d(sprite) = &handle.model.data else {
            panic!()
        };
        let gaze = sprite.parameter_index("gaze_x").unwrap();
        let Pose::Sprite2d(values) = &sim
            .world
            .get::<super::super::components::PoseState>(entity)
            .unwrap()
            .curr
        else {
            panic!()
        };
        assert!(
            values[gaze] > 0.0,
            "instance 3 must look right at partner 4, not left at nearest neighbour 2"
        );
    }

    #[test]
    fn poke_cancels_both_and_cooldown_prevents_immediate_repair() {
        let (mut sim, a, b) = pair();
        tick(&mut sim, 1);
        sim.drain_social_events();
        sim.poke_reaction(a, Vec3::ZERO).unwrap();
        assert!(sim.instance(a).unwrap().social.is_none());
        assert!(sim.instance(b).unwrap().social.is_none());
        assert_eq!(sim.drain_social_events()[0].phase, SocialPhase::Cancel);
        tick(&mut sim, 1);
        assert_eq!(sim.instance(a).unwrap().action, Some(ActionKind::Reaction));
        assert_eq!(sim.instance(b).unwrap().action, Some(ActionKind::Idle));
        tick(&mut sim, 700);
        assert!(sim.drain_social_events().is_empty());
        tick(&mut sim, 21);
        reciprocal(&sim, a, b, ActionKind::Greet);
    }

    #[test]
    fn drag_lock_is_independent_and_never_released_by_partner() {
        let (mut sim, a, b) = pair();
        tick(&mut sim, 1);
        sim.suspend_wander(a).unwrap();
        sim.poke_reaction(b, Vec3::ZERO).unwrap();
        tick(&mut sim, 1000);
        assert!(
            sim.world
                .get::<WanderState>(sim.entities[&a])
                .unwrap()
                .suspended
        );
        assert!(
            !sim.world
                .get::<WanderState>(sim.entities[&b])
                .unwrap()
                .suspended
        );
        assert!(sim.instance(b).unwrap().social.is_none());
        sim.resume_wander(a).unwrap();
        tick(&mut sim, 1);
        reciprocal(&sim, a, b, ActionKind::Greet);
    }

    #[test]
    fn successful_position_replace_and_remove_cancel_reciprocally() {
        for operation in 0..3 {
            let (mut sim, a, b) = pair();
            tick(&mut sim, 1);
            sim.drain_social_events();
            match operation {
                0 => sim.set_position(a, Vec3::new(-4.0, 0.0, 0.0)).unwrap(),
                1 => {
                    let asset = sim.insert_asset(model(false)).unwrap();
                    sim.replace_model(a, asset).unwrap();
                }
                _ => {
                    assert!(sim.despawn(a));
                }
            }
            assert!(sim.instance(b).unwrap().social.is_none());
            assert_eq!(sim.drain_social_events()[0].phase, SocialPhase::Cancel);
            tick(&mut sim, 10);
            assert_eq!(sim.instance(b).unwrap().action, Some(ActionKind::Idle));
        }
    }

    #[test]
    fn rejected_mutations_and_exhausted_poke_keep_the_pair() {
        let (mut sim, a, b) = pair();
        tick(&mut sim, 1);
        assert!(sim.set_position(a, Vec3::splat(f32::NAN)).is_err());
        assert!(sim.replace_model(a, u64::MAX).is_err());
        let mut bad = model(true);
        Arc::get_mut(&mut bad)
            .unwrap()
            .actions
            .as_mut()
            .unwrap()
            .idle = usize::MAX;
        let bad_asset = sim.insert_asset(bad).unwrap();
        assert!(sim.replace_model(a, bad_asset).is_err());
        sim.world
            .get_mut::<PokeReaction>(sim.entities[&a])
            .unwrap()
            .revision = u64::MAX;
        assert!(sim.poke_reaction(a, Vec3::ZERO).is_err());
        reciprocal(&sim, a, b, ActionKind::Greet);
        assert_eq!(sim.drain_social_events().len(), 1);
    }

    #[test]
    fn too_far_vertical_or_overlapping_instances_do_not_start() {
        for position in [
            Vec3::new(3.0, 0.15, 0.0),
            Vec3::new(0.65, 0.5, 0.0),
            Vec3::new(-0.4, 0.15, 0.0),
        ] {
            let (mut sim, a, b) = pair();
            sim.set_position(b, position).unwrap();
            tick(&mut sim, 10);
            assert!(sim.instance(a).unwrap().social.is_none());
            assert!(sim.drain_social_events().is_empty());
        }
    }
}
