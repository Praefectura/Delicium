mod common;

use delicium::model::{
    LoadLimits, ModelData, ModelKind, load_model_bytes, read_model_file, try_deform_layer,
    try_sample_joints, try_sample_parameters, try_skin_matrices,
};
use glam::{Mat4, Vec3};
use serde_json::{Value, json};

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
