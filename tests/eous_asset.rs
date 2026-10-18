use delicium::model::{
    AlphaMode, ClipPlayback, JointTransform, LoadLimits, ModelData, load_model_bytes,
    try_sample_joints_clip, try_skin_matrices,
};
use glam::{Mat4, Quat, Vec3};
use std::ops::Range;

fn islands_in_range(indices: &[u32], range: Range<usize>) -> Vec<Vec<usize>> {
    let mut adjacency = vec![Vec::new(); range.len()];
    let (triangles, remainder) = indices.as_chunks::<3>();
    assert!(remainder.is_empty());
    for triangle in triangles {
        if !range.contains(&(triangle[0] as usize)) {
            continue;
        }
        assert!(triangle.iter().all(|i| range.contains(&(*i as usize))));
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            let a = triangle[a] as usize - range.start;
            let b = triangle[b] as usize - range.start;
            adjacency[a].push(b);
            adjacency[b].push(a);
        }
    }
    let mut seen = vec![false; range.len()];
    let mut result = Vec::new();
    for start in 0..range.len() {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        let mut component = Vec::new();
        while let Some(i) = stack.pop() {
            component.push(i + range.start);
            for &j in &adjacency[i] {
                if !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        result.push(component);
    }
    result
}

const EOUS: &[u8] = include_bytes!("../assets/examples/bangboo-eous.dmodel.json");

fn eous_bytes() -> std::borrow::Cow<'static, [u8]> {
    match std::env::var_os("DELICIUM_EOUS_TEST_MODEL") {
        Some(path) => {
            std::borrow::Cow::Owned(std::fs::read(path).expect("candidate model must exist"))
        }
        None => std::borrow::Cow::Borrowed(EOUS),
    }
}

#[test]
fn eous_loads_with_the_existing_limits_and_all_five_actions() {
    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    assert_eq!(model.name, "bangboo");
    assert!(bytes.len() <= 8 * 1024 * 1024);
    let ModelData::Skeleton3d(mesh) = &model.data else {
        panic!("Eous must remain a native skinned model");
    };
    assert_eq!(mesh.joints.len(), 22);
    assert_eq!(mesh.indices.len(), 18224 * 3);
    assert!(mesh.vertices.len() >= 9592 && mesh.vertices.len() <= 50000);
    assert_eq!(mesh.texture, Some(0));
    assert_eq!(mesh.material.unwrap().alpha_mode, AlphaMode::Opaque);
    assert_eq!(
        model.images.len(),
        1,
        "Do not embed the display stool atlas"
    );
    let atlas = &model.images[0];
    assert_eq!((atlas.width, atlas.height), (2048, 2048));
    assert_eq!(atlas.rgba.len(), 2048 * 2048 * 4);
    assert!(atlas.opaque);
    let bindings = model.actions.unwrap();
    for (index, name) in [
        (bindings.idle, "idle"),
        (bindings.walk, "walk"),
        (bindings.reaction, "reaction"),
        (bindings.greet.unwrap(), "greet"),
        (bindings.play.unwrap(), "play"),
    ] {
        assert_eq!(mesh.animations[index].name, name);
        assert_eq!(mesh.animations[index].channels.len(), 22);
    }
    assert_eq!(model.interaction.head_joint, mesh.joint_index("head"));
    let minimum = mesh
        .vertices
        .iter()
        .map(|v| v.position[1])
        .fold(f32::INFINITY, f32::min);
    let maximum = mesh
        .vertices
        .iter()
        .map(|v| v.position[1])
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(minimum.abs() < 1e-5 && (maximum - 1.65).abs() < 1e-5);
}

#[test]
fn eous_walk_steps_forward_instead_of_splaying_sideways() {
    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    let ModelData::Skeleton3d(mesh) = &model.data else {
        unreachable!()
    };
    let clip = model.actions.unwrap().walk;
    let root = mesh.joint_index("hips").unwrap();
    // These are actual sole vertices, not the terminal Foot joint origins:
    // the source has no vertices weighted to either terminal Foot joint.
    let soles: Vec<Vec<usize>> = [1.0, -1.0]
        .into_iter()
        .map(|sign| {
            mesh.vertices
                .iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    (v.position[1] < 0.075 && v.position[0] * sign > 0.04).then_some(i)
                })
                .collect()
        })
        .collect();
    assert!(soles.iter().all(|sole| sole.len() == 354));
    let mut centers = Vec::new();
    let mut floors = Vec::new();
    for step in 0..=96 {
        let time = f64::from(mesh.animations[clip].duration) * step as f64 / 96.0;
        let pose = try_sample_joints_clip(mesh, clip, time, ClipPlayback::Clamp).unwrap();
        let matrices = try_skin_matrices(mesh, &pose).unwrap();
        let unyaw = pose[root].rotation.inverse();
        let mut pair = [Vec3::ZERO; 2];
        let mut bottom = [f32::INFINITY; 2];
        for (side, sole) in soles.iter().enumerate() {
            for &index in sole {
                let v = &mesh.vertices[index];
                let point = v
                    .joints
                    .iter()
                    .zip(v.weights)
                    .fold(Vec3::ZERO, |sum, (&j, w)| {
                        sum + matrices[j as usize].transform_point3(Vec3::from_array(v.position))
                            * w
                    });
                bottom[side] = bottom[side].min(point.y);
                pair[side] += unyaw * (point - pose[root].translation);
            }
            pair[side] /= sole.len() as f32;
        }
        assert!(bottom.iter().all(|y| *y >= -0.0005));
        assert!(pair[0].x - pair[1].x > 0.18, "leg lanes collapse at {time}");
        centers.push(pair);
        floors.push(bottom);
    }
    for side in 0..2 {
        let range = |axis: usize| {
            let min = centers
                .iter()
                .map(|p| p[side][axis])
                .fold(f32::INFINITY, f32::min);
            let max = centers
                .iter()
                .map(|p| p[side][axis])
                .fold(f32::NEG_INFINITY, f32::max);
            max - min
        };
        println!(
            "EOUS_WALK_SOLE_TRAVEL side={side} forward={} lateral={}",
            range(2),
            range(0)
        );
        assert!(range(2) > 0.07, "the step needs forward/back travel");
        assert!(
            range(0) < 0.002,
            "the leg must not splay sideways: {}",
            range(0)
        );
    }
    for (step, bottom) in floors.iter().enumerate() {
        assert!(
            bottom[0].min(bottom[1]) < 0.003,
            "support floats at step {step}: {bottom:?}"
        );
    }
    // Walk starts at neutral hip swing; maximum separation is a quarter
    // cycle later, and swaps sides again after half a cycle.
    assert!(centers[72][0].z - centers[72][1].z > 0.07);
    assert!(centers[24][1].z - centers[24][0].z > 0.07);
    println!(
        "EOUS_WALK_SOLES support_max={} right_lift={} left_lift={}",
        floors
            .iter()
            .map(|p| p[0].min(p[1]))
            .fold(f32::NEG_INFINITY, f32::max),
        floors[0][1] - floors[0][0],
        floors[48][0] - floors[48][1]
    );
    assert!(
        floors[0][1] - floors[0][0] > 0.0015,
        "right swing foot must lift"
    );
    assert!(
        floors[48][0] - floors[48][1] > 0.0015,
        "left swing foot must lift"
    );
}

#[test]
fn eous_walk_crossfades_and_interruption_keep_the_actual_skin_above_ground() {
    use delicium::renderer::InstanceDraw;
    use delicium::simulation::{ActionKind, FIXED_DT, Simulation, WorldXBounds};
    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    let ModelData::Skeleton3d(mesh) = &model.data else {
        unreachable!()
    };
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(model.clone()).unwrap();
    let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
    simulation
        .set_wander_bounds(Some(WorldXBounds {
            left: -3.0,
            right: 3.0,
        }))
        .unwrap();
    let mut previous_action = ActionKind::Idle;
    let mut previous_front: Option<Vec3> = None;
    let mut seen = [false; 4];
    let mut interrupt_after = None;
    let mut interrupted = false;
    let mut settled = false;
    let mut minimum = (f32::INFINITY, 0, ActionKind::Idle);
    let mut returned_after = None;
    let mut return_minimum = (f32::INFINITY, 0, ActionKind::Idle);
    for tick in 0..1800 {
        let state = simulation.instance(id).unwrap();
        let action = state.action.unwrap();
        if action != previous_action {
            println!(
                "EOUS_WALK_TRANSITION tick={tick} from={previous_action:?} to={action:?} seen={seen:?}"
            );
        }
        if previous_action == ActionKind::Idle && action == ActionKind::Walk {
            seen[0] = true;
            if seen[2] {
                seen[3] = true;
            }
            if seen[1] && !interrupted {
                interrupt_after = Some(tick + 10);
            }
        }
        if previous_action == ActionKind::Walk && action == ActionKind::Idle {
            seen[1] = true;
        }
        if action == ActionKind::Reaction {
            seen[2] = true;
        }
        if previous_action == ActionKind::Reaction && action != ActionKind::Reaction {
            returned_after = Some(tick + 20);
        }
        if interrupt_after == Some(tick) {
            simulation
                .poke_reaction(id, state.position + Vec3::X)
                .unwrap();
            interrupted = true;
        }
        let frame = simulation.render_frame().unwrap();
        let InstanceDraw::Skeleton3d {
            skin_matrices,
            transform,
            ..
        } = &frame.instances[0]
        else {
            unreachable!()
        };
        let front = skin_matrices[0].transform_vector3(Vec3::Z).normalize();
        if let Some(previous) = previous_front {
            assert!(
                previous.angle_between(front) < 0.2,
                "root yaw snapped at tick {tick}"
            );
        }
        previous_front = Some(front);
        for vertex in &mesh.vertices {
            let p = vertex
                .joints
                .iter()
                .zip(vertex.weights)
                .fold(Vec3::ZERO, |sum, (&j, w)| {
                    sum + skin_matrices[j as usize]
                        .transform_point3(Vec3::from_array(vertex.position))
                        * w
                });
            // Ground is relative to the instance anchor. The renderer also
            // applies PokeReaction's presentation hop; local skin alone is not
            // the actual foot height during an interrupted action.
            let presented = transform.transform_point3(p) - state.position;
            assert!(presented.is_finite(), "nonfinite skin at tick {tick}");
            // Cover the complete chain, including Reaction -> Idle -> Walk.
            // A new action can interrupt a crossfade after just one tick;
            // action names alone do not identify a settled source pose.
            if presented.y < minimum.0 {
                minimum = (presented.y, tick, action);
            }
            if returned_after.is_some() && presented.y < return_minimum.0 {
                return_minimum = (presented.y, tick, action);
            }
        }
        previous_action = action;
        if seen.iter().all(|value| *value) && returned_after.is_some_and(|end| tick >= end) {
            settled = true;
            break;
        }
        simulation.advance(FIXED_DT).unwrap();
    }
    assert!(
        seen.iter().all(|value| *value) && settled,
        "missing real transition or settling: {seen:?}"
    );
    println!(
        "EOUS_WALK_CLEARANCE minimum={} tick={} action={:?}",
        minimum.0, minimum.1, minimum.2
    );
    assert!(return_minimum.0.is_finite());
    println!(
        "EOUS_POST_REACTION_CLEARANCE minimum={} tick={} action={:?}",
        return_minimum.0, return_minimum.1, return_minimum.2
    );
    assert!(minimum.0 >= -0.0005, "crossfade floor: {minimum:?}");
}

#[test]
fn eous_walking_left_faces_left_without_mirroring_the_scarf() {
    use delicium::renderer::InstanceDraw;
    use delicium::simulation::{FIXED_DT, Simulation, WorldXBounds};
    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(model).unwrap();
    let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
    // Bounds entirely left of the spawn: the recovery walk is deterministically
    // leftward, so the reported backwards walk is reproduced without waiting
    // for a random target.
    simulation
        .set_wander_bounds(Some(WorldXBounds {
            left: -6.0,
            right: 0.0,
        }))
        .unwrap();
    let mut walked_left = false;
    let mut walked_right = false;
    // Length of the current continuous leftward run. A free walk may
    // legitimately turn around after arriving, so the check is per run.
    let mut left_run = 0u32;
    let mut worst_lag = 0u32;
    for tick in 0..600 {
        let before = simulation.instance(id).unwrap().position.x;
        simulation.advance(FIXED_DT).unwrap();
        let after = simulation.instance(id).unwrap().position.x;
        if after < before {
            walked_left = true;
            left_run += 1;
        } else {
            walked_right |= after > before;
            left_run = 0;
        }
        if left_run == 0 {
            continue;
        }
        let frame = simulation.render_frame().unwrap();
        let InstanceDraw::Skeleton3d {
            transform,
            skin_matrices,
            ..
        } = &frame.instances[0]
        else {
            unreachable!()
        };
        // The scarf print must never be mirrored: a mirror reverses UVs and
        // would make the printed "01" read backwards.
        assert_eq!(
            transform.x_axis.x, 1.0,
            "tick {tick}: walking left must not mirror the Eous root"
        );
        let forward = transform.transform_vector3(skin_matrices[0].transform_vector3(Vec3::Z));
        // The walk clip itself animates the hips, so the forward vector is not
        // exactly horizontal; the turn we add must not tilt it further. A
        // reflected root would instead swing it past vertical.
        assert!(
            forward.y.abs() < 0.02,
            "tick {tick}: the body turn must stay horizontal: {forward:?}"
        );
        // The turn is eased over about a dozen ticks; once a leftward run has
        // lasted longer than that, continuing to face right is the reported
        // backwards walk.
        if left_run > 12 && forward.x >= -0.1 {
            worst_lag = worst_lag.max(left_run);
        }
    }
    println!(
        "EOUS_LEFT_RUN walked_left={walked_left} walked_right={walked_right} worst_lag={worst_lag}"
    );
    assert!(walked_left, "the recovery walk never moved left");
    assert!(
        worst_lag == 0,
        "a leftward run of {worst_lag} ticks still faced the body right"
    );
}

#[test]
fn eous_canonical_joint_frames_preserve_bind_skin_and_yaw_axis() {
    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    let ModelData::Skeleton3d(mesh) = &model.data else {
        unreachable!()
    };
    let rest: Vec<_> = mesh
        .joints
        .iter()
        .map(|joint| {
            assert!(joint.rotation.abs_diff_eq(Quat::IDENTITY, 1e-6));
            assert!(joint.scale.abs_diff_eq(Vec3::ONE, 1e-6));
            JointTransform {
                translation: joint.translation,
                rotation: joint.rotation,
                scale: joint.scale,
            }
        })
        .collect();
    let matrices = try_skin_matrices(mesh, &rest).unwrap();
    assert!(matrices.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)));
    let mut driven = rest;
    let head = model.interaction.head_joint.unwrap();
    driven[head].rotation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_3);
    let matrices = try_skin_matrices(mesh, &driven).unwrap();
    let turned_front = matrices[head].transform_vector3(Vec3::Z);
    assert!(turned_front.x > 0.85 && turned_front.y.abs() < 1e-5);
    assert!((turned_front.z - 0.5).abs() < 1e-5);
}

#[test]
fn eous_head_panel_and_eyes_share_one_transform_through_actions_and_gaze() {
    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    let ModelData::Skeleton3d(mesh) = &model.data else {
        unreachable!()
    };
    assert_eq!(mesh.vertices.len(), 11331);
    // Stable converter mesh order: arms 624, stitching 350, body 4172,
    // ear screws 642, eyes 480. Guard islands too, not only numeric ranges.
    let body = islands_in_range(&mesh.indices, 974..5146);
    let mut sizes: Vec<_> = body.iter().map(Vec::len).collect();
    sizes.sort_unstable();
    assert_eq!(
        sizes,
        [
            44, 44, 63, 63, 101, 101, 221, 331, 331, 375, 375, 1026, 1097
        ]
    );
    let eyes = islands_in_range(&mesh.indices, 5788..6268);
    assert_eq!(eyes.iter().map(Vec::len).collect::<Vec<_>>(), [240, 240]);
    let panel = body.iter().find(|c| c.len() == 221).unwrap();
    let face: Vec<_> = panel.iter().copied().chain(5788..6268).collect();
    let head = model.interaction.head_joint.unwrap();
    for &index in &face {
        let vertex = &mesh.vertices[index];
        let head_weight: f32 = vertex
            .joints
            .iter()
            .zip(vertex.weights)
            .filter(|(j, _)| **j as usize == head)
            .map(|(_, w)| w)
            .sum();
        assert!((head_weight - 1.0).abs() < 1e-6, "face vertex {index}");
    }
    for ear in body.iter().filter(|c| matches!(c.len(), 44 | 63 | 101)) {
        assert!(
            ear.iter().any(|&i| {
                mesh.vertices[i]
                    .joints
                    .iter()
                    .zip(mesh.vertices[i].weights)
                    .any(|(&joint, weight)| {
                        mesh.joints[joint as usize].name.contains("Ear") && weight > 0.25
                    })
            }),
            "Do not rigidify the ear islands with the hood"
        );
    }
    // Hood above the covered neck must follow the same head drive. Ear-base
    // seam vertices may retain ear weights: their own ear motion is intentional.
    let hood: Vec<_> = body
        .iter()
        .filter(|c| c.len() == 1026 || c.len() == 1097)
        .flatten()
        .copied()
        .filter(|&i| mesh.vertices[i].position[1] > 0.95)
        .collect();
    assert!(hood.len() > 400);
    let checked: Vec<_> = face.iter().copied().chain(hood).collect();
    let skin = |index: usize, matrices: &[Mat4]| {
        let vertex = &mesh.vertices[index];
        vertex
            .joints
            .iter()
            .zip(vertex.weights)
            .fold(Vec3::ZERO, |sum, (&joint, weight)| {
                sum + matrices[joint as usize].transform_point3(Vec3::from_array(vertex.position))
                    * weight
            })
    };
    for (clip_index, clip) in mesh.animations.iter().enumerate() {
        for fraction in [0.0, 0.137, 0.5, 0.863, 1.0] {
            let pose = try_sample_joints_clip(
                mesh,
                clip_index,
                f64::from(clip.duration) * fraction,
                ClipPlayback::Clamp,
            )
            .unwrap();
            let base = try_skin_matrices(mesh, &pose).unwrap();
            for degrees in [-60.0_f32, -30.0, 0.0, 30.0, 60.0] {
                let mut driven = pose.clone();
                driven[head].rotation = (Quat::from_rotation_y(degrees.to_radians())
                    * driven[head].rotation)
                    .normalize();
                let matrices = try_skin_matrices(mesh, &driven).unwrap();
                let common = matrices[head] * base[head].inverse();
                for &index in &checked {
                    let expected = common.transform_point3(skin(index, &base));
                    let actual = skin(index, &matrices);
                    assert!(
                        actual.distance(expected) < 0.0002,
                        "{} fraction={fraction} yaw={degrees} vertex={index}: unequal head motion",
                        clip.name
                    );
                }
                // Distances in 3D, including eye-to-panel landmarks, must not
                // shear. A correctly turned eye can still project as an ellipse.
                for &a in face.iter().step_by(13) {
                    for &b in face.iter().step_by(17) {
                        let before = skin(a, &base).distance(skin(b, &base));
                        let after = skin(a, &matrices).distance(skin(b, &matrices));
                        assert!(
                            (after - before).abs() < 0.0002,
                            "{} fraction={fraction} yaw={degrees}: face distance changed",
                            clip.name
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn eous_attention_keeps_actual_skin_grounded_and_the_face_on_its_head() {
    use delicium::renderer::InstanceDraw;
    use delicium::simulation::{FIXED_DT, Simulation, WorldRect};

    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    let ModelData::Skeleton3d(mesh) = &model.data else {
        unreachable!()
    };
    let head = model.interaction.head_joint.unwrap();
    let body = islands_in_range(&mesh.indices, 974..5146);
    let panel = body.iter().find(|island| island.len() == 221).unwrap();
    let face: Vec<_> = panel.iter().copied().chain(5788..6268).collect();
    assert_eq!(face.len(), 701);
    let hood: Vec<_> = body
        .iter()
        .filter(|island| matches!(island.len(), 1026 | 1097))
        .flatten()
        .copied()
        .filter(|&index| mesh.vertices[index].position[1] > 0.95)
        .collect();
    assert!(hood.len() > 400);
    let checked: Vec<_> = face.into_iter().chain(hood).collect();
    let mut simulation = Simulation::new();
    simulation.set_attention_enabled(true);
    let asset = simulation.insert_asset(model.clone()).unwrap();
    let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
    let mut reference = Simulation::new();
    let reference_asset = reference.insert_asset(model.clone()).unwrap();
    assert_eq!(reference.spawn(reference_asset, Vec3::ZERO).unwrap(), id);
    let mut minimum = f32::INFINITY;
    let mut nonneutral = false;
    for tick in 0..600 {
        for simulation in [&mut simulation, &mut reference] {
            match tick {
                1 => simulation
                    .set_pointer_world(Some(Vec3::new(0.6, 1.1, 0.0)))
                    .unwrap(),
                90 => simulation
                    .set_pointer_world(Some(Vec3::new(-0.6, 1.1, 0.0)))
                    .unwrap(),
                180 => {
                    simulation.set_pointer_world(None).unwrap();
                    simulation.suspend_wander(id).unwrap();
                    simulation.begin_drag_feedback(id).unwrap();
                }
                220 => {
                    simulation.end_drag_feedback(id, true).unwrap();
                    simulation.resume_wander(id).unwrap();
                }
                250 | 260 | 270 => simulation.poke_reaction(id, Vec3::X).unwrap(),
                350 => simulation
                    .set_attention_edges(
                        &[WorldRect {
                            left: -3.0,
                            right: 3.0,
                            bottom: 0.0,
                            top: 5.0,
                        }],
                        &[],
                        &[],
                    )
                    .unwrap(),
                440 => simulation.set_attention_edges(&[], &[], &[]).unwrap(),
                _ => {}
            }
            if (181..220).contains(&tick) {
                simulation
                    .set_position(id, Vec3::new((tick as f32 - 180.0) * 0.02 - 0.4, 0.0, 0.0))
                    .unwrap();
            }
            simulation.advance(FIXED_DT).unwrap();
        }
        let state = simulation.attention_snapshot(id).unwrap();
        nonneutral |= state.shear_x.abs() > 0.01 || (state.scale_y - 1.0).abs() > 0.01;
        if tick % 3 != 0 {
            continue;
        }
        let frame = simulation.render_frame().unwrap();
        let InstanceDraw::Skeleton3d {
            transform,
            skin_matrices,
            ..
        } = &frame.instances[0]
        else {
            unreachable!()
        };
        let position = simulation.instance(id).unwrap().position;
        let base = reference.render_frame().unwrap();
        let InstanceDraw::Skeleton3d {
            transform: base_transform,
            skin_matrices: base_matrices,
            ..
        } = &base.instances[0]
        else {
            unreachable!()
        };
        for (vertex_index, vertex) in mesh.vertices.iter().enumerate() {
            let local = vertex.joints.iter().zip(vertex.weights).fold(
                Vec3::ZERO,
                |sum, (&joint, weight)| {
                    sum + skin_matrices[joint as usize]
                        .transform_point3(Vec3::from_array(vertex.position))
                        * weight
                },
            );
            let world = transform.transform_point3(local);
            assert!(world.is_finite(), "nonfinite attention skin at tick {tick}");
            minimum = minimum.min(world.y - position.y);
            assert!(
                world.y - position.y >= -0.0005,
                "attention sole below ground: tick={tick} vertex={vertex_index} point={world:?} reference={:?} action={:?} attention={state:?}",
                base_transform.transform_point3(vertex.joints.iter().zip(vertex.weights).fold(
                    Vec3::ZERO,
                    |sum, (&joint, weight)| {
                        sum + base_matrices[joint as usize]
                            .transform_point3(Vec3::from_array(vertex.position))
                            * weight
                    },
                )),
                simulation.instance(id).unwrap().action,
            );
        }
        let common = skin_matrices[head] * base_matrices[head].inverse();
        for &index in &checked {
            let vertex = &mesh.vertices[index];
            let skin = |matrices: &[Mat4]| {
                vertex.joints.iter().zip(vertex.weights).fold(
                    Vec3::ZERO,
                    |sum, (&joint, weight)| {
                        sum + matrices[joint as usize]
                            .transform_point3(Vec3::from_array(vertex.position))
                            * weight
                    },
                )
            };
            let actual = skin(skin_matrices);
            let expected = common.transform_point3(skin(base_matrices));
            assert!(
                actual.distance(expected) < 0.0002,
                "face/hood detached at tick={tick} vertex={index}"
            );
        }
    }
    assert!(
        nonneutral,
        "test must exercise actual new root presentation"
    );
    println!(
        "EOUS_ATTENTION_SKIN pass=true frames=200 checked_face_hood_vertices={} floor={minimum}",
        checked.len()
    );
}

#[test]
fn eous_reaction_returns_to_sustained_idle_without_crossing_the_floor() {
    use delicium::renderer::InstanceDraw;
    use delicium::simulation::{ActionKind, FIXED_DT, Simulation};

    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    let ModelData::Skeleton3d(mesh) = &model.data else {
        unreachable!()
    };
    let first_half = FIXED_DT / 2;
    let second_half = FIXED_DT - first_half;
    let poke_sequences: [&[usize]; 5] = [&[0], &[0, 10, 20], &[0, 57], &[0, 63], &[0, 68]];
    let mut minimum = f32::INFINITY;
    let mut frames = 0;
    for attention in [false, true] {
        for initial_idle_ticks in [0, 17, 41] {
            for pokes in poke_sequences {
                let mut simulation = Simulation::new();
                simulation.set_attention_enabled(attention);
                let asset = simulation.insert_asset(model.clone()).unwrap();
                let id = simulation.spawn(asset, Vec3::ZERO).unwrap();
                for _ in 0..initial_idle_ticks {
                    simulation.advance(FIXED_DT).unwrap();
                }
                let mut saw_reaction = false;
                let mut settled_idle_ticks = 0;
                for tick in 0..160 {
                    if pokes.contains(&tick) {
                        if tick == 63 || tick == 68 {
                            assert_eq!(
                                simulation.instance(id).unwrap().action,
                                Some(ActionKind::Idle)
                            );
                        }
                        simulation.poke_reaction(id, Vec3::X).unwrap();
                    }
                    // Split the exact fixed duration rather than rounding both
                    // halves: check committed and interpolated render poses.
                    for elapsed in [first_half, second_half] {
                        simulation.advance(elapsed).unwrap();
                        let state = simulation.instance(id).unwrap();
                        saw_reaction |= state.action == Some(ActionKind::Reaction);
                        let frame = simulation.render_frame().unwrap();
                        let InstanceDraw::Skeleton3d {
                            transform,
                            skin_matrices,
                            ..
                        } = &frame.instances[0]
                        else {
                            unreachable!()
                        };
                        for (index, vertex) in mesh.vertices.iter().enumerate() {
                            let local = vertex.joints.iter().zip(vertex.weights).fold(
                                Vec3::ZERO,
                                |sum, (&joint, weight)| {
                                    sum + skin_matrices[joint as usize]
                                        .transform_point3(Vec3::from_array(vertex.position))
                                        * weight
                                },
                            );
                            let presented = transform.transform_point3(local) - state.position;
                            minimum = minimum.min(presented.y);
                            assert!(presented.is_finite());
                            assert!(
                                presented.y >= -0.0005,
                                "reaction return floor: attention={attention} initial_idle_ticks={initial_idle_ticks} pokes={pokes:?} tick={tick} action={:?} vertex={index} point={presented:?}",
                                state.action,
                            );
                        }
                        frames += 1;
                    }
                    if tick >= pokes.last().unwrap() + 62 {
                        assert_eq!(
                            simulation.instance(id).unwrap().action,
                            Some(ActionKind::Idle),
                            "the return must stay Idle, not immediately switch to Walk"
                        );
                        settled_idle_ticks += 1;
                    }
                }
                assert!(saw_reaction && settled_idle_ticks >= 20);
            }
        }
    }
    println!("EOUS_REACTION_RETURN frames={frames} minimum={minimum}");
}

#[test]
fn eous_looping_clips_never_snap_back_through_a_rest_pose() {
    // Equal start and end poses are not enough. A loop can still contain a
    // rest-flavoured pose that the cycle jumps away from, which reads as a
    // pose reset once per cycle. The root motion is a steady sway, so a real
    // discontinuity stands out as a single frame whose root rotation step is
    // many times the cycle's median step. Ratios are scale-free, so the check
    // does not depend on how large the sway is.
    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    let ModelData::Skeleton3d(mesh) = &model.data else {
        unreachable!()
    };
    let bindings = model.actions.unwrap();
    let root = 0;
    for clip_index in [bindings.idle, bindings.walk] {
        let clip = &mesh.animations[clip_index];
        let duration = f64::from(clip.duration);
        let steps = (duration * 60.0).round() as usize;
        // One whole cycle under loop semantics, sampled at the engine step.
        let rotations: Vec<Quat> = (0..steps)
            .map(|index| {
                let time = duration * index as f64 / steps as f64;
                let pose =
                    try_sample_joints_clip(mesh, clip_index, time, ClipPlayback::Loop).unwrap();
                pose[root].rotation
            })
            .collect();
        let steps_between: Vec<f32> = (0..steps)
            .map(|index| {
                let a = rotations[index];
                let b = rotations[(index + 1) % steps];
                // Rotation step in radians, independent of quaternion sign.
                2.0 * (a.dot(b).abs().min(1.0)).acos()
            })
            .collect();
        let mut sorted = steps_between.clone();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[sorted.len() / 2];
        if median <= 1e-6 {
            // No root sway at all (the walk drives the legs instead).
            continue;
        }
        let peak = sorted[sorted.len() - 1];
        assert!(
            peak <= median * 4.0,
            "{} jumps {peak} rad in one frame against a median root step of \
             {median} rad; the loop snaps back through a rest pose",
            clip.name
        );
    }
}

#[test]
fn eous_interpolated_actions_remain_finite_above_the_floor_and_loop_cleanly() {
    let bytes = eous_bytes();
    let model = load_model_bytes(&bytes, &LoadLimits::default()).unwrap();
    let ModelData::Skeleton3d(mesh) = &model.data else {
        unreachable!()
    };
    for (clip_index, clip) in mesh.animations.iter().enumerate() {
        let start = try_sample_joints_clip(mesh, clip_index, 0.0, ClipPlayback::Clamp).unwrap();
        let end = try_sample_joints_clip(
            mesh,
            clip_index,
            f64::from(clip.duration),
            ClipPlayback::Clamp,
        )
        .unwrap();
        for (a, b) in start.iter().zip(&end) {
            assert!(
                a.translation.abs_diff_eq(b.translation, 1e-5),
                "{} translation seam",
                clip.name
            );
            assert!(
                a.rotation.abs_diff_eq(b.rotation, 1e-5)
                    || a.rotation.abs_diff_eq(-b.rotation, 1e-5),
                "{} rotation seam",
                clip.name
            );
            assert!(
                a.scale.abs_diff_eq(b.scale, 1e-5),
                "{} scale seam",
                clip.name
            );
        }
        // Half-key intervals exercise the consumer's interpolation, not only
        // the converter's 60 Hz source samples.
        let steps = (f64::from(clip.duration) * 120.0).ceil() as usize;
        for step in 0..=steps {
            let time = f64::from(clip.duration) * step as f64 / steps as f64;
            let pose = try_sample_joints_clip(mesh, clip_index, time, ClipPlayback::Clamp).unwrap();
            let matrices = try_skin_matrices(mesh, &pose).unwrap();
            for vertex in &mesh.vertices {
                let original = Vec3::from_array(vertex.position);
                let skinned = vertex.joints.iter().zip(vertex.weights).fold(
                    Vec3::ZERO,
                    |sum, (&joint, weight)| {
                        sum + matrices[joint as usize].transform_point3(original) * weight
                    },
                );
                assert!(skinned.is_finite(), "{} at {time}", clip.name);
                assert!(
                    skinned.y >= -0.0005,
                    "{} at {time}: below floor {skinned:?}",
                    clip.name
                );
                assert!(
                    skinned.abs().max_element() < 2.2,
                    "Unbounded Eous deformation"
                );
            }
        }
    }
}
