use delicium::renderer::{scene_projection, screen_to_world};
use glam::Vec3;

#[test]
fn picking_coordinates_match_the_render_projection() {
    for [width, height] in [[1, 1], [960, 640], [320, 900], [3840, 2160]] {
        let projection = scene_projection(width, height);
        for [x, y] in [[0.0, 0.0], [0.25, 0.75], [0.5, 0.5], [1.0, 1.0]] {
            let pixel = [x * width as f32, y * height as f32];
            let world = screen_to_world(pixel, [width, height]);
            let ndc = projection.project_point3(Vec3::new(world[0], world[1], 0.0));
            let round_trip = [
                (ndc.x + 1.0) * 0.5 * width as f32,
                (1.0 - ndc.y) * 0.5 * height as f32,
            ];
            assert!((round_trip[0] - pixel[0]).abs() < 0.001);
            assert!((round_trip[1] - pixel[1]).abs() < 0.001);
            assert!((0.0..=1.0).contains(&ndc.z));
        }
    }
}

#[test]
fn zero_extent_does_not_generate_nonfinite_coordinates() {
    assert!(scene_projection(0, 0).is_finite());
    assert!(
        screen_to_world([0.0, 0.0], [0, 0])
            .iter()
            .all(|value| value.is_finite())
    );
}
