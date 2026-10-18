mod common;

use std::{sync::Arc, time::Duration};

use delicium::{
    model::{
        LoadLimits, ModelData, ModelKind, load_model_bytes, try_deform_layer, try_sample_parameters,
    },
    renderer::{InstanceDraw, RenderFrame},
    simulation::{
        FIXED_DT, POKE_HOP_AMPLITUDE, Simulation, WANDER_EDGE_MARGIN, WANDER_IDLE_MAX_SECONDS,
        WANDER_IDLE_MIN_SECONDS, WanderEvent, WorldRect, WorldXBounds,
    },
};
use glam::{Mat4, Vec3};
use serde_json::json;

fn action_sprite() -> Arc<delicium::model::CompiledModel> {
    let mut document = common::sprite_document();
    document["format_version"] = json!("0.2");
    document["actions"] = json!({"idle":"idle", "walk":"walk", "reaction":"reaction"});
    document["sprite2d"]["animations"] = json!([
        {"name":"idle", "duration":1.0, "channels":[{"parameter":"sway", "keys":[
            {"time":0.0,"value":0.0}, {"time":1.0,"value":0.0}
        ]}]},
        {"name":"walk", "duration":1.0, "channels":[{"parameter":"sway", "keys":[
            {"time":0.0,"value":-0.8}, {"time":1.0,"value":-0.8}
        ]}]},
        {"name":"reaction", "duration":1.0, "channels":[{"parameter":"sway", "keys":[
            {"time":0.0,"value":0.8}, {"time":1.0,"value":0.8}
        ]}]}
    ]);
    common::load(&document)
}

#[test]
fn bound_actions_follow_a_real_walk_then_return_to_idle_and_drain_once() {
    use delicium::simulation::ActionKind;

    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(action_sprite()).unwrap();
    let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
    assert_eq!(
        simulation.instance(id).unwrap().action,
        Some(ActionKind::Idle)
    );
    let events = simulation.drain_action_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, ActionKind::Idle);
    assert!(simulation.drain_action_events().is_empty());
    simulation
        .set_wander_bounds(Some(WorldXBounds {
            left: -2.0,
            right: 2.0,
        }))
        .unwrap();
    let mut walked = false;
    let mut arrived = false;
    for _ in 0..600 {
        let previous = simulation.instance(id).unwrap().position;
        simulation.advance(FIXED_DT).unwrap();
        let instance = simulation.instance(id).unwrap();
        if instance.action == Some(ActionKind::Walk) {
            assert_ne!(previous.x, instance.position.x);
            walked = true;
        } else if walked && previous == instance.position {
            arrived = true;
            break;
        }
    }
    assert!(walked && arrived);
    let events = simulation.drain_action_events();
    assert_eq!(
        events.iter().map(|event| event.action).collect::<Vec<_>>(),
        [ActionKind::Walk, ActionKind::Idle]
    );
    simulation.advance(Duration::ZERO).unwrap();
    assert!(simulation.drain_action_events().is_empty());
}

#[test]
fn click_priority_repeat_drag_and_resume_are_visible_through_public_state() {
    use delicium::simulation::ActionKind;

    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(action_sprite()).unwrap();
    let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
    simulation
        .set_wander_bounds(Some(WorldXBounds {
            left: -3.0,
            right: 3.0,
        }))
        .unwrap();
    for _ in 0..300 {
        simulation.advance(FIXED_DT).unwrap();
        if simulation.instance(id).unwrap().action == Some(ActionKind::Walk) {
            break;
        }
    }
    assert_eq!(
        simulation.instance(id).unwrap().action,
        Some(ActionKind::Walk)
    );
    let position = simulation.instance(id).unwrap().position;
    simulation.drain_action_events();
    simulation.poke_reaction(id, position + Vec3::X).unwrap();
    step(&mut simulation, 12);
    assert_eq!(
        simulation.instance(id).unwrap().action,
        Some(ActionKind::Reaction)
    );
    assert_eq!(simulation.instance(id).unwrap().position, position);
    simulation.drain_action_events();
    simulation.poke_reaction(id, position + Vec3::X).unwrap();
    simulation.advance(FIXED_DT).unwrap();
    let events = simulation.drain_action_events();
    assert_eq!(events.len(), 1);
    assert!(events[0].restarted);
    simulation.suspend_wander(id).unwrap();
    step(&mut simulation, 61);
    assert_eq!(
        simulation.instance(id).unwrap().action,
        Some(ActionKind::Idle)
    );
    assert_eq!(simulation.instance(id).unwrap().position, position);
    step(&mut simulation, 300);
    assert_eq!(
        simulation.instance(id).unwrap().action,
        Some(ActionKind::Idle)
    );
    simulation.resume_wander(id).unwrap();
    simulation.advance(FIXED_DT).unwrap();
    assert_eq!(
        simulation.instance(id).unwrap().action,
        Some(ActionKind::Idle)
    );
    for _ in 0..300 {
        simulation.advance(FIXED_DT).unwrap();
        if simulation.instance(id).unwrap().action == Some(ActionKind::Walk) {
            break;
        }
    }
    assert_eq!(
        simulation.instance(id).unwrap().action,
        Some(ActionKind::Walk)
    );
    assert_ne!(simulation.instance(id).unwrap().position, position);
}

#[test]
fn fixed_step_is_bounded_and_residual_is_interpolated() {
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(common::sprite()).unwrap();
    simulation.spawn(asset, Vec3::ZERO).unwrap();
    let report = simulation.advance(Duration::from_millis(25)).unwrap();
    assert_eq!(report.steps, 1);
    assert_eq!(simulation.tick_count(), 1);
    assert!((simulation.alpha() - 0.5).abs() < 0.01);
    let report = simulation.advance(Duration::from_secs(1)).unwrap();
    assert_eq!(report.steps, 4);
    assert!(report.dropped_steps > 0);
    assert_eq!(simulation.tick_count(), 5);
    assert!((0.0..1.0).contains(&simulation.alpha()));
}

#[test]
fn static_asset_is_shared_but_animation_state_is_independent() {
    let model = common::sprite();
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(model.clone()).unwrap();
    let first = simulation.spawn(asset, Vec3::new(-1.0, 0.0, 0.0)).unwrap();
    let second = simulation.spawn(asset, Vec3::new(1.0, 0.0, 0.0)).unwrap();
    simulation.advance(Duration::from_millis(50)).unwrap();
    assert!(Arc::ptr_eq(simulation.asset(asset).unwrap(), &model));
    let a = simulation.instance(first).unwrap();
    let b = simulation.instance(second).unwrap();
    assert_eq!(a.asset, b.asset);
    assert_ne!(a.animation_time, b.animation_time);
    let frame = simulation.render_frame().unwrap();
    let vertices: Vec<_> = frame
        .instances
        .iter()
        .filter_map(|draw| match draw {
            InstanceDraw::Sprite2d { layers, .. } => Some(layers),
            _ => None,
        })
        .collect();
    assert_eq!(vertices.len(), 2);
    assert_ne!(vertices[0], vertices[1]);
}

fn interacting_simulation(sprite_x: f32) -> Simulation {
    let mut sprite = common::sprite_document();
    sprite["interaction"] = json!({"gaze_parameter": "sway"});
    let mut skeleton = common::skeleton_document();
    skeleton["interaction"] = json!({"head_joint": "head"});
    let mut simulation = Simulation::new();
    let sprite = simulation.insert_asset(common::load(&sprite)).unwrap();
    let skeleton = simulation.insert_asset(common::load(&skeleton)).unwrap();
    simulation
        .spawn(sprite, Vec3::new(sprite_x, 0.0, 0.0))
        .unwrap();
    simulation
        .spawn(skeleton, Vec3::new(0.8, 0.0, 0.0))
        .unwrap();
    simulation.advance(Duration::from_millis(60)).unwrap();
    simulation
}

#[test]
fn cross_type_interactions_have_visible_pose_effects() {
    let nearby = interacting_simulation(-0.8);
    let distant = interacting_simulation(-5.0);
    assert_eq!(nearby.interactions().len(), 2);
    assert!(distant.interactions().is_empty());
    for edge in nearby.interactions() {
        assert_ne!(
            nearby.instance(edge.from).unwrap().kind,
            nearby.instance(edge.to).unwrap().kind
        );
        assert!((edge.distance - 1.6).abs() < 0.001);
    }
    let a = nearby.render_frame().unwrap();
    let b = distant.render_frame().unwrap();
    let a_2d = a
        .instances
        .iter()
        .find_map(|draw| match draw {
            InstanceDraw::Sprite2d { layers, .. } => Some(layers),
            _ => None,
        })
        .unwrap();
    let b_2d = b
        .instances
        .iter()
        .find_map(|draw| match draw {
            InstanceDraw::Sprite2d { layers, .. } => Some(layers),
            _ => None,
        })
        .unwrap();
    assert_ne!(a_2d, b_2d);
    let a_3d = a
        .instances
        .iter()
        .find_map(|draw| match draw {
            InstanceDraw::Skeleton3d { skin_matrices, .. } => Some(skin_matrices),
            _ => None,
        })
        .unwrap();
    let b_3d = b
        .instances
        .iter()
        .find_map(|draw| match draw {
            InstanceDraw::Skeleton3d { skin_matrices, .. } => Some(skin_matrices),
            _ => None,
        })
        .unwrap();
    assert_ne!(a_3d, b_3d);
}

#[test]
fn skeletal_interpolation_preserves_rotation_instead_of_lerping_matrices() {
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(common::skeleton()).unwrap();
    simulation.spawn(asset, Vec3::ZERO).unwrap();
    simulation.advance(Duration::from_millis(25)).unwrap();
    let frame = simulation.render_frame().unwrap();
    let InstanceDraw::Skeleton3d { skin_matrices, .. } = &frame.instances[0] else {
        panic!("wrong kind");
    };
    for matrix in skin_matrices {
        assert!(matrix.is_finite());
        assert!((matrix.determinant() - 1.0).abs() < 0.00001);
    }
}

#[test]
fn failed_replace_preserves_old_state_and_valid_replace_changes_kind() {
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(common::sprite()).unwrap();
    let instance = simulation.spawn(asset, Vec3::new(1.0, 2.0, 0.0)).unwrap();
    simulation.advance(Duration::from_millis(50)).unwrap();
    let before = simulation.instance(instance).unwrap();
    let before_frame = simulation.render_frame().unwrap();
    assert!(load_model_bytes(b"{invalid", &LoadLimits::default()).is_err());
    assert!(simulation.replace_model(instance, u64::MAX).is_err());
    let mut broken = Arc::try_unwrap(common::sprite()).unwrap();
    let ModelData::Sprite2d(sprite) = &mut broken.data else {
        panic!("expected 2D model")
    };
    sprite.animations[0].channels[0].parameter = usize::MAX;
    let rejected = simulation.insert_asset(Arc::new(broken)).unwrap();
    assert!(simulation.replace_model(instance, rejected).is_err());
    assert!(simulation.discard_asset(rejected));
    let after_frame = simulation.render_frame().unwrap();
    let InstanceDraw::Sprite2d {
        layers: before_layers,
        transform: before_transform,
        ..
    } = &before_frame.instances[0]
    else {
        panic!("expected 2D frame")
    };
    let InstanceDraw::Sprite2d {
        layers: after_layers,
        transform: after_transform,
        ..
    } = &after_frame.instances[0]
    else {
        panic!("expected 2D frame")
    };
    assert_eq!(after_layers, before_layers);
    assert_eq!(after_transform, before_transform);
    let after = simulation.instance(instance).unwrap();
    assert_eq!(after.asset, before.asset);
    assert_eq!(after.animation_time, before.animation_time);
    assert_eq!(after.position, before.position);
    let new_asset = simulation.insert_asset(common::skeleton()).unwrap();
    simulation.replace_model(instance, new_asset).unwrap();
    let after = simulation.instance(instance).unwrap();
    assert_eq!(after.asset, new_asset);
    assert_eq!(after.kind, ModelKind::Skeleton3d);
    assert_eq!(after.position, before.position);
    assert!(matches!(
        simulation.render_frame().unwrap().instances[0],
        InstanceDraw::Skeleton3d { .. }
    ));
    assert_eq!(simulation.collect_unused_assets(&[]), vec![asset]);
    assert!(simulation.asset(asset).is_none());
}

#[test]
fn replacement_migrates_parameters_by_name_and_clamps_only_when_needed() {
    for (min, max, animation_value) in [(-1.0_f32, 1.0_f32, 1.0_f32), (0.0, 0.1, 0.1)] {
        let old_model = common::sprite();
        let ModelData::Sprite2d(old_sprite) = &old_model.data else {
            panic!("expected 2D model")
        };
        let mut simulation = Simulation::new();
        let old_asset = simulation.insert_asset(old_model.clone()).unwrap();
        let id = simulation.spawn(old_asset, Vec3::ZERO).unwrap();
        simulation.advance(Duration::from_millis(50)).unwrap();
        let time = simulation.instance(id).unwrap().animation_time;
        let old_value = try_sample_parameters(old_sprite, time).unwrap()[0];
        let mut document = common::sprite_document();
        document["sprite2d"]["parameters"] = json!([
            {"name": "extra", "min": 0.0, "max": 1.0, "default": 0.7},
            {"name": "sway", "min": min, "max": max, "default": 0.0}
        ]);
        document["sprite2d"]["animations"][0]["channels"] = json!([
            {"parameter": "sway", "keys": [
                {"time": 0.0, "value": animation_value},
                {"time": 2.0, "value": animation_value}
            ]},
            {"parameter": "extra", "keys": [
                {"time": 0.0, "value": 0.3}, {"time": 2.0, "value": 0.3}
            ]}
        ]);
        document["sprite2d"]["deformers"][0]["parent"] = json!("extra_shift");
        document["sprite2d"]["deformers"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "name": "extra_shift", "type": "warp", "parameter": "extra",
                "bounds": [-1.0, -1.0, 2.0, 3.0], "grid": [2, 2],
                "offsets": [[0.5, 0.0], [0.5, 0.0], [0.5, 0.0], [0.5, 0.0]]
            }));
        let model = common::load(&document);
        let ModelData::Sprite2d(sprite) = &model.data else {
            panic!("expected 2D model")
        };
        let asset = simulation.insert_asset(model.clone()).unwrap();
        simulation.replace_model(id, asset).unwrap();
        assert_eq!(simulation.instance(id).unwrap().animation_time, time);
        let expected = try_deform_layer(sprite, 0, &[0.3, old_value.clamp(min, max)]).unwrap();
        let frame = simulation.render_frame().unwrap();
        let InstanceDraw::Sprite2d { layers, .. } = &frame.instances[0] else {
            panic!("expected 2D frame")
        };
        assert_eq!(layers[0], expected);
    }
}

#[test]
fn asset_budget_covers_active_models_pins_and_staging_without_evicting_live_assets() {
    let mut simulation = Simulation::new();
    let model = common::sprite();
    let pins = [
        simulation.insert_asset(model.clone()).unwrap(),
        simulation.insert_asset(model.clone()).unwrap(),
    ];
    let mut live = Vec::new();
    for index in 0..64 {
        let asset = simulation.insert_asset(model.clone()).unwrap();
        simulation
            .spawn(asset, Vec3::new(index as f32, 0.0, 0.0))
            .unwrap();
        live.push(asset);
    }
    let staging: Vec<_> = (0..4)
        .map(|_| simulation.insert_asset(model.clone()).unwrap())
        .collect();
    assert_eq!(simulation.asset_count(), 70);
    assert!(simulation.insert_asset(model).is_err());
    assert_eq!(simulation.asset_count(), 70);
    assert_eq!(simulation.collect_unused_assets(&pins), staging);
    assert_eq!(simulation.asset_count(), 66);
    for asset in live {
        assert!(simulation.asset(asset).is_some());
        assert!(!simulation.discard_asset(asset));
    }
    for asset in pins {
        assert!(simulation.asset(asset).is_some());
    }
}

#[test]
fn position_validation_instance_cap_and_asset_collection() {
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(common::sprite()).unwrap();
    let pinned = simulation.insert_asset(common::skeleton()).unwrap();
    let first = simulation.spawn(asset, Vec3::ZERO).unwrap();
    assert!(
        simulation
            .set_position(first, Vec3::new(f32::NAN, 0.0, 0.0))
            .is_err()
    );
    assert_eq!(simulation.instance(first).unwrap().position, Vec3::ZERO);
    for index in 1..64 {
        simulation
            .spawn(asset, Vec3::new(index as f32 * 0.1, 0.0, 0.0))
            .unwrap();
    }
    assert!(simulation.spawn(asset, Vec3::ZERO).is_err());
    assert!(!simulation.discard_asset(asset));
    assert!(simulation.collect_unused_assets(&[pinned]).is_empty());
    let ids: Vec<_> = simulation.instances().iter().map(|info| info.id).collect();
    for id in ids {
        assert!(simulation.despawn(id));
    }
    assert_eq!(simulation.collect_unused_assets(&[pinned]), vec![asset]);
    assert!(simulation.discard_asset(pinned));
    assert_eq!(simulation.asset_count(), 0);
}

#[test]
fn removing_neighbor_clears_interaction_without_waiting_for_a_tick() {
    let mut simulation = interacting_simulation(-0.8);
    let instances = simulation.instances();
    assert!(simulation.despawn(instances[0].id));
    assert!(
        simulation
            .interactions()
            .iter()
            .all(|edge| edge.from != instances[0].id && edge.to != instances[0].id)
    );
    assert_eq!(simulation.instance(instances[1].id).unwrap().neighbor, None);
}

// -- autonomous wandering ------------------------------------------------

/// The walkable range tests use: the 960x640 bounded scene at 1.5 scale with
/// its 1.2-unit edge margin removed.
fn walkable_bounds() -> WorldXBounds {
    WorldXBounds {
        left: -3.375,
        right: 3.375,
    }
}

fn step(simulation: &mut Simulation, ticks: u32) {
    for _ in 0..ticks {
        let report = simulation.advance(FIXED_DT).unwrap();
        assert_eq!(report.steps, 1);
        assert_eq!(report.dropped_steps, 0);
    }
}

/// Runs a simulation of one sprite with the walkable bounds and the given
/// start position, tick by tick, and returns it with its instance id.
fn wandering_simulation(start_x: f32, ticks: u32) -> (Simulation, u64) {
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(common::sprite()).unwrap();
    let instance = simulation
        .spawn(asset, Vec3::new(start_x, 0.15, 0.0))
        .unwrap();
    simulation
        .set_wander_bounds(Some(walkable_bounds()))
        .unwrap();
    step(&mut simulation, ticks);
    (simulation, instance)
}

#[test]
fn adjacent_regions_preserve_the_bounds_only_position_and_rng_stream() {
    let (mut original, id) = wandering_simulation(0.0, 0);
    let (mut desktop, other_id) = wandering_simulation(0.0, 0);
    desktop
        .set_wander_regions(Some(vec![
            WorldRect {
                left: -3.375,
                right: 0.0,
                bottom: -1.0,
                top: 1.0,
            },
            WorldRect {
                left: 0.0,
                right: 3.375,
                bottom: -1.0,
                top: 1.0,
            },
        ]))
        .unwrap();
    let mut left = false;
    let mut right = false;
    for _ in 0..1800 {
        step(&mut original, 1);
        step(&mut desktop, 1);
        let position = desktop.instance(other_id).unwrap().position;
        assert_eq!(original.instance(id).unwrap().position, position);
        assert_eq!(
            original.drain_wander_events(),
            desktop.drain_wander_events()
        );
        left |= position.x < -0.5;
        right |= position.x > 0.5;
    }
    assert!(
        left || right,
        "the equivalence check must exercise walking, not only idle ticks"
    );
    desktop.set_wander_regions(None).unwrap();
    for _ in 0..120 {
        step(&mut original, 1);
        step(&mut desktop, 1);
        assert_eq!(
            original.instance(id).unwrap().position,
            desktop.instance(other_id).unwrap().position
        );
        assert_eq!(
            original.drain_wander_events(),
            desktop.drain_wander_events()
        );
    }
}

#[test]
fn staggered_and_disconnected_regions_do_not_walk_through_invisible_space() {
    let bounds = WorldXBounds {
        left: -6.0,
        right: 13.5,
    };
    let regions = vec![
        WorldRect {
            left: -6.0,
            right: 6.0,
            bottom: -0.9,
            top: 6.6,
        },
        WorldRect {
            left: 6.0,
            right: 13.5,
            bottom: -3.0046875,
            top: 8.995313,
        },
    ];
    for y in [-2.0, 8.0] {
        let (mut simulation, id) = wandering_simulation(9.0, 0);
        simulation.set_wander_bounds(Some(bounds)).unwrap();
        simulation
            .set_wander_regions(Some(regions.clone()))
            .unwrap();
        simulation.set_position(id, Vec3::new(9.0, y, 0.0)).unwrap();
        let mut moved = false;
        for _ in 0..900 {
            step(&mut simulation, 1);
            let p = simulation.instance(id).unwrap().position;
            assert!(p.x >= 7.2 - 1e-5 && p.x <= 12.3 + 1e-5);
            assert_eq!(p.y, y);
            moved |= (p.x - 9.0).abs() > 0.1;
        }
        assert!(moved);
        simulation.set_position(id, Vec3::new(0.0, y, 0.0)).unwrap();
        step(&mut simulation, 180);
        assert_eq!(
            simulation.instance(id).unwrap().position,
            Vec3::new(0.0, y, 0.0)
        );
    }
    let (mut simulation, id) = wandering_simulation(-3.0, 0);
    simulation
        .set_wander_bounds(Some(WorldXBounds {
            left: -6.0,
            right: 6.0,
        }))
        .unwrap();
    simulation
        .set_wander_regions(Some(vec![
            WorldRect {
                left: -6.0,
                right: -1.0,
                bottom: -1.0,
                top: 1.0,
            },
            WorldRect {
                left: 1.0,
                right: 6.0,
                bottom: -1.0,
                top: 1.0,
            },
        ]))
        .unwrap();
    for _ in 0..900 {
        step(&mut simulation, 1);
        assert!(simulation.instance(id).unwrap().position.x <= -2.2 + 1e-5);
    }
}

#[test]
fn visible_regions_validate_atomically_and_resume_after_pause_or_replacement() {
    let (mut simulation, id) = wandering_simulation(0.0, 0);
    let narrow = WorldRect {
        left: -0.5,
        right: 0.5,
        bottom: -1.0,
        top: 1.0,
    };
    simulation.set_wander_regions(Some(vec![narrow])).unwrap();
    for bad in [
        WorldRect {
            left: f32::NAN,
            ..narrow
        },
        WorldRect {
            right: -0.5,
            ..narrow
        },
        WorldRect {
            top: -1.0,
            ..narrow
        },
    ] {
        assert!(simulation.set_wander_regions(Some(vec![bad])).is_err());
    }
    step(&mut simulation, 180);
    assert_eq!(simulation.instance(id).unwrap().position.x, 0.0);
    simulation.set_wander_regions(Some(vec![])).unwrap();
    simulation
        .set_position(id, Vec3::new(0.3, 0.15, 0.0))
        .unwrap();
    step(&mut simulation, 180);
    assert_eq!(simulation.instance(id).unwrap().position.x, 0.3);
    simulation.set_wander_regions(Some(vec![narrow])).unwrap();
    simulation.suspend_wander(id).unwrap();
    step(&mut simulation, 180);
    assert_eq!(simulation.instance(id).unwrap().position.x, 0.3);
    simulation.resume_wander(id).unwrap();
    simulation
        .poke_reaction(id, Vec3::new(0.5, 0.15, 0.0))
        .unwrap();
    step(&mut simulation, 30);
    assert_eq!(simulation.instance(id).unwrap().position.x, 0.3);
    step(&mut simulation, 240);
    assert_eq!(simulation.instance(id).unwrap().position.x, 0.0);
}

#[test]
fn wander_without_bounds_keeps_positions_and_stays_silent() {
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(common::sprite()).unwrap();
    let instance = simulation.spawn(asset, Vec3::new(-2.4, 0.15, 0.0)).unwrap();
    let baseline = simulation.render_frame().unwrap();
    step(&mut simulation, 600);
    assert_eq!(
        simulation.instance(instance).unwrap().position,
        Vec3::new(-2.4, 0.15, 0.0)
    );
    assert!(simulation.drain_wander_events().is_empty());
    // The transform stays the exact identity-scaled translation: the facing
    // is +1.0 while no walk has ever run.
    let frame = simulation.render_frame().unwrap();
    for (before, after) in baseline.instances.iter().zip(frame.instances.iter()) {
        match (before, after) {
            (
                InstanceDraw::Sprite2d {
                    transform: before, ..
                },
                InstanceDraw::Sprite2d {
                    transform: after, ..
                },
            ) => assert_eq!(before, after),
            _ => panic!("expected sprite draws"),
        }
    }
}

#[test]
fn wander_is_deterministic_across_identical_simulations() {
    let (mut first, first_id) = wandering_simulation(0.0, 1200);
    let (mut second, second_id) = wandering_simulation(0.0, 1200);
    assert_eq!(first_id, second_id);
    assert_eq!(
        first.instance(first_id).unwrap().position,
        second.instance(second_id).unwrap().position
    );
    // Compare the pending tails, then walk further tick by tick in lockstep:
    // positions and event sequences must stay identical.
    assert_eq!(first.drain_wander_events(), second.drain_wander_events());
    for _ in 0..120 {
        step(&mut first, 1);
        step(&mut second, 1);
        assert_eq!(
            first.instance(first_id).unwrap().position,
            second.instance(second_id).unwrap().position
        );
        assert_eq!(first.drain_wander_events(), second.drain_wander_events());
    }
}

#[test]
fn wander_walks_inside_bounds_with_constant_y_and_returns_to_idle() {
    let (mut simulation, instance) = wandering_simulation(0.0, 0);
    let lo = walkable_bounds().left + WANDER_EDGE_MARGIN;
    let hi = walkable_bounds().right - WANDER_EDGE_MARGIN;
    let mut events = Vec::new();
    let mut previous = simulation.instance(instance).unwrap().position;
    // Tick of the last Arrive: the next Start opens the next idle span.
    let mut idle_start_tick = 0u32;
    let mut idle_spans: Vec<u32> = Vec::new();
    // Standing-still invariant: a tick that starts idle with no Start event
    // must not move the instance. (The Arrive tick itself snaps onto the
    // target; the Start tick itself leaves idle.)
    let mut standing = false;
    for tick in 1..=3600u32 {
        let was_standing = standing;
        step(&mut simulation, 1);
        let position = simulation.instance(instance).unwrap().position;
        assert!(
            position.x >= lo - 1e-6 && position.x <= hi + 1e-6,
            "x={} outside [{lo}, {hi}] at tick {tick}",
            position.x
        );
        assert_eq!(position.y, 0.15);
        assert_eq!(position.z, 0.0);
        let mut started_this_tick = false;
        for event in simulation.drain_wander_events() {
            match event {
                WanderEvent::Start { .. } => {
                    idle_spans.push(tick - idle_start_tick);
                    standing = false;
                    started_this_tick = true;
                }
                WanderEvent::Arrive { .. } => {
                    idle_start_tick = tick;
                    standing = true;
                }
            }
            events.push(event);
        }
        if was_standing && !started_this_tick {
            assert_eq!(position, previous, "moved while idle at tick {tick}");
        }
        previous = position;
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, WanderEvent::Start { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, WanderEvent::Arrive { .. }))
    );
    // Every closed idle span lasted within the configured window (the first
    // starts at spawn; a still-running span simply has no Start yet). The
    // countdown accumulates f32 rounding over up to 270 steps, so allow a
    // small absolute slack on each end.
    assert!(idle_spans.len() >= 2, "not enough idle spans to check");
    let seconds_per_tick = FIXED_DT.as_secs_f64();
    for ticks in &idle_spans {
        let seconds = *ticks as f64 * seconds_per_tick;
        assert!(
            seconds >= WANDER_IDLE_MIN_SECONDS as f64 - 0.02
                && seconds <= WANDER_IDLE_MAX_SECONDS as f64 + 0.02,
            "idle span {seconds}s outside [{WANDER_IDLE_MIN_SECONDS}, {WANDER_IDLE_MAX_SECONDS}]"
        );
    }
}

#[test]
fn wander_recovers_a_position_placed_outside_the_bounds() {
    let (mut simulation, instance) = wandering_simulation(0.0, 0);
    simulation
        .set_position(instance, Vec3::new(4.0, 0.15, 0.0))
        .unwrap();
    step(&mut simulation, 1);
    let after_one = simulation.instance(instance).unwrap().position;
    assert!(
        after_one.x < 4.0,
        "recovery walk did not start: {after_one}"
    );
    assert_eq!(after_one.y, 0.15);
    // The recovery ignores the idle countdown, so the second tick moves on.
    step(&mut simulation, 1);
    assert!(simulation.instance(instance).unwrap().position.x < after_one.x);
    // Eventually arrives inside the walkable range.
    step(&mut simulation, 2000);
    let settle = simulation.instance(instance).unwrap().position.x;
    let hi = walkable_bounds().right - WANDER_EDGE_MARGIN;
    assert!(settle <= hi + 1e-6, "x={settle} still outside ({hi})");
}

#[test]
fn suspend_wander_parks_an_instance_and_release_resumes_it() {
    let (mut simulation, instance) = wandering_simulation(0.0, 0);
    step(&mut simulation, 300);
    simulation.suspend_wander(instance).unwrap();
    let parked = simulation.instance(instance).unwrap().position;
    // Longer than one full idle period: a mere timer reset would let the
    // instance walk away again, a parked instance must not move at all.
    let long = (WANDER_IDLE_MAX_SECONDS / FIXED_DT.as_secs_f32()) as u32 + 120;
    step(&mut simulation, long);
    assert_eq!(simulation.instance(instance).unwrap().position, parked);
    let _ = simulation.drain_wander_events();
    simulation.resume_wander(instance).unwrap();
    // The fresh idle means no movement on the first resumed tick.
    step(&mut simulation, 1);
    assert_eq!(simulation.instance(instance).unwrap().position, parked);
    step(
        &mut simulation,
        (WANDER_IDLE_MAX_SECONDS / FIXED_DT.as_secs_f32()) as u32 + 2,
    );
    assert!(
        simulation
            .drain_wander_events()
            .iter()
            .any(|event| matches!(event, WanderEvent::Start { .. })),
        "no new walk after resume"
    );
    assert!(simulation.suspend_wander(9999).is_err());
    assert!(simulation.resume_wander(9999).is_err());
}

#[test]
fn wander_facing_flips_the_mirror_sign_in_render_transforms() {
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(common::sprite()).unwrap();
    simulation.spawn(asset, Vec3::new(-2.0, 0.15, 0.0)).unwrap();
    simulation
        .set_wander_bounds(Some(walkable_bounds()))
        .unwrap();
    // Wait for the first walk and use its target to know the expected
    // facing; then verify the m00 mirror sign both while walking and after
    // arrival (the facing must be kept while idling).
    let mut started = None;
    for _ in 0..600 {
        step(&mut simulation, 1);
        for event in simulation.drain_wander_events() {
            if let WanderEvent::Start { target_x, x, .. } = event {
                started = Some((target_x, x));
            }
        }
        if let Some((target_x, x)) = started {
            let expected = if target_x > x { 1.0 } else { -1.0 };
            let frame = simulation.render_frame().unwrap();
            let InstanceDraw::Sprite2d { transform, .. } = &frame.instances[0] else {
                panic!("expected sprite draw");
            };
            assert_eq!(transform.x_axis.x, expected, "mirror sign at walk start");
            for _ in 0..4000 {
                step(&mut simulation, 1);
                if simulation
                    .drain_wander_events()
                    .iter()
                    .any(|event| matches!(event, WanderEvent::Arrive { .. }))
                {
                    break;
                }
            }
            let frame = simulation.render_frame().unwrap();
            let InstanceDraw::Sprite2d { transform, .. } = &frame.instances[0] else {
                panic!("expected sprite draw");
            };
            assert_eq!(transform.x_axis.x, expected, "facing kept while idle");
            return;
        }
    }
    panic!("no walk started within 600 ticks");
}

#[test]
fn skeleton_walking_left_turns_the_body_without_mirroring_it() {
    let mut simulation = Simulation::new();
    let asset = simulation
        .insert_asset(common::load(&common::turned_skeleton_document()))
        .unwrap();
    let instance = simulation.spawn(asset, Vec3::new(0.0, 0.15, 0.0)).unwrap();
    // Bounds entirely left of the spawn: the recovery walk is deterministically
    // leftward without depending on the instance's random stream.
    simulation
        .set_wander_bounds(Some(WorldXBounds {
            left: -3.0,
            right: 0.0,
        }))
        .unwrap();
    let mut moved_left = false;
    let mut settled_at = None;
    for step_index in 0..40 {
        let before = simulation.instance(instance).unwrap().position.x;
        step(&mut simulation, 1);
        let after = simulation.instance(instance).unwrap().position.x;
        moved_left |= after < before;
        let frame = simulation.render_frame().unwrap();
        let InstanceDraw::Skeleton3d {
            transform,
            skin_matrices,
            ..
        } = &frame.instances[0]
        else {
            panic!("expected a skeleton draw");
        };
        // The root is never mirrored, so asymmetric prints stay readable.
        assert_eq!(
            transform.x_axis.x, 1.0,
            "a 3D root must stay unmirrored while walking left"
        );
        assert_eq!(transform.x_axis.y, 0.0);
        assert_eq!(transform.x_axis.z, 0.0);
        // The body turns toward the walk direction: the root's forward rotates
        // to -X. The fixture holds the bundled asset's 25 degrees toward +X, so
        // an unturned walk would keep it positive and read as walking backwards.
        // The turn is eased, not snapped, so convergence gets a few ticks.
        let forward = transform.transform_vector3(skin_matrices[0].transform_vector3(Vec3::Z));
        assert!(forward.y.abs() < 1e-4, "turn must stay purely horizontal");
        if forward.x < -0.1 {
            settled_at.get_or_insert(step_index);
        } else if let Some(settled) = settled_at {
            panic!("step {step_index}: the turn regressed after settling at {settled}");
        }
    }
    let settled_at = settled_at.expect("the leftward walk never turned the body");
    assert!(
        settled_at <= 12,
        "the eased turn took {settled_at} ticks to face left"
    );
    assert!(moved_left, "the fixture must actually walk left");
    // Arriving keeps the turned orientation instead of snapping back.
    step(&mut simulation, 400);
    let frame = simulation.render_frame().unwrap();
    let InstanceDraw::Skeleton3d {
        transform,
        skin_matrices,
        ..
    } = &frame.instances[0]
    else {
        panic!("expected a skeleton draw");
    };
    assert_eq!(transform.x_axis.x, 1.0, "3D facing must never mirror");
    let forward = transform.transform_vector3(skin_matrices[0].transform_vector3(Vec3::Z));
    assert!(
        forward.x < -0.1,
        "the turn must be kept while idling after the walk: {forward:?}"
    );
}

#[test]
fn skeleton_walking_right_keeps_the_authored_orientation() {
    let mut simulation = Simulation::new();
    let asset = simulation
        .insert_asset(common::load(&common::turned_skeleton_document()))
        .unwrap();
    let instance = simulation.spawn(asset, Vec3::new(0.0, 0.15, 0.0)).unwrap();
    // Bounds entirely right of the spawn: a deterministic rightward recovery.
    simulation
        .set_wander_bounds(Some(WorldXBounds {
            left: 0.0,
            right: 3.0,
        }))
        .unwrap();
    let mut moved_right = false;
    for _ in 0..40 {
        let before = simulation.instance(instance).unwrap().position.x;
        step(&mut simulation, 1);
        let after = simulation.instance(instance).unwrap().position.x;
        moved_right |= after > before;
        let frame = simulation.render_frame().unwrap();
        let InstanceDraw::Skeleton3d {
            transform,
            skin_matrices,
            ..
        } = &frame.instances[0]
        else {
            panic!("expected a skeleton draw");
        };
        assert_eq!(transform.x_axis.x, 1.0);
        let forward = transform.transform_vector3(skin_matrices[0].transform_vector3(Vec3::Z));
        assert!(
            forward.x > 0.1,
            "walking right must keep the authored three-quarter stance: {forward:?}"
        );
    }
    assert!(moved_right, "the fixture must actually walk right");
}

#[test]
fn wander_bounds_update_retargets_an_ongoing_walk() {
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(common::sprite()).unwrap();
    let instance = simulation.spawn(asset, Vec3::new(0.0, 0.15, 0.0)).unwrap();
    simulation
        .set_wander_bounds(Some(WorldXBounds {
            left: -6.0,
            right: 6.0,
        }))
        .unwrap();
    // Start a walk inside the wide range.
    let mut started = false;
    for _ in 0..600 {
        step(&mut simulation, 1);
        if simulation
            .drain_wander_events()
            .iter()
            .any(|event| matches!(event, WanderEvent::Start { .. }))
        {
            started = true;
            break;
        }
    }
    assert!(started, "no walk started");
    // Shrink the range mid-walk (as a window resize would). The instance may
    // temporarily sit between the new walkable bound and the new visible
    // bound; it must never leave the visible range and must walk back in.
    simulation
        .set_wander_bounds(Some(WorldXBounds {
            left: -2.0,
            right: 2.0,
        }))
        .unwrap();
    let hi = 2.0 - WANDER_EDGE_MARGIN;
    let mut arrived = false;
    for _ in 0..4000 {
        step(&mut simulation, 1);
        let position = simulation.instance(instance).unwrap().position;
        assert!(
            position.x <= 2.0 + 1e-6,
            "x={} beyond the visible range",
            position.x
        );
        assert_eq!(position.y, 0.15);
        if simulation
            .drain_wander_events()
            .iter()
            .any(|event| matches!(event, WanderEvent::Arrive { .. }))
        {
            arrived = true;
            break;
        }
    }
    assert!(arrived, "never settled inside the small range");
    assert!(
        simulation.instance(instance).unwrap().position.x <= hi + 1e-6,
        "arrival outside the walkable range"
    );
}

#[test]
fn wander_rejects_invalid_bounds_and_keeps_the_previous_ones() {
    let (mut first, first_id) = wandering_simulation(0.0, 0);
    let (mut second, second_id) = wandering_simulation(0.0, 0);
    for invalid in [
        WorldXBounds {
            left: f32::NAN,
            right: 1.0,
        },
        WorldXBounds {
            left: 0.0,
            right: f32::INFINITY,
        },
        WorldXBounds {
            left: 1.0,
            right: -1.0,
        },
    ] {
        assert!(first.set_wander_bounds(Some(invalid)).is_err());
        // Only the first simulation sees the errors; the second stays the
        // untouched control. Both must still agree tick for tick.
        step(&mut first, 1);
        step(&mut second, 1);
        assert_eq!(
            first.instance(first_id).unwrap().position,
            second.instance(second_id).unwrap().position
        );
    }
    assert_eq!(first.wander_bounds(), Some(walkable_bounds()));
}

// -- poke reactions -------------------------------------------------------

/// One sprite with the poke-friendly interaction binding at `start_x`, ticked
/// a few times so the pose ring is warm; no wander bounds, so it never walks.
fn poke_simulation(start_x: f32) -> (Simulation, u64) {
    let mut sprite = common::sprite_document();
    sprite["interaction"] = json!({"gaze_parameter": "sway"});
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(common::load(&sprite)).unwrap();
    let instance = simulation
        .spawn(asset, Vec3::new(start_x, 0.15, 0.0))
        .unwrap();
    simulation.advance(Duration::from_millis(60)).unwrap();
    (simulation, instance)
}

/// The transform of the single drawing in `frame`.
fn transform_of(frame: &RenderFrame, instance: u64) -> Mat4 {
    frame
        .instances
        .iter()
        .find_map(|draw| match draw {
            InstanceDraw::Sprite2d {
                instance: id,
                transform,
                ..
            }
            | InstanceDraw::Skeleton3d {
                instance: id,
                transform,
                ..
            } if *id == instance => Some(*transform),
            _ => None,
        })
        .expect("instance not found in the frame")
}

/// The sprite's layer vertices of the instance in `frame`.
fn layer_of(frame: &RenderFrame, instance: u64) -> Vec<[f32; 2]> {
    frame
        .instances
        .iter()
        .find_map(|draw| match draw {
            InstanceDraw::Sprite2d {
                instance: id,
                layers,
                ..
            } if *id == instance => Some(layers[0].clone()),
            _ => None,
        })
        .expect("sprite draw not found")
}

/// The sprite's layer vertices mapped through the instance transform, i.e.
/// what the renderer would draw.
fn world_layer(frame: &RenderFrame, instance: u64) -> Vec<[f32; 2]> {
    let (transform, vertices) = frame
        .instances
        .iter()
        .find_map(|draw| match draw {
            InstanceDraw::Sprite2d {
                instance: id,
                transform,
                layers,
                ..
            } if *id == instance => Some((transform, &layers[0])),
            _ => None,
        })
        .expect("sprite draw not found");
    vertices
        .iter()
        .map(|vertex| {
            [
                transform.x_axis.x * vertex[0]
                    + transform.y_axis.x * vertex[1]
                    + transform.w_axis.x,
                transform.x_axis.y * vertex[0]
                    + transform.y_axis.y * vertex[1]
                    + transform.w_axis.y,
            ]
        })
        .collect()
}

#[test]
fn poke_turns_gaze_toward_the_click_point() {
    // The same sprite poked from the left and from the right must end up
    // turned; the walk is off (no bounds), so only the poke can change the
    // pose, and an un-poked twin is the control.
    let (mut left, left_id) = poke_simulation(0.0);
    let (mut right, right_id) = poke_simulation(0.0);
    let (mut control, control_id) = poke_simulation(0.0);
    left.poke_reaction(left_id, Vec3::new(-2.0, 0.5, 0.0))
        .unwrap();
    right
        .poke_reaction(right_id, Vec3::new(2.0, 0.5, 0.0))
        .unwrap();
    // Half a second in, the envelope is past its rise, so the turn is
    // strongly applied.
    step(&mut left, 30);
    step(&mut right, 30);
    step(&mut control, 30);
    let left_frame = left.render_frame().unwrap();
    let right_frame = right.render_frame().unwrap();
    let control_frame = control.render_frame().unwrap();
    let left_local = layer_of(&left_frame, left_id);
    let right_local = layer_of(&right_frame, right_id);
    let control_local = layer_of(&control_frame, control_id);
    // The rotation deformer's bottom edge reads out the turn: its y
    // difference is sin(angle), so a poke must increase it well past the
    // un-poked sway.
    let turn = |vertices: &[[f32; 2]]| vertices[1][1] - vertices[0][1];
    assert!(
        turn(&left_local) > turn(&control_local) + 0.05,
        "a poke did not turn the gaze: {} vs {}",
        turn(&left_local),
        turn(&control_local)
    );
    // The poke point's side and the mirror facing cancel: both pokes drive
    // the same local pose, and only the facing differs. The world-space
    // poses are therefore exact x-mirrors across the instance centre.
    assert_eq!(left_local, right_local);
    let left_world = world_layer(&left_frame, left_id);
    let right_world = world_layer(&right_frame, right_id);
    for (left_vertex, right_vertex) in left_world.iter().zip(right_world.iter()) {
        assert!(
            (left_vertex[0] + right_vertex[0]).abs() < 1e-4
                && (left_vertex[1] - right_vertex[1]).abs() < 1e-4,
            "poke sides are not mirrored: {left_vertex:?} vs {right_vertex:?}"
        );
    }
}

#[test]
fn poke_flips_sprite_facing_only_for_2d() {
    // 2D: the mirror sign of the root transform follows the poke direction.
    let (mut simulation, instance) = poke_simulation(0.0);
    let before = transform_of(&simulation.render_frame().unwrap(), instance);
    assert_eq!(before.x_axis.x, 1.0);
    simulation
        .poke_reaction(instance, Vec3::new(-2.0, 0.5, 0.0))
        .unwrap();
    step(&mut simulation, 2);
    let turned = transform_of(&simulation.render_frame().unwrap(), instance);
    assert_eq!(turned.x_axis.x, -1.0);
    simulation
        .poke_reaction(instance, Vec3::new(2.0, 0.5, 0.0))
        .unwrap();
    step(&mut simulation, 2);
    let turned_back = transform_of(&simulation.render_frame().unwrap(), instance);
    assert_eq!(turned_back.x_axis.x, 1.0);
    // 3D: the turn happens through the head yaw instead; the root keeps its
    // facing. Compare the skin matrices against an un-poked twin.
    let mut skeleton = common::skeleton_document();
    skeleton["interaction"] = json!({"head_joint": "head"});
    let mut posed = Simulation::new();
    let asset = posed.insert_asset(common::load(&skeleton)).unwrap();
    let id = posed.spawn(asset, Vec3::ZERO).unwrap();
    let mut control = Simulation::new();
    let asset = control.insert_asset(common::load(&skeleton)).unwrap();
    control.spawn(asset, Vec3::ZERO).unwrap();
    posed.poke_reaction(id, Vec3::new(-2.0, 1.0, 0.0)).unwrap();
    step(&mut posed, 10);
    step(&mut control, 10);
    let posed_frame = posed.render_frame().unwrap();
    let control_frame = control.render_frame().unwrap();
    assert_eq!(
        transform_of(&posed_frame, id).x_axis.x,
        1.0,
        "3D root facing must not flip"
    );
    let InstanceDraw::Skeleton3d {
        skin_matrices: posed_skins,
        ..
    } = posed_frame.instances[0].clone()
    else {
        panic!("expected 3D draw");
    };
    let InstanceDraw::Skeleton3d {
        skin_matrices: control_skins,
        ..
    } = control_frame.instances[0].clone()
    else {
        panic!("expected 3D draw");
    };
    assert_ne!(posed_skins, control_skins, "3D head yaw did not react");
}

#[test]
fn poked_walk_pauses_for_the_reaction_and_resumes_after() {
    let (mut simulation, instance) = wandering_simulation(0.0, 0);
    // Wait for a walk to start, then poke the walker.
    let mut walking = false;
    for _ in 0..600 {
        step(&mut simulation, 1);
        if simulation
            .drain_wander_events()
            .iter()
            .any(|event| matches!(event, WanderEvent::Start { .. }))
        {
            walking = true;
            break;
        }
    }
    assert!(walking, "no walk started within the idle window");
    simulation
        .poke_reaction(instance, Vec3::new(-2.0, 0.15, 0.0))
        .unwrap();
    let poked = simulation.instance(instance).unwrap().position;
    // The pause lasts one reaction second, i.e. about 60 ticks (f32
    // accumulation can shift the exact resume tick by one): the first 58
    // ticks definitely stay still and silent.
    for tick in 0..58 {
        step(&mut simulation, 1);
        let position = simulation.instance(instance).unwrap().position;
        assert_eq!(
            position,
            poked,
            "instance moved {} ticks into the reaction",
            tick + 1
        );
    }
    assert!(
        simulation.drain_wander_events().is_empty(),
        "a paused walk must not emit events"
    );
    // The walk then resumes within a few ticks, from the same spot: the
    // phase kept its target and position, so the first moving tick continues
    // the old walk rather than starting a new one.
    let mut resumed = false;
    for _ in 0..5 {
        step(&mut simulation, 1);
        if simulation.instance(instance).unwrap().position != poked {
            resumed = true;
            break;
        }
    }
    assert!(resumed, "the walk did not resume after the reaction");
}

#[test]
fn poke_hop_does_not_move_the_simulation_position() {
    let (mut simulation, instance) = poke_simulation(0.0);
    simulation
        .poke_reaction(instance, Vec3::new(2.0, 0.15, 0.0))
        .unwrap();
    let base = transform_of(&simulation.render_frame().unwrap(), instance);
    // The hop is a presentation offset: the transform rises, the simulation
    // position never changes.
    let mut peak = 0.0f32;
    let reaction_ticks = (1.0 / FIXED_DT.as_secs_f64()).round() as u32;
    for _ in 0..reaction_ticks {
        step(&mut simulation, 1);
        let transform = transform_of(&simulation.render_frame().unwrap(), instance);
        let rise = transform.w_axis.y - base.w_axis.y;
        assert!(rise >= -1e-6, "the hop went below the base line");
        peak = peak.max(rise);
        assert_eq!(simulation.instance(instance).unwrap().position.y, 0.15);
    }
    assert!(
        (peak - POKE_HOP_AMPLITUDE).abs() < 0.005,
        "hop peak {peak} is not near {POKE_HOP_AMPLITUDE}"
    );
    // After the reaction the hop is back to zero: the transform returns to
    // the exact base line.
    step(&mut simulation, 1);
    let settled = transform_of(&simulation.render_frame().unwrap(), instance);
    assert!((settled.w_axis.y - base.w_axis.y).abs() < 1e-6);
}

#[test]
fn poke_reaction_restarts_and_is_deterministic() {
    // Two identical simulations, poked at the same ticks, must agree frame
    // for frame including the reaction phase.
    let (mut first, first_id) = poke_simulation(0.0);
    let (mut second, second_id) = poke_simulation(0.0);
    for (simulation, id) in [(&mut first, first_id), (&mut second, second_id)] {
        simulation
            .poke_reaction(id, Vec3::new(-1.5, 0.5, 0.0))
            .unwrap();
    }
    for tick in 0..90 {
        step(&mut first, 1);
        step(&mut second, 1);
        // Re-poke midway: the reaction must restart without diverging.
        if tick == 30 {
            for (simulation, id) in [(&mut first, first_id), (&mut second, second_id)] {
                simulation
                    .poke_reaction(id, Vec3::new(1.5, 0.5, 0.0))
                    .unwrap();
            }
        }
        let first_frame = first.render_frame().unwrap();
        let second_frame = second.render_frame().unwrap();
        first_frame
            .instances
            .iter()
            .zip(second_frame.instances.iter())
            .for_each(|(a, b)| {
                let (
                    InstanceDraw::Sprite2d { layers: a, .. },
                    InstanceDraw::Sprite2d { layers: b, .. },
                ) = (a, b)
                else {
                    panic!("expected sprite draws");
                };
                assert_eq!(a, b, "frames diverged at tick {tick}");
            });
    }
    // A poke after all of the above still restarts the reaction and turns
    // the sprite: the restart path works even after the previous reaction
    // ended mid-sequence.
    first
        .poke_reaction(first_id, Vec3::new(-1.0, 0.5, 0.0))
        .unwrap();
    step(&mut first, 2);
    assert_eq!(
        transform_of(&first.render_frame().unwrap(), first_id)
            .x_axis
            .x,
        -1.0,
        "the sprite did not turn toward the re-poke"
    );
}

#[test]
fn poke_rejects_invalid_points_and_unknown_ids() {
    let (mut simulation, instance) = poke_simulation(0.0);
    for invalid in [
        Vec3::new(f32::NAN, 0.0, 0.0),
        Vec3::new(0.0, f32::INFINITY, 0.0),
        Vec3::new(20_001.0, 0.0, 0.0),
    ] {
        assert!(simulation.poke_reaction(instance, invalid).is_err());
    }
    assert!(simulation.poke_reaction(9999, Vec3::ZERO).is_err());
    // A rejected poke changes nothing: the transform stays the base one.
    let transform = transform_of(&simulation.render_frame().unwrap(), instance);
    assert_eq!(transform.w_axis.y, 0.15);
}

#[test]
fn poke_on_the_instance_center_hops_without_turning() {
    // A poke inside the direction dead zone hops but never turns: the gaze
    // parameter and the facing both stay at their defaults.
    let (mut centred, centred_id) = poke_simulation(1.0);
    let (mut control, control_id) = poke_simulation(1.0);
    centred
        .poke_reaction(centred_id, Vec3::new(1.01, 0.15, 0.0))
        .unwrap();
    step(&mut centred, 20);
    step(&mut control, 20);
    let centred_frame = centred.render_frame().unwrap();
    let control_frame = control.render_frame().unwrap();
    assert_eq!(
        layer_of(&centred_frame, centred_id),
        layer_of(&control_frame, control_id),
        "a centred poke must not turn the gaze"
    );
    assert_eq!(transform_of(&centred_frame, centred_id).x_axis.x, 1.0);
    // The hop still happens.
    assert!(transform_of(&centred_frame, centred_id).w_axis.y > 0.15);
}
