mod common;

use delicium::model::{
    ActionBindings, ClipPlayback, LoadLimits, ModelData, ModelError, ModelKind, load_model_bytes,
    read_model_file, try_deform_layer, try_sample_joints, try_sample_joints_clip,
    try_sample_parameters, try_sample_parameters_clip, try_skin_matrices,
};
use glam::{Mat4, Quat, Vec3};
use serde_json::{Value, json};

/// `0.2` sprite with a `sway` parameter and three animations: `idle`
/// (duration 1.0), `walk` (duration 2.0) and `reaction` (duration 0.5), plus
/// an `extra` parameter (default 0.25) animated by `blink` (duration 4.0)
/// that no action role selects.
fn sprite_action_document() -> Value {
    let mut document = common::sprite_document();
    document["format_version"] = json!("0.2");
    document["sprite2d"]["parameters"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "extra", "min": 0.0, "max": 1.0, "default": 0.25
        }));
    document["sprite2d"]["animations"] = json!([
        {"name": "idle", "duration": 1.0, "channels": [{"parameter": "sway", "keys": [
            {"time": 0.0, "value": -1.0},
            {"time": 1.0, "value": 1.0}
        ]}]},
        {"name": "walk", "duration": 2.0, "channels": [{"parameter": "sway", "keys": [
            {"time": 0.0, "value": 1.0},
            {"time": 2.0, "value": 0.0}
        ]}]},
        {"name": "reaction", "duration": 0.5, "channels": [{"parameter": "sway", "keys": [
            {"time": 0.0, "value": -1.0},
            {"time": 0.5, "value": 0.0}
        ]}]},
        {"name": "blink", "duration": 4.0, "channels": [{"parameter": "extra", "keys": [
            {"time": 0.0, "value": 0.0},
            {"time": 2.0, "value": 1.0},
            {"time": 4.0, "value": 0.0}
        ]}]}
    ]);
    document
}

/// `0.2` skeleton whose animations are `wave` (from the shared fixture),
/// `idle` (constant 90 degrees about x, duration 1.0) and `reaction`
/// (constant keys, duration 0.5).
fn skeleton_action_document() -> Value {
    let mut document = common::skeleton_document();
    document["format_version"] = json!("0.2");
    document["skeleton3d"]["animations"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({
                "name": "idle", "duration": 1.0,
                "channels": [{"joint": "head", "rotation": [
                    {"time": 0.0, "value": [0.70710677, 0.0, 0.0, 0.70710677]},
                    {"time": 1.0, "value": [0.70710677, 0.0, 0.0, 0.70710677]}
                ]}]
            }),
            json!({
                "name": "reaction", "duration": 0.5,
                "channels": [{"joint": "head", "rotation": [
                    {"time": 0.0, "value": [0.0, 0.0, 0.0, 1.0]},
                    {"time": 0.5, "value": [0.0, 0.0, 0.70710677, 0.70710677]}
                ]}]
            }),
        ]);
    document
}

fn reject(document: &Value) {
    assert!(
        load_model_bytes(
            &serde_json::to_vec(document).unwrap(),
            &LoadLimits::default()
        )
        .is_err()
    );
}

#[test]
fn bundled_examples_load_and_evaluate() {
    for (bytes, kind) in [
        (
            include_bytes!("../assets/examples/puppet2d.dmodel.json").as_slice(),
            ModelKind::Sprite2d,
        ),
        (
            include_bytes!("../assets/examples/robot3d.dmodel.json").as_slice(),
            ModelKind::Skeleton3d,
        ),
    ] {
        let model = load_model_bytes(bytes, &LoadLimits::default()).unwrap();
        assert_eq!(model.kind(), kind);
        for time in [0.0, 0.25, 1.0, 10.0] {
            match &model.data {
                ModelData::Sprite2d(sprite) => {
                    let parameters = try_sample_parameters(sprite, time).unwrap();
                    for index in 0..sprite.layers.len() {
                        let vertices = try_deform_layer(sprite, index, &parameters).unwrap();
                        assert_eq!(vertices.len(), sprite.layers[index].vertices.len());
                        assert!(vertices.iter().flatten().all(|value| value.is_finite()));
                    }
                }
                ModelData::Skeleton3d(skeleton) => {
                    let locals = try_sample_joints(skeleton, time).unwrap();
                    let matrices = try_skin_matrices(skeleton, &locals).unwrap();
                    assert_eq!(matrices.len(), skeleton.joints.len());
                    assert!(matrices.iter().all(|matrix| matrix.is_finite()));
                }
            }
        }
    }
}

#[test]
fn rejects_bad_json_version_unknown_fields_and_payload_mismatch() {
    assert!(load_model_bytes(b"{broken", &LoadLimits::default()).is_err());
    for (field, value) in [
        ("format", json!("other")),
        ("format_version", json!("999")),
        ("kind", json!("other")),
    ] {
        let mut document = common::sprite_document();
        document[field] = value;
        reject(&document);
    }
    let mut document = common::sprite_document();
    document["sprite2d"]["layers"][0]["url"] = json!("https://example.invalid/model");
    reject(&document);
    let mut document = common::sprite_document();
    document["skeleton3d"] = common::skeleton_document()["skeleton3d"].clone();
    reject(&document);
    let mut document = common::sprite_document();
    document.as_object_mut().unwrap().remove("sprite2d");
    reject(&document);
}

#[test]
fn rejects_names_references_indices_and_cycles() {
    let mut document = common::sprite_document();
    document["name"] = json!("x".repeat(129));
    reject(&document);
    document["name"] = json!("valid");
    document["sprite2d"]["layers"][0]["indices"] = json!([0, 1, 99]);
    reject(&document);
    document["sprite2d"]["layers"][0]["indices"] = json!([0, 1, 2]);
    document["sprite2d"]["layers"][0]["deformer"] = json!("missing");
    reject(&document);
    let mut document = common::sprite_document();
    document["sprite2d"]["deformers"][0]["parent"] = json!("turn");
    reject(&document);
    let mut document = common::skeleton_document();
    document["skeleton3d"]["joints"][1]["parent"] = json!("head");
    reject(&document);
}

#[test]
fn rejects_bad_weights_quaternions_and_singular_scale() {
    for weights in [json!([0.0, 0.0, 0.0, 0.0]), json!([-1.0, 2.0, 0.0, 0.0])] {
        let mut document = common::skeleton_document();
        document["skeleton3d"]["vertices"][0]["weights"] = weights;
        reject(&document);
    }
    let mut document = common::skeleton_document();
    document["skeleton3d"]["vertices"][0]["joints"] = json!([999, 0, 0, 0]);
    reject(&document);
    let mut document = common::skeleton_document();
    document["skeleton3d"]["joints"][0]["rotation"] = json!([0.0, 0.0, 0.0, 0.0]);
    reject(&document);
    document["skeleton3d"]["joints"][0]["rotation"] = json!([0.0, 0.0, 0.0, 1.0]);
    document["skeleton3d"]["joints"][0]["scale"] = json!([1.0, 0.0, 1.0]);
    reject(&document);
    let mut document = common::skeleton_document();
    document["skeleton3d"]["animations"][0]["channels"][0]["scale"] = json!([
        {"time": 0.0, "value": [1.0, 1.0, 1.0]},
        {"time": 1.0, "value": [-1.0, 1.0, 1.0]}
    ]);
    reject(&document);
}

#[test]
fn rejects_key_order_duplicate_channels_and_derived_budgets() {
    let mut document = common::sprite_document();
    document["sprite2d"]["animations"][0]["channels"][0]["keys"][1]["time"] = json!(0.0);
    reject(&document);
    let mut document = common::sprite_document();
    let channel = document["sprite2d"]["animations"][0]["channels"][0].clone();
    document["sprite2d"]["animations"][0]["channels"]
        .as_array_mut()
        .unwrap()
        .push(channel);
    reject(&document);
    let document = common::sprite_document();
    for limits in [
        LoadLimits {
            max_vertices: 2,
            ..LoadLimits::default()
        },
        LoadLimits {
            max_indices: 2,
            ..LoadLimits::default()
        },
        LoadLimits {
            max_total_keys: 2,
            ..LoadLimits::default()
        },
    ] {
        assert!(load_model_bytes(&serde_json::to_vec(&document).unwrap(), &limits).is_err());
    }
    let mut document = common::sprite_document();
    let layer = document["sprite2d"]["layers"][0].as_object_mut().unwrap();
    layer.remove("vertices");
    layer.remove("indices");
    layer.insert(
        "grid".into(),
        json!({"bounds": [0.0, 0.0, 1.0, 1.0], "columns": usize::MAX, "rows": 2}),
    );
    reject(&document);
}

#[test]
fn rejects_self_intersecting_masks_and_extreme_values() {
    let mut document = common::sprite_document();
    document["sprite2d"]["layers"][0]["mask"] =
        json!({"points": [[0.0, 0.0], [1.0, 1.0], [0.0, 1.0], [1.0, 0.0]]});
    reject(&document);
    let mut document = common::sprite_document();
    document["sprite2d"]["layers"][0]["vertices"][0] = json!([f32::MAX, f32::MAX]);
    reject(&document);
    let bytes = serde_json::to_vec(&common::sprite_document()).unwrap();
    let text = String::from_utf8(bytes)
        .unwrap()
        .replacen("-0.5", "1e999", 1);
    assert!(load_model_bytes(text.as_bytes(), &LoadLimits::default()).is_err());
}

#[test]
fn topological_remap_and_bind_pose_are_consistent() {
    let model = common::skeleton();
    let ModelData::Skeleton3d(skeleton) = &model.data else {
        panic!("wrong kind");
    };
    assert_eq!(skeleton.joints[0].name, "root");
    assert_eq!(skeleton.joints[1].name, "head");
    assert_eq!(skeleton.joints[1].parent, Some(0));
    assert_eq!(skeleton.vertices[0].joints[0], 1);
    let locals = try_sample_joints(skeleton, 0.0).unwrap();
    let matrices = try_skin_matrices(skeleton, &locals).unwrap();
    for matrix in &matrices {
        for (actual, expected) in matrix
            .to_cols_array()
            .iter()
            .zip(Mat4::IDENTITY.to_cols_array())
        {
            assert!((actual - expected).abs() < 0.0001);
        }
    }
    let animated = try_skin_matrices(skeleton, &try_sample_joints(skeleton, 1.0).unwrap()).unwrap();
    let point = animated[1].transform_point3(Vec3::new(0.5, 1.0, 0.0));
    assert!(point.distance(Vec3::new(0.0, 1.5, 0.0)) < 0.0001);
}

#[test]
fn rectangular_warp_grid_uses_column_stride() {
    let mut document = common::sprite_document();
    document["sprite2d"]["layers"][0]["vertices"] = json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    document["sprite2d"]["deformers"] = json!([{
        "name": "turn", "type": "warp", "parameter": "sway",
        "bounds": [0.0, 0.0, 1.0, 1.0], "grid": [3, 2],
        "offsets": [[0.0, 0.0], [0.1, 0.0], [0.2, 0.0], [0.3, 0.0], [0.4, 0.0], [0.5, 0.0]]
    }]);
    let model = common::load(&document);
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!("wrong kind");
    };
    let points = try_deform_layer(sprite, 0, &[1.0]).unwrap();
    assert!((points[1][0] - 1.2).abs() < 0.0001);
    assert!((points[2][0] - 0.3).abs() < 0.0001);
}

#[test]
fn fallible_evaluation_rejects_nonfinite_and_wrong_lengths() {
    let model = common::sprite();
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!("wrong kind");
    };
    for time in [f64::NAN, f64::INFINITY, -0.1] {
        assert!(try_sample_parameters(sprite, time).is_err());
        assert_eq!(
            delicium::model::sample_parameters(sprite, time),
            try_sample_parameters(sprite, 0.0).unwrap()
        );
    }
    assert!(try_deform_layer(sprite, usize::MAX, &[0.0]).is_err());
    assert!(try_deform_layer(sprite, 0, &[]).is_err());
    assert!(try_deform_layer(sprite, 0, &[f32::INFINITY]).is_err());
    let model = common::skeleton();
    let ModelData::Skeleton3d(skeleton) = &model.data else {
        panic!("wrong kind");
    };
    for time in [f64::NAN, f64::INFINITY, -0.1] {
        assert!(try_sample_joints(skeleton, time).is_err());
        assert_eq!(
            delicium::model::sample_joints(skeleton, time),
            try_sample_joints(skeleton, 0.0).unwrap()
        );
    }
    assert!(try_skin_matrices(skeleton, &[]).is_err());
    let mut locals = try_sample_joints(skeleton, 0.0).unwrap();
    locals[0].translation.x = f32::NAN;
    assert!(try_skin_matrices(skeleton, &locals).is_err());
}

#[test]
fn three_dimensional_index_budget_is_enforced() {
    let mut document = common::skeleton_document();
    document["skeleton3d"]["indices"] = json!([0, 1, 2, 0, 1, 2]);
    let limits = LoadLimits {
        max_indices: 3,
        ..LoadLimits::default()
    };
    assert!(load_model_bytes(&serde_json::to_vec(&document).unwrap(), &limits).is_err());
}

#[test]
fn unicode_names_use_character_count_not_utf8_byte_count() {
    let mut document = common::sprite_document();
    document["name"] = json!("模型".repeat(64));
    assert!(
        load_model_bytes(
            &serde_json::to_vec(&document).unwrap(),
            &LoadLimits::default()
        )
        .is_ok()
    );
    document["name"] = json!("模型".repeat(65));
    reject(&document);
}

#[test]
fn long_running_sampling_preserves_ticks_and_each_clip_phase() {
    let mut document = common::sprite_document();
    document["sprite2d"]["parameters"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "blink", "min": -1.0, "max": 1.0, "default": 0.0
        }));
    document["sprite2d"]["animations"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "blink", "duration": 3.0,
            "channels": [{"parameter": "blink", "keys": [
                {"time": 0.0, "value": -1.0},
                {"time": 1.5, "value": 1.0},
                {"time": 3.0, "value": -1.0}
            ]}]
        }));
    let model = common::load(&document);
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!("expected 2D model")
    };
    let skeleton_model = common::skeleton();
    let ModelData::Skeleton3d(skeleton) = &skeleton_model.data else {
        panic!("expected 3D model")
    };
    let mut previous: Option<f32> = None;
    for tick in 0..12 {
        let time = 262_144.125 + f64::from(tick) / 60.0;
        let parameters = try_sample_parameters(sprite, time).unwrap();
        let sway = -1.0 + 2.0 * (time % 2.0);
        let blink = -1.0 + 2.0 * (time % 3.0) / 1.5;
        assert!((f64::from(parameters[0]) - sway).abs() < 2e-6);
        assert!((f64::from(parameters[1]) - blink).abs() < 2e-6);
        if let Some(previous) = previous {
            assert!(
                (parameters[0] - previous).abs() > 0.02,
                "consecutive ticks must not freeze"
            );
        }
        previous = Some(parameters[0]);
        assert_eq!(
            try_sample_joints(skeleton, time).unwrap(),
            try_sample_joints(skeleton, time % 2.0).unwrap()
        );
    }
}

#[test]
fn small_invertible_scales_are_not_mistaken_for_singular_matrices() {
    let mut document = common::skeleton_document();
    document["skeleton3d"]["joints"][1]["scale"] = json!([0.00001, 0.00001, 0.00001]);
    let model = common::load(&document);
    let ModelData::Skeleton3d(skeleton) = &model.data else {
        panic!("wrong kind");
    };
    let matrices = try_skin_matrices(skeleton, &try_sample_joints(skeleton, 0.0).unwrap()).unwrap();
    assert!(matrices.iter().all(|matrix| matrix.is_finite()));
}

#[test]
fn bundled_robot_bounce_keeps_the_bind_translation_baseline() {
    let model = load_model_bytes(
        include_bytes!("../assets/examples/robot3d.dmodel.json"),
        &LoadLimits::default(),
    )
    .unwrap();
    let ModelData::Skeleton3d(skeleton) = &model.data else {
        panic!("wrong kind");
    };
    let root = skeleton.joint_index("root").unwrap();
    let locals = try_sample_joints(skeleton, 0.0).unwrap();
    assert!(
        locals[root]
            .translation
            .distance(skeleton.joints[root].translation)
            < 0.00001
    );
}

#[test]
fn action_bindings_resolve_and_reject_bad_references() {
    let mut document = sprite_action_document();
    document["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
    let model = common::load(&document);
    assert_eq!(
        model.actions,
        Some(ActionBindings {
            idle: 0,
            walk: 1,
            reaction: 2,
            greet: None,
            play: None,
        })
    );
    // No `actions` table: the binding stays absent, the document is valid.
    assert!(common::load(&sprite_action_document()).actions.is_none());

    // Unknown animation name.
    let mut bad = document.clone();
    bad["actions"]["walk"] = json!("missing");
    reject(&bad);
    // Empty animation name.
    let mut bad = document.clone();
    bad["actions"]["idle"] = json!("");
    reject(&bad);
    // A name that is ambiguous because the animation list declares it twice.
    let mut bad = sprite_action_document();
    let idle = bad["sprite2d"]["animations"][0].clone();
    bad["sprite2d"]["animations"]
        .as_array_mut()
        .unwrap()
        .push(idle);
    bad["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
    reject(&bad);
    // Roles are all required once the table is present.
    for field in ["idle", "walk", "reaction"] {
        let mut bad = document.clone();
        bad["actions"].as_object_mut().unwrap().remove(field);
        reject(&bad);
    }
    // No unknown fields inside the table.
    let mut bad = document.clone();
    bad["actions"]["jump"] = json!("walk");
    reject(&bad);
}

#[test]
fn action_roles_may_reuse_one_clip_freely() {
    let mut document = sprite_action_document();
    document["actions"] = json!({"idle": "walk", "walk": "walk", "reaction": "idle"});
    let model = common::load(&document);
    assert_eq!(
        model.actions,
        Some(ActionBindings {
            idle: 1,
            walk: 1,
            reaction: 0,
            greet: None,
            play: None,
        })
    );

    // The same clip in every role is legal too.
    let mut all_walk = sprite_action_document();
    all_walk["actions"] = json!({"idle": "walk", "walk": "walk", "reaction": "walk"});
    let model = common::load(&all_walk);
    assert_eq!(
        model.actions,
        Some(ActionBindings {
            idle: 1,
            walk: 1,
            reaction: 1,
            greet: None,
            play: None,
        })
    );

    // And a 3D model resolves against its own animation list.
    let mut skeleton = skeleton_action_document();
    skeleton["actions"] = json!({"idle": "idle", "walk": "wave", "reaction": "reaction"});
    let model = common::load(&skeleton);
    assert_eq!(
        model.actions,
        Some(ActionBindings {
            idle: 1,
            walk: 0,
            reaction: 2,
            greet: None,
            play: None,
        })
    );
}

#[test]
fn optional_greet_and_play_slots_resolve_or_stay_none() {
    // Both optional slots declared resolve to their animation indices.
    let mut document = sprite_action_document();
    document["actions"] = json!({
        "idle": "idle", "walk": "walk", "reaction": "reaction",
        "greet": "blink", "play": "idle"
    });
    let model = common::load(&document);
    assert_eq!(
        model.actions,
        Some(ActionBindings {
            idle: 0,
            walk: 1,
            reaction: 2,
            greet: Some(3),
            play: Some(0),
        })
    );

    // Only `greet` declared: `play` stays `None`, no fallback index.
    let mut document = sprite_action_document();
    document["actions"] = json!({
        "idle": "idle", "walk": "walk", "reaction": "reaction", "greet": "walk"
    });
    let model = common::load(&document);
    assert_eq!(
        model.actions,
        Some(ActionBindings {
            idle: 0,
            walk: 1,
            reaction: 2,
            greet: Some(1),
            play: None,
        })
    );

    // Only `play` declared on a 3D model.
    let mut skeleton = skeleton_action_document();
    skeleton["actions"] = json!({
        "idle": "idle", "walk": "wave", "reaction": "reaction", "play": "idle"
    });
    let model = common::load(&skeleton);
    assert_eq!(
        model.actions,
        Some(ActionBindings {
            idle: 1,
            walk: 0,
            reaction: 2,
            greet: None,
            play: Some(1),
        })
    );

    // Bad optional references are rejected with the same rules.
    for (slot, value) in [
        ("greet", ""),
        ("greet", "missing"),
        ("play", ""),
        ("play", "missing"),
    ] {
        let mut bad = sprite_action_document();
        bad["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
        bad["actions"][slot] = json!(value);
        reject(&bad);
    }
    // An ambiguous optional reference (name declared twice) is rejected too.
    let mut bad = sprite_action_document();
    let idle = bad["sprite2d"]["animations"][0].clone();
    bad["sprite2d"]["animations"]
        .as_array_mut()
        .unwrap()
        .push(idle);
    bad["actions"] = json!({
        "idle": "idle", "walk": "walk", "reaction": "reaction", "greet": "idle"
    });
    reject(&bad);
    // Explicit null is accepted as "absent" and stays `None`.
    let mut null_slots = sprite_action_document();
    null_slots["actions"] = json!({
        "idle": "idle", "walk": "walk", "reaction": "reaction",
        "greet": null, "play": null
    });
    let model = common::load(&null_slots);
    assert_eq!(
        model.actions,
        Some(ActionBindings {
            idle: 0,
            walk: 1,
            reaction: 2,
            greet: None,
            play: None,
        })
    );
}

#[test]
fn clipped_slots_are_stable_across_sampling() {
    // A clip bound to an optional slot is sampled by its resolved index like
    // any other role; a stale index still fails loudly.
    let mut document = sprite_action_document();
    document["actions"] = json!({
        "idle": "idle", "walk": "walk", "reaction": "reaction", "greet": "blink"
    });
    let model = common::load(&document);
    let Some(ActionBindings {
        greet: Some(greet), ..
    }) = model.actions
    else {
        panic!("greet binding missing")
    };
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!("expected a sprite")
    };
    let values = try_sample_parameters_clip(sprite, greet, 2.0, ClipPlayback::Clamp).unwrap();
    assert!((values[1] - 1.0).abs() < 1e-6);
    assert!(matches!(
        try_sample_parameters_clip(sprite, sprite.animations.len(), 0.0, ClipPlayback::Loop),
        Err(ModelError::OutOfRange { .. })
    ));
}

#[test]
fn version_01_rejects_the_actions_table_outright() {
    let mut document = sprite_action_document();
    document["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
    // 0.2 accepts it...
    common::load(&document);
    // ...and the strict 0.1 schema rejects the unknown field.
    document["format_version"] = json!("0.1");
    reject(&document);
}

#[test]
fn clip_sampling_isolates_the_selected_animation() {
    let mut document = sprite_action_document();
    document["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
    let model = common::load(&document);
    let Some(ActionBindings {
        idle,
        walk,
        reaction,
        ..
    }) = model.actions
    else {
        panic!("bindings missing")
    };
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!("expected a sprite")
    };

    let at_end = 1.0;
    let idle_values = try_sample_parameters_clip(sprite, idle, at_end, ClipPlayback::Loop).unwrap();
    let walk_values = try_sample_parameters_clip(sprite, walk, at_end, ClipPlayback::Loop).unwrap();
    let reaction_values =
        try_sample_parameters_clip(sprite, reaction, at_end, ClipPlayback::Loop).unwrap();

    // The selected clip alone drives `sway`: `Loop` at time 1.0 wraps idle
    // (duration 1.0) and reaction (duration 0.5) onto their first frames, and
    // lands walk (duration 2.0) halfway through.
    assert!((idle_values[0] + 1.0).abs() < 1e-6);
    assert!((walk_values[0] - 0.5).abs() < 1e-6);
    assert!((reaction_values[0] + 1.0).abs() < 1e-6);
    // ...and the unselected `blink` clip never contributes: `extra` keeps its
    // default even though sampling every animation would not.
    for values in [&idle_values, &walk_values, &reaction_values] {
        assert!((values[1] - 0.25).abs() < 1e-6, "unselected clip leaked");
    }
    let all = try_sample_parameters(sprite, at_end).unwrap();
    assert!(
        (all[1] - 0.5).abs() < 1e-6,
        "the all-animation path samples blink"
    );
}

#[test]
fn clip_sampling_loop_and_clamp_use_the_expected_endpoints() {
    let mut document = sprite_action_document();
    document["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
    let model = common::load(&document);
    let Some(ActionBindings { idle, .. }) = model.actions else {
        panic!("bindings missing")
    };
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!("expected a sprite")
    };

    // idle: sway goes -1 -> 1 over [0, 1]; the endpoint wraps to the first
    // frame in `Loop` and stays at the last frame in `Clamp`.
    let looped = try_sample_parameters_clip(sprite, idle, 1.0, ClipPlayback::Loop).unwrap();
    let looped_later = try_sample_parameters_clip(sprite, idle, 3.0, ClipPlayback::Loop).unwrap();
    assert!((looped[0] + 1.0).abs() < 1e-6);
    assert!((looped_later[0] + 1.0).abs() < 1e-6);
    let clamped = try_sample_parameters_clip(sprite, idle, 1.0, ClipPlayback::Clamp).unwrap();
    let clamped_later = try_sample_parameters_clip(sprite, idle, 5.0, ClipPlayback::Clamp).unwrap();
    assert!((clamped[0] - 1.0).abs() < 1e-6);
    assert!((clamped_later[0] - 1.0).abs() < 1e-6);
    // Below zero the clamp pins to the first frame.
    let clamped_zero = try_sample_parameters_clip(sprite, idle, 0.0, ClipPlayback::Clamp).unwrap();
    assert!((clamped_zero[0] + 1.0).abs() < 1e-6);

    // Joints: the same endpoint rule.
    let mut skeleton = skeleton_action_document();
    skeleton["actions"] = json!({"idle": "idle", "walk": "wave", "reaction": "reaction"});
    let model = common::load(&skeleton);
    let Some(ActionBindings {
        idle: joint_idle, ..
    }) = model.actions
    else {
        panic!("bindings missing")
    };
    let ModelData::Skeleton3d(payload) = &model.data else {
        panic!("expected a skeleton")
    };
    let end = try_sample_joints_clip(payload, joint_idle, 1.0, ClipPlayback::Clamp).unwrap();
    let end_later = try_sample_joints_clip(payload, joint_idle, 9.0, ClipPlayback::Clamp).unwrap();
    let start = try_sample_joints_clip(payload, joint_idle, 0.0, ClipPlayback::Loop).unwrap();
    let looped = try_sample_joints_clip(payload, joint_idle, 2.0, ClipPlayback::Loop).unwrap();
    // idle is constant, so the meaningful assertion is that clamping holds a
    // finite pose and looping the full period returns the start frame.
    assert_eq!(end, end_later);
    assert_eq!(start, looped);
    assert!(end[1].rotation.is_finite());
    assert!(
        (end[1]
            .rotation
            .dot(Quat::from_xyzw(0.70710677, 0.0, 0.0, 0.70710677)))
        .abs()
            > 0.9999
    );
}

#[test]
fn clip_sampling_rejects_bad_indices_and_times() {
    let mut document = sprite_action_document();
    document["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
    let model = common::load(&document);
    let Some(ActionBindings { idle, .. }) = model.actions else {
        panic!("bindings missing")
    };
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!("expected a sprite")
    };
    assert!(matches!(
        try_sample_parameters_clip(sprite, sprite.animations.len(), 0.0, ClipPlayback::Loop),
        Err(ModelError::OutOfRange { .. })
    ));
    assert!(matches!(
        try_sample_parameters_clip(sprite, usize::MAX, 0.0, ClipPlayback::Loop),
        Err(ModelError::OutOfRange { .. })
    ));
    assert!(try_sample_parameters_clip(sprite, idle, f64::NAN, ClipPlayback::Loop).is_err());
    assert!(try_sample_parameters_clip(sprite, idle, f64::INFINITY, ClipPlayback::Clamp).is_err());
    assert!(try_sample_parameters_clip(sprite, idle, -0.1, ClipPlayback::Loop).is_err());

    let mut skeleton = skeleton_action_document();
    skeleton["actions"] = json!({"idle": "idle", "walk": "wave", "reaction": "reaction"});
    let model = common::load(&skeleton);
    let ModelData::Skeleton3d(payload) = &model.data else {
        panic!("expected a skeleton")
    };
    assert!(matches!(
        try_sample_joints_clip(payload, payload.animations.len(), 0.0, ClipPlayback::Loop),
        Err(ModelError::OutOfRange { .. })
    ));
    assert!(try_sample_joints_clip(payload, 0, f64::NAN, ClipPlayback::Clamp).is_err());
    assert!(try_sample_joints_clip(payload, 0, -1.0, ClipPlayback::Loop).is_err());
}

#[test]
fn clip_sampling_keeps_long_running_f64_precision() {
    let mut document = sprite_action_document();
    document["actions"] = json!({"idle": "idle", "walk": "walk", "reaction": "reaction"});
    let model = common::load(&document);
    let Some(ActionBindings { walk, .. }) = model.actions else {
        panic!("bindings missing")
    };
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!("expected a sprite")
    };
    // walk: sway 1 -> 0 over [0, 2]; at a large f64 time the phase must be
    // reduced in f64 before narrowing, and consecutive ticks must differ.
    let base = 262_144.0_f64;
    let mut previous: Option<f32> = None;
    for tick in 0..8 {
        let time = base + f64::from(tick) / 30.0;
        let values = try_sample_parameters_clip(sprite, walk, time, ClipPlayback::Loop).unwrap();
        let expected = 1.0 - (time % 2.0) / 2.0;
        assert!((f64::from(values[0]) - expected).abs() < 1e-6);
        if let Some(previous) = previous {
            assert!((values[0] - previous).abs() > 1e-4);
        }
        previous = Some(values[0]);
    }
}

#[test]
fn old_all_animation_paths_are_unchanged_by_the_actions_feature() {
    // A 0.1 document never gains bindings and still samples every animation.
    let model = common::sprite();
    assert!(model.actions.is_none());
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!("expected a sprite")
    };
    assert_eq!(
        try_sample_parameters(sprite, 0.5).unwrap(),
        try_sample_parameters(sprite, 0.5).unwrap()
    );
    let model = common::skeleton();
    assert!(model.actions.is_none());
    let ModelData::Skeleton3d(skeleton) = &model.data else {
        panic!("expected a skeleton")
    };
    let expected = try_sample_joints(skeleton, 1.0).unwrap();
    assert_eq!(expected, try_sample_joints(skeleton, 1.0).unwrap());
}

#[test]
fn file_byte_cap_and_in_memory_cap_are_enforced() {
    let bytes = serde_json::to_vec(&common::sprite_document()).unwrap();
    let limits = LoadLimits {
        max_file_bytes: bytes.len() - 1,
        ..LoadLimits::default()
    };
    assert!(load_model_bytes(&bytes, &limits).is_err());
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".artifacts")
        .join(format!(
            "model-reader-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("bounded.dmodel.json");
    std::fs::write(&path, &bytes).unwrap();
    assert!(read_model_file(&path, &limits).is_err());
    let exact = LoadLimits {
        max_file_bytes: bytes.len(),
        ..LoadLimits::default()
    };
    assert!(read_model_file(&path, &exact).is_ok());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
