use delicium::renderer::{
    LEGACY, SceneView, desktop_view, desktop_view_at, scene_projection, scene_projection_with,
    screen_to_world, screen_to_world_with, world_to_screen, world_to_screen_with,
};
use glam::{Mat4, Vec3};

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

/// The pre-`center_x` arithmetic of [`screen_to_world`], transcribed verbatim
/// so the regression below compares operations rather than re-stated facts.
fn frozen_legacy_screen_to_world(cursor: [f32; 2], size: [u32; 2]) -> [f32; 2] {
    let width = size[0].max(1) as f32;
    let height = size[1].max(1) as f32;
    let half_width: f32 = 4.5 * 0.5 * width / height;
    let top: f32 = -0.9 + 4.5;
    [
        (cursor[0] / width * 2.0 - 1.0) * half_width,
        top - cursor[1] / height * 4.5,
    ]
}

/// The pre-`center_x` arithmetic of [`world_to_screen`], transcribed verbatim.
fn frozen_legacy_world_to_screen(point: [f32; 2], size: [u32; 2]) -> [f32; 2] {
    let width = size[0].max(1) as f32;
    let height = size[1].max(1) as f32;
    let half_width: f32 = 4.5 * 0.5 * width / height;
    let top: f32 = -0.9 + 4.5;
    [
        (point[0] / half_width + 1.0) * 0.5 * width,
        (top - point[1]) / 4.5 * height,
    ]
}

/// The pre-`center_x` [`scene_projection`] matrix, transcribed verbatim.
fn frozen_legacy_scene_projection(width: u32, height: u32) -> Mat4 {
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    let half_height: f32 = 4.5 * 0.5;
    let half_width = half_height * aspect;
    let top: f32 = -0.9 + 4.5;
    glam::camera::rh::proj::directx::orthographic(-half_width, half_width, -0.9, top, 0.1, 100.0)
        * glam::camera::rh::view::look_at_mat4(
            glam::Vec3::new(0.0, 0.0, 10.0),
            glam::Vec3::ZERO,
            glam::Vec3::Y,
        )
}

fn assert_bits_eq(actual: [f32; 2], expected: [f32; 2], context: &str) {
    for axis in 0..2 {
        assert_eq!(
            actual[axis].to_bits(),
            expected[axis].to_bits(),
            "{context}: axis {axis} {actual:?} vs {expected:?}"
        );
    }
}

fn assert_round_trip(world: [f32; 2], size: [u32; 2], view: SceneView) {
    let pixel = world_to_screen_with(world, size, view);
    assert!(pixel.iter().all(|value| value.is_finite()), "{world:?}");
    let round_trip = screen_to_world_with(pixel, size, view);
    for axis in 0..2 {
        assert!(
            (round_trip[axis] - world[axis]).abs() < 0.001,
            "{world:?} at {size:?}"
        );
    }
}

#[test]
fn legacy_coordinates_stay_bit_exact_with_the_center_x_extension() {
    // Extending `SceneView` with `center_x` must not move a single bit of the
    // legacy mapping, so the production paths are compared against the frozen
    // pre-extension arithmetic transcribed above. The new `+ 0.0`/`- 0.0`
    // steps are exact for every finite operand these formulas can produce.
    assert_eq!(LEGACY.span, 4.5);
    assert_eq!(LEGACY.bottom, -0.9);
    assert_eq!(LEGACY.center_x, 0.0);
    assert_eq!(SceneView::default(), LEGACY);

    for [width, height] in [
        [0, 0],
        [1, 1],
        [960, 640],
        [1440, 960],
        [320, 900],
        [3840, 2160],
    ] {
        let frozen_projection = frozen_legacy_scene_projection(width, height);
        assert_eq!(
            scene_projection(width, height)
                .to_cols_array()
                .map(f32::to_bits),
            frozen_projection.to_cols_array().map(f32::to_bits),
            "projection at {width}x{height}"
        );
        assert_eq!(
            scene_projection_with(width, height, LEGACY)
                .to_cols_array()
                .map(f32::to_bits),
            frozen_projection.to_cols_array().map(f32::to_bits),
            "explicit projection at {width}x{height}"
        );
        for pixel in [
            [0.0, 0.0],
            [123.5, 456.25],
            [480.5, 320.25],
            [width as f32, height as f32],
        ] {
            assert_bits_eq(
                screen_to_world(pixel, [width, height]),
                frozen_legacy_screen_to_world(pixel, [width, height]),
                "screen_to_world",
            );
            assert_bits_eq(
                screen_to_world_with(pixel, [width, height], LEGACY),
                frozen_legacy_screen_to_world(pixel, [width, height]),
                "screen_to_world_with LEGACY",
            );
        }
        for point in [[-2.4, 0.15], [0.0, 0.0], [3.0, 3.6], [12.0, -8.0]] {
            assert_bits_eq(
                world_to_screen(point, [width, height]),
                frozen_legacy_world_to_screen(point, [width, height]),
                "world_to_screen",
            );
            assert_bits_eq(
                world_to_screen_with(point, [width, height], LEGACY),
                frozen_legacy_world_to_screen(point, [width, height]),
                "world_to_screen_with LEGACY",
            );
        }
    }
}

#[test]
fn offset_view_anchors_world_coordinates_to_the_primary_monitor() {
    // Host layout: primary 2560x1600 at (0, 0), secondary 1600x2560 at
    // (2560, -511). Union 4160x2560, its center 800 physical px right of the
    // primary center and its bottom 449 px below the primary bottom.
    let view = desktop_view_at(2560, 1.5, 800.0, 449.0);
    assert_eq!(view.span, 12.0);
    assert!(
        (view.center_x - 3.75).abs() < 0.00001,
        "center_x {}",
        view.center_x
    );
    assert!(
        (view.bottom + 3.0046875).abs() < 0.00001,
        "bottom {}",
        view.bottom
    );

    let size = [4160, 2560];
    // Primary anchor: world x = 0 lands on the primary center column 1280,
    // and the ground line y = 0.15 lands 1376 px below the primary top edge,
    // which is 511 px below the union top (the union's -511 origin offset).
    let ground = world_to_screen_with([0.0, 0.15], size, view);
    assert!((ground[0] - 1280.0).abs() < 0.001, "ground x {}", ground[0]);
    assert!(
        (ground[1] - 511.0 - 1376.0).abs() < 0.001,
        "ground y {}",
        ground[1]
    );
    let primary_top = world_to_screen_with([0.0, 6.6], size, view);
    assert!((primary_top[1] - 511.0).abs() < 0.001);
    // The view edges map to the union rectangle edges.
    let top = world_to_screen_with([view.center_x, view.bottom + view.span], size, view);
    assert!(top[1].abs() < 0.001);
    assert!((top[0] - 2080.0).abs() < 0.001);
    let left = world_to_screen_with([-6.0, 0.0], size, view);
    assert!(left[0].abs() < 0.001);
    let right = world_to_screen_with([13.5, 0.0], size, view);
    assert!((right[0] - 4160.0).abs() < 0.001);

    for world in [
        [-6.0, 0.15],
        [-2.4, 0.15],
        [0.0, 0.0],
        [6.9, 6.6],
        [13.5, -2.0],
    ] {
        assert_round_trip(world, size, view);
    }
    for pixel in [
        [0.0, 0.0],
        [768.0, 1887.0],
        [2080.0, 1280.0],
        [4160.0, 2560.0],
    ] {
        let world = screen_to_world_with(pixel, size, view);
        let round_trip = world_to_screen_with(world, size, view);
        for axis in 0..2 {
            assert!(
                (round_trip[axis] - pixel[axis]).abs() < 0.001,
                "{pixel:?} through {world:?}"
            );
        }
    }
    // The same world point keeps its primary-local pixel under the union view
    // and the primary-only view; the union view shifts y by the 511 px top
    // offset of the secondary monitor.
    let primary = desktop_view(1600, 1.5);
    for world_x in [-2.4, -0.8, 0.0, 0.8, 2.4] {
        let union = world_to_screen_with([world_x, 0.15], size, view);
        let local = world_to_screen_with([world_x, 0.15], [2560, 1600], primary);
        assert!((union[0] - local[0]).abs() < 0.001, "x {world_x}");
        assert!((union[1] - local[1] - 511.0).abs() < 0.001, "y {world_x}");
    }
}

#[test]
fn degenerate_offset_view_matches_desktop_view() {
    // A single monitor is its own union: both offsets vanish and the offset
    // constructor must fold back onto `desktop_view` exactly.
    for (height, scale) in [
        (1600u32, 1.5f32),
        (960, 1.0),
        (2560, 1.5),
        (1080, 1.25),
        (0, 1.0),
    ] {
        let plain = desktop_view(height, scale);
        let degenerate = desktop_view_at(height, scale, 0.0, 0.0);
        assert_eq!(degenerate, plain, "{height}@{scale}");
        assert_eq!(degenerate.center_x, 0.0);
        assert_eq!(degenerate.bottom, LEGACY.bottom);
    }
    // Staggering one axis leaves the other untouched.
    let centered = desktop_view_at(1600, 1.5, 800.0, 0.0);
    assert_eq!(centered.span, desktop_view(1600, 1.5).span);
    assert_eq!(centered.bottom, LEGACY.bottom);
    assert!((centered.center_x - 3.75).abs() < 0.00001);
    let shifted = desktop_view_at(1600, 1.5, 0.0, 449.0);
    assert_eq!(shifted.center_x, 0.0);
    assert!(
        (shifted.bottom + 3.0046875).abs() < 0.00001,
        "bottom {}",
        shifted.bottom
    );
}

#[test]
fn staggered_negative_origin_views_keep_the_primary_anchor() {
    // Secondary above-left: union 4160x2600, center 800 px left of the
    // primary center, bottom 800 px below the primary bottom.
    let above_left = desktop_view_at(2600, 1.5, -800.0, 800.0);
    assert_eq!(above_left.span, 12.1875);
    assert!(
        (above_left.center_x + 3.75).abs() < 0.00001,
        "center_x {}",
        above_left.center_x
    );
    assert!(
        (above_left.bottom + 4.65).abs() < 0.00001,
        "bottom {}",
        above_left.bottom
    );
    let size = [4160, 2600];
    // Primary center column in the union is 2880; the primary top edge is
    // 200 px below the union top, so the ground line keeps local row 1376.
    let ground = world_to_screen_with([0.0, 0.15], size, above_left);
    assert!((ground[0] - 2880.0).abs() < 0.001);
    assert!((ground[1] - 200.0 - 1376.0).abs() < 0.001);
    for world in [[-13.0, 0.15], [-3.75, 0.0], [0.0, 7.0], [5.0, -3.0]] {
        assert_round_trip(world, size, above_left);
    }

    // Secondary above-left, shallow: union 4160x1900 with no bottom offset.
    let shallow = desktop_view_at(1900, 1.5, -800.0, 0.0);
    assert_eq!(shallow.span, 8.90625);
    assert_eq!(shallow.bottom, LEGACY.bottom);
    assert!((shallow.center_x + 3.75).abs() < 0.00001);
    let size = [4160, 1900];
    let ground = world_to_screen_with([0.0, 0.15], size, shallow);
    assert!((ground[0] - 2880.0).abs() < 0.001);
    assert!((ground[1] - 300.0 - 1376.0).abs() < 0.001);
    for world in [[-13.5, 0.15], [-3.75, 0.0], [5.5, 5.0]] {
        assert_round_trip(world, size, shallow);
    }

    // Secondary right with no vertical offset: union 4480x1600.
    let right = desktop_view_at(1600, 1.5, 960.0, 0.0);
    assert_eq!(right.span, 7.5);
    assert_eq!(right.bottom, LEGACY.bottom);
    assert!(
        (right.center_x - 4.5).abs() < 0.00001,
        "center_x {}",
        right.center_x
    );
    let size = [4480, 1600];
    let ground = world_to_screen_with([0.0, 0.15], size, right);
    assert!((ground[0] - 1280.0).abs() < 0.001);
    assert!((ground[1] - 1376.0).abs() < 0.001);
    for world in [[-6.0, 0.15], [0.0, 0.0], [4.5, 3.0], [15.0, -0.5]] {
        assert_round_trip(world, size, right);
    }
}
