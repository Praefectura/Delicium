mod common;

use base64::{Engine, engine::general_purpose::STANDARD};
use delicium::model::{AlphaMode, LoadLimits, ModelData, load_model_bytes, try_deform_layer};
use serde_json::{Value, json};

fn png_bytes(
    color: png::ColorType,
    depth: png::BitDepth,
    width: u32,
    height: u32,
    data: &[u8],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
    }
    bytes
}

fn rgba_png(pixel: [u8; 4]) -> Vec<u8> {
    png_bytes(png::ColorType::Rgba, png::BitDepth::Eight, 1, 1, &pixel)
}

fn sprite_with_png(bytes: &[u8]) -> Value {
    let mut document = common::sprite_document();
    document["format_version"] = json!("0.2");
    document["images"] = json!([{"id": "paint", "png_base64": STANDARD.encode(bytes)}]);
    let layer = &mut document["sprite2d"]["layers"][0];
    layer["texture"] = json!("paint");
    layer["uvs"] = json!([[0.0, 1.0], [1.0, 1.0], [0.5, 0.0]]);
    document
}

fn skeleton_with_png(bytes: &[u8]) -> Value {
    let mut document = common::skeleton_document();
    document["format_version"] = json!("0.2");
    document["images"] = json!([{"id": "paint", "png_base64": STANDARD.encode(bytes)}]);
    document["skeleton3d"]["texture"] = json!("paint");
    document["skeleton3d"]["uvs"] = json!([[0.0, 1.0], [1.0, 1.0], [0.5, 0.0]]);
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
fn both_textured_examples_load_without_changing_the_original_geometry() {
    for (old, new) in [
        (
            include_bytes!("../assets/examples/puppet2d.dmodel.json").as_slice(),
            include_bytes!("../assets/examples/puppet2d-textured.dmodel.json").as_slice(),
        ),
        (
            include_bytes!("../assets/examples/robot3d.dmodel.json").as_slice(),
            include_bytes!("../assets/examples/robot3d-textured.dmodel.json").as_slice(),
        ),
    ] {
        let old = load_model_bytes(old, &LoadLimits::default()).unwrap();
        let new = load_model_bytes(new, &LoadLimits::default()).unwrap();
        assert!(old.images.is_empty());
        assert!(!new.images.is_empty());
        assert_eq!(old.name, new.name);
        match (&old.data, &new.data) {
            (ModelData::Sprite2d(a), ModelData::Sprite2d(b)) => {
                assert_eq!(a.layers.len(), b.layers.len());
                for (a, b) in a.layers.iter().zip(&b.layers) {
                    assert_eq!(a.vertices, b.vertices);
                    assert_eq!(a.indices, b.indices);
                    if b.texture.is_some() {
                        assert_eq!(b.vertices.len(), b.uvs.len());
                    }
                }
            }
            (ModelData::Skeleton3d(a), ModelData::Skeleton3d(b)) => {
                assert_eq!(a.indices, b.indices);
                assert_eq!(a.joints.len(), b.joints.len());
                for (a, b) in a.vertices.iter().zip(&b.vertices) {
                    assert_eq!(a.position, b.position);
                    assert_eq!(a.normal, b.normal);
                    assert_eq!(a.joints, b.joints);
                    assert_eq!(a.weights, b.weights);
                }
                assert!(a.material.is_none());
                assert!(b.material.is_some());
            }
            _ => panic!("Example kind changed"),
        }
    }
}

#[test]
fn image_alpha_is_premultiplied_in_linear_space_not_in_srgb_bytes() {
    let model = common::load(&sprite_with_png(&rgba_png([255, 128, 64, 128])));
    let image = &model.images[0];
    assert_eq!((image.width, image.height), (1, 1));
    assert_eq!(image.rgba[3], 128);
    assert!(image.rgba[0].abs_diff(188) <= 1);
    assert!(image.rgba[1].abs_diff(93) <= 1);
    assert!(image.rgba[2].abs_diff(44) <= 1);
    assert!(!image.opaque);
    assert!(!image.fully_transparent);
    let hidden = common::load(&sprite_with_png(&rgba_png([255, 128, 64, 0])));
    assert_eq!(hidden.images[0].rgba.as_ref(), &[0, 0, 0, 0]);
    let ModelData::Sprite2d(sprite) = &hidden.data else {
        panic!()
    };
    assert!(!sprite.layers[0].texture_visible);
}

#[test]
fn untextured_v02_uses_the_legacy_color_fallback() {
    for mut document in [common::sprite_document(), common::skeleton_document()] {
        document["format_version"] = json!("0.2");
        let model = common::load(&document);
        assert!(model.images.is_empty());
        match &model.data {
            ModelData::Sprite2d(sprite) => {
                assert!(sprite.layers[0].texture.is_none());
                assert_eq!(sprite.layers[0].color, [1.0, 0.2, 0.1, 1.0]);
            }
            ModelData::Skeleton3d(skeleton) => {
                assert!(skeleton.material.is_none());
                assert!(skeleton.texture.is_none());
                assert_eq!(skeleton.vertices[0].uv, [0.0, 0.0]);
            }
        }
    }
}

#[test]
fn grid_uv_has_top_left_origin_and_remains_static_during_deformation() {
    let mut document = sprite_with_png(&rgba_png([255, 255, 255, 255]));
    let layer = document["sprite2d"]["layers"][0].as_object_mut().unwrap();
    layer.remove("vertices");
    layer.remove("indices");
    layer.remove("uvs");
    layer.insert(
        "grid".into(),
        json!({"bounds": [-0.5, 0.0, 1.0, 1.0], "columns": 2, "rows": 2}),
    );
    let model = common::load(&document);
    let ModelData::Sprite2d(sprite) = &model.data else {
        panic!()
    };
    let layer = &sprite.layers[0];
    let bottom_left = layer
        .vertices
        .iter()
        .position(|p| *p == [-0.5, 0.0])
        .unwrap();
    let top_right = layer
        .vertices
        .iter()
        .position(|p| *p == [0.5, 1.0])
        .unwrap();
    assert_eq!(layer.uvs[bottom_left], [0.0, 1.0]);
    assert_eq!(layer.uvs[top_right], [1.0, 0.0]);
    let uv = layer.uvs.clone();
    let deformed = try_deform_layer(sprite, 0, &[1.0]).unwrap();
    assert_ne!(deformed, layer.vertices);
    assert_eq!(layer.uvs, uv);
}

#[test]
fn strict_versions_fields_references_and_resource_use_are_enforced() {
    let original = sprite_with_png(&rgba_png([255, 255, 255, 255]));
    common::load(&original);
    let mut doc = original.clone();
    doc["format_version"] = json!("0.1");
    reject(&doc);
    doc = original.clone();
    doc["format_version"] = json!("0.3");
    reject(&doc);
    doc = original.clone();
    doc["images"][0]["url"] = json!("https://example.invalid/image.png");
    reject(&doc);
    doc = original.clone();
    doc["sprite2d"]["layers"][0]["texture"] = json!("missing");
    reject(&doc);
    doc = original.clone();
    let image = doc["images"][0].clone();
    doc["images"].as_array_mut().unwrap().push(image);
    reject(&doc);
    doc = original.clone();
    let mut image = doc["images"][0].clone();
    image["id"] = json!("unused");
    doc["images"].as_array_mut().unwrap().push(image);
    reject(&doc);
    let bytes = serde_json::to_string(&original).unwrap();
    let duplicate = bytes.replacen('{', "{\"format_version\":\"0.2\",", 1);
    assert!(load_model_bytes(duplicate.as_bytes(), &LoadLimits::default()).is_err());
}

#[test]
fn explicit_textured_uv_must_match_vertices_and_stay_finite_in_range() {
    let original = sprite_with_png(&rgba_png([255, 255, 255, 255]));
    for uv in [
        json!([]),
        json!([[0, 0]]),
        json!([[0, 0], [1.1, 1], [0, 1]]),
        json!([[0, 0], [1, 1], [-0.1, 1]]),
        json!([[0, 0], [1, 1], [1e200, 1]]),
    ] {
        let mut doc = original.clone();
        doc["sprite2d"]["layers"][0]["uvs"] = uv;
        reject(&doc);
    }
    let mut doc = original;
    doc["sprite2d"]["layers"][0]
        .as_object_mut()
        .unwrap()
        .remove("uvs");
    reject(&doc);
    let mut doc = skeleton_with_png(&rgba_png([255, 255, 255, 255]));
    doc["skeleton3d"]["uvs"] = json!([[0, 0]]);
    reject(&doc);
}

#[test]
fn malformed_base64_is_not_accepted_as_an_image() {
    let original = sprite_with_png(&rgba_png([255, 255, 255, 255]));
    let encoded = original["images"][0]["png_base64"].as_str().unwrap();
    for invalid in [
        "!".to_owned(),
        format!("{encoded}\n"),
        format!("data:image/png;base64,{encoded}"),
        format!("{encoded}="),
    ] {
        let mut doc = original.clone();
        doc["images"][0]["png_base64"] = json!(invalid);
        reject(&doc);
    }
}

#[test]
fn base64_requires_canonical_padding_for_an_otherwise_valid_png() {
    let bytes = (0..3)
        .find_map(|length| {
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .add_text_chunk("padding".into(), "x".repeat(length))
                    .unwrap();
                let mut writer = encoder.write_header().unwrap();
                writer.write_image_data(&[255; 4]).unwrap();
                writer.finish().unwrap();
            }
            (bytes.len() % 3 != 0).then_some(bytes)
        })
        .unwrap();
    let mut document = sprite_with_png(&bytes);
    common::load(&document);
    let encoded = document["images"][0]["png_base64"].as_str().unwrap();
    assert!(encoded.ends_with('='));
    document["images"][0]["png_base64"] = json!(encoded.trim_end_matches('='));
    reject(&document);
}

#[test]
fn png_crc_and_end_of_stream_are_validated() {
    let bytes = rgba_png([255, 255, 255, 255]);
    for corrupt in [
        {
            let mut value = bytes.clone();
            value[29] ^= 1;
            value
        },
        {
            let mut value = bytes.clone();
            let index = value.len() - 1;
            value[index] ^= 1;
            value
        },
        bytes[..bytes.len() - 12].to_vec(),
        bytes[..bytes.len() / 2].to_vec(),
    ] {
        reject(&sprite_with_png(&corrupt));
    }
}

#[test]
fn image_dimensions_and_decoded_budget_are_checked() {
    let largest = png_bytes(
        png::ColorType::Rgba,
        png::BitDepth::Eight,
        2048,
        2048,
        &vec![255; 16 * 1024 * 1024],
    );
    let model = common::load(&sprite_with_png(&largest));
    assert_eq!(
        (model.images[0].width, model.images[0].height),
        (2048, 2048)
    );
    assert_eq!(model.images[0].rgba.len(), 16 * 1024 * 1024);
    let oversized = png_bytes(
        png::ColorType::Rgba,
        png::BitDepth::Eight,
        2049,
        1,
        &vec![255; 2049 * 4],
    );
    reject(&sprite_with_png(&oversized));
    let doc = sprite_with_png(&rgba_png([255, 255, 255, 255]));
    let bytes = serde_json::to_vec(&doc).unwrap();
    for limits in [
        LoadLimits {
            max_image_bytes: 3,
            ..LoadLimits::default()
        },
        LoadLimits {
            max_total_image_bytes: 3,
            ..LoadLimits::default()
        },
        LoadLimits {
            max_images: 0,
            ..LoadLimits::default()
        },
        LoadLimits {
            max_png_decoder_bytes: 0,
            ..LoadLimits::default()
        },
    ] {
        assert!(load_model_bytes(&bytes, &limits).is_err());
    }
    let mut doc = doc;
    let mut layer = doc["sprite2d"]["layers"][0].clone();
    layer["name"] = json!("second");
    layer["texture"] = json!("second-paint");
    doc["sprite2d"]["layers"]
        .as_array_mut()
        .unwrap()
        .push(layer);
    let mut image = doc["images"][0].clone();
    image["id"] = json!("second-paint");
    doc["images"].as_array_mut().unwrap().push(image);
    common::load(&doc);
    let limits = LoadLimits {
        max_total_image_bytes: 4,
        ..LoadLimits::default()
    };
    assert!(load_model_bytes(&serde_json::to_vec(&doc).unwrap(), &limits).is_err());
}

#[test]
fn gray_and_palette_png_expand_to_rgba8() {
    let gray = png_bytes(
        png::ColorType::Grayscale,
        png::BitDepth::Eight,
        1,
        1,
        &[128],
    );
    assert_eq!(
        common::load(&sprite_with_png(&gray)).images[0]
            .rgba
            .as_ref(),
        &[128, 128, 128, 255]
    );
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::One);
        encoder.set_palette(vec![255, 0, 0]);
        encoder.set_trns(vec![128]);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0]).unwrap();
        writer.finish().unwrap();
    }
    let model = common::load(&sprite_with_png(&bytes));
    assert!(model.images[0].rgba[0].abs_diff(188) <= 1);
    assert_eq!(&model.images[0].rgba[1..], &[0, 0, 128]);
}

#[test]
fn sixteen_bit_and_animated_png_are_rejected() {
    let image = png_bytes(
        png::ColorType::Rgba,
        png::BitDepth::Sixteen,
        1,
        1,
        &[255; 8],
    );
    reject(&sprite_with_png(&image));
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_animated(1, 0).unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255; 4]).unwrap();
        writer.finish().unwrap();
    }
    reject(&sprite_with_png(&bytes));
}

#[test]
fn new_material_enforces_opaque_and_cutout_alpha() {
    let opaque = skeleton_with_png(&rgba_png([255, 255, 255, 255]));
    let model = common::load(&opaque);
    let ModelData::Skeleton3d(skeleton) = &model.data else {
        panic!()
    };
    assert_eq!(skeleton.material.unwrap().alpha_mode, AlphaMode::Opaque);
    reject(&skeleton_with_png(&rgba_png([255, 255, 255, 128])));
    let mut doc = opaque;
    doc["skeleton3d"]["vertices"][0]["color"][3] = json!(0.5);
    reject(&doc);
    doc["skeleton3d"]["material"] =
        json!({"roughness": 0.25, "metallic": 0.75, "alpha_mode": "cutout", "alpha_cutoff": 0.5});
    common::load(&doc);
    for (field, value) in [
        ("roughness", json!(-0.1)),
        ("roughness", json!(1.1)),
        ("metallic", json!(1.1)),
        ("alpha_mode", json!("blend")),
        ("alpha_cutoff", json!(0.0)),
        ("alpha_cutoff", json!(1.1)),
        ("normal_texture", json!("no")),
    ] {
        let mut bad = doc.clone();
        bad["skeleton3d"]["material"][field] = value;
        reject(&bad);
    }
    let mut hidden = skeleton_with_png(&rgba_png([255, 255, 255, 0]));
    hidden["skeleton3d"]["material"] = json!({"alpha_mode": "cutout"});
    let model = common::load(&hidden);
    let ModelData::Skeleton3d(skeleton) = &model.data else {
        panic!()
    };
    assert!(!skeleton.visible);
    let mut below_cutoff = skeleton_with_png(&rgba_png([255, 255, 255, 128]));
    below_cutoff["skeleton3d"]["material"] = json!({"alpha_mode": "cutout", "alpha_cutoff": 0.75});
    let model = common::load(&below_cutoff);
    let ModelData::Skeleton3d(skeleton) = &model.data else {
        panic!()
    };
    assert!(!skeleton.visible);
    let mut color_only = common::skeleton_document();
    color_only["format_version"] = json!("0.2");
    color_only["skeleton3d"]["material"] = json!({"alpha_mode": "cutout"});
    for vertex in color_only["skeleton3d"]["vertices"].as_array_mut().unwrap() {
        vertex["color"][3] = json!(0.25);
    }
    let model = common::load(&color_only);
    let ModelData::Skeleton3d(skeleton) = &model.data else {
        panic!()
    };
    assert!(!skeleton.visible);
}
