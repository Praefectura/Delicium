use winit::window::{Window, WindowAttributes, WindowLevel};

pub fn overlay_attributes(attributes: WindowAttributes) -> WindowAttributes {
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::WindowAttributesExtWindows;
        // The scene overlay presents through a DirectComposition visual
        // (DX12 `DxgiFromVisual`). Without `WS_EX_NOREDIRECTIONBITMAP` the
        // composition content is drawn over an opaque, never-painted
        // redirection surface, which reads as a white backdrop instead of
        // the desktop.
        attributes
            .with_skip_taskbar(true)
            .with_no_redirection_bitmap(true)
    }
    #[cfg(not(target_os = "windows"))]
    {
        attributes
    }
}

pub fn raise_controls(control: &Window) {
    // Force a native z-order update even if the cached level is already topmost.
    control.set_window_level(WindowLevel::Normal);
    control.set_window_level(WindowLevel::AlwaysOnTop);
}

pub fn set_click_through(overlay: &Window, control: &Window, enabled: bool) -> Result<(), String> {
    overlay
        .set_cursor_hittest(!enabled)
        .map_err(|error| format!("Cannot change scene hit testing: {error}"))?;
    raise_controls(control);
    control.request_redraw();
    Ok(())
}

/// Set the overlay window region to the union of `polygons`, in physical
/// window coordinates (top-left origin, y downwards), the coordinate space of
/// [`crate::renderer::world_to_screen`].
///
/// The region clips rendering and defines which pixels can receive input;
/// outside the union the window behaves as if it did not exist. An empty
/// `polygons` list installs an empty region, making the whole window
/// click-through.
///
/// These are window coordinates, not client coordinates; the scene overlay is
/// created without decorations, so for this application the window and client
/// origins coincide.
///
/// Must be called from the thread that owns the window. This function touches
/// only the window region; window styles and z-order are left alone
/// ([`set_click_through`] owns those). On failure the window keeps its
/// previous region.
#[cfg(target_os = "windows")]
pub fn set_hit_region(overlay: &Window, polygons: &[Vec<[i32; 2]>]) -> Result<(), String> {
    win32::set_hit_region(overlay, polygons)
}

/// Windows-only API; fails everywhere else so callers fail closed.
#[cfg(not(target_os = "windows"))]
pub fn set_hit_region(_overlay: &Window, _polygons: &[Vec<[i32; 2]>]) -> Result<(), String> {
    Err("Window hit regions are not supported on this platform".to_owned())
}

/// Remove the overlay window region, restoring normal hit testing and
/// unclipped rendering over the whole window.
///
/// Must be called from the thread that owns the window; see [`set_hit_region`].
#[cfg(target_os = "windows")]
pub fn clear_hit_region(overlay: &Window) -> Result<(), String> {
    win32::clear_hit_region(overlay)
}

/// Windows-only API; fails everywhere else so callers fail closed.
#[cfg(not(target_os = "windows"))]
pub fn clear_hit_region(_overlay: &Window) -> Result<(), String> {
    Err("Window hit regions are not supported on this platform".to_owned())
}

#[cfg(all(test, target_os = "windows"))]
pub(crate) fn hit_region_contains(
    polygons: &[Vec<[i32; 2]>],
    points: &[[i32; 2]],
) -> Result<Vec<bool>, String> {
    win32::hit_region_contains(polygons, points)
}

#[cfg(all(test, target_os = "windows"))]
pub(crate) fn hit_region_matches_sequential_oracle(
    polygons: &[Vec<[i32; 2]>],
) -> Result<bool, String> {
    win32::hit_region_matches_sequential_oracle(polygons)
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::hit_region_contains;

    #[test]
    fn native_region_union_preserves_concave_gaps() {
        let polygons = vec![
            vec![
                [0, 0],
                [10, 0],
                [10, 30],
                [30, 30],
                [30, 0],
                [40, 0],
                [40, 40],
                [0, 40],
            ],
            vec![[50, 50], [60, 50], [60, 60], [50, 60]],
        ];
        assert_eq!(
            hit_region_contains(
                &polygons,
                &[[5, 15], [20, 10], [35, 15], [20, 35], [55, 55], [80, 80]]
            )
            .unwrap(),
            vec![true, false, true, true, true, false]
        );
    }

    #[test]
    fn empty_native_region_contains_no_points() {
        assert_eq!(
            hit_region_contains(&[], &[[0, 0], [10, 10]]).unwrap(),
            vec![false, false]
        );
    }

    #[test]
    fn native_region_rejects_short_and_out_of_range_polygons() {
        for polygon in [
            vec![[0, 0], [1, 1]],
            vec![[0, 0], [1 << 26, 0], [0, 10]],
            vec![[0, 0], [-(1 << 26) - 1, 0], [0, 10]],
        ] {
            assert!(hit_region_contains(&[polygon], &[]).is_err());
        }
    }
}

/// Minimal hand-written Win32 FFI for window regions, deliberately free of new
/// dependencies. Every owned `HRGN` is wrapped in an RAII guard; ownership is
/// released to the system only after `SetWindowRgn` reports success, and any
/// failure path deletes whatever is still owned.
#[cfg(target_os = "windows")]
mod win32 {
    use core::ffi::c_void;
    use core::mem::ManuallyDrop;

    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    /// A window handle (Win32 `HWND`).
    type Hwnd = *mut c_void;
    /// A GDI region handle (Win32 `HRGN`).
    type Hrgn = *mut c_void;

    /// Win32 `POINT`: two `LONG` (`i32`) coordinates.
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    /// `CombineRgn` mode: destination = source1 union source2.
    const RGN_OR: i32 = 2;
    /// `CombineRgn` failure return value.
    const RGN_ERROR: i32 = 0;
    #[cfg(test)]
    const RGN_XOR: i32 = 3;
    #[cfg(test)]
    const NULLREGION: i32 = 1;
    /// `CreatePolygonRgn` fill mode: winding (nonzero).
    const WINDING: i32 = 2;
    /// GDI stores coordinates as signed 27-bit values; anything outside this
    /// range is rejected instead of letting GDI silently clip it.
    const GDI_MIN_COORD: i32 = -(1 << 26);
    const GDI_MAX_COORD: i32 = (1 << 26) - 1;

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreatePolygonRgn(points: *const Point, count: i32, fill_mode: i32) -> Hrgn;
        fn CreateRectRgn(x1: i32, y1: i32, x2: i32, y2: i32) -> Hrgn;
        fn CombineRgn(destination: Hrgn, source1: Hrgn, source2: Hrgn, mode: i32) -> i32;
        fn DeleteObject(object: *mut c_void) -> i32;
        #[cfg(test)]
        fn PtInRegion(region: Hrgn, x: i32, y: i32) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetWindowRgn(window: Hwnd, region: Hrgn, redraw: i32) -> i32;
    }

    /// Owns a GDI region until it is dropped (deleted here) or handed to a
    /// window through `SetWindowRgn`.
    struct GdiRegion(Hrgn);

    impl GdiRegion {
        /// Take ownership of a freshly created region; a null handle means the
        /// GDI call failed and there is nothing to own.
        fn from_raw(region: Hrgn, call: &str) -> Result<Self, String> {
            if region.is_null() {
                Err(format!("{call} failed to create a GDI region"))
            } else {
                #[cfg(test)]
                region_tests::created();
                Ok(Self(region))
            }
        }

        fn handle(&self) -> Hrgn {
            self.0
        }

        /// Give up ownership without deleting: `SetWindowRgn` makes the system
        /// the owner of the region.
        fn into_raw(self) -> Hrgn {
            #[cfg(test)]
            region_tests::released();
            let this = ManuallyDrop::new(self);
            this.0
        }
    }

    impl Drop for GdiRegion {
        fn drop(&mut self) {
            // SAFETY: `self.0` is non-null and this value is its unique owner
            // (handing the region over consumes the value), so deleting the
            // object exactly once through its GDI handle is valid.
            let _deleted = unsafe { DeleteObject(self.0) };
            #[cfg(test)]
            {
                assert_ne!(_deleted, 0, "DeleteObject failed for an owned region");
                region_tests::released();
            }
        }
    }

    /// A valid region containing no area.
    fn empty_region() -> Result<GdiRegion, String> {
        #[cfg(test)]
        if region_tests::fail_call(region_tests::Call::Empty) {
            return GdiRegion::from_raw(core::ptr::null_mut(), "CreateRectRgn");
        }
        // SAFETY: `CreateRectRgn` only reads its four coordinates; an empty
        // rectangle is a supported way to create an empty region.
        let region = unsafe { CreateRectRgn(0, 0, 0, 0) };
        GdiRegion::from_raw(region, "CreateRectRgn")
    }

    /// One polygon as a winding-fill GDI region; malformed input is rejected
    /// rather than silently replaced by something else.
    fn polygon_region(points: &[[i32; 2]]) -> Result<GdiRegion, String> {
        if points.len() < 3 {
            return Err(format!(
                "A hit region polygon needs at least 3 points, got {}",
                points.len()
            ));
        }
        let count = i32::try_from(points.len()).map_err(|_| {
            format!(
                "Hit region polygon has {} points, more than GDI accepts",
                points.len()
            )
        })?;
        let mut native = Vec::with_capacity(points.len());
        for point in points {
            if point[0] < GDI_MIN_COORD
                || point[0] > GDI_MAX_COORD
                || point[1] < GDI_MIN_COORD
                || point[1] > GDI_MAX_COORD
            {
                return Err(format!(
                    "Hit region point ({}, {}) is outside the GDI coordinate range of {GDI_MIN_COORD}..={GDI_MAX_COORD}",
                    point[0], point[1]
                ));
            }
            native.push(Point {
                x: point[0],
                y: point[1],
            });
        }
        #[cfg(test)]
        if region_tests::fail_call(region_tests::Call::Polygon) {
            return GdiRegion::from_raw(core::ptr::null_mut(), "CreatePolygonRgn");
        }
        // SAFETY: `native` holds `count` initialized points and stays alive for
        // the whole call; `CreatePolygonRgn` only reads that buffer. The
        // coordinates were range-checked above.
        let region = unsafe { CreatePolygonRgn(native.as_ptr(), count, WINDING) };
        GdiRegion::from_raw(region, "CreatePolygonRgn")
    }

    /// The union of two owned regions in a freshly allocated destination.
    ///
    /// Consuming both sources means every `CombineRgn` writes a destination
    /// that no other handle aliases; a failure drops (and so deletes)
    /// whatever the call still owns.
    fn union_regions(left: GdiRegion, right: GdiRegion) -> Result<GdiRegion, String> {
        let destination = empty_region()?;
        // SAFETY: `destination`, `left` and `right` are three separate live
        // regions owned by this function; `CombineRgn` only reads the sources
        // and rewrites `destination`, whose handle nothing else owns. RGN_OR
        // may legitimately leave the destination empty (NULLREGION), so only
        // the RGN_ERROR return means failure.
        let result =
            unsafe { CombineRgn(destination.handle(), left.handle(), right.handle(), RGN_OR) };
        #[cfg(test)]
        let result = if region_tests::fail_call(region_tests::Call::Union) {
            RGN_ERROR
        } else {
            result
        };
        if result == RGN_ERROR {
            return Err("CombineRgn failed to union the hit region polygons".to_owned());
        }
        // Dropping both sources deletes them; the fresh destination holds
        // their union.
        Ok(destination)
    }

    /// The union of all polygons, or an empty region when there are none.
    ///
    /// Polygons are validated and their regions created one at a time, in
    /// input order. The regions fold through carry slots that mirror a binary
    /// counter: slot `k` holds the union of a run of `2^k` polygons, and a new
    /// leaf merges upward while its level is occupied, so every merge combines
    /// two runs of equal size. The leftover slots are folded low to high; live
    /// regions are bounded by the slot count plus the merge in flight.
    fn unified_region(polygons: &[Vec<[i32; 2]>]) -> Result<GdiRegion, String> {
        // One slot per bit of the polygon count: a carry standing on level `k`
        // is the union of `2^k` polygons, and `polygons.len()` cannot exceed
        // `usize::MAX = 2^usize::BITS - 1`, so the carry level never indexes
        // past the slots.
        let mut carry_slots: [Option<GdiRegion>; usize::BITS as usize] =
            core::array::from_fn(|_| None);
        for polygon in polygons {
            let mut carry = polygon_region(polygon)?;
            let mut level = 0;
            while let Some(occupant) = carry_slots[level].take() {
                carry = union_regions(occupant, carry)?;
                level += 1;
            }
            carry_slots[level] = Some(carry);
        }
        // Fold the leftover slots from low to high; each slot holds a run at
        // least as large as everything already accumulated below it.
        let mut unified: Option<GdiRegion> = None;
        for slot in &mut carry_slots {
            let Some(region) = slot.take() else {
                continue;
            };
            unified = Some(match unified {
                Some(accumulated) => union_regions(accumulated, region)?,
                None => region,
            });
        }
        // An input without polygons occupies no slot and still gets a valid
        // empty region.
        match unified {
            Some(region) => Ok(region),
            None => empty_region(),
        }
    }

    #[cfg(test)]
    pub(super) fn hit_region_contains(
        polygons: &[Vec<[i32; 2]>],
        points: &[[i32; 2]],
    ) -> Result<Vec<bool>, String> {
        let region = unified_region(polygons)?;
        Ok(points
            .iter()
            .map(|point| {
                // SAFETY: the owned region remains valid throughout this
                // query; PtInRegion reads the handle and integer coordinates.
                unsafe { PtInRegion(region.handle(), point[0], point[1]) != 0 }
            })
            .collect())
    }

    #[cfg(test)]
    fn unified_region_sequential(polygons: &[Vec<[i32; 2]>]) -> Result<GdiRegion, String> {
        let Some((first, rest)) = polygons.split_first() else {
            return empty_region();
        };
        let mut unified = polygon_region(first)?;
        for polygon in rest {
            let source = polygon_region(polygon)?;
            let destination = empty_region()?;
            // SAFETY: all three regions have distinct live owners. This is the
            // original left-fold implementation, independent of union_regions.
            let result = unsafe {
                CombineRgn(
                    destination.handle(),
                    unified.handle(),
                    source.handle(),
                    RGN_OR,
                )
            };
            if result == RGN_ERROR {
                return Err("CombineRgn failed to union the hit region polygons".to_owned());
            }
            unified = destination;
        }
        Ok(unified)
    }

    #[cfg(test)]
    fn regions_equal(left: &GdiRegion, right: &GdiRegion) -> Result<bool, String> {
        let difference = empty_region()?;
        // SAFETY: the sources remain borrowed and live; only the separate
        // owned destination is changed. XOR is empty exactly for equal sets.
        let result =
            unsafe { CombineRgn(difference.handle(), left.handle(), right.handle(), RGN_XOR) };
        if result == RGN_ERROR {
            return Err("CombineRgn failed to compare the hit regions".to_owned());
        }
        Ok(result == NULLREGION)
    }

    #[cfg(test)]
    pub(super) fn hit_region_matches_sequential_oracle(
        polygons: &[Vec<[i32; 2]>],
    ) -> Result<bool, String> {
        let balanced = unified_region(polygons)?;
        let sequential = unified_region_sequential(polygons)?;
        regions_equal(&balanced, &sequential)
    }

    /// Resolve a winit window to its Win32 `HWND`.
    fn overlay_hwnd(overlay: &Window) -> Result<Hwnd, String> {
        let handle = overlay
            .window_handle()
            .map_err(|error| format!("Cannot query the overlay window handle: {error}"))?;
        match handle.as_raw() {
            RawWindowHandle::Win32(raw) => Ok(raw.hwnd.get() as *mut c_void),
            _ => Err("The overlay window is not a Win32 window".to_owned()),
        }
    }

    pub(super) fn set_hit_region(
        overlay: &Window,
        polygons: &[Vec<[i32; 2]>],
    ) -> Result<(), String> {
        let window = overlay_hwnd(overlay)?;
        let region = unified_region(polygons)?;
        // SAFETY: `window` is this live window's `HWND`, kept valid for the
        // call by the caller's borrow of `overlay`, and `region` is a valid
        // `HRGN` this call owns. On success the system takes over the region;
        // on failure the guard below still owns and deletes it. `redraw = 0`
        // skips the system-triggered repaint because the render loop repaints.
        let applied = unsafe { SetWindowRgn(window, region.handle(), 0) };
        if applied == 0 {
            return Err("SetWindowRgn failed; the window keeps the previous region".to_owned());
        }
        // Ownership passes to the system; do not delete the region here.
        region.into_raw();
        Ok(())
    }

    pub(super) fn clear_hit_region(overlay: &Window) -> Result<(), String> {
        let window = overlay_hwnd(overlay)?;
        // SAFETY: `window` is this live window's `HWND`; a null region is the
        // documented way to remove the window region, so no region ownership
        // is involved. `redraw = 0` as above.
        let applied = unsafe { SetWindowRgn(window, core::ptr::null_mut(), 0) };
        if applied == 0 {
            return Err("SetWindowRgn failed to clear the window region".to_owned());
        }
        Ok(())
    }

    #[cfg(test)]
    mod region_tests {
        use super::*;
        use std::cell::RefCell;

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(super) enum Call {
            Polygon,
            Empty,
            Union,
        }

        #[derive(Clone, Copy, Default)]
        struct State {
            live: usize,
            peak: usize,
            calls: [usize; 3],
            failure: Option<(Call, usize)>,
        }

        std::thread_local! {
            static STATE: RefCell<State> = RefCell::new(State::default());
        }

        pub(super) fn created() {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.live += 1;
                state.peak = state.peak.max(state.live);
            });
        }

        pub(super) fn released() {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                assert!(state.live > 0, "Released an unowned region");
                state.live -= 1;
            });
        }

        pub(super) fn fail_call(call: Call) -> bool {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.calls[call as usize] += 1;
                state.failure == Some((call, state.calls[call as usize]))
            })
        }

        fn state() -> State {
            STATE.with(|state| *state.borrow())
        }

        fn reset() {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                assert_eq!(state.live, 0, "Previous build leaked an owned region");
                *state = State::default();
            });
        }

        struct FailureGuard;

        impl FailureGuard {
            fn new(call: Call, ordinal: usize) -> Self {
                reset();
                STATE.with(|state| state.borrow_mut().failure = Some((call, ordinal)));
                Self
            }
        }

        impl Drop for FailureGuard {
            fn drop(&mut self) {
                STATE.with(|state| state.borrow_mut().failure = None);
            }
        }

        fn rectangle(x: i32, y: i32, width: i32, height: i32) -> Vec<[i32; 2]> {
            vec![
                [x, y],
                [x + width, y],
                [x + width, y + height],
                [x, y + height],
            ]
        }

        #[test]
        fn balanced_union_matches_sequential_oracle() {
            let square = rectangle(0, 0, 10, 10);
            let concave = vec![
                [0, 0],
                [10, 0],
                [10, 30],
                [30, 30],
                [30, 0],
                [40, 0],
                [40, 40],
                [0, 40],
            ];
            let mut reversed = square.clone();
            reversed.reverse();
            let cases = vec![
                vec![],
                vec![square.clone()],
                vec![square.clone(), square.clone(), reversed],
                vec![rectangle(-100, -100, 8, 8), rectangle(100, 100, 8, 8)],
                vec![square, rectangle(5, 5, 10, 10), rectangle(10, 0, 10, 10)],
                vec![concave, rectangle(50, 50, 10, 10)],
                vec![
                    vec![[0, 0], [1, 30], [0, 30]],
                    vec![[1, 0], [2, 30], [1, 30]],
                ],
                vec![vec![[0, 0], [1, 1], [2, 2]], vec![[0, 0], [0, 0], [0, 0]]],
                vec![
                    rectangle(GDI_MIN_COORD, GDI_MIN_COORD, 16, 16),
                    rectangle(GDI_MAX_COORD - 16, GDI_MAX_COORD - 16, 16, 16),
                ],
            ];
            for mut polygons in cases {
                reset();
                assert!(hit_region_matches_sequential_oracle(&polygons).unwrap());
                if !polygons.is_empty() {
                    polygons.rotate_left(1);
                    assert!(hit_region_matches_sequential_oracle(&polygons).unwrap());
                }
                polygons.reverse();
                for polygon in &mut polygons {
                    polygon.reverse();
                }
                assert!(hit_region_matches_sequential_oracle(&polygons).unwrap());
                assert_eq!(state().live, 0);
            }
        }

        #[test]
        fn xor_oracle_detects_a_one_pixel_difference() {
            reset();
            {
                let left = unified_region(&[rectangle(0, 0, 10, 10)]).unwrap();
                let right = unified_region(&[rectangle(0, 0, 11, 10)]).unwrap();
                assert!(!regions_equal(&left, &right).unwrap());
                assert!(regions_equal(&left, &left).unwrap());
            }
            assert_eq!(state().live, 0);
        }

        #[test]
        fn balanced_union_has_logarithmic_peak_and_exact_stress_equivalence() {
            let mut seed = 7_u32;
            let polygons: Vec<_> = (0..10_512)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let x = (seed % 640) as i32 - 32;
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let y = (seed % 440) as i32 - 24;
                    rectangle(x, y, 5, 4)
                })
                .collect();
            for count in [0_usize, 1, 2, 3, 4, 7, 8, 9, 31, 32, 33, 10_512] {
                reset();
                let region = unified_region(&polygons[..count]).unwrap();
                let stats = state();
                let bound = if count < 2 {
                    1
                } else {
                    (usize::BITS - count.leading_zeros()) as usize + 1
                };
                assert_eq!(stats.live, 1);
                assert!(
                    stats.peak <= bound,
                    "N={count}: peak {} > {bound}",
                    stats.peak
                );
                assert_eq!(stats.calls[Call::Polygon as usize], count);
                assert_eq!(stats.calls[Call::Union as usize], count.saturating_sub(1));
                assert_eq!(
                    stats.calls[Call::Empty as usize],
                    count.max(1) - usize::from(count > 0)
                );
                drop(region);
                assert_eq!(state().live, 0);
            }
            assert!(hit_region_matches_sequential_oracle(&polygons).unwrap());
            assert_eq!(state().live, 0);
        }

        #[test]
        fn late_invalid_leaf_releases_occupied_levels() {
            for prefix in [31, 10_511] {
                for invalid in [
                    vec![[0, 0], [1, 1]],
                    vec![[GDI_MIN_COORD - 1, 0], [0, 0], [0, 10]],
                    vec![[0, GDI_MAX_COORD + 1], [10, 0], [0, 10]],
                ] {
                    reset();
                    let mut polygons = vec![rectangle(0, 0, 8, 8); prefix];
                    polygons.push(invalid);
                    assert!(unified_region(&polygons).is_err());
                    assert_eq!(state().calls[Call::Polygon as usize], prefix);
                    assert_eq!(state().live, 0);
                }
            }
        }

        #[test]
        fn injected_native_failures_release_all_owned_regions() {
            let polygons: Vec<_> = (0..9).map(|x| rectangle(x * 12, 0, 8, 8)).collect();
            for (call, ordinals) in [
                (Call::Polygon, &[1, 2, 4, 8, 9][..]),
                (Call::Empty, &[1, 2, 4, 8][..]),
                (Call::Union, &[1, 2, 4, 8][..]),
            ] {
                for &ordinal in ordinals {
                    {
                        let _failure = FailureGuard::new(call, ordinal);
                        assert!(unified_region(&polygons).is_err(), "{call:?} #{ordinal}");
                        assert_eq!(state().calls[call as usize], ordinal);
                        assert_eq!(state().live, 0, "{call:?} #{ordinal}");
                    }
                    assert!(unified_region(&polygons).is_ok());
                    assert_eq!(state().live, 0);
                }
            }
            let _failure = FailureGuard::new(Call::Empty, 1);
            assert!(unified_region(&[]).is_err());
            assert_eq!(state().live, 0);
        }

        #[test]
        fn ownership_transfer_leaves_the_local_counter() {
            reset();
            let raw = empty_region().unwrap().into_raw();
            let live_after_transfer = state().live;
            // SAFETY: the test is the recipient of this transferred handle;
            // no window owns it, so it must delete it exactly once itself.
            let deleted = unsafe { DeleteObject(raw) };
            assert_ne!(deleted, 0);
            assert_eq!(live_after_transfer, 0);
            assert_eq!(state().live, 0);
        }
    }
}
