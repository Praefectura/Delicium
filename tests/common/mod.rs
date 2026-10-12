#![allow(dead_code)]

use std::sync::Arc;

use delicium::model::{CompiledModel, LoadLimits, load_model_bytes};
use serde_json::{Value, json};

pub fn sprite_document() -> Value {
    json!({
        "format": "delicium.model", "format_version": "0.1", "name": "Test sprite",
        "kind": "sprite2d",
        "sprite2d": {
            "parameters": [{"name": "sway", "min": -1.0, "max": 1.0, "default": 0.0}],
            "layers": [{
                "name": "body", "color": [1.0, 0.2, 0.1, 1.0],
                "vertices": [[-0.5, 0.0], [0.5, 0.0], [0.0, 1.0]],
                "indices": [0, 1, 2], "deformer": "turn"
            }],
            "deformers": [{
                "name": "turn", "type": "rotation", "parameter": "sway",
                "pivot": [0.0, 0.0], "max_angle_radians": 0.4
            }],
            "animations": [{
                "name": "sway", "duration": 2.0,
                "channels": [{"parameter": "sway", "keys": [
                    {"time": 0.0, "value": -1.0},
                    {"time": 1.0, "value": 1.0},
                    {"time": 2.0, "value": -1.0}
                ]}]
            }]
        }
    })
}

pub fn skeleton_document() -> Value {
    json!({
        "format": "delicium.model", "format_version": "0.1", "name": "Test skeleton",
        "kind": "skeleton3d",
        "skeleton3d": {
            "vertices": [
                {"position": [-0.5, 1.0, 0.0], "normal": [0.0, 0.0, 1.0], "color": [0.1, 0.8, 0.2, 1.0], "joints": [0, 0, 0, 0], "weights": [1.0, 0.0, 0.0, 0.0]},
                {"position": [0.5, 1.0, 0.0], "normal": [0.0, 0.0, 1.0], "color": [0.1, 0.8, 0.2, 1.0], "joints": [0, 0, 0, 0], "weights": [1.0, 0.0, 0.0, 0.0]},
                {"position": [0.0, 2.0, 0.0], "normal": [0.0, 0.0, 1.0], "color": [0.1, 0.8, 0.2, 1.0], "joints": [0, 0, 0, 0], "weights": [1.0, 0.0, 0.0, 0.0]}
            ],
            "indices": [0, 1, 2],
            "joints": [
                {"name": "head", "parent": "root", "translation": [0.0, 1.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0]},
                {"name": "root", "translation": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0]}
            ],
            "animations": [{
                "name": "wave", "duration": 2.0,
                "channels": [{"joint": "head", "rotation": [
                    {"time": 0.0, "value": [0.0, 0.0, 0.0, 1.0]},
                    {"time": 1.0, "value": [0.0, 0.0, 0.70710677, 0.70710677]},
                    {"time": 2.0, "value": [0.0, 0.0, 0.0, 1.0]}
                ]}]
            }]
        }
    })
}

pub fn load(document: &Value) -> Arc<CompiledModel> {
    load_model_bytes(
        &serde_json::to_vec(document).unwrap(),
        &LoadLimits::default(),
    )
    .unwrap()
}

pub fn sprite() -> Arc<CompiledModel> {
    load(&sprite_document())
}
pub fn skeleton() -> Arc<CompiledModel> {
    load(&skeleton_document())
}
