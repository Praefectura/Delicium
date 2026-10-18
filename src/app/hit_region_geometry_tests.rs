use super::super::{RegionChange, skeleton_outline};
use super::*;
use crate::app::{HitRegionCache, RegionMode, benchmark::RegionDecision};
use crate::model::{
    ClipPlayback, CompiledSkeleton3d, ModelData, try_sample_joints_clip, try_skin_matrices,
};
use crate::renderer::LEGACY;

fn raw(dx: f32, count: usize) -> HitGeometry {
    HitGeometry {
        size: [960, 640],
        view: LEGACY,
        instances: vec![InstanceOutline {
            asset: 1,
            instance: 1,
            polys: vec![
                vec![
                    [100.0 + dx, 100.0],
                    [140.0 + dx, 100.0],
                    [120.0 + dx, 200.0]
                ];
                count
            ],
        }],
        polygons: Vec::new(),
        instance_polygon_ends: Vec::new(),
    }
}

#[test]
fn movement_classification_has_dead_zone_and_hard_safe_lag() {
    let baseline = raw(0.0, 1);
    for (dx, expected) in [
        (0.0, RegionChange::Unchanged),
        (3.9, RegionChange::Unchanged),
        (4.0, RegionChange::Unchanged),
        (4.01, RegionChange::WithinMargin),
        (6.99, RegionChange::WithinMargin),
        (7.0, RegionChange::Immediate),
        (10.0, RegionChange::Immediate),
    ] {
        assert_eq!(raw(dx, 1).change_since(&baseline), expected, "dx={dx}");
    }
    let mut diagonal = raw(5.0, 1);
    for p in &mut diagonal.instances[0].polys[0] {
        p[1] += 5.0;
    }
    assert_eq!(diagonal.change_since(&baseline), RegionChange::Immediate);
    for field in 0..8 {
        let mut changed = raw(0.0, 1);
        match field {
            0 => changed.size[0] += 1,
            1 => changed.view.center_x += 0.0001,
            2 => changed.instances[0].asset += 1,
            3 => changed.instances[0].instance += 1,
            4 => changed.instances[0].polys.push(vec![[1.0, 1.0]; 3]),
            5 => changed.instances[0].polys[0].push([1.0, 1.0]),
            6 => changed.instances.clear(),
            _ => changed.view.bottom += 0.01,
        }
        assert_eq!(
            changed.change_since(&baseline),
            RegionChange::Immediate,
            "field={field}"
        );
    }
    // Intermediate candidates never become comparison baselines.
    assert_eq!(raw(3.9, 1).change_since(&baseline), RegionChange::Unchanged);
    assert_eq!(raw(7.8, 1).change_since(&baseline), RegionChange::Immediate);
}

#[test]
fn safe_deferral_retains_cadence_and_only_success_consumes_install() {
    for (count, k) in [(1, 1), (2048, 2), (8192, 4)] {
        let mut baseline = raw(0.0, count);
        baseline.materialize().unwrap();
        let mut cache = HitRegionCache {
            mode: RegionMode::Pixel,
            geometry: Some(baseline),
            click_through: false,
            failed: false,
            dirty_frames: 0,
            force_install: false,
        };
        for n in 1..=k {
            let current = raw(5.0, count);
            let decision = cache.decision(&current);
            assert!(current.polygons.is_empty(), "decision must not materialize");
            assert!(matches!(decision, RegionDecision::Rebuilt) == (n == k));
        }
        // Simulated failed installation: do not call note_installed or replace baseline.
        assert!(matches!(
            cache.decision(&raw(5.0, count)),
            RegionDecision::Rebuilt
        ));
        assert_eq!(
            cache.geometry.as_ref().unwrap().instances[0].polys[0][0][0],
            100.0
        );
        assert!(matches!(
            cache.decision(&raw(7.0, count)),
            RegionDecision::Rebuilt
        ));
        assert!(matches!(
            cache.decision(&raw(0.0, count)),
            RegionDecision::Reused
        ));
        assert_eq!(cache.dirty_frames, 0);
        cache.force_install = true;
        assert!(matches!(
            cache.decision(&raw(0.0, count)),
            RegionDecision::Rebuilt
        ));
        cache.note_installed();
        assert!(!cache.force_install);
        let mut resized = raw(0.0, count);
        resized.size[0] += 1;
        assert!(matches!(cache.decision(&resized), RegionDecision::Rebuilt));
        cache.geometry = None;
        assert!(matches!(
            cache.decision(&raw(0.0, count)),
            RegionDecision::Rebuilt
        ));
    }
}

#[test]
fn lazy_materialization_is_exact_and_idempotent_including_edges() {
    for shift in [-140.0, -100.0, -90.5, 0.0, 800.0, 850.0, 860.0] {
        let mut geometry = raw(shift, 2);
        let expected =
            super::frozen::expected_materialize(&geometry.instances[0].polys, geometry.size);
        assert!(geometry.polygons.is_empty());
        geometry.materialize().unwrap();
        assert_eq!(geometry.polygons, expected);
        assert_eq!(geometry.instance_polygon_ends, [expected.len()]);
        geometry.materialize().unwrap();
        assert_eq!(geometry.polygons, expected);
    }
    let mut geometry = raw(0.0, 0);
    geometry.instances.clear();
    geometry.size = [0, 0];
    geometry.materialize().unwrap();
    assert!(geometry.polygons.is_empty());
}

// Frozen scalar oracle: the pre-optimization per-corner implementation.
fn scalar_outline(
    asset: u64,
    mesh: &CompiledSkeleton3d,
    transform: &Mat4,
    skin: &[Mat4],
    size: [u32; 2],
) -> Result<Vec<Vec<[f32; 2]>>, String> {
    if !mesh.visible {
        return Ok(Vec::new());
    }
    if !mesh.indices.len().is_multiple_of(3) {
        return Err(format!(
            "asset {asset} has {} indices, not a triangle list",
            mesh.indices.len()
        ));
    }
    let mut polys = Vec::new();
    for tri in mesh.indices.as_chunks::<3>().0 {
        let mut corners = [[0.0; 3]; 3];
        for (corner, index) in tri.iter().enumerate() {
            let v = mesh.vertices.get(*index as usize).ok_or_else(|| {
                format!(
                    "asset {asset} references vertex {index} but only {} exist",
                    mesh.vertices.len()
                )
            })?;
            let mut p = Vec3::ZERO;
            for (j, w) in v.joints.iter().zip(v.weights) {
                let m=skin.get(*j as usize).ok_or_else(||format!("asset {asset} vertex {index} references joint {j} but only {} skin matrices are ready",skin.len()))?;
                p += m.transform_point3(Vec3::from_array(v.position)) * w;
            }
            let p = transform.transform_point3(p);
            if !p.is_finite() {
                return Err(format!(
                    "asset {asset} vertex {index} skins to a non-finite position"
                ));
            }
            corners[corner] = p.to_array();
        }
        push_triangle(
            &mut polys,
            corners,
            size,
            LEGACY,
            size[0].max(1) as f32,
            size[1].max(1) as f32,
        )
        .map_err(|e| format!("asset {asset}: {e}"))?;
    }
    Ok(polys)
}

#[test]
fn indexed_skinning_matches_scalar_oracle_and_preserves_error_order() {
    let model = load_model_bytes(
        include_bytes!("../../assets/examples/bangboo-eous.dmodel.json"),
        &LoadLimits::default(),
    )
    .unwrap();
    let ModelData::Skeleton3d(mut mesh) = Arc::try_unwrap(model).unwrap().data else {
        unreachable!()
    };
    let size = [960, 640];
    for clip in 0..mesh.animations.len() {
        for phase in [0.0, 0.25, 0.5, 0.75] {
            let pose = try_sample_joints_clip(
                &mesh,
                clip,
                f64::from(mesh.animations[clip].duration) * phase,
                ClipPlayback::Clamp,
            )
            .unwrap();
            let skin = try_skin_matrices(&mesh, &pose).unwrap();
            for facing in [-1.0, 1.0] {
                let transform = Mat4::from_translation(Vec3::new(0.2, 0.15, 0.0))
                    * Mat4::from_scale(Vec3::new(facing, 1.0, 1.0));
                assert_eq!(
                    skeleton_outline(1, &mesh, &transform, &skin, size, LEGACY),
                    scalar_outline(1, &mesh, &transform, &skin, size)
                );
            }
        }
    }
    let pose = try_sample_joints_clip(&mesh, 0, 0.0, ClipPlayback::Clamp).unwrap();
    let skin = try_skin_matrices(&mesh, &pose).unwrap();
    for transform in [
        Mat4::IDENTITY,
        Mat4::from_translation(Vec3::new(0.0, 0.0, 9.9)),
        Mat4::from_translation(Vec3::new(f32::NAN, 0.0, 0.0)),
    ] {
        assert_eq!(
            skeleton_outline(1, &mesh, &transform, &skin, size, LEGACY),
            scalar_outline(1, &mesh, &transform, &skin, size)
        );
    }
    let saved = mesh.indices.clone();
    mesh.indices.extend([u32::MAX, 0, 0]);
    assert_eq!(
        skeleton_outline(1, &mesh, &Mat4::IDENTITY, &skin, size, LEGACY),
        scalar_outline(1, &mesh, &Mat4::IDENTITY, &skin, size)
    );
    mesh.indices = saved;
    mesh.indices.push(0);
    assert_eq!(
        skeleton_outline(1, &mesh, &Mat4::IDENTITY, &skin, size, LEGACY),
        scalar_outline(1, &mesh, &Mat4::IDENTITY, &skin, size)
    );
    mesh.indices.pop();
    assert_eq!(
        skeleton_outline(1, &mesh, &Mat4::IDENTITY, &skin[..1], size, LEGACY),
        scalar_outline(1, &mesh, &Mat4::IDENTITY, &skin[..1], size)
    );
    let first = mesh.indices[0] as usize;
    let original = mesh.vertices[first];
    let mut unreferenced = original;
    unreferenced.position[0] = f32::NAN;
    mesh.vertices.push(unreferenced);
    let expected = scalar_outline(1, &mesh, &Mat4::IDENTITY, &skin, size);
    assert!(
        expected.is_ok(),
        "unreferenced vertices must not be evaluated"
    );
    assert_eq!(
        skeleton_outline(1, &mesh, &Mat4::IDENTITY, &skin, size, LEGACY),
        expected
    );
    mesh.vertices.pop();
    mesh.vertices[first].position[0] = f32::NAN;
    assert_eq!(
        skeleton_outline(1, &mesh, &Mat4::IDENTITY, &skin, size, LEGACY),
        scalar_outline(1, &mesh, &Mat4::IDENTITY, &skin, size)
    );
    mesh.vertices[first] = original;
    mesh.vertices[first].joints[3] = 65535;
    mesh.vertices[first].weights[3] = 0.0;
    let result = skeleton_outline(1, &mesh, &Mat4::IDENTITY, &skin, size, LEGACY);
    assert!(
        result.is_err(),
        "zero weight slots still validate their joint index"
    );
    assert_eq!(
        result,
        scalar_outline(1, &mesh, &Mat4::IDENTITY, &skin, size)
    );
}
