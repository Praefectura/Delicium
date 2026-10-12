mod common;

use std::{sync::Arc, time::Duration};

use delicium::{
    model::{
        LoadLimits, ModelData, ModelKind, load_model_bytes, try_deform_layer, try_sample_parameters,
    },
    renderer::InstanceDraw,
    simulation::Simulation,
};
use glam::Vec3;
use serde_json::json;

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
