use delicium::renderer::{
    LEGACY, desktop_view, scene_projection, scene_projection_with, screen_to_world,
    screen_to_world_with, world_to_screen, world_to_screen_with,
};
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
fn world_and_screen_coordinates_are_inverse() {
    for size in [
        [0, 0],
        [0, 640],
        [960, 0],
        [1, 1],
        [960, 640],
        [1440, 960],
        [320, 900],
        [3840, 2160],
    ] {
        for world in [
            [-2.4, 0.15],
            [0.0, 0.0],
            [-3.0, 3.6],
            [3.0, -0.9],
            [12.0, -8.0],
        ] {
            let pixel = world_to_screen(world, size);
            assert!(pixel.iter().all(|value| value.is_finite()));
            let round_trip = screen_to_world(pixel, size);
            for axis in 0..2 {
                assert!(
                    (round_trip[axis] - world[axis]).abs() < 0.001,
                    "{world:?} at {size:?}"
                );
            }
        }
    }
    let anchor = world_to_screen([-2.4, 0.15], [960, 640]);
    assert!((anchor[0] - 138.66667).abs() < 0.001);
    assert!((anchor[1] - 490.66667).abs() < 0.001);
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

#[test]
fn desktop_view_keeps_the_native_pixel_scale() {
    // Host monitor: 2560x1600 at 1.5 scale. The scene spans the physical
    // rectangle, so the view height becomes 1600 / (640 * 1.5) stacked 4.5s.
    let view = desktop_view(1600, 1.5);
    assert!((view.span - 7.5).abs() < 1e-6, "span {}", view.span);
    assert_eq!(view.bottom, LEGACY.bottom);
    // The vertical pixel-per-world-unit ratio equals the bounded scene's
    // ratio at its 960-physical-pixel height (960 / 4.5).
    let desktop_ratio = 1600.0 / view.span;
    let bounded_ratio = 960.0 / LEGACY.span;
    assert!((desktop_ratio - 213.33333).abs() < 0.001);
    assert!((desktop_ratio - bounded_ratio).abs() < 1e-4);
    // A degenerate height still produces a usable span.
    assert!(desktop_view(0, 1.0).span > 0.0);
}

#[test]
fn explicit_view_functions_match_legacy_values_exactly() {
    // With LEGACY the parameterized functions use bit-identical arithmetic to
    // the frozen originals, so parity can be exact rather than approximate.
    for [width, height] in [[960, 640], [1440, 960], [256, 256], [3840, 2160]] {
        assert_eq!(
            scene_projection(width, height),
            scene_projection_with(width, height, LEGACY),
            "projection at {width}x{height}"
        );
        for pixel in [[0.0, 0.0], [123.0, 456.0], [width as f32, height as f32]] {
            assert_eq!(
                screen_to_world(pixel, [width, height]),
                screen_to_world_with(pixel, [width, height], LEGACY),
                "screen_to_world {pixel:?} at {width}x{height}"
            );
        }
        for world in [[-2.4, 0.15], [0.0, 0.0], [3.0, 3.6]] {
            assert_eq!(
                world_to_screen(world, [width, height]),
                world_to_screen_with(world, [width, height], LEGACY),
                "world_to_screen {world:?} at {width}x{height}"
            );
        }
    }
}

#[test]
fn desktop_view_coordinates_round_trip() {
    let view = desktop_view(1600, 1.5);
    let size = [2560, 1600];
    for world in [[-4.0, 0.2], [0.0, 0.0], [2.4, 3.0], [6.9, 6.6]] {
        let pixel = world_to_screen_with(world, size, view);
        let round_trip = screen_to_world_with(pixel, size, view);
        for axis in 0..2 {
            assert!(
                (round_trip[axis] - world[axis]).abs() < 0.001,
                "{world:?} at desktop view"
            );
        }
    }
    // A world point at the view top maps to pixel row 0 and the bottom to the
    // window height under any view.
    let top = world_to_screen_with([0.0, LEGACY.bottom + view.span], size, view);
    assert!(top[1].abs() < 0.001);
    let bottom = world_to_screen_with([0.0, LEGACY.bottom], size, view);
    assert!((bottom[1] - 1600.0).abs() < 0.001);
}
