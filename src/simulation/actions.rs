//! Optional per-instance action playback. Unbound models keep legacy sampling.

use bevy_ecs::{component::Component, resource::Resource};

use crate::model::{
    ClipPlayback, CompiledModel, ModelData, try_sample_joints_clip, try_sample_parameters_clip,
};

use super::components::PokeReaction;
use super::pose::{DriveInput, Pose, apply_pose_drive, blend_pose, sample_pose};
use super::{
    ActionEvent, ActionKind, FIXED_DT_SECONDS, InstanceId, POKE_REACTION_SECONDS, SimError,
};

/// Cross-fades use a frozen source, not an exponentially smoothed last pose.
const TRANSITION_SECONDS: f64 = 0.15;
const EVENT_CAP: usize = 4096;

#[derive(Component, Default)]
pub(super) struct ActionState(pub Option<Playback>);

#[derive(Clone, Debug)]
pub(super) struct Playback {
    pub kind: ActionKind,
    pub time: f64,
    pub consumed_revision: u64,
    pub base: Pose,
    transition: Option<Transition>,
}

#[derive(Clone, Debug)]
struct Transition {
    source: Pose,
    elapsed: f64,
}

#[derive(Resource, Default)]
pub(super) struct ActionEvents(pub Vec<ActionEvent>);

impl ActionEvents {
    pub fn push(&mut self, event: ActionEvent) {
        if self.0.len() < EVENT_CAP {
            self.0.push(event);
        }
    }
}

fn sample_base(
    model: &CompiledModel,
    kind: ActionKind,
    time: f64,
    id: InstanceId,
) -> Result<Pose, SimError> {
    let bindings = model.actions.ok_or_else(|| SimError::Inconsistent {
        instance: id,
        detail: "action playback requires model bindings".to_string(),
    })?;
    let (index, playback) = match kind {
        ActionKind::Idle => (bindings.idle, ClipPlayback::Loop),
        ActionKind::Walk => (bindings.walk, ClipPlayback::Loop),
        ActionKind::Reaction => (bindings.reaction, ClipPlayback::Clamp),
        ActionKind::Greet | ActionKind::Play => {
            let index = if kind == ActionKind::Greet {
                bindings.greet
            } else {
                bindings.play
            }
            .ok_or_else(|| SimError::Inconsistent {
                instance: id,
                detail: "social playback requires an explicit clip binding".into(),
            })?;
            (index, ClipPlayback::Clamp)
        }
    };
    match &model.data {
        ModelData::Sprite2d(sprite) => Ok(Pose::Sprite2d(try_sample_parameters_clip(
            sprite, index, time, playback,
        )?)),
        ModelData::Skeleton3d(skeleton) => Ok(Pose::Skeleton3d(try_sample_joints_clip(
            skeleton, index, time, playback,
        )?)),
    }
}

/// Prepares a fresh model-shaped state before spawn or replacement commits.
pub(super) fn initialize(
    model: &CompiledModel,
    idle_time: f64,
    legacy_time: f64,
    reaction: &PokeReaction,
    drive: &DriveInput,
    id: InstanceId,
) -> Result<(ActionState, Pose), SimError> {
    if model.actions.is_none() {
        return Ok((
            ActionState(None),
            sample_pose(model, legacy_time, drive, id)?,
        ));
    }
    let (kind, time) = if reaction.elapsed < POKE_REACTION_SECONDS {
        (ActionKind::Reaction, f64::from(reaction.elapsed))
    } else {
        (ActionKind::Idle, idle_time)
    };
    let base = sample_base(model, kind, time, id)?;
    let mut pose = base.clone();
    apply_pose_drive(model, &mut pose, drive, id)?;
    Ok((
        ActionState(Some(Playback {
            kind,
            time,
            consumed_revision: reaction.revision,
            base,
            transition: None,
        })),
        pose,
    ))
}

/// Computes a candidate; the caller commits it with time and pose together.
#[allow(clippy::too_many_arguments)]
pub(super) fn advance(
    model: &CompiledModel,
    current: &ActionState,
    moved: bool,
    social: Option<super::SocialInfo>,
    reaction: &PokeReaction,
    drive: &DriveInput,
    legacy_time: f64,
    id: InstanceId,
) -> Result<(ActionState, Pose, Option<ActionEvent>), SimError> {
    if model.actions.is_none() {
        return Ok((
            ActionState(None),
            sample_pose(model, legacy_time, drive, id)?,
            None,
        ));
    }
    let current = current.0.as_ref().ok_or_else(|| SimError::Inconsistent {
        instance: id,
        detail: "bound model has no action playback state".to_string(),
    })?;
    let kind = if reaction.elapsed < POKE_REACTION_SECONDS {
        ActionKind::Reaction
    } else if let Some(social) = social {
        social.action
    } else if moved {
        ActionKind::Walk
    } else {
        ActionKind::Idle
    };
    let poke_changed =
        kind == ActionKind::Reaction && current.consumed_revision != reaction.revision;
    let restarted = kind == current.kind && poke_changed;
    let switched = kind != current.kind || poke_changed;
    let time = if kind == ActionKind::Reaction {
        f64::from(reaction.elapsed)
    } else if let Some(social) = social {
        social.elapsed
    } else if switched {
        0.0
    } else {
        current.time + FIXED_DT_SECONDS
    };
    let mut transition = if switched {
        Some(Transition {
            source: current.base.clone(),
            elapsed: 0.0,
        })
    } else {
        current.transition.clone().map(|mut transition| {
            transition.elapsed = (transition.elapsed + FIXED_DT_SECONDS).min(TRANSITION_SECONDS);
            transition
        })
    };
    let target = sample_base(model, kind, time, id)?;
    let base = if let Some(transition) = &transition {
        blend_pose(
            model,
            &transition.source,
            &target,
            (transition.elapsed / TRANSITION_SECONDS) as f32,
            id,
        )?
    } else {
        target
    };
    if transition
        .as_ref()
        .is_some_and(|transition| transition.elapsed >= TRANSITION_SECONDS)
    {
        transition = None;
    }
    let mut pose = base.clone();
    apply_pose_drive(model, &mut pose, drive, id)?;
    let next = ActionState(Some(Playback {
        kind,
        time,
        consumed_revision: reaction.revision,
        base,
        transition,
    }));
    let event = switched.then_some(ActionEvent {
        instance: id,
        action: kind,
        restarted,
    });
    Ok((next, pose, event))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::Vec3;
    use serde_json::{Value, json};

    use super::*;
    use crate::model::{LoadLimits, load_model_bytes, try_sample_parameters};
    use crate::simulation::components::{
        ModelHandle, PoseState, TimeState, WanderPhase, WanderState,
    };
    use crate::simulation::{FIXED_DT, Simulation, WorldRect, WorldXBounds};

    fn advance(
        model: &CompiledModel,
        current: &ActionState,
        moved: bool,
        reaction: &PokeReaction,
        drive: &DriveInput,
        legacy_time: f64,
        id: InstanceId,
    ) -> Result<(ActionState, Pose, Option<ActionEvent>), SimError> {
        super::advance(
            model,
            current,
            moved,
            None,
            reaction,
            drive,
            legacy_time,
            id,
        )
    }

    fn document() -> Value {
        let mut doc: Value =
            serde_json::from_slice(include_bytes!("../../assets/examples/puppet2d.dmodel.json"))
                .unwrap();
        doc["format_version"] = json!("0.2");
        doc["actions"] = json!({"idle":"idle", "walk":"walk", "reaction":"reaction"});
        doc["sprite2d"]["animations"] = json!([
            {"name":"idle", "duration":2.0, "channels":[{"parameter":"breath", "keys":[
                {"time":0.0,"value":0.0}, {"time":2.0,"value":0.0}
            ]}]},
            {"name":"walk", "duration":2.0, "channels":[{"parameter":"breath", "keys":[
                {"time":0.0,"value":1.0}, {"time":2.0,"value":1.0}
            ]}]},
            {"name":"reaction", "duration":1.0, "channels":[{"parameter":"breath", "keys":[
                {"time":0.0,"value":0.25}, {"time":1.0,"value":0.75}
            ]}]}
        ]);
        doc
    }

    fn load(doc: &Value) -> Arc<CompiledModel> {
        load_model_bytes(&serde_json::to_vec(doc).unwrap(), &LoadLimits::default()).unwrap()
    }

    fn initial(model: &CompiledModel) -> ActionState {
        initialize(
            model,
            0.0,
            0.0,
            &PokeReaction::default(),
            &DriveInput::none(),
            1,
        )
        .unwrap()
        .0
    }

    fn breath(model: &CompiledModel, pose: &Pose) -> f32 {
        let (ModelData::Sprite2d(sprite), Pose::Sprite2d(values)) = (&model.data, pose) else {
            panic!("expected sprite pose")
        };
        values[sprite.parameter_index("breath").unwrap()]
    }

    fn tick(simulation: &mut Simulation, count: usize) {
        for _ in 0..count {
            simulation.advance(FIXED_DT).unwrap();
        }
    }

    #[test]
    fn transition_has_a_fixed_frozen_source_and_finishes() {
        let model = load(&document());
        let mut state = initial(&model);
        let reaction = PokeReaction::default();
        let drive = DriveInput::none();
        for index in 0..=10 {
            let (next, pose, event) =
                advance(&model, &state, true, &reaction, &drive, 0.0, 1).unwrap();
            let expected = (index as f64 * FIXED_DT_SECONDS / TRANSITION_SECONDS).min(1.0) as f32;
            assert!((breath(&model, &pose) - expected).abs() < 1e-6);
            assert_eq!(event.is_some(), index == 0);
            assert_eq!(next.0.as_ref().unwrap().kind, ActionKind::Walk);
            state = next;
        }
        assert!(state.0.as_ref().unwrap().transition.is_none());
    }

    #[test]
    fn interruption_freezes_the_current_undriven_mix_and_applies_drive_once() {
        let model = load(&document());
        let mut state = initial(&model);
        let drive = DriveInput {
            direction: Vec3::X,
            weight: 0.5,
            facing: 1.0,
            turn: 1.0,
        };
        for _ in 0..5 {
            state = advance(
                &model,
                &state,
                true,
                &PokeReaction::default(),
                &drive,
                0.0,
                1,
            )
            .unwrap()
            .0;
        }
        let before = breath(&model, &state.0.as_ref().unwrap().base);
        let reaction = PokeReaction {
            direction: Vec3::X,
            elapsed: 0.2,
            revision: 1,
        };
        let (mut state, pose, event) =
            advance(&model, &state, false, &reaction, &drive, 0.0, 1).unwrap();
        assert_eq!(breath(&model, &pose), before);
        let event = event.unwrap();
        assert_eq!(event.action, ActionKind::Reaction);
        assert!(!event.restarted);
        let ModelData::Sprite2d(sprite) = &model.data else {
            panic!()
        };
        let gaze = sprite.parameter_index("gaze_x").unwrap();
        for _ in 0..12 {
            let (next, pose, _) =
                advance(&model, &state, false, &reaction, &drive, 0.0, 1).unwrap();
            let Pose::Sprite2d(values) = pose else {
                panic!()
            };
            let Pose::Sprite2d(base) = &next.0.as_ref().unwrap().base else {
                panic!()
            };
            assert_eq!(base[gaze], 0.0);
            assert!((values[gaze] - 0.5).abs() < 1e-6);
            state = next;
        }
    }

    #[test]
    fn repeated_same_direction_poke_restarts_without_a_second_clock() {
        let model = load(&document());
        let reaction = PokeReaction {
            direction: Vec3::X,
            elapsed: 0.7,
            revision: 1,
        };
        let (state, _, _) = advance(
            &model,
            &initial(&model),
            false,
            &reaction,
            &DriveInput::none(),
            0.0,
            1,
        )
        .unwrap();
        let repeat = PokeReaction {
            elapsed: 0.02,
            revision: 2,
            ..reaction
        };
        let (next, _, event) =
            advance(&model, &state, false, &repeat, &DriveInput::none(), 0.0, 1).unwrap();
        let event = event.unwrap();
        assert!(event.restarted);
        assert_eq!(event.action, ActionKind::Reaction);
        assert_eq!(next.0.as_ref().unwrap().time, f64::from(repeat.elapsed));
        assert_eq!(next.0.as_ref().unwrap().consumed_revision, 2);
        assert!(
            advance(&model, &next, false, &repeat, &DriveInput::none(), 0.0, 1)
                .unwrap()
                .2
                .is_none()
        );
    }

    #[test]
    fn short_and_long_reaction_clips_follow_the_existing_poke_window() {
        for duration in [0.25, 2.0] {
            let mut doc = document();
            doc["sprite2d"]["animations"][2]["duration"] = json!(duration);
            doc["sprite2d"]["animations"][2]["channels"][0]["keys"][1]["time"] = json!(duration);
            let model = load(&doc);
            let reaction = PokeReaction {
                direction: Vec3::ZERO,
                elapsed: 0.8,
                revision: 1,
            };
            let (mut state, _) =
                initialize(&model, 0.0, 0.0, &reaction, &DriveInput::none(), 1).unwrap();
            let expected = 0.25 + 0.5 * (f64::from(reaction.elapsed) / duration).min(1.0) as f32;
            assert!((breath(&model, &state.0.as_ref().unwrap().base) - expected).abs() < 1e-6);
            for _ in 0..12 {
                state = advance(
                    &model,
                    &state,
                    false,
                    &reaction,
                    &DriveInput::none(),
                    0.0,
                    1,
                )
                .unwrap()
                .0;
            }
            assert_eq!(state.0.as_ref().unwrap().kind, ActionKind::Reaction);
            let ended = PokeReaction {
                elapsed: POKE_REACTION_SECONDS,
                ..reaction
            };
            assert_eq!(
                advance(&model, &state, true, &ended, &DriveInput::none(), 0.0, 1)
                    .unwrap()
                    .0
                    .0
                    .unwrap()
                    .kind,
                ActionKind::Walk
            );
        }
    }

    #[test]
    fn failed_sampling_keeps_clock_pose_revision_and_events() {
        let mut model = load(&document());
        Arc::get_mut(&mut model)
            .unwrap()
            .actions
            .as_mut()
            .unwrap()
            .reaction = usize::MAX;
        let mut simulation = Simulation::new();
        let asset = simulation.insert_asset(model).unwrap();
        let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
        let entity = simulation.entities[&id];
        simulation.drain_action_events();
        let before = simulation.instance(id).unwrap().animation_time;
        let base = breath(
            &simulation.world.get::<ModelHandle>(entity).unwrap().model,
            &simulation.world.get::<PoseState>(entity).unwrap().curr,
        );
        simulation.poke_reaction(id, Vec3::X).unwrap();
        assert!(simulation.advance(FIXED_DT).is_err());
        assert_eq!(simulation.instance(id).unwrap().animation_time, before);
        let state = simulation
            .world
            .get::<ActionState>(entity)
            .unwrap()
            .0
            .as_ref()
            .unwrap();
        assert_eq!(state.kind, ActionKind::Idle);
        assert_eq!(state.consumed_revision, 0);
        assert_eq!(
            breath(
                &simulation.world.get::<ModelHandle>(entity).unwrap().model,
                &simulation.world.get::<PoseState>(entity).unwrap().curr
            ),
            base
        );
        assert!(simulation.drain_action_events().is_empty());
        assert!(simulation.advance(FIXED_DT).is_err());
        assert_eq!(
            simulation
                .world
                .get::<ActionState>(entity)
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .consumed_revision,
            0
        );
    }

    #[test]
    fn poke_generation_exhaustion_is_atomic() {
        let mut simulation = Simulation::new();
        let asset = simulation.insert_asset(load(&document())).unwrap();
        let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
        let entity = simulation.entities[&id];
        simulation
            .world
            .get_mut::<PokeReaction>(entity)
            .unwrap()
            .revision = u64::MAX;
        assert!(matches!(simulation.poke_reaction(id, Vec3::X),
            Err(SimError::PokeRevisionExhausted { instance }) if instance == id));
        let reaction = simulation.world.get::<PokeReaction>(entity).unwrap();
        assert_eq!(reaction.revision, u64::MAX);
        assert_eq!(reaction.elapsed, POKE_REACTION_SECONDS);
        assert_eq!(reaction.direction, Vec3::ZERO);
    }

    #[test]
    fn movement_marker_is_cleared_for_all_early_returns_and_arrival() {
        let mut simulation = Simulation::new();
        let asset = simulation.insert_asset(load(&document())).unwrap();
        let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
        let entity = simulation.entities[&id];
        let bounds = Some(WorldXBounds {
            left: -4.0,
            right: 4.0,
        });
        simulation.set_wander_bounds(bounds).unwrap();
        simulation
            .world
            .get_mut::<WanderState>(entity)
            .unwrap()
            .phase = WanderPhase::Walking { target_x: 1.0 };
        tick(&mut simulation, 1);
        assert_eq!(
            simulation.instance(id).unwrap().action,
            Some(ActionKind::Walk)
        );
        simulation.set_wander_regions(Some(Vec::new())).unwrap();
        tick(&mut simulation, 1);
        assert!(
            !simulation
                .world
                .get::<WanderState>(entity)
                .unwrap()
                .moved_this_tick
        );
        assert_eq!(
            simulation.instance(id).unwrap().action,
            Some(ActionKind::Idle)
        );
        assert!(matches!(
            simulation.world.get::<WanderState>(entity).unwrap().phase,
            WanderPhase::Walking { .. }
        ));
        simulation
            .set_wander_regions(Some(vec![WorldRect {
                left: -4.0,
                right: 4.0,
                bottom: -1.0,
                top: 1.0,
            }]))
            .unwrap();
        tick(&mut simulation, 1);
        assert_eq!(
            simulation.instance(id).unwrap().action,
            Some(ActionKind::Walk)
        );
        simulation.set_wander_bounds(None).unwrap();
        tick(&mut simulation, 1);
        assert_eq!(
            simulation.instance(id).unwrap().action,
            Some(ActionKind::Idle)
        );
        simulation.set_wander_bounds(bounds).unwrap();
        tick(&mut simulation, 1);
        assert_eq!(
            simulation.instance(id).unwrap().action,
            Some(ActionKind::Walk)
        );
        simulation.suspend_wander(id).unwrap();
        tick(&mut simulation, 1);
        assert_eq!(
            simulation.instance(id).unwrap().action,
            Some(ActionKind::Idle)
        );
        simulation.resume_wander(id).unwrap();
        let x = simulation.instance(id).unwrap().position.x;
        simulation
            .world
            .get_mut::<WanderState>(entity)
            .unwrap()
            .phase = WanderPhase::Walking { target_x: x };
        tick(&mut simulation, 1);
        assert_eq!(
            simulation.instance(id).unwrap().action,
            Some(ActionKind::Idle)
        );
        assert!(
            !simulation
                .world
                .get::<WanderState>(entity)
                .unwrap()
                .moved_this_tick
        );
    }

    #[test]
    fn action_selection_does_not_change_legacy_motion_or_random_streams() {
        let bound = document();
        let mut legacy = bound.clone();
        legacy.as_object_mut().unwrap().remove("actions");
        let mut simulations = [Simulation::new(), Simulation::new()];
        for (simulation, doc) in simulations.iter_mut().zip([&bound, &legacy]) {
            let asset = simulation.insert_asset(load(doc)).unwrap();
            simulation.spawn(asset, Vec3::ZERO).unwrap();
            simulation
                .set_wander_bounds(Some(WorldXBounds {
                    left: -3.0,
                    right: 3.0,
                }))
                .unwrap();
        }
        for index in 0..600 {
            for simulation in &mut simulations {
                if index == 220 {
                    let position = simulation.instance(1).unwrap().position;
                    simulation.poke_reaction(1, position + Vec3::X).unwrap();
                }
                if index == 360 {
                    simulation.suspend_wander(1).unwrap();
                }
                if index == 400 {
                    simulation.resume_wander(1).unwrap();
                }
                tick(simulation, 1);
            }
            let [a, b] = &simulations;
            assert_eq!(
                a.instance(1).unwrap().position,
                b.instance(1).unwrap().position
            );
            let wa = a.world.get::<WanderState>(a.entities[&1]).unwrap();
            let wb = b.world.get::<WanderState>(b.entities[&1]).unwrap();
            assert_eq!(wa.phase, wb.phase);
            assert_eq!(wa.rng, wb.rng);
            assert_eq!(wa.facing, wb.facing);
            assert_eq!(wa.suspended, wb.suspended);
        }
    }

    #[test]
    fn replacement_rebuilds_bound_shapes_and_failure_preserves_transition() {
        let model = load(&document());
        let mut simulation = Simulation::new();
        let asset = simulation.insert_asset(model.clone()).unwrap();
        let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
        let entity = simulation.entities[&id];
        simulation
            .set_wander_bounds(Some(WorldXBounds {
                left: -4.0,
                right: 4.0,
            }))
            .unwrap();
        simulation
            .world
            .get_mut::<WanderState>(entity)
            .unwrap()
            .phase = WanderPhase::Walking { target_x: 1.0 };
        tick(&mut simulation, 3);
        let before = simulation
            .world
            .get::<ActionState>(entity)
            .unwrap()
            .0
            .as_ref()
            .unwrap()
            .clone();
        assert!(before.transition.is_some());
        let mut invalid = load(&document());
        Arc::get_mut(&mut invalid)
            .unwrap()
            .actions
            .as_mut()
            .unwrap()
            .idle = usize::MAX;
        let invalid = simulation.insert_asset(invalid).unwrap();
        simulation.drain_action_events();
        assert!(simulation.replace_model(id, invalid).is_err());
        assert_eq!(simulation.instance(id).unwrap().asset, asset);
        assert_eq!(
            simulation
                .world
                .get::<ActionState>(entity)
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .time,
            before.time
        );
        assert_eq!(
            simulation
                .world
                .get::<ActionState>(entity)
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .transition
                .as_ref()
                .unwrap()
                .elapsed,
            before.transition.as_ref().unwrap().elapsed
        );
        assert!(simulation.drain_action_events().is_empty());
        let mut doc = document();
        doc["sprite2d"]["parameters"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"unused", "min":-1.0, "max":1.0, "default":0.0}));
        let replacement = simulation.insert_asset(load(&doc)).unwrap();
        simulation.replace_model(id, replacement).unwrap();
        let state = simulation
            .world
            .get::<ActionState>(entity)
            .unwrap()
            .0
            .as_ref()
            .unwrap();
        assert_eq!(state.kind, ActionKind::Idle);
        assert_eq!(state.time, 0.0);
        assert!(state.transition.is_none());
        simulation.render_frame().unwrap();
        tick(&mut simulation, 1);
        simulation.render_frame().unwrap();
        simulation.poke_reaction(id, Vec3::X).unwrap();
        tick(&mut simulation, 1);
        simulation.replace_model(id, asset).unwrap();
        assert_eq!(
            simulation.instance(id).unwrap().action,
            Some(ActionKind::Reaction)
        );
        assert!(
            simulation
                .world
                .get::<ActionState>(entity)
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .transition
                .is_none()
        );
        let legacy = load_model_bytes(
            include_bytes!("../../assets/examples/robot3d.dmodel.json"),
            &LoadLimits::default(),
        )
        .unwrap();
        let legacy = simulation.insert_asset(legacy).unwrap();
        simulation.replace_model(id, legacy).unwrap();
        assert_eq!(simulation.instance(id).unwrap().action, None);
        simulation.render_frame().unwrap();
    }

    #[test]
    fn bound_local_time_keeps_long_session_precision_and_clones_are_independent() {
        let mut doc = document();
        doc["sprite2d"]["animations"][0]["channels"][0]["keys"] = json!([
            {"time":0.0,"value":0.0}, {"time":1.0,"value":1.0}, {"time":2.0,"value":0.0}
        ]);
        let model = load(&doc);
        let mut simulation = Simulation::new();
        let asset = simulation.insert_asset(model.clone()).unwrap();
        let first = simulation.spawn(asset, Vec3::new(-5.0, 0.0, 0.0)).unwrap();
        let second = simulation.spawn(asset, Vec3::new(5.0, 0.0, 0.0)).unwrap();
        let entity = simulation.entities[&first];
        let other = simulation.entities[&second];
        assert_ne!(
            simulation
                .world
                .get::<ActionState>(entity)
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .time,
            simulation
                .world
                .get::<ActionState>(other)
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .time
        );
        let mut time = 262_144.125;
        simulation
            .world
            .get_mut::<ActionState>(entity)
            .unwrap()
            .0
            .as_mut()
            .unwrap()
            .time = time;
        for _ in 0..12 {
            tick(&mut simulation, 1);
            let playback = simulation
                .world
                .get::<ActionState>(entity)
                .unwrap()
                .0
                .as_ref()
                .unwrap();
            assert!((playback.time - time - FIXED_DT_SECONDS).abs() < 1e-8);
            assert!((breath(&model, &playback.base) - (playback.time % 2.0) as f32).abs() < 1e-6);
            time = playback.time;
        }
        simulation
            .poke_reaction(first, Vec3::new(-4.0, 0.0, 0.0))
            .unwrap();
        tick(&mut simulation, 1);
        assert_eq!(
            simulation.instance(first).unwrap().action,
            Some(ActionKind::Reaction)
        );
        assert_eq!(
            simulation.instance(second).unwrap().action,
            Some(ActionKind::Idle)
        );
    }

    fn skeleton() -> Arc<CompiledModel> {
        let mut doc: Value =
            serde_json::from_slice(include_bytes!("../../assets/examples/robot3d.dmodel.json"))
                .unwrap();
        doc["format_version"] = json!("0.2");
        doc["actions"] = json!({"idle":"idle", "walk":"walk", "reaction":"reaction"});
        doc["skeleton3d"]["animations"] = Value::Array(
            [("idle", 0.0_f32), ("walk", 0.6), ("reaction", 1.2)]
                .into_iter()
                .map(|(name, angle)| {
                    let rotation = glam::Quat::from_rotation_z(angle).to_array();
                    json!({"name":name, "duration":1.0, "channels":[{"joint":"head", "rotation":[
                        {"time":0.0,"value":rotation}, {"time":1.0,"value":rotation}
                    ]}]})
                })
                .collect(),
        );
        load(&doc)
    }

    #[test]
    fn skeleton_actions_slerp_local_rotations_before_a_single_head_drive() {
        let model = skeleton();
        let ModelData::Skeleton3d(skeleton) = &model.data else {
            panic!()
        };
        let head = skeleton.joint_index("head").unwrap();
        let mut state = initial(&model);
        let drive = DriveInput {
            direction: Vec3::X,
            weight: 0.5,
            facing: 1.0,
            turn: 1.0,
        };
        for index in 0..=10 {
            let (next, pose, _) = advance(
                &model,
                &state,
                true,
                &PokeReaction::default(),
                &drive,
                0.0,
                1,
            )
            .unwrap();
            let alpha = (index as f64 * FIXED_DT_SECONDS / TRANSITION_SECONDS).min(1.0) as f32;
            let base_rotation = glam::Quat::from_rotation_z(0.6 * alpha);
            let expected =
                glam::Quat::from_rotation_y(super::super::HEAD_YAW_LIMIT * 0.5) * base_rotation;
            let Pose::Skeleton3d(locals) = &pose else {
                panic!()
            };
            let Pose::Skeleton3d(base) = &next.0.as_ref().unwrap().base else {
                panic!()
            };
            assert!(base[head].rotation.dot(base_rotation).abs() > 1.0 - 1e-6);
            assert!(locals[head].rotation.dot(expected).abs() > 1.0 - 1e-6);
            assert!(
                crate::model::try_skin_matrices(skeleton, locals)
                    .unwrap()
                    .iter()
                    .all(|matrix| matrix.is_finite())
            );
            state = next;
        }
        let mut simulation = Simulation::new();
        let sprite = simulation.insert_asset(load(&document())).unwrap();
        let robot = simulation.insert_asset(model).unwrap();
        let id = simulation.spawn(sprite, Vec3::ZERO).unwrap();
        simulation.poke_reaction(id, Vec3::X).unwrap();
        tick(&mut simulation, 3);
        simulation.replace_model(id, robot).unwrap();
        assert_eq!(
            simulation.instance(id).unwrap().action,
            Some(ActionKind::Reaction)
        );
        simulation.render_frame().unwrap();
        tick(&mut simulation, 1);
        simulation.replace_model(id, sprite).unwrap();
        simulation.render_frame().unwrap();
    }

    #[test]
    fn unbound_playback_keeps_exact_legacy_samples_and_stays_silent() {
        let model = load_model_bytes(
            include_bytes!("../../assets/examples/puppet2d.dmodel.json"),
            &LoadLimits::default(),
        )
        .unwrap();
        let ModelData::Sprite2d(sprite) = &model.data else {
            panic!()
        };
        let mut simulation = Simulation::new();
        let asset = simulation.insert_asset(model.clone()).unwrap();
        let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
        let entity = simulation.entities[&id];
        for _ in 0..20 {
            tick(&mut simulation, 1);
            let time = simulation.world.get::<TimeState>(entity).unwrap().time;
            let expected = try_sample_parameters(sprite, time).unwrap();
            let Pose::Sprite2d(actual) = &simulation.world.get::<PoseState>(entity).unwrap().curr
            else {
                panic!()
            };
            assert_eq!(actual, &expected);
            assert_eq!(simulation.instance(id).unwrap().action, None);
            assert!(simulation.drain_action_events().is_empty());
        }
    }
}
