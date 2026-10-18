//! Read-only desktop observations. None of these queries change input routing.

use std::time::{Duration, Instant};

use glam::Vec3;
use winit::window::Window;

use crate::{
    platform::{self, DesktopRect},
    renderer::{SceneView, screen_to_world_with},
    simulation::WorldRect,
};

use super::{Application, RegionMode};

pub(super) struct AttentionPoll {
    next_pointer: Instant,
    next_edges: Instant,
}

impl AttentionPoll {
    pub fn new(now: Instant) -> Self {
        Self {
            next_pointer: now,
            next_edges: now,
        }
    }

    fn pointer_due(&mut self, now: Instant) -> bool {
        if now < self.next_pointer {
            return false;
        }
        self.next_pointer = now + Duration::from_nanos(33_333_334);
        true
    }

    fn edges_due(&mut self, now: Instant) -> bool {
        if now < self.next_edges {
            return false;
        }
        self.next_edges = now + Duration::from_secs(1);
        true
    }
}

fn contains(rect: DesktopRect, point: [i32; 2]) -> bool {
    point[0] >= rect.left && point[0] < rect.right && point[1] >= rect.top && point[1] < rect.bottom
}

fn pointer_allowed(
    point: [i32; 2],
    fallback: bool,
    monitors: Option<&[DesktopRect]>,
    control_visible: bool,
    control: Option<DesktopRect>,
) -> bool {
    !fallback
        && monitors.is_none_or(|rects| rects.iter().any(|&rect| contains(rect, point)))
        && !(control_visible && control.is_none_or(|rect| contains(rect, point)))
}

fn window_rect(window: &Window) -> Option<DesktopRect> {
    DesktopRect::from_physical(window.outer_position().ok()?, window.outer_size()).ok()
}

fn pointer_world(
    point: [i32; 2],
    origin: [i32; 2],
    size: [u32; 2],
    view: SceneView,
) -> Option<Vec3> {
    let x = i64::from(point[0]) - i64::from(origin[0]);
    let y = i64::from(point[1]) - i64::from(origin[1]);
    if size.contains(&0) || x < 0 || y < 0 || x >= i64::from(size[0]) || y >= i64::from(size[1]) {
        return None;
    }
    let world = screen_to_world_with([x as f32, y as f32], size, view);
    let world = Vec3::new(world[0], world[1], 0.0);
    world.is_finite().then_some(world)
}

fn world_rect(
    rect: DesktopRect,
    origin: [i32; 2],
    size: [u32; 2],
    view: SceneView,
) -> Option<WorldRect> {
    if size.contains(&0) || rect.left >= rect.right || rect.top >= rect.bottom {
        return None;
    }
    let local = |x, y| {
        screen_to_world_with(
            [
                (i64::from(x) - i64::from(origin[0])) as f32,
                (i64::from(y) - i64::from(origin[1])) as f32,
            ],
            size,
            view,
        )
    };
    let top = local(rect.left, rect.top);
    let bottom = local(rect.right, rect.bottom);
    let result = WorldRect {
        left: top[0],
        right: bottom[0],
        bottom: bottom[1],
        top: top[1],
    };
    ([result.left, result.right, result.bottom, result.top]
        .iter()
        .all(|v| v.is_finite())
        && result.left < result.right
        && result.bottom < result.top)
        .then_some(result)
}

impl Application {
    pub(super) fn poll_attention(&mut self, now: Instant) {
        if self.options.smoke_test || self.benchmark.is_some() {
            return;
        }
        let Some(runtime) = &self.runtime else {
            return;
        };
        let Ok(position) = runtime.overlay.outer_position() else {
            let _ = self.simulation.set_pointer_world(None);
            let _ = self.simulation.set_attention_edges(&[], &[], &[]);
            return;
        };
        let origin = [position.x, position.y];
        let size = runtime.overlay.inner_size();
        let extent = [size.width, size.height];
        let view = runtime.scene_view;
        if self.attention_poll.pointer_due(now) {
            let point = platform::attention::cursor_position().filter(|&point| {
                pointer_allowed(
                    point,
                    self.hit_region.mode == RegionMode::Fallback,
                    runtime
                        .desktop
                        .as_ref()
                        .map(|layout| layout.monitors.as_slice()),
                    runtime.control.is_visible().unwrap_or(true),
                    window_rect(&runtime.control),
                )
            });
            let world = point.and_then(|point| pointer_world(point, origin, extent, view));
            if let Err(error) = self.simulation.set_pointer_world(world) {
                let _ = self.simulation.set_pointer_world(None);
                log::warn!("pointer attention cleared: {error}");
            }
        }
        if self.attention_poll.edges_due(now) {
            let Ok(bounds) = DesktopRect::from_physical(position, size) else {
                let _ = self.simulation.set_attention_edges(&[], &[], &[]);
                return;
            };
            let screens = runtime
                .desktop
                .as_ref()
                .map_or_else(|| vec![bounds], |layout| layout.monitors.clone());
            let work = platform::attention::work_area_rects().unwrap_or_default();
            let windows = platform::attention::visible_window_rects(bounds);
            let convert = |rects: &[DesktopRect]| -> Vec<WorldRect> {
                rects
                    .iter()
                    .filter_map(|&rect| world_rect(rect, origin, extent, view))
                    .collect()
            };
            if let Err(error) = self.simulation.set_attention_edges(
                &convert(&screens),
                &convert(&work),
                &convert(&windows),
            ) {
                let _ = self.simulation.set_attention_edges(&[], &[], &[]);
                log::warn!("edge attention cleared: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{LEGACY, desktop_view_at};

    #[test]
    fn negative_screen_origin_maps_like_local_cursor_without_integer_overflow() {
        let size = [4160, 2560];
        let origin = [-1920, -511];
        let view = desktop_view_at(size[1], 1.5, -120.0, 449.0);
        let local = [2000.0, 700.0];
        let expected = screen_to_world_with(local, size, view);
        let actual = pointer_world([80, 189], origin, size, view).unwrap();
        assert_eq!(actual.x.to_bits(), expected[0].to_bits());
        assert_eq!(actual.y.to_bits(), expected[1].to_bits());
        assert_eq!(actual.z, 0.0);
        assert!(pointer_world([i32::MAX, i32::MAX], [i32::MIN; 2], size, view).is_none());
        assert!(pointer_world([80, 189], origin, [0, 2560], view).is_none());
        assert!(pointer_world([-1921, 0], origin, size, view).is_none());
        assert!(pointer_world([2240, 0], origin, size, view).is_none());
    }

    #[test]
    fn edge_rects_use_the_same_negative_origin_projection() {
        let origin = [-960, -320];
        let rect = DesktopRect {
            left: -900,
            right: -500,
            top: -300,
            bottom: 200,
        };
        let actual = world_rect(rect, origin, [960, 640], LEGACY).unwrap();
        let a = screen_to_world_with([60.0, 20.0], [960, 640], LEGACY);
        let b = screen_to_world_with([460.0, 520.0], [960, 640], LEGACY);
        assert_eq!([actual.left, actual.top], a);
        assert_eq!([actual.right, actual.bottom], b);
        assert!(world_rect(rect, origin, [0, 640], LEGACY).is_none());
        assert!(contains(rect, [-900, -300]));
        assert!(!contains(rect, [-500, 0]));
        assert!(!contains(rect, [-700, 200]));
    }

    #[test]
    fn pointer_filter_clears_control_gaps_fallback_and_unreadable_visible_control() {
        let left = DesktopRect {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        let right = DesktopRect {
            left: 0,
            top: -511,
            right: 2560,
            bottom: 1600,
        };
        let monitors = [left, right];
        let control = DesktopRect {
            left: 100,
            top: 100,
            right: 500,
            bottom: 600,
        };
        let allowed = |point, fallback, visible, rect| {
            Some(point)
                .filter(|&point| pointer_allowed(point, fallback, Some(&monitors), visible, rect))
        };
        assert_eq!(
            allowed([200, 200], false, false, Some(control)),
            Some([200, 200])
        );
        assert_eq!(allowed([200, 200], false, true, Some(control)), None);
        assert_eq!(
            allowed([700, 200], false, true, Some(control)),
            Some([700, 200])
        );
        assert_eq!(allowed([700, 200], false, true, None), None);
        assert_eq!(allowed([700, 200], false, false, None), Some([700, 200]));
        assert_eq!(
            allowed([-100, -200], false, false, None),
            None,
            "monitor gap"
        );
        assert_eq!(allowed([-100, 200], false, false, None), Some([-100, 200]));
        assert_eq!(allowed([700, 200], true, false, None), None, "fallback");
        assert!(!pointer_allowed([0, 0], false, Some(&[]), false, None));
        assert!(
            pointer_allowed([0, 0], false, None, false, None),
            "bounded scene has no monitor filter"
        );
    }

    #[test]
    fn polling_is_time_bounded_and_does_not_replay_missed_queries() {
        let start = Instant::now();
        let mut poll = AttentionPoll::new(start);
        assert!(poll.pointer_due(start));
        assert!(poll.edges_due(start));
        assert!(!poll.pointer_due(start + Duration::from_millis(33)));
        assert!(!poll.edges_due(start + Duration::from_millis(999)));
        assert!(poll.pointer_due(start + Duration::from_secs(5)));
        assert!(!poll.pointer_due(start + Duration::from_secs(5)));
        assert!(poll.edges_due(start + Duration::from_secs(5)));
        assert!(!poll.edges_due(start + Duration::from_secs(5)));
    }
}
