mod common;

use std::time::Duration;

use delicium::{
    renderer::{InstanceDraw, RenderFrame},
    simulation::{AttentionSource, FIXED_DT, Simulation, WorldRect, WorldXBounds},
};
use glam::Vec3;
use serde_json::json;

fn scene(enabled: bool) -> (Simulation, u64, u64) {
    let mut document = common::sprite_document();
    document["format_version"] = json!("0.2");
    document["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
    document["sprite2d"]["animations"] = json!(["idle", "walk", "reaction"].map(|name| json!({
        "name": name, "duration": 1.0,
        "channels": [{"parameter": "sway", "keys": [
            {"time": 0.0, "value": 0.0}, {"time": 1.0, "value": 0.0}
        ]}]
    })));
    document["interaction"] = json!({"gaze_parameter": "sway"});
    let mut simulation = Simulation::new();
    simulation.set_attention_enabled(enabled);
    let asset = simulation.insert_asset(common::load(&document)).unwrap();
    let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
    (simulation, id, asset)
}

fn step(simulation: &mut Simulation, count: usize) {
    for _ in 0..count {
        simulation.advance(FIXED_DT).unwrap();
    }
}

fn assert_frames_identical(a: &RenderFrame, b: &RenderFrame) {
    assert_eq!(a.instances.len(), b.instances.len());
    for (a, b) in a.instances.iter().zip(&b.instances) {
        match (a, b) {
            (
                InstanceDraw::Sprite2d {
                    asset: aa,
                    instance: ai,
                    transform: at,
                    layers: al,
                },
                InstanceDraw::Sprite2d {
                    asset: ba,
                    instance: bi,
                    transform: bt,
                    layers: bl,
                },
            ) => {
                assert_eq!((aa, ai), (ba, bi));
                assert_eq!(
                    at.to_cols_array().map(f32::to_bits),
                    bt.to_cols_array().map(f32::to_bits)
                );
                assert_eq!(al.len(), bl.len());
                for (a, b) in al.iter().zip(bl) {
                    assert_eq!(a.len(), b.len());
                    for (a, b) in a.iter().zip(b) {
                        assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits));
                    }
                }
            }
            (
                InstanceDraw::Skeleton3d {
                    asset: aa,
                    instance: ai,
                    transform: at,
                    skin_matrices: am,
                },
                InstanceDraw::Skeleton3d {
                    asset: ba,
                    instance: bi,
                    transform: bt,
                    skin_matrices: bm,
                },
            ) => {
                assert_eq!((aa, ai), (ba, bi));
                assert_eq!(
                    at.to_cols_array().map(f32::to_bits),
                    bt.to_cols_array().map(f32::to_bits)
                );
                assert_eq!(am.len(), bm.len());
                for (a, b) in am.iter().zip(bm) {
                    assert_eq!(
                        a.to_cols_array().map(f32::to_bits),
                        b.to_cols_array().map(f32::to_bits)
                    );
                }
            }
            _ => panic!("draw kind changed"),
        }
    }
}

fn rect(left: f32, right: f32, bottom: f32, top: f32) -> WorldRect {
    WorldRect {
        left,
        right,
        bottom,
        top,
    }
}

#[test]
fn pointer_has_hysteresis_eases_and_clears_without_changing_position() {
    let (mut simulation, id, _) = scene(true);
    simulation
        .set_pointer_world(Some(Vec3::new(0.59, 0.6, 0.0)))
        .unwrap();
    step(&mut simulation, 30);
    let close = simulation.attention_snapshot(id).unwrap();
    assert_eq!(close.source, AttentionSource::Pointer);
    assert!(close.pointer_weight > 0.9 && close.pointer_weight <= 1.0);
    assert_eq!(simulation.instance(id).unwrap().position, Vec3::ZERO);
    simulation
        .set_pointer_world(Some(Vec3::new(0.68, 0.6, 0.0)))
        .unwrap();
    step(&mut simulation, 1);
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().source,
        AttentionSource::Pointer
    );
    simulation
        .set_pointer_world(Some(Vec3::new(0.8, 0.6, 0.0)))
        .unwrap();
    step(&mut simulation, 1);
    assert_ne!(
        simulation.attention_snapshot(id).unwrap().source,
        AttentionSource::Pointer
    );
    simulation.set_pointer_world(None).unwrap();
    step(&mut simulation, 120);
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().pointer_weight,
        0.0
    );
    assert_eq!(simulation.instance(id).unwrap().position, Vec3::ZERO);
}

#[test]
fn pointer_gaze_compensates_for_an_existing_sprite_mirror() {
    let (mut left, id, _) = scene(true);
    let (mut right, _, _) = scene(true);
    left.poke_reaction(id, -Vec3::X).unwrap();
    right.poke_reaction(id, Vec3::X).unwrap();
    for simulation in [&mut left, &mut right] {
        step(simulation, 90);
        simulation
            .set_pointer_world(Some(Vec3::new(0.3, 0.6, 0.0)))
            .unwrap();
        step(simulation, 45);
    }
    let a = left.render_frame().unwrap();
    let b = right.render_frame().unwrap();
    let InstanceDraw::Sprite2d {
        transform: at,
        layers: al,
        ..
    } = &a.instances[0]
    else {
        unreachable!()
    };
    let InstanceDraw::Sprite2d {
        transform: bt,
        layers: bl,
        ..
    } = &b.instances[0]
    else {
        unreachable!()
    };
    assert!(at.x_axis.x < 0.0 && bt.x_axis.x > 0.0);
    for (left_index, right_index) in [(0, 1), (1, 0), (2, 2)] {
        let a = at.transform_point3(Vec3::new(al[0][left_index][0], al[0][left_index][1], 0.0));
        let b = bt.transform_point3(Vec3::new(bl[0][right_index][0], bl[0][right_index][1], 0.0));
        assert!(
            a.distance(b) < 0.00001,
            "same world pointer must produce matching mirrored gaze: {a:?} vs {b:?}"
        );
    }
}

#[test]
fn skeleton_pointer_gaze_turns_both_ways_without_saturating_or_mirroring_the_root() {
    let mut document = common::skeleton_document();
    document["interaction"] = json!({"head_joint": "head"});
    let model = common::load(&document);
    let head = model.interaction.head_joint.unwrap();
    for sign in [-1.0_f32, 1.0] {
        let mut active = Simulation::new();
        active.set_attention_enabled(true);
        let mut original = Simulation::new();
        for simulation in [&mut active, &mut original] {
            let asset = simulation.insert_asset(model.clone()).unwrap();
            simulation.spawn(asset, Vec3::ZERO).unwrap();
        }
        active
            .set_pointer_world(Some(Vec3::new(sign * 0.3, 1.6, 0.0)))
            .unwrap();
        step(&mut active, 45);
        step(&mut original, 45);
        let frame = active.render_frame().unwrap();
        let base = original.render_frame().unwrap();
        let InstanceDraw::Skeleton3d {
            transform,
            skin_matrices,
            ..
        } = &frame.instances[0]
        else {
            unreachable!()
        };
        let InstanceDraw::Skeleton3d {
            skin_matrices: base_matrices,
            ..
        } = &base.instances[0]
        else {
            unreachable!()
        };
        let relative = skin_matrices[head] * base_matrices[head].inverse();
        let forward = relative.transform_vector3(Vec3::Z).normalize();
        let yaw = forward.x.atan2(forward.z);
        assert!(
            yaw * sign > 0.01,
            "pointer must visibly turn the head toward its side"
        );
        assert!(
            yaw.abs() <= 18.0_f32.to_radians() + 0.00001,
            "new attention exceeded its yaw limit: {yaw}"
        );
        assert!(
            transform.x_axis.x > 0.0,
            "pointer attention must not flip root facing"
        );
    }
}

#[test]
fn invalid_pointer_and_edge_inputs_preserve_previous_state_atomically() {
    let (mut simulation, id, _) = scene(true);
    let (mut control, _, _) = scene(true);
    for simulation in [&mut simulation, &mut control] {
        simulation
            .set_pointer_world(Some(Vec3::new(0.3, 0.6, 0.0)))
            .unwrap();
        simulation
            .set_attention_edges(&[rect(-2.0, 2.0, 0.0, 4.0)], &[], &[])
            .unwrap();
        step(simulation, 12);
    }
    let before = simulation.render_frame().unwrap();
    for point in [Vec3::NAN, Vec3::INFINITY, Vec3::splat(f32::MAX)] {
        assert!(simulation.set_pointer_world(Some(point)).is_err());
        assert_frames_identical(&before, &simulation.render_frame().unwrap());
    }
    for invalid in [rect(1.0, 0.0, 0.0, 2.0), rect(0.0, 1.0, f32::NAN, 2.0)] {
        assert!(
            simulation
                .set_attention_edges(&[], &[invalid], &[])
                .is_err()
        );
        assert_frames_identical(&before, &simulation.render_frame().unwrap());
    }
    assert!(
        simulation
            .set_attention_edges(&[rect(-1.0, 1.0, 0.0, 3.0); 65], &[], &[])
            .is_err()
    );
    assert!(
        simulation
            .set_attention_edges(&[], &[rect(-1.0, 1.0, 0.0, 3.0); 65], &[])
            .is_err()
    );
    assert!(
        simulation
            .set_attention_edges(&[], &[], &[rect(-1.0, 1.0, 0.0, 3.0); 33])
            .is_err()
    );
    step(&mut simulation, 12);
    step(&mut control, 12);
    assert_frames_identical(
        &control.render_frame().unwrap(),
        &simulation.render_frame().unwrap(),
    );
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().source,
        AttentionSource::Pointer
    );
    for simulation in [&mut simulation, &mut control] {
        simulation.set_pointer_world(None).unwrap();
        step(simulation, 12);
    }
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().source,
        AttentionSource::ScreenEdge
    );
    assert_frames_identical(
        &control.render_frame().unwrap(),
        &simulation.render_frame().unwrap(),
    );
}

#[test]
fn repeated_render_and_event_drain_do_not_advance_presentation() {
    let (mut simulation, id, _) = scene(true);
    simulation
        .set_pointer_world(Some(Vec3::new(0.2, 0.6, 0.0)))
        .unwrap();
    step(&mut simulation, 20);
    let frame = simulation.render_frame().unwrap();
    let state = simulation.attention_snapshot(id).unwrap();
    let events = simulation.drain_attention_events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.source == AttentionSource::Pointer)
            .count(),
        1
    );
    assert!(simulation.drain_attention_events().is_empty());
    simulation.advance(Duration::ZERO).unwrap();
    for _ in 0..4 {
        assert_frames_identical(&frame, &simulation.render_frame().unwrap());
        let after = simulation.attention_snapshot(id).unwrap();
        assert_eq!(state.source, after.source);
        assert_eq!(
            state.pointer_weight.to_bits(),
            after.pointer_weight.to_bits()
        );
        assert_eq!(state.shear_x.to_bits(), after.shear_x.to_bits());
    }
}

#[test]
fn disabling_clears_all_new_presentation_and_retains_original_frames() {
    let (mut original, id, _) = scene(false);
    let (mut toggled, _, _) = scene(true);
    toggled
        .set_pointer_world(Some(Vec3::new(0.2, 0.6, 0.0)))
        .unwrap();
    toggled.begin_drag_feedback(id).unwrap();
    toggled.poke_reaction(id, Vec3::X).unwrap();
    original.poke_reaction(id, Vec3::X).unwrap();
    toggled.set_attention_enabled(false);
    assert!(toggled.drain_attention_events().is_empty());
    let cleared = toggled.attention_snapshot(id).unwrap();
    assert_eq!(cleared.source, AttentionSource::None);
    assert!(!cleared.dragging);
    assert_eq!(cleared.streak, 0);
    assert_eq!(cleared.scale_y, 1.0);
    assert_eq!(cleared.shear_x, 0.0);
    for tick in 0..160 {
        if tick % 7 == 0 {
            assert_frames_identical(
                &original.render_frame().unwrap(),
                &toggled.render_frame().unwrap(),
            );
        }
        step(&mut original, 1);
        step(&mut toggled, 1);
    }
}

#[test]
fn idle_rng_is_deterministic_and_does_not_consume_wander_randomness() {
    let (mut a, id, _) = scene(true);
    let (mut b, _, _) = scene(true);
    let (mut old, _, _) = scene(false);
    for simulation in [&mut a, &mut b, &mut old] {
        simulation
            .set_wander_bounds(Some(WorldXBounds {
                left: -2.0,
                right: 2.0,
            }))
            .unwrap();
    }
    let mut variants = std::collections::BTreeSet::new();
    for tick in 0..1500 {
        step(&mut a, 1);
        step(&mut b, 1);
        step(&mut old, 1);
        let first = a.attention_snapshot(id).unwrap();
        let second = b.attention_snapshot(id).unwrap();
        assert_eq!(first.idle_variant, second.idle_variant);
        assert_eq!(first.shear_x.to_bits(), second.shear_x.to_bits());
        assert_eq!(
            a.instance(id).unwrap().position,
            old.instance(id).unwrap().position
        );
        assert_eq!(
            a.instance(id).unwrap().action,
            old.instance(id).unwrap().action
        );
        assert_eq!(
            format!("{:?}", a.drain_wander_events()),
            format!("{:?}", old.drain_wander_events())
        );
        if tick % 60 == 0 {
            variants.insert(first.idle_variant);
            assert_frames_identical(&a.render_frame().unwrap(), &b.render_frame().unwrap());
        }
    }
    assert!(variants.len() > 1, "idle variants must actually change");
}

#[test]
fn a_single_poke_retains_the_original_reaction_even_with_a_nearby_pointer() {
    let (mut active, id, _) = scene(true);
    let (mut original, _, _) = scene(false);
    active
        .set_pointer_world(Some(Vec3::new(-0.3, 0.6, 0.0)))
        .unwrap();
    step(&mut active, 45);
    step(&mut original, 45);
    active.poke_reaction(id, Vec3::X).unwrap();
    original.poke_reaction(id, Vec3::X).unwrap();
    step(&mut active, 2);
    step(&mut original, 2);
    for _ in 0..40 {
        assert_frames_identical(
            &original.render_frame().unwrap(),
            &active.render_frame().unwrap(),
        );
        assert_ne!(
            active.attention_snapshot(id).unwrap().source,
            AttentionSource::Pointer
        );
        step(&mut active, 1);
        step(&mut original, 1);
    }
}

#[test]
fn active_social_actions_keep_priority_over_pointer_and_idle_presentation() {
    use delicium::model::{LoadLimits, load_model_bytes};
    use delicium::simulation::ActionKind;
    let model = load_model_bytes(
        include_bytes!("../assets/examples/bangboo-eous.dmodel.json"),
        &LoadLimits::default(),
    )
    .unwrap();
    let mut active = Simulation::new();
    active.set_attention_enabled(true);
    let mut original = Simulation::new();
    for simulation in [&mut active, &mut original] {
        let asset = simulation.insert_asset(model.clone()).unwrap();
        for x in [-0.65, 0.65] {
            simulation.spawn(asset, Vec3::new(x, 0.15, 0.0)).unwrap();
        }
    }
    active
        .set_pointer_world(Some(Vec3::new(-0.3, 1.2, 0.0)))
        .unwrap();
    let mut compared = 0;
    for _ in 0..120 {
        step(&mut active, 1);
        step(&mut original, 1);
        if active
            .instances()
            .iter()
            .all(|state| matches!(state.action, Some(ActionKind::Greet | ActionKind::Play)))
        {
            if compared > 1 {
                assert_frames_identical(
                    &original.render_frame().unwrap(),
                    &active.render_frame().unwrap(),
                );
            }
            for id in [1, 2] {
                let state = active.attention_snapshot(id).unwrap();
                assert_ne!(state.source, AttentionSource::Pointer);
                assert_eq!(state.shear_x, 0.0);
                assert_eq!(state.scale_y, 1.0);
            }
            compared += 1;
        }
    }
    assert!(
        compared > 20,
        "must compare actual social poses, not an idle scene"
    );
}

#[test]
fn streak_caps_expires_and_actual_drag_clears_it() {
    let (mut simulation, id, _) = scene(true);
    for poke in 1..=7 {
        simulation.poke_reaction(id, Vec3::X).unwrap();
        assert_eq!(
            simulation.attention_snapshot(id).unwrap().streak,
            poke.min(3)
        );
        step(&mut simulation, 4);
    }
    let repeated = simulation.attention_snapshot(id).unwrap();
    assert!(repeated.shear_x.abs() <= 0.060001);
    step(&mut simulation, 90);
    assert_eq!(simulation.attention_snapshot(id).unwrap().streak, 0);
    simulation.poke_reaction(id, Vec3::X).unwrap();
    assert_eq!(simulation.attention_snapshot(id).unwrap().streak, 1);
    simulation.begin_drag_feedback(id).unwrap();
    assert_eq!(simulation.attention_snapshot(id).unwrap().streak, 0);
}

#[test]
fn drag_feedback_preserves_positions_bounds_velocity_and_settles_only_after_release() {
    let (mut simulation, id, _) = scene(true);
    simulation.suspend_wander(id).unwrap();
    simulation.begin_drag_feedback(id).unwrap();
    for x in [0.1, 1.0, -1.0, 4.0, -4.0] {
        let position = Vec3::new(x, 0.0, 0.0);
        simulation.set_position(id, position).unwrap();
        step(&mut simulation, 1);
        assert_eq!(simulation.instance(id).unwrap().position, position);
        let state = simulation.attention_snapshot(id).unwrap();
        assert!(state.dragging);
        assert_eq!(state.source, AttentionSource::Drag);
        assert!(state.shear_x.abs() <= 0.080001);
    }
    simulation.end_drag_feedback(id, true).unwrap();
    let settled = simulation.attention_snapshot(id).unwrap();
    assert!(!settled.dragging);
    assert!(settled.settle_remaining > 0.0 && settled.settle_remaining <= 0.2);
    for _ in 0..15 {
        step(&mut simulation, 1);
        let state = simulation.attention_snapshot(id).unwrap();
        assert!(state.scale_y >= 0.95999 && state.scale_y <= 1.01201);
    }
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().settle_remaining,
        0.0
    );
    simulation.begin_drag_feedback(id).unwrap();
    simulation.end_drag_feedback(id, false).unwrap();
    let canceled = simulation.attention_snapshot(id).unwrap();
    assert_eq!(canceled.settle_remaining, 0.0);
    assert_eq!(canceled.shear_x, 0.0);
    assert_eq!(canceled.scale_y, 1.0);
}

#[test]
fn edge_priority_uses_work_before_screen_before_windows_and_clears_old_snapshots() {
    let (mut simulation, id, _) = scene(true);
    let boundary = rect(-2.0, 2.0, 0.0, 3.0);
    simulation
        .set_attention_edges(&[boundary], &[boundary], &[boundary])
        .unwrap();
    step(&mut simulation, 12);
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().source,
        AttentionSource::WorkAreaEdge
    );
    simulation
        .set_attention_edges(&[boundary], &[], &[boundary])
        .unwrap();
    step(&mut simulation, 12);
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().source,
        AttentionSource::ScreenEdge
    );
    simulation
        .set_attention_edges(&[], &[], &[boundary])
        .unwrap();
    step(&mut simulation, 12);
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().source,
        AttentionSource::WindowEdge
    );
    simulation.set_attention_edges(&[], &[], &[]).unwrap();
    step(&mut simulation, 120);
    let cleared = simulation.attention_snapshot(id).unwrap();
    assert!(!matches!(
        cleared.source,
        AttentionSource::ScreenEdge | AttentionSource::WorkAreaEdge | AttentionSource::WindowEdge
    ));
    assert_eq!(cleared.edge_weight, 0.0);
}

#[test]
fn attention_handoffs_and_target_changes_keep_the_actual_head_yaw_eased() {
    let mut document = common::skeleton_document();
    document["interaction"] = json!({"head_joint": "head"});
    document["format_version"] = json!("0.2");
    document["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
    document["skeleton3d"]["animations"] = json!(["idle", "walk", "reaction"].map(|name| json!({
        "name": name, "duration": 2.0, "channels": [{"joint": "head", "rotation": [
            {"time": 0.0, "value": [0.0, 0.0, 0.0, 1.0]},
            {"time": 2.0, "value": [0.0, 0.0, 0.0, 1.0]}
        ]}]
    })));
    let model = common::load(&document);
    let head = model.interaction.head_joint.unwrap();
    let mut simulation = Simulation::new();
    simulation.set_attention_enabled(true);
    let asset = simulation.insert_asset(model).unwrap();
    let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
    simulation
        .set_attention_edges(&[rect(-0.3, 3.0, -3.0, 4.0)], &[], &[])
        .unwrap();
    let yaw = |simulation: &Simulation| {
        let frame = simulation.render_frame().unwrap();
        let InstanceDraw::Skeleton3d { skin_matrices, .. } = &frame.instances[0] else {
            unreachable!()
        };
        let forward = skin_matrices[head].transform_vector3(Vec3::Z);
        forward.x.atan2(forward.z)
    };
    step(&mut simulation, 120);
    let mut previous = yaw(&simulation);
    assert!(previous < -0.25, "edge must actually drive the head");
    for tick in 0..480 {
        match tick {
            0 | 240 => simulation
                .set_pointer_world(Some(Vec3::new(0.3, 1.6, 0.0)))
                .unwrap(),
            120 => simulation.set_pointer_world(None).unwrap(),
            360 => simulation
                .set_pointer_world(Some(Vec3::new(-0.3, 1.6, 0.0)))
                .unwrap(),
            _ => {}
        }
        step(&mut simulation, 1);
        let current = yaw(&simulation);
        assert!(
            (current - previous).abs() <= 4.1_f32.to_radians(),
            "head snapped during source/target handoff: tick={tick} previous={previous} current={current}"
        );
        assert!(current.abs() <= 18.0_f32.to_radians() + 0.00001);
        previous = current;
        if tick == 119 || tick == 359 {
            assert!(current > 0.25);
        }
        if tick == 239 || tick == 479 {
            assert!(current < -0.25);
        }
    }
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().source,
        AttentionSource::Pointer
    );
    simulation.set_pointer_world(None).unwrap();
    simulation.set_attention_edges(&[], &[], &[]).unwrap();
    step(&mut simulation, 240);
    assert_eq!(
        simulation.attention_snapshot(id).unwrap().source,
        AttentionSource::Idle
    );
    previous = yaw(&simulation);
    assert!(previous.abs() > 0.05, "idle must actually drive the head");
    let sign = -previous.signum();
    simulation
        .set_pointer_world(Some(Vec3::new(sign * 0.3, 1.6, 0.0)))
        .unwrap();
    for tick in 0..120 {
        step(&mut simulation, 1);
        let current = yaw(&simulation);
        assert!(
            (current - previous).abs() <= 4.1_f32.to_radians(),
            "head snapped from idle to pointer: tick={tick} previous={previous} current={current}"
        );
        previous = current;
    }
    assert!(previous * sign > 0.25);
}

#[test]
fn lifecycle_reset_is_committed_only_after_successful_model_replacement() {
    let (mut simulation, id, asset) = scene(true);
    simulation.poke_reaction(id, Vec3::X).unwrap();
    simulation.begin_drag_feedback(id).unwrap();
    let before = simulation.render_frame().unwrap();
    assert!(simulation.replace_model(id, u64::MAX).is_err());
    assert!(simulation.attention_snapshot(id).unwrap().dragging);
    assert_frames_identical(&before, &simulation.render_frame().unwrap());
    simulation.replace_model(id, asset).unwrap();
    let replaced = simulation.attention_snapshot(id).unwrap();
    assert!(!replaced.dragging);
    assert_eq!(replaced.streak, 0);
    assert_eq!(replaced.shear_x, 0.0);
    assert_eq!(replaced.scale_y, 1.0);
    assert!(simulation.despawn(id));
    assert!(simulation.attention_snapshot(id).is_none());
    assert!(simulation.begin_drag_feedback(id).is_err());
    assert!(simulation.end_drag_feedback(id, true).is_err());
    assert!(
        simulation
            .drain_attention_events()
            .iter()
            .all(|event| event.instance != id)
    );
}
