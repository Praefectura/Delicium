pub mod attention;
pub mod desktop;
pub mod tray;

pub use desktop::{DesktopRect, desktop_bounding_rect, mark_non_fullscreen};
pub use tray::{TrayAction, TrayIcon};

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

/// One region-build plan for the Phase-0 decision-gate probe
/// (`region_batch_gate_probe` in the app tests); evidence and the verdict
/// live in `.artifacts/perf-perbatch-20261016-015856/`.
#[cfg(all(test, target_os = "windows"))]
#[derive(Clone, Copy, Debug)]
pub(crate) enum ProbedPlan<'a> {
    /// The gate's A arm: the previous production fold, one
    /// `CreatePolygonRgn` per polygon.
    Unified,
    /// Batches of `chunk` consecutive polygons through the streamed fold.
    Chunked(usize),
    /// Diagnostics only: build every batch of `chunk` polygons and drop it
    /// again, with no unions at all (leaf-cost lower bound).
    BuildOnlyChunked(usize),
    /// One batch per instance, from the flat polygon list's end offsets.
    PerInstance(&'a [usize]),
}

/// Builds `polygons` under `plan` and drops the result; returns the build
/// outcome. Panics if any region is still owned afterwards, so a leaker
/// fails the probe instead of skewing the timings.
#[cfg(all(test, target_os = "windows"))]
pub(crate) fn probe_region_build(
    polygons: &[Vec<[i32; 2]>],
    plan: ProbedPlan<'_>,
) -> Result<(), String> {
    win32::probe_region_build(polygons, plan)
}

/// Builds `polygons` under `plan` and compares the union against the
/// independent sequential oracle. `BuildOnlyChunked` builds no union and is
/// rejected.
#[cfg(all(test, target_os = "windows"))]
pub(crate) fn probe_region_equivalence(
    polygons: &[Vec<[i32; 2]>],
    plan: ProbedPlan<'_>,
) -> Result<bool, String> {
    win32::probe_region_equivalence(polygons, plan)
}

/// Scanline prototype (P0, test only): the exact integer fill and one
/// `ExtCreateRegion` call, without touching the production dispatcher.
#[cfg(all(test, target_os = "windows"))]
pub(crate) mod scan_probe {
    pub(crate) use super::win32::scan::{
        MAX_SCAN_POLYGON_POINTS, MIN_SCAN_POLYGONS, NotApplicable, Scan, applicable,
        is_strictly_convex_simple, scanline_matches_chunked, scanline_matches_sequential,
        try_region,
    };

    /// The fill rule measured from GDI on this machine.
    pub(crate) fn measured_rule() -> super::win32::scan::FillRule {
        super::win32::scan::MEASURED_RULE
    }

    /// Build statistics for one scanned frame: exact rectangle count,
    /// pixel area, and the scan work bound actually consumed. Only call it
    /// on geometry the fast path accepted.
    pub(crate) fn build_stats(polygons: &[Vec<[i32; 2]>]) -> Result<(Scan, f64), String> {
        let bounds = super::win32::scan::applicable_bounds(polygons)
            .map_err(|decline| decline.reason().to_owned())?;
        let start = std::time::Instant::now();
        let scan = super::win32::scan::scan_convex_polygons(polygons, measured_rule(), bounds)
            .map_err(super::win32::scan::ScanError::into_message)?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        Ok((scan, elapsed))
    }

    /// The fast path's decision for one frame, or the region it built.
    pub(crate) fn classify_frame(
        polygons: &[Vec<[i32; 2]>],
    ) -> Result<Result<super::win32::GdiRegion, NotApplicable>, String> {
        try_region(polygons, measured_rule())
    }

    /// Per-stage timings in milliseconds for one frame: applicability,
    /// the fill, the RGNDATA packing and `ExtCreateRegion`.
    pub(crate) fn stage_stats(polygons: &[Vec<[i32; 2]>]) -> Result<[f64; 4], String> {
        let rule = measured_rule();
        let start = std::time::Instant::now();
        let decision = super::win32::scan::applicable_bounds(polygons);
        let apply_ms = start.elapsed().as_secs_f64() * 1000.0;
        let bounds = match decision {
            Ok(bounds) => bounds,
            Err(decline) => return Err(format!("declined: {}", decline.reason())),
        };
        let start = std::time::Instant::now();
        let scan = super::win32::scan::scan_convex_polygons(polygons, rule, bounds)
            .map_err(super::win32::scan::ScanError::into_message)?;
        let fill_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = std::time::Instant::now();
        let region = super::win32::scan::region_from_scan(&scan)
            .map_err(super::win32::scan::ScanError::into_message)?;
        let pack_ms = start.elapsed().as_secs_f64() * 1000.0;
        drop(region);
        Ok([apply_ms, fill_ms, pack_ms, 0.0])
    }
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

    /// Win32 `RECT`: left/top/right/bottom, `right` and `bottom` exclusive.
    #[cfg(test)]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    /// Win32 `RGNDATAHEADER`; `rgn_size` counts only the RECT payload, while
    /// the byte count passed to `ExtCreateRegion` includes this header.
    #[cfg(test)]
    #[repr(C)]
    struct RgndataHeader {
        size: u32,
        kind: u32,
        count: u32,
        rgn_size: u32,
        bound: Rect,
    }

    #[cfg(test)]
    const _: () = assert!(core::mem::size_of::<Rect>() == 16);
    #[cfg(test)]
    const _: () = assert!(core::mem::size_of::<RgndataHeader>() == 32);

    /// `RGNDATAHEADER.dwType`: the data is an array of rectangles.
    const RDH_RECTANGLES: u32 = 1;

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
        #[cfg(test)]
        fn CreatePolygonRgn(points: *const Point, count: i32, fill_mode: i32) -> Hrgn;
        /// One region from many polygons in a single call: `points` holds all
        /// polygons end to end, `counts[i]` is the point count of polygon `i`,
        /// and `polygons` is the number of polygons.
        fn CreatePolyPolygonRgn(
            points: *const Point,
            counts: *const i32,
            polygons: i32,
            fill_mode: i32,
        ) -> Hrgn;
        fn CreateRectRgn(x1: i32, y1: i32, x2: i32, y2: i32) -> Hrgn;
        fn CombineRgn(destination: Hrgn, source1: Hrgn, source2: Hrgn, mode: i32) -> i32;
        fn DeleteObject(object: *mut c_void) -> i32;
        #[cfg(test)]
        fn PtInRegion(region: Hrgn, x: i32, y: i32) -> i32;
        /// One region from an `RGNDATA` rectangle array (or the documented
        /// `RGNDATA` forms). `bytes` is the whole buffer length.
        fn ExtCreateRegion(xform: *const c_void, bytes: u32, data: *const c_void) -> Hrgn;
        /// Fills a caller buffer with the region's `RGNDATA`; a null buffer
        /// or zero size returns the required byte count.
        #[cfg(test)]
        fn GetRegionData(region: Hrgn, bytes: u32, data: *mut c_void) -> u32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetWindowRgn(window: Hwnd, region: Hrgn, redraw: i32) -> i32;
        /// GDI object count for a process (`GR_GDIOBJECTS`). The region
        /// accounting tests compare this against Windows' own count so a
        /// missed `DeleteObject` cannot hide behind a self-maintained
        /// counter that was never incremented.
        #[cfg(test)]
        fn GetGuiResources(process: Hwnd, flags: u32) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[cfg(test)]
        fn GetCurrentProcess() -> Hwnd;
        #[cfg(test)]
        fn GetLastError() -> u32;
    }

    /// `GR_GDIOBJECTS`, the flag [`GetGuiResources`] takes for GDI objects.
    #[cfg(test)]
    const GR_GDIOBJECTS: u32 = 0;

    /// The process's own GDI object count, as Windows reports it. Used by the
    /// leak tests: the self-maintained region counter proves we call
    /// `DeleteObject`, this proves GDI agrees nothing is left behind.
    ///
    /// Returns the Win32 error when the query itself fails, because
    /// `GetGuiResources` reports failure as `0` — indistinguishable from a
    /// process that genuinely owns no GDI objects.
    #[cfg(test)]
    fn gdi_object_count() -> Result<u32, u32> {
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // cleanup, and `GetGuiResources` only reads a per-process counter.
        unsafe {
            SetLastError(0);
            let count = GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS);
            if count == 0 {
                let error = GetLastError();
                if error != 0 {
                    return Err(error);
                }
            }
            Ok(count)
        }
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[cfg(test)]
        fn SetLastError(error: u32);
    }

    /// Owns a GDI region until it is dropped (deleted here) or handed to a
    /// window through `SetWindowRgn`.
    pub(crate) struct GdiRegion(Hrgn);

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

    /// Validates one polygon: at least three points, a point count GDI
    /// accepts, and every coordinate inside the signed 27-bit GDI range.
    /// Returns the point count as the `i32` GDI expects.
    fn validate_polygon(points: &[[i32; 2]]) -> Result<i32, String> {
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
        }
        Ok(count)
    }

    /// Appends validated points to `native` in the native `POINT` layout.
    fn push_native_points(points: &[[i32; 2]], native: &mut Vec<Point>) {
        native.extend(points.iter().map(|point| Point {
            x: point[0],
            y: point[1],
        }));
    }

    /// One polygon as a winding-fill GDI region; malformed input is rejected
    /// rather than silently replaced by something else.
    ///
    /// Test reference of the previous production leaf build (now batched);
    /// kept next to the batch path so both share validation and messages.
    #[cfg(test)]
    fn polygon_region(points: &[[i32; 2]]) -> Result<GdiRegion, String> {
        let count = validate_polygon(points)?;
        let mut native = Vec::with_capacity(points.len());
        push_native_points(points, &mut native);
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

    /// Number of polygons per `CreatePolyPolygonRgn` batch in the production
    /// region build.
    ///
    /// Chosen by the Phase-0 decision gate (see
    /// `.artifacts/perf-perbatch-20261016-015856/02-gate-decision.md`,
    /// 2026-10-16): on the real 64-instance benchmark geometry streamed
    /// through the carry-slot fold, 41-polygon batches measured 0.332x the
    /// paired median of the per-polygon balanced build (50.6ms -> 16.8ms p50
    /// at 10512 polygons) with exact sequential-oracle equivalence — the
    /// registered 82/164/328-polygon arms measured 0.370x/0.473x/0.694x and
    /// per-instance (~250 polygons) 0.599x. The optimum sits at small batches
    /// because the batch call cost grows superlinearly in the batch's polygon
    /// count while the extra union count only grows linearly with 1/chunk.
    const REGION_CHUNK_POLYGONS: usize = 41;

    /// Process-side cap on the total point count of one batched region.
    ///
    /// It is a fail-fast guard far above the production workload (with
    /// `REGION_CHUNK_POLYGONS` polygons a batch holds a few hundred points)
    /// and far below any batch whose `POINT` buffer could exhaust the
    /// process; the exact native region limits are not documented and, when
    /// exceeded, surface as a null `CreatePolyPolygonRgn` that this module
    /// turns into a fail-closed error (the caller falls back). The batch
    /// probe additionally measured that a batch whose geometry spans >= 2^24
    /// on the y axis is rejected the same fail-closed way, while the x axis
    /// and single polygons carry no such limit; production batches are
    /// window-local (a few thousand pixels), orders of magnitude below that
    /// boundary.
    const MAX_BATCH_POINTS: usize = 1 << 22;

    /// Twice the signed area of a polygon (shoelace); the sign follows
    /// winding. `i128` keeps the sum exact across the full GDI coordinate
    /// range and any batch size.
    fn signed_area_twice(points: &[[i32; 2]]) -> i128 {
        let mut sum = 0_i128;
        for (index, &[x, y]) in points.iter().enumerate() {
            let [next_x, next_y] = points[(index + 1) % points.len()];
            sum += i128::from(x) * i128::from(next_y) - i128::from(next_x) * i128::from(y);
        }
        sum
    }

    /// Validates and flattens `polygons` for a single `CreatePolyPolygonRgn`
    /// call: returns the native point array, each polygon's point count, and
    /// the polygon count. Every polygon is validated first, so a malformed
    /// batch is rejected before any native call.
    ///
    /// Point order is normalized to one winding direction: under the winding
    /// fill rule a point covered by `k` same-winding polygons fills with
    /// winding `k >= 1`, so the batch fill is the union of the polygons.
    /// Reversing a polygon mirrors the sign of its winding numbers everywhere
    /// without changing where they are zero, so normalization never moves an
    /// edge of the polygon's own filled area.
    fn batch_points(polygons: &[Vec<[i32; 2]>]) -> Result<(Vec<Point>, Vec<i32>, i32), String> {
        let mut total_points = 0_usize;
        let mut counts = Vec::with_capacity(polygons.len());
        for polygon in polygons {
            let count = validate_polygon(polygon)?;
            counts.push(count);
            total_points += polygon.len();
        }
        if total_points > MAX_BATCH_POINTS {
            return Err(format!(
                "Hit region batch has {total_points} points, more than GDI accepts in one region"
            ));
        }
        let polygon_count = i32::try_from(polygons.len()).map_err(|_| {
            format!(
                "Hit region batch has {} polygons, more than GDI accepts",
                polygons.len()
            )
        })?;
        let mut native = Vec::with_capacity(total_points);
        for polygon in polygons {
            if signed_area_twice(polygon) < 0 {
                native.extend(polygon.iter().rev().map(|point| Point {
                    x: point[0],
                    y: point[1],
                }));
            } else {
                push_native_points(polygon, &mut native);
            }
        }
        Ok((native, counts, polygon_count))
    }

    /// One region from every polygon in a single `CreatePolyPolygonRgn` call,
    /// or an empty region when there are none.
    ///
    /// See [`batch_points`] for validation and the winding-normalization
    /// argument that makes the batch the exact union of the polygons.
    fn batched_region(polygons: &[Vec<[i32; 2]>]) -> Result<GdiRegion, String> {
        if polygons.is_empty() {
            return empty_region();
        }
        let (native, counts, polygon_count) = batch_points(polygons)?;
        #[cfg(test)]
        if region_tests::fail_call(region_tests::Call::PolyPoly) {
            return GdiRegion::from_raw(core::ptr::null_mut(), "CreatePolyPolygonRgn");
        }
        // SAFETY: `native` holds one initialized point per region vertex and
        // `counts` one point count per polygon; both buffers stay alive for
        // the call and `CreatePolyPolygonRgn` only reads them. Coordinates,
        // counts and the batch size were validated above.
        let region = unsafe {
            CreatePolyPolygonRgn(native.as_ptr(), counts.as_ptr(), polygon_count, WINDING)
        };
        GdiRegion::from_raw(region, "CreatePolyPolygonRgn")
    }

    /// Batches of consecutive polygons (`start..end` runs) split at chunk
    /// size, covering every polygon exactly once; empty input yields no
    /// boundaries. `chunk` is at least 1.
    fn boundaries_from_chunk(polygons_len: usize, chunk: usize) -> Vec<usize> {
        let chunk = chunk.max(1);
        let mut boundaries = Vec::new();
        let mut end = 0_usize;
        while end < polygons_len {
            end = (end + chunk).min(polygons_len);
            boundaries.push(end);
        }
        boundaries
    }

    /// Validates per-instance end offsets into one flat polygon list and
    /// collapses zero-length instances: returns the strictly increasing
    /// batch boundaries, or an error when the offsets are not non-decreasing
    /// or do not cover exactly `polygons_len` polygons (fail-closed, no
    /// batch is attempted on inconsistent input).
    #[cfg(test)]
    fn grouped_boundaries(
        polygons_len: usize,
        instance_ends: &[usize],
    ) -> Result<Vec<usize>, String> {
        let mut previous = 0_usize;
        let mut boundaries = Vec::with_capacity(instance_ends.len());
        for &end in instance_ends {
            if end < previous || end > polygons_len {
                return Err(format!(
                    "Hit region instance boundary {end} is invalid for {polygons_len} polygons"
                ));
            }
            if end > previous {
                boundaries.push(end);
                previous = end;
            }
        }
        if previous != polygons_len {
            return Err(format!(
                "Hit region instance boundaries cover {previous} of {polygons_len} polygons"
            ));
        }
        Ok(boundaries)
    }

    /// Batched regions streamed through the same carry-slot fold as the
    /// per-polygon build, with one `CreatePolyPolygonRgn` call per
    /// `boundaries` run instead of one `CreatePolygonRgn` per polygon.
    ///
    /// `boundaries` holds the exclusive end of every batch and must be
    /// strictly increasing with the last entry equal to `polygons.len()`
    /// (empty for empty input); inconsistency is rejected before any native
    /// call. Each batch folds into the slots as soon as it is built, so live
    /// regions stay bounded by the slot count plus one batch in flight.
    fn streamed_batched_region(
        polygons: &[Vec<[i32; 2]>],
        boundaries: &[usize],
    ) -> Result<GdiRegion, String> {
        let mut previous = 0_usize;
        for &end in boundaries {
            if end <= previous || end > polygons.len() {
                return Err(format!(
                    "Hit region batch boundary {end} is invalid for {} polygons",
                    polygons.len()
                ));
            }
            previous = end;
        }
        if previous != polygons.len() {
            return Err(format!(
                "Hit region batch boundaries cover {previous} of {} polygons",
                polygons.len()
            ));
        }
        let mut carry_slots: [Option<GdiRegion>; usize::BITS as usize] =
            core::array::from_fn(|_| None);
        let mut start = 0_usize;
        for &end in boundaries {
            let mut carry = batched_region(&polygons[start..end])?;
            start = end;
            let mut level = 0;
            while let Some(occupant) = carry_slots[level].take() {
                carry = union_regions(occupant, carry)?;
                level += 1;
            }
            carry_slots[level] = Some(carry);
        }
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
        match unified {
            Some(region) => Ok(region),
            None => empty_region(),
        }
    }

    /// [`streamed_batched_region`] over fixed-size batches of `chunk`
    /// consecutive polygons; the production build passes
    /// [`REGION_CHUNK_POLYGONS`].
    fn chunked_region(polygons: &[Vec<[i32; 2]>], chunk: usize) -> Result<GdiRegion, String> {
        let boundaries = boundaries_from_chunk(polygons.len(), chunk);
        streamed_batched_region(polygons, &boundaries)
    }

    /// [`streamed_batched_region`] over one batch per instance, from the
    /// flat list's per-instance end offsets; empty groups are skipped.
    #[cfg(test)]
    fn grouped_region(
        polygons: &[Vec<[i32; 2]>],
        instance_ends: &[usize],
    ) -> Result<GdiRegion, String> {
        let boundaries = grouped_boundaries(polygons.len(), instance_ends)?;
        streamed_batched_region(polygons, &boundaries)
    }

    /// The union of all polygons, or an empty region when there are none.
    ///
    /// Polygons are validated and their regions created one at a time, in
    /// input order. The regions fold through carry slots that mirror a binary
    /// counter: slot `k` holds the union of a run of `2^k` polygons, and a new
    /// leaf merges upward while its level is occupied, so every merge combines
    /// two runs of equal size. The leftover slots are folded low to high; live
    /// regions are bounded by the slot count plus the merge in flight.
    ///
    /// Test reference of the previous production fold (now batched via
    /// [`chunked_region`]); the oracle and the probe's `Unified` arm hold the
    /// per-polygon behavior under test.
    #[cfg(test)]
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

    /// The production region build for `polygons`.
    ///
    /// Dense frames whose polygons are all small, strictly convex and simple
    /// take the exact integer scanline fill and a single `ExtCreateRegion`;
    /// every other input — low density, non-convex, self-intersecting and any
    /// resource-cap overrun — keeps the batched GDI build unchanged. A budget
    /// decline is decided before any region is created, so falling back never
    /// leaks a partially built region, and a real error (malformed input,
    /// overflow, allocation or `ExtCreateRegion` failure) stays an error
    /// instead of being hidden as a fallback.
    fn region_for_polygons(polygons: &[Vec<[i32; 2]>]) -> Result<GdiRegion, String> {
        match scan::try_region(polygons, scan::MEASURED_RULE)? {
            Ok(region) => Ok(region),
            Err(_decline) => chunked_region(polygons, REGION_CHUNK_POLYGONS),
        }
    }

    #[cfg(test)]
    pub(super) fn hit_region_contains(
        polygons: &[Vec<[i32; 2]>],
        points: &[[i32; 2]],
    ) -> Result<Vec<bool>, String> {
        let region = region_for_polygons(polygons)?;
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
        // The production dispatcher, so this hook tracks what
        // `set_hit_region` installs rather than a fixed plan.
        let production = region_for_polygons(polygons)?;
        let sequential = unified_region_sequential(polygons)?;
        let equal = regions_equal(&production, &sequential);
        drop(production);
        drop(sequential);
        equal
    }

    /// One batch per `chunk` consecutive polygons, each built and dropped
    /// immediately, with no unions: the batched path's leaf-cost floor.
    #[cfg(test)]
    fn build_only_chunked_region(polygons: &[Vec<[i32; 2]>], chunk: usize) -> Result<(), String> {
        let boundaries = boundaries_from_chunk(polygons.len(), chunk);
        let mut start = 0_usize;
        for &end in &boundaries {
            let region = batched_region(&polygons[start..end])?;
            drop(region);
            start = end;
        }
        Ok(())
    }

    /// Number of batches `Chunked(chunk)` will build for `polygons_len`
    /// polygons.
    #[cfg(test)]
    fn expected_batches(polygons_len: usize, chunk: usize) -> usize {
        polygons_len.div_ceil(chunk.max(1))
    }

    /// Builds `polygons` under `plan` (dropping the result), checks that the
    /// plan actually used its intended native calls, and panics if any
    /// region is still owned afterwards. Returns the build outcome.
    #[cfg(test)]
    pub(super) fn probe_region_build(
        polygons: &[Vec<[i32; 2]>],
        plan: super::ProbedPlan<'_>,
    ) -> Result<(), String> {
        region_tests::reset();
        let result = match plan {
            super::ProbedPlan::Unified => unified_region(polygons).map(drop),
            super::ProbedPlan::Chunked(chunk) => chunked_region(polygons, chunk).map(drop),
            super::ProbedPlan::BuildOnlyChunked(chunk) => {
                build_only_chunked_region(polygons, chunk)
            }
            super::ProbedPlan::PerInstance(ends) => grouped_region(polygons, ends).map(drop),
        };
        if result.is_ok() {
            match plan {
                super::ProbedPlan::Unified => {
                    assert_eq!(
                        region_tests::call_count(region_tests::Call::Polygon),
                        polygons.len(),
                        "Unified must call CreatePolygonRgn once per polygon"
                    );
                    assert_eq!(region_tests::call_count(region_tests::Call::PolyPoly), 0);
                }
                super::ProbedPlan::Chunked(chunk) => {
                    assert_eq!(
                        region_tests::call_count(region_tests::Call::Polygon),
                        0,
                        "Chunked must not call CreatePolygonRgn"
                    );
                    assert_eq!(
                        region_tests::call_count(region_tests::Call::PolyPoly),
                        expected_batches(polygons.len(), chunk),
                        "Chunked must call CreatePolyPolygonRgn once per batch"
                    );
                }
                super::ProbedPlan::BuildOnlyChunked(chunk) => {
                    assert_eq!(region_tests::call_count(region_tests::Call::Polygon), 0);
                    assert_eq!(
                        region_tests::call_count(region_tests::Call::PolyPoly),
                        expected_batches(polygons.len(), chunk)
                    );
                    assert_eq!(
                        region_tests::call_count(region_tests::Call::Union),
                        0,
                        "BuildOnlyChunked must not union"
                    );
                    assert_eq!(region_tests::call_count(region_tests::Call::Empty), 0);
                }
                super::ProbedPlan::PerInstance(ends) => {
                    assert_eq!(
                        region_tests::call_count(region_tests::Call::Polygon),
                        0,
                        "PerInstance must not call CreatePolygonRgn"
                    );
                    let mut previous = 0_usize;
                    let mut groups = 0_usize;
                    for &end in ends {
                        if end > previous {
                            groups += 1;
                            previous = end;
                        }
                    }
                    assert_eq!(
                        region_tests::call_count(region_tests::Call::PolyPoly),
                        groups,
                        "PerInstance must call CreatePolyPolygonRgn once per non-empty instance"
                    );
                }
            }
        } else if !matches!(plan, super::ProbedPlan::Unified) {
            assert_eq!(
                region_tests::call_count(region_tests::Call::Polygon),
                0,
                "Batched plans must not call CreatePolygonRgn, even when they fail"
            );
        }
        assert_eq!(
            region_tests::live_regions(),
            0,
            "Probe build leaked an owned region: {plan:?}"
        );
        result
    }

    /// Builds `polygons` under `plan` and compares the union against the
    /// independent sequential oracle. `BuildOnlyChunked` builds no union and
    /// is rejected. Panics if any region is still owned afterwards, so a
    /// leaker fails the probe instead of skewing the timings.
    #[cfg(test)]
    pub(super) fn probe_region_equivalence(
        polygons: &[Vec<[i32; 2]>],
        plan: super::ProbedPlan<'_>,
    ) -> Result<bool, String> {
        region_tests::reset();
        let result = match match plan {
            super::ProbedPlan::Unified => unified_region(polygons),
            super::ProbedPlan::Chunked(chunk) => chunked_region(polygons, chunk),
            super::ProbedPlan::PerInstance(ends) => grouped_region(polygons, ends),
            super::ProbedPlan::BuildOnlyChunked(_) => {
                return Err("BuildOnlyChunked has no union to compare".to_owned());
            }
        } {
            Ok(planned) => match unified_region_sequential(polygons) {
                Ok(sequential) => {
                    let equal = regions_equal(&planned, &sequential);
                    drop(planned);
                    drop(sequential);
                    equal
                }
                Err(error) => {
                    drop(planned);
                    Err(error)
                }
            },
            Err(error) => Err(error),
        };
        assert_eq!(
            region_tests::live_regions(),
            0,
            "Probe equivalence leaked an owned region: {plan:?}"
        );
        result
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
        let region = region_for_polygons(polygons)?;
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

    /// Exact CPU scanline fill of integer polygons into a rectangle list, and
    /// one `ExtCreateRegion` to turn that list into a region.
    ///
    /// This is the production fast path for dense strictly convex input; the
    /// P0 gate (`.artifacts/eous-region-native-*`) established pixel-exact
    /// equivalence with `chunked_region` and the sequential oracle on real
    /// Eous frames before it was wired in. The general winding fill here also
    /// serves the calibration battery, which re-derives GDI's fill rule from
    /// GDI itself on every test run.
    pub(super) mod scan {
        use super::*;

        /// Fail-fast caps for the prototype; a real frame uses ~35.8k
        /// polygons inside a <= 2560x1600 window.
        pub(crate) const MAX_SCAN_POLYGONS: usize = 1 << 18;
        pub(crate) const MAX_SCAN_POINTS: usize = 1 << 22;
        pub(crate) const MAX_SCAN_SPAN: i32 = 16_384;
        pub(crate) const MAX_SCAN_PIXELS: u64 = 64_000_000;
        pub(crate) const MAX_SCAN_WORK: u64 = 64_000_000;
        pub(crate) const MAX_SCAN_RECTS: usize = 262_144;
        /// RGNDATA budget: 4 MiB of RECT payload plus the 32-byte header.
        ///
        /// This equals `MAX_SCAN_RECTS * 16 + 32`, so inside the scan loop the
        /// rectangle count always rejects first and this bound is never the
        /// one that fires. It is still enforced in [`region_from_scan`],
        /// because that function takes a `Scan` from its caller and is the
        /// point where the buffer is actually sized: the check has to hold for
        /// whatever list it is handed, not just for the one the scan produces.
        pub(crate) const MAX_SCAN_BYTES: usize = MAX_SCAN_RECTS * 16 + 32;

        /// How a scanline crossing maps to pixel indices: where inside a pixel
        /// the sample point sits (top-left corner or center, independently on
        /// each axis), whether a sample exactly on a boundary counts as
        /// inside, and whether every polygon's fill is OR-ed independently.
        /// GDI's own rule is measured by `calibrate_fill_rule`, never assumed.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) struct FillRule {
            /// `0` samples the pixel's left edge, `1` its center, in
            /// half-pixel units.
            pub(super) sample_half: i64,
            /// `0` samples the pixel's top edge, `1` its center, in
            /// half-pixel units.
            pub(super) sample_y_half: i64,
            pub(super) left_inclusive: bool,
            pub(super) right_inclusive: bool,
        }

        /// Every rule the calibration considers over the two sampling axes
        /// and both endpoint conventions; exactly one must reproduce GDI on
        /// the calibration battery.
        #[cfg(test)]
        pub(crate) const CANDIDATE_RULES: [FillRule; 16] = {
            let mut rules = [FillRule {
                sample_half: 0,
                sample_y_half: 0,
                left_inclusive: true,
                right_inclusive: false,
            }; 16];
            let mut index = 0;
            while index < 16 {
                rules[index] = FillRule {
                    sample_half: (index & 1) as i64,
                    sample_y_half: ((index >> 1) & 1) as i64,
                    left_inclusive: index & 4 == 0,
                    right_inclusive: index & 8 == 0,
                };
                index += 1;
            }
            rules
        };

        /// Rule identified by measurement on this machine; see
        /// `gdi_fill_rule_is_calibrated_by_measurement`, which re-derives it
        /// from GDI output every run.
        pub(crate) const MEASURED_RULE: FillRule = FillRule {
            sample_half: 0,
            sample_y_half: 0,
            left_inclusive: true,
            right_inclusive: false,
        };

        pub(crate) struct Scan {
            /// Half-open `[left, top, right, bottom)` rectangles, sorted by
            /// top then left, pairwise disjoint.
            pub(crate) rects: Vec<[i32; 4]>,
            /// Filled pixel count, recorded as gate evidence. Production only
            /// needs `rects`; the probe reads these two to report the resource
            /// envelope actually consumed.
            #[allow(dead_code)]
            pub(crate) pixels: u64,
            /// Scan work consumed against `MAX_SCAN_WORK`, recorded for the
            /// same reason.
            #[allow(dead_code)]
            pub(crate) work: u64,
        }

        /// Why the fast path declined an input. Every variant is a fallback
        /// to the production chunk-41 path, never an error.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum NotApplicable {
            TooFewPolygons,
            NonConvex,
            PolygonTooLarge,
            TooManyPoints,
            TooManyPolygons,
            SpanTooLarge,
            TooManyPixels,
            TooMuchWork,
            TooManyRects,
            TooManyBytes,
        }

        impl NotApplicable {
            #[cfg(test)]
            pub(crate) fn reason(self) -> &'static str {
                match self {
                    Self::TooFewPolygons => "too_few_polygons",
                    Self::NonConvex => "non_convex",
                    Self::PolygonTooLarge => "polygon_too_large",
                    Self::TooManyPoints => "too_many_points",
                    Self::TooManyPolygons => "too_many_polygons",
                    Self::SpanTooLarge => "span_too_large",
                    Self::TooManyPixels => "too_many_pixels",
                    Self::TooMuchWork => "too_much_work",
                    Self::TooManyRects => "too_many_rects",
                    Self::TooManyBytes => "too_many_bytes",
                }
            }
        }

        /// A scan failure split by what the caller must do with it. Budget
        /// overruns decline to the old path; malformed input, overflow and
        /// allocation failure are real errors that must not be hidden as a
        /// fallback.
        #[derive(Clone, Debug)]
        pub(crate) enum ScanError {
            Invalid(String),
            Over(NotApplicable),
        }

        impl ScanError {
            #[cfg(test)]
            pub(crate) fn into_message(self) -> String {
                match self {
                    Self::Invalid(message) => message,
                    Self::Over(decline) => {
                        format!("scanline fast path declined: {}", decline.reason())
                    }
                }
            }
        }

        /// Minimum polygon count for the fast path. The real Eous frame has
        /// ~35.8k polygons; the archived 10.5k c64 bench stays on the old
        /// path, which measured 0.332x the per-polygon fold there.
        pub(crate) const MIN_SCAN_POLYGONS: usize = 16_384;

        /// Largest polygon the fast path accepts. Production hulls of padded,
        /// clipped triangles stay well inside this; the probe records the
        /// actual maximum so the cap is checked against real geometry.
        pub(crate) const MAX_SCAN_POLYGON_POINTS: usize = 32;

        /// True only for a strictly convex, simple polygon: no repeated
        /// vertex, no collinear triple, every turn the same sign, and the
        /// edges winding exactly once around the origin.
        ///
        /// The local-sign test alone is not enough: a pentagram over a convex
        /// pentagon's own vertices turns the same positive way at every
        /// vertex. Turning number separates them without a sort, a hull build
        /// or a rotation search: a simple closed polygon's turning number is
        /// `+1` or `-1` in the direction it is wound, while the pentagram
        /// winds twice (`+2`). Counting how often the edge direction crosses
        /// a fixed ray gives that number in one pass with no trigonometry.
        #[cfg(test)]
        pub(crate) fn is_strictly_convex_simple(polygon: &[[i32; 2]]) -> bool {
            strictly_convex(polygon)
        }

        /// One turn of the edge direction past this many quadrants would be a
        /// second winding, so the count `strictly_convex` accumulates is a
        /// direct read of the turning number. Any ray direction works; `+x` is
        /// the cheapest to test.
        ///
        /// True only for a strictly convex, simple polygon: no repeated
        /// vertex, no collinear triple, all turns the same sign, and exactly
        /// one full turn of the edge direction. That combination is exactly
        /// simplicity plus strict convexity for a closed chain, so it is
        /// strict on hostile input (the pentagram declines on its `+2`) and
        /// complete on real geometry, without allocating or sorting.
        fn strictly_convex(polygon: &[[i32; 2]]) -> bool {
            let count = polygon.len();
            if count < 3 {
                return false;
            }
            // Rolling triple over the cyclic vertex sequence: no modulo (a
            // hardware division) on this per-vertex path.
            let mut previous = polygon[count - 1];
            let mut current = polygon[0];
            let mut sign = 0_i64;
            // Edge directions, kept as the previous one and the current one,
            // so the turning number can be counted while the turns are checked.
            let mut first = (0_i64, 0_i64);
            let mut first_cross = 0_i64;
            let mut edge = (0_i64, 0_i64);
            let mut turns = 0_i32;
            for index in 0..count {
                let next = polygon[if index + 1 == count { 0 } else { index + 1 }];
                let direction = (
                    i64::from(next[0]) - i64::from(current[0]),
                    i64::from(next[1]) - i64::from(current[1]),
                );
                // The turn at this vertex and the ray crossing of the same
                // direction step are the same cross product, so it is formed
                // once and handed to both.
                let cross = if index == 0 {
                    let dx = i64::from(current[0]) - i64::from(previous[0]);
                    let dy = i64::from(current[1]) - i64::from(previous[1]);
                    dx * direction.1 - dy * direction.0
                } else {
                    edge.0 * direction.1 - edge.1 * direction.0
                };
                if cross == 0 {
                    // Collinear or repeated vertices: the fast path's
                    // scanline argument does not cover them.
                    return false;
                }
                let step = if cross > 0 { 1 } else { -1 };
                if sign == 0 {
                    sign = step;
                } else if sign != step {
                    return false;
                }
                if index == 0 {
                    first = direction;
                    first_cross = cross;
                } else {
                    turns += ray_crossings(edge, direction, cross);
                }
                edge = direction;
                previous = current;
                current = next;
            }
            turns += ray_crossings(edge, first, first_cross);
            // A closed chain whose turns are all one sign is simple and convex
            // exactly when its edge direction winds once around the origin
            // (the turning-tangent theorem). A doubly wound chain such as a
            // pentagram winds twice and declines here.
            turns == sign as i32
        }

        /// Winding number of the closed direction curve around the origin,
        /// contributed by the step `from -> to`, whose cross product the
        /// caller already formed. This is the textbook crossing count for a
        /// ray along `+x`: no trigonometry, no division, and the sign
        /// convention is the same as the local turns'.
        #[inline]
        fn ray_crossings(from: (i64, i64), to: (i64, i64), side: i64) -> i32 {
            if from.1 <= 0 {
                if to.1 > 0 && side > 0 { 1 } else { 0 }
            } else if to.1 <= 0 && side < 0 {
                -1
            } else {
                0
            }
        }

        /// [`is_strictly_convex_simple`] without the test-only wrapper. The
        /// scratch vectors the hull builder needed are gone: this is a single
        /// O(n) pass with no allocation, so a scan of tens of thousands of
        /// polygons costs nothing per polygon beyond the walk itself.
        ///
        /// Callers must have checked every coordinate against the GDI range
        /// first (as `applicable` and `validate_polygon` do): the cross
        /// products below are formed in i64 and only stay in range because a
        /// coordinate difference is bounded by 2^27.
        fn strictly_convex_with(polygon: &[[i32; 2]]) -> bool {
            strictly_convex(polygon)
        }

        /// The frame's bounding box, which `applicable` already folded out of
        /// the same walk that proves convexity, handed to the scan so the
        /// production path does not walk 35k polygons a second and third time.
        pub(crate) type Bounds = [i32; 4];

        /// Whether the fast path may take `polygons`, discarding the bounding
        /// box it proves on the way. The scan entry needs that box, so
        /// production calls [`applicable_bounds`] directly and this stays for
        /// the tests that only assert the decision.
        #[cfg(test)]
        pub(crate) fn applicable(polygons: &[Vec<[i32; 2]>]) -> Result<(), NotApplicable> {
            applicable_bounds(polygons).map(|_| ())
        }

        /// Whether the fast path may take `polygons`: enough polygons, every
        /// one a small strictly convex simple polygon, and every resource cap
        /// satisfied at the cheap (pre-allocation) level.
        ///
        /// Returns the frame's `[min_x, min_y, max_x, max_y]`.
        pub(crate) fn applicable_bounds(
            polygons: &[Vec<[i32; 2]>],
        ) -> Result<Bounds, NotApplicable> {
            if polygons.len() < MIN_SCAN_POLYGONS {
                return Err(NotApplicable::TooFewPolygons);
            }
            if polygons.len() > MAX_SCAN_POLYGONS {
                return Err(NotApplicable::TooManyPolygons);
            }
            let mut points = 0_usize;
            let mut min_x = i32::MAX;
            let mut min_y = i32::MAX;
            let mut max_x = i32::MIN;
            let mut max_y = i32::MIN;
            // One pass over the geometry: the coordinate range, the bounding
            // box and the convexity argument are all cheap per point, and the
            // polygon list is 35k separate allocations, so a second walk costs
            // real cache misses.
            //
            // The range check is what makes the cross products safe. Within
            // one polygon two coordinates can differ by 2^32 when hostile i32
            // input is fed straight in, and their product would overflow an
            // i64; once every coordinate is inside the GDI range (2^26 in
            // magnitude) the differences fit in 2^27 and the products in
            // 2^54. It is the same bound `validate_polygon` enforces, so a
            // frame that fails it declines here and still surfaces the
            // validation error from the fallback build.
            for polygon in polygons {
                if polygon.len() > MAX_SCAN_POLYGON_POINTS {
                    return Err(NotApplicable::PolygonTooLarge);
                }
                let mut inside = true;
                for &[x, y] in polygon {
                    if !(GDI_MIN_COORD..=GDI_MAX_COORD).contains(&x)
                        || !(GDI_MIN_COORD..=GDI_MAX_COORD).contains(&y)
                    {
                        inside = false;
                    }
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
                if !inside {
                    return Err(NotApplicable::NonConvex);
                }
                if !strictly_convex_with(polygon) {
                    return Err(NotApplicable::NonConvex);
                }
                points = points.saturating_add(polygon.len());
            }
            if points > MAX_SCAN_POINTS {
                return Err(NotApplicable::TooManyPoints);
            }
            let width = max_x.saturating_sub(min_x);
            let height = max_y.saturating_sub(min_y);
            if width > MAX_SCAN_SPAN || height > MAX_SCAN_SPAN {
                return Err(NotApplicable::SpanTooLarge);
            }
            let area = u64::from(width.max(0) as u32) * u64::from(height.max(0) as u32);
            if area > MAX_SCAN_PIXELS {
                return Err(NotApplicable::TooManyPixels);
            }
            Ok([min_x, min_y, max_x, max_y])
        }

        /// Maps a scan failure onto the fallback reason it represents. A
        /// budget overrun is `NotApplicable` (fall back); a validation or
        /// arithmetic error is a real failure and stays an `Err`.
        fn classify(error: ScanError) -> Result<NotApplicable, String> {
            match error {
                ScanError::Over(decline) => Ok(decline),
                ScanError::Invalid(message) => Err(message),
            }
        }

        /// One region from `polygons` through the fast path, or the reason it
        /// declined. Declining must happen before any region is created, and
        /// only budget overruns may decline; malformed input stays an error.
        pub(crate) fn try_region(
            polygons: &[Vec<[i32; 2]>],
            rule: FillRule,
        ) -> Result<Result<GdiRegion, NotApplicable>, String> {
            let bounds = match applicable_bounds(polygons) {
                Ok(bounds) => bounds,
                Err(decline) => return Ok(Err(decline)),
            };
            // `applicable` already proved every polygon strictly convex and
            // simple, so the scan does not repeat that check.
            let scan = match scan_convex_polygons(polygons, rule, bounds) {
                Ok(scan) => scan,
                Err(error) => return classify(error).map(Err),
            };
            match region_from_scan(&scan) {
                Ok(region) => Ok(Ok(region)),
                Err(error) => classify(error).map(Err),
            }
        }

        fn ceil_div(num: i64, den: i64) -> i64 {
            debug_assert!(den > 0);
            let quotient = num.div_euclid(den);
            if num.rem_euclid(den) == 0 {
                quotient
            } else {
                quotient + 1
            }
        }

        fn floor_div(num: i64, den: i64) -> i64 {
            debug_assert!(den > 0);
            num.div_euclid(den)
        }

        /// First pixel whose sample is not left of `num / den` (inclusive) or
        /// strictly right of it (exclusive). The sample position is given in
        /// half-pixels by [`FillRule::sample_half`].
        fn left_bound(num: i64, den: i64, rule: FillRule) -> i64 {
            // sample = index + sample_half / 2; compare against num / den.
            let numerator = 2 * num - rule.sample_half * den;
            let denominator = 2 * den;
            if rule.left_inclusive {
                ceil_div(numerator, denominator)
            } else {
                floor_div(numerator, denominator) + 1
            }
        }

        /// Exclusive end of the pixel run: samples strictly left of
        /// `num / den` (exclusive) or at-or-left (inclusive).
        fn right_bound(num: i64, den: i64, rule: FillRule) -> i64 {
            let numerator = 2 * num - rule.sample_half * den;
            let denominator = 2 * den;
            if rule.right_inclusive {
                floor_div(numerator, denominator) + 1
            } else {
                ceil_div(numerator, denominator)
            }
        }

        /// One non-horizontal edge of a convex polygon, normalized to rise
        /// from its lower endpoint, with the exact DDA terms already derived.
        struct ConvexEdge {
            low_row: u32,
            high_row: u32,
            low_x: i32,
            quotient: i64,
            slope: i64,
            dy: i64,
        }

        /// Reused edge list for [`fill_convex`]: both y-monotone chains of the
        /// current polygon in one vector, the second starting at `split`.
        /// Cleared and refilled per polygon, so a frame of tens of thousands
        /// of polygons allocates nothing after the first.
        struct ConvexScratch {
            edges: Vec<ConvexEdge>,
            split: usize,
        }

        impl ConvexScratch {
            fn new() -> Result<Self, ScanError> {
                let mut edges: Vec<ConvexEdge> = Vec::new();
                // At most `MAX_SCAN_POLYGON_POINTS` edges per polygon, and a
                // polygon over that cap never reaches the span fill, so the
                // vector is reserved once and never grows afterwards.
                edges
                    .try_reserve_exact(MAX_SCAN_POLYGON_POINTS)
                    .map_err(|_| {
                        ScanError::Invalid("Scanline edge slot allocation failed".to_owned())
                    })?;
                Ok(Self { edges, split: 0 })
            }
        }

        /// Walk state for one monotone chain: which edge is active, the row
        /// its DDA state is valid for, and the exact crossing there.
        struct ConvexCursor {
            index: usize,
            row: u32,
            integer: i64,
            fraction: i64,
        }

        impl ConvexCursor {
            /// Starts on the chain's first edge, whose lower endpoint is the
            /// polygon's lowest row.
            fn new(chain: &[ConvexEdge]) -> Self {
                let mut cursor = Self {
                    index: 0,
                    row: 0,
                    integer: 0,
                    fraction: 0,
                };
                if let Some(edge) = chain.first() {
                    cursor.row = edge.low_row;
                    cursor.integer = i64::from(edge.low_x);
                }
                cursor
            }

            /// Advances to the edge active on `row`, or `None` once the chain
            /// is spent. A monotone chain's next edge starts exactly where the
            /// previous one ended, so the DDA restarts at that row's crossing.
            #[inline]
            fn seek<'a>(&mut self, chain: &'a [ConvexEdge], row: u32) -> Option<&'a ConvexEdge> {
                loop {
                    let edge = chain.get(self.index)?;
                    if edge.high_row > row {
                        while self.row < row {
                            self.step(edge);
                        }
                        return Some(edge);
                    }
                    self.index += 1;
                    if let Some(next) = chain.get(self.index) {
                        self.row = next.low_row;
                        self.integer = i64::from(next.low_x);
                        self.fraction = 0;
                    }
                }
            }

            /// Exact `ceil(low_x + dx*t/dy)`: the rational is
            /// `integer + fraction/dy` with `0 <= fraction < dy`.
            #[inline]
            fn crossing(&self) -> i64 {
                self.integer + i64::from(self.fraction != 0)
            }

            #[inline]
            fn step(&mut self, edge: &ConvexEdge) {
                self.fraction += edge.slope;
                if self.fraction >= edge.dy {
                    self.fraction -= edge.dy;
                    self.integer += edge.quotient + 1;
                } else {
                    self.integer += edge.quotient;
                }
                self.row += 1;
            }
        }

        /// Appends one edge of a monotone chain, returning the rows it covers
        /// as its work charge. Horizontal edges cover no sampled row and are
        /// skipped.
        fn push_convex_edge(
            edges: &mut Vec<ConvexEdge>,
            from: [i32; 2],
            to: [i32; 2],
            min_y: i32,
        ) -> u64 {
            let [x0, y0] = from;
            let [x1, y1] = to;
            if y0 == y1 {
                return 0;
            }
            let (low_y, high_y, low_x, high_x) = if y0 < y1 {
                (y0, y1, x0, x1)
            } else {
                (y1, y0, x1, x0)
            };
            let dy = i64::from(high_y - low_y);
            let dx = i64::from(high_x - low_x);
            // `dx == quotient*dy + slope` with `0 <= slope < dy`, so the
            // integer part advances by `quotient` and the fraction is an
            // exact Bresenham accumulator that stays in `[0, dy)`.
            //
            // Nearly vertical edges — the common case on a dense mesh — fit
            // `|dx| < dy`, where the quotient is 0 or -1 and no division is
            // needed at all. `div_euclid` would also run a second hardware
            // division for the remainder; deriving it from the quotient costs
            // one multiply.
            let (quotient, slope) = if dx >= 0 {
                if dx < dy {
                    (0, dx)
                } else {
                    let quotient = dx / dy;
                    (quotient, dx - quotient * dy)
                }
            } else if -dx < dy {
                (-1, dx + dy)
            } else {
                let quotient = dx.div_euclid(dy);
                (quotient, dx - quotient * dy)
            };
            debug_assert!((0..dy).contains(&slope));
            edges.push(ConvexEdge {
                low_row: (low_y - min_y) as u32,
                high_row: (high_y - min_y) as u32,
                low_x,
                quotient,
                slope,
                dy,
            });
            u64::from((high_y - low_y) as u32)
        }

        /// The frame grid both fills write into: the bitset, its row stride,
        /// the translation from world coordinates into it, and the running
        /// work charge the caller checks against [`MAX_SCAN_WORK`].
        struct Grid<'a> {
            bits: &'a mut [u64],
            words_per_row: usize,
            min_x: i32,
            min_y: i32,
            width: i32,
            /// Conservative row-visit and word-write count. Checked once per
            /// polygon instead of once per row: every term is non-negative,
            /// one polygon contributes at most `32 * 16384` row visits plus
            /// the words its spans touch, and a frame holds at most `2^18`
            /// polygons, so the total cannot approach `u64::MAX` and the
            /// per-polygon granularity still fails closed long before any
            /// large amount of work is done.
            work: u64,
        }

        impl Grid<'_> {
            fn charge(&mut self, units: u64) {
                self.work += units;
            }

            fn exhausted(&self) -> bool {
                self.work > MAX_SCAN_WORK
            }

            /// Sets `[left, right)` for `row`; both bounds are already exact
            /// and are only clamped to the frame here.
            fn paint(&mut self, row: u32, left: i64, right: i64) {
                let left = left.max(0);
                let right = right.min(i64::from(self.width));
                if left >= right {
                    return;
                }
                self.work += ((right - left) / 64 + 2) as u64;
                self.paint_words(row, left as usize, right as usize);
            }

            /// [`Grid::paint`] for the convex span fill, whose crossings are
            /// exact and provably inside `[0, width]` because every vertex is.
            #[inline]
            fn paint_inside(&mut self, row: u32, left: i64, right: i64) {
                if left >= right {
                    return;
                }
                // Charged exactly as `paint` does: an upper bound of one word
                // per `64` pixels plus the two edge words. `right > left` here,
                // so the shift is the same as the division without the sign
                // correction the compiler must emit for a signed denominator.
                self.work += (((right - left) as u64) >> 6) + 2;
                self.paint_words(row, left as usize, right as usize);
            }

            /// ORs the bits of `[left, right)` in `row`. `64` is a power of
            /// two, so the word index and offset are a shift and a mask.
            #[inline]
            fn paint_words(&mut self, row: u32, left: usize, right: usize) {
                let row_base = row as usize * self.words_per_row;
                let mut word = row_base + (left >> 6);
                let mut bit = left & 63;
                let mut remaining = right - left;
                while remaining > 0 {
                    let take = remaining.min(64 - bit);
                    let mask = if take == 64 {
                        u64::MAX
                    } else {
                        ((1_u64 << take) - 1) << bit
                    };
                    self.bits[word] |= mask;
                    remaining -= take;
                    word += 1;
                    bit = 0;
                }
            }
        }

        /// Exact span fill for one strictly convex, simple polygon under the
        /// measured GDI rule. A convex polygon's boundary is two y-monotone
        /// chains from its lowest vertex to its highest, so every sampled row
        /// has exactly one active edge per chain and the span can be painted
        /// in a single pass: no per-row scratch array, no event list, no sort
        /// and no winding accumulation. Each crossing is an exact integer DDA
        /// (`x = x0 + q*t + (s*t)/dy` with `q = dx div dy`, `s = dx rem dy`),
        /// so the only per-row work is an add and a conditional subtract,
        /// never a division or a float.
        fn fill_convex(
            polygon: &[[i32; 2]],
            grid: &mut Grid<'_>,
            scratch: &mut ConvexScratch,
        ) -> Result<(), ScanError> {
            let min_x = grid.min_x;
            let min_y = grid.min_y;
            let count = polygon.len();
            // The lowest and highest vertices, tie-broken by x so each is
            // unique even when the polygon has a horizontal bottom or top
            // edge. Strict convexity makes y unimodal around the boundary, so
            // these two vertices split the ring into the two chains.
            let mut bottom_index = 0;
            let mut top_index = 0;
            for index in 1..count {
                let [x, y] = polygon[index];
                let [low_x, low_y] = polygon[bottom_index];
                if y < low_y || (y == low_y && x < low_x) {
                    bottom_index = index;
                }
                let [high_x, high_y] = polygon[top_index];
                if y > high_y || (y == high_y && x > high_x) {
                    top_index = index;
                }
            }
            let bottom_row = (polygon[bottom_index][1] - min_y) as u32;
            let top_row = (polygon[top_index][1] - min_y) as u32;
            if top_row <= bottom_row {
                // Every vertex on one row: no sampled row, nothing to fill.
                return Ok(());
            }
            scratch.edges.clear();
            let mut work = 0_u64;
            let mut index = bottom_index;
            while index != top_index {
                let next = if index + 1 == count { 0 } else { index + 1 };
                work += push_convex_edge(&mut scratch.edges, polygon[index], polygon[next], min_y);
                index = next;
            }
            scratch.split = scratch.edges.len();
            let mut index = bottom_index;
            while index != top_index {
                let next = if index == 0 { count - 1 } else { index - 1 };
                work += push_convex_edge(&mut scratch.edges, polygon[index], polygon[next], min_y);
                index = next;
            }
            // One charge per polygon rather than one per row keeps the hot
            // row loop down to the accumulator add. A fast-path polygon has
            // at most `MAX_SCAN_POLYGON_POINTS` edges spanning at most
            // `MAX_SCAN_SPAN` rows, so one polygon adds at most ~512k work
            // units; the caller checks the cap after every polygon, which
            // fails closed within far less than the budget.
            grid.charge(work);
            let (left_chain, right_chain) = scratch.edges.split_at(scratch.split);
            let mut left = ConvexCursor::new(left_chain);
            let mut right = ConvexCursor::new(right_chain);
            let shift = i64::from(min_x);
            for row in bottom_row..top_row {
                // Both chains span this whole row range, so the only way to
                // get here without an active edge would be a polygon that is
                // not convex; the fills that reach this point were all proved
                // convex, and stopping is still the honest answer if not.
                let (Some(left_edge), Some(right_edge)) =
                    (left.seek(left_chain, row), right.seek(right_chain, row))
                else {
                    break;
                };
                let left_x = left.crossing() - shift;
                let right_x = right.crossing() - shift;
                // Every vertex is inside the frame box and a sampled crossing
                // lies between two vertices' x values, so `ceil` of it lands
                // in `0..=width` and no clamp is needed here.
                if left_x <= right_x {
                    grid.paint_inside(row, left_x, right_x);
                } else {
                    grid.paint_inside(row, right_x, left_x);
                }
                left.step(left_edge);
                right.step(right_edge);
            }
            Ok(())
        }

        /// Pushes onto a scratch list without ever aborting the process.
        ///
        /// The scan runs inside the per-frame install callback, so a bare
        /// `Vec::push` that fails to allocate calls `handle_alloc_error` and
        /// takes the application down; a scan error instead surfaces as a real
        /// error through [`classify`] and the caller keeps its old region. The
        /// list is only grown when it is already full, and `try_reserve` grows
        /// geometrically, so a real frame — a few hundred rectangles — never
        /// resizes more than a handful of times and never reserves the caps.
        fn push_checked<T>(list: &mut Vec<T>, value: T) -> Result<(), ScanError> {
            if list.len() == list.capacity() {
                list.try_reserve(1).map_err(|_| {
                    ScanError::Invalid("Scanline scratch list allocation failed".to_owned())
                })?;
            }
            list.push(value);
            Ok(())
        }

        /// General exact fill: one event per edge crossing per row, sorted by
        /// row then crossing, then nonzero-winding spans. Used for every
        /// non-convex or self-intersecting input and for every fill rule the
        /// calibration battery still explores.
        fn fill_winding(
            polygon: &[[i32; 2]],
            grid: &mut Grid<'_>,
            rule: FillRule,
            events: &mut Vec<(u32, i64, i64, i32)>,
        ) -> Result<(), ScanError> {
            let min_x = grid.min_x;
            let min_y = grid.min_y;
            events.clear();
            for (index, &[x0, y0]) in polygon.iter().enumerate() {
                let [x1, y1] = polygon[(index + 1) % polygon.len()];
                if y0 == y1 {
                    continue;
                }
                let ascending = y0 < y1;
                let (low_y, high_y, low_x, high_x) = if ascending {
                    (y0, y1, x0, x1)
                } else {
                    (y1, y0, x1, x0)
                };
                let direction = if ascending { 1_i32 } else { -1 };
                let first_row = (low_y - min_y) as u32;
                let end_row = (high_y - min_y) as u32;
                let dy = i64::from(high_y - low_y);
                let denominator = 2 * dy;
                let dx = i64::from(high_x - low_x);
                grid.charge(u64::from(end_row - first_row));
                if grid.exhausted() {
                    return Err(ScanError::Over(NotApplicable::TooMuchWork));
                }
                for row in first_row..end_row {
                    // Scanline y = row + sample_y_half / 2; scanning a
                    // row only when the polygon covers that sample keeps
                    // horizontal edges from crossing.
                    let sample_y = 2 * i64::from(row) + rule.sample_y_half;
                    let rose = sample_y - 2 * i64::from(low_y - min_y);
                    if rose < 0 || rose >= 2 * i64::from(high_y - low_y) {
                        continue;
                    }
                    let numerator = i64::from(low_x - min_x) * denominator + dx * rose;
                    push_checked(events, (row, numerator, denominator, direction))?;
                }
            }
            events.sort_unstable_by(|a, b| {
                a.0.cmp(&b.0).then_with(|| {
                    (i128::from(a.1) * i128::from(b.2)).cmp(&(i128::from(b.1) * i128::from(a.2)))
                })
            });
            let mut index = 0_usize;
            while index < events.len() {
                let row = events[index].0;
                let mut end = index;
                while end < events.len() && events[end].0 == row {
                    end += 1;
                }
                let mut winding = 0_i32;
                let mut previous: Option<(i64, i64)> = None;
                for &(_, numerator, denominator, direction) in &events[index..end] {
                    if winding != 0
                        && let Some((from_num, from_den)) = previous
                    {
                        let left = left_bound(from_num, from_den, rule);
                        let right = right_bound(numerator, denominator, rule);
                        grid.paint(row, left, right);
                    }
                    winding += direction;
                    previous = Some((numerator, denominator));
                }
                index = end;
            }
            Ok(())
        }

        /// Fills the exact nonzero-winding union of `polygons` on the integer
        /// pixel grid. All coordinates are validated and translated into the
        /// bounding box first; translations by whole pixels preserve the
        /// half-integer scanline grid. Test-only entry: production reaches the
        /// same fill through [`scan_convex_polygons`], which skips the
        /// convexity re-check `applicable` already performed.
        #[cfg(test)]
        pub(crate) fn scan_polygons(
            polygons: &[Vec<[i32; 2]>],
            rule: FillRule,
        ) -> Result<Scan, ScanError> {
            scan_polygons_inner(polygons, rule, false, None)
        }

        /// [`scan_convex_polygons`] for the test entry points, which reach the
        /// fill without having run [`applicable_bounds`]: the box is measured
        /// here. Unlike `applicable`, this does not demand the fast path's
        /// production thresholds (polygon count, per-polygon point cap), so a
        /// test can exercise the exact fill on a handful of polygons.
        #[cfg(test)]
        pub(crate) fn scan_convex_measured(
            polygons: &[Vec<[i32; 2]>],
            rule: FillRule,
        ) -> Result<Scan, ScanError> {
            let mut min_x = i32::MAX;
            let mut min_y = i32::MAX;
            let mut max_x = i32::MIN;
            let mut max_y = i32::MIN;
            for polygon in polygons {
                validate_polygon(polygon).map_err(ScanError::Invalid)?;
                for &[x, y] in polygon {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
            scan_convex_polygons(polygons, rule, [min_x, min_y, max_x, max_y])
        }

        /// [`scan_polygons`] for a caller that has already proven every
        /// polygon strictly convex and simple (`applicable`), which is the
        /// production decision path. `bounds` is the box that same walk
        /// produced, so the 35k-polygon list is not walked again to
        /// re-validate and re-measure what was just checked.
        pub(crate) fn scan_convex_polygons(
            polygons: &[Vec<[i32; 2]>],
            rule: FillRule,
            bounds: Bounds,
        ) -> Result<Scan, ScanError> {
            scan_polygons_inner(polygons, rule, true, Some(bounds))
        }

        fn scan_polygons_inner(
            polygons: &[Vec<[i32; 2]>],
            rule: FillRule,
            convex_input: bool,
            proven_bounds: Option<Bounds>,
        ) -> Result<Scan, ScanError> {
            if polygons.len() > MAX_SCAN_POLYGONS {
                return Err(ScanError::Over(NotApplicable::TooManyPolygons));
            }
            let mut points = 0_usize;
            let [min_x, min_y, max_x, max_y] = match proven_bounds {
                // `applicable` validated every coordinate, capped the point
                // count and capped the box, so this walk is only the point
                // total the packing budget still needs.
                Some(bounds) => {
                    for polygon in polygons {
                        points = points.checked_add(polygon.len()).ok_or_else(|| {
                            ScanError::Invalid("Scanline region point count overflowed".to_owned())
                        })?;
                    }
                    bounds
                }
                None => {
                    // Validation and the bounding box in one walk: the test
                    // entry point reaches the fill without `applicable`, so it
                    // has to prove the input itself.
                    let mut min_x = i32::MAX;
                    let mut min_y = i32::MAX;
                    let mut max_x = i32::MIN;
                    let mut max_y = i32::MIN;
                    for polygon in polygons {
                        validate_polygon(polygon).map_err(ScanError::Invalid)?;
                        for &[x, y] in polygon {
                            min_x = min_x.min(x);
                            min_y = min_y.min(y);
                            max_x = max_x.max(x);
                            max_y = max_y.max(y);
                        }
                        points = points.checked_add(polygon.len()).ok_or_else(|| {
                            ScanError::Invalid("Scanline region point count overflowed".to_owned())
                        })?;
                    }
                    [min_x, min_y, max_x, max_y]
                }
            };
            if points > MAX_SCAN_POINTS {
                return Err(ScanError::Over(NotApplicable::TooManyPoints));
            }
            if polygons.is_empty() {
                return Ok(Scan {
                    rects: Vec::new(),
                    pixels: 0,
                    work: 0,
                });
            }
            let width = max_x - min_x;
            let height = max_y - min_y;
            if width > MAX_SCAN_SPAN || height > MAX_SCAN_SPAN {
                return Err(ScanError::Over(NotApplicable::SpanTooLarge));
            }
            if width <= 0 || height <= 0 {
                return Ok(Scan {
                    rects: Vec::new(),
                    pixels: 0,
                    work: 0,
                });
            }
            let area = u64::from(width as u32) * u64::from(height as u32);
            if area > MAX_SCAN_PIXELS {
                return Err(ScanError::Over(NotApplicable::TooManyPixels));
            }
            let words_per_row = (width as usize).div_ceil(64);
            let total_words = words_per_row.checked_mul(height as usize).ok_or_else(|| {
                ScanError::Invalid("Scanline region bitset length overflowed".to_owned())
            })?;
            let mut bits: Vec<u64> = Vec::new();
            bits.try_reserve_exact(total_words).map_err(|_| {
                ScanError::Invalid("Scanline region bitset allocation failed".to_owned())
            })?;
            bits.resize(total_words, 0);

            // The measured GDI rule rounds both ends of a span the same way,
            // so a strictly convex polygon's row span is exactly
            // `[ceil(min crossing), ceil(max crossing))` and the per-polygon
            // event sort — the dominant cost on dense frames — is not needed.
            // Any other rule (only the calibration battery explores those)
            // and any non-convex polygon keep the general winding path.
            let mut scratch = if rule == MEASURED_RULE {
                Some(ConvexScratch::new()?)
            } else {
                None
            };

            // Same fail-closed requirement as the rectangle list: the general
            // winding fill is reached from the install callback too. It is
            // cleared per polygon and holds at most one event per row of that
            // polygon's span, so it grows through the same checked push.
            let mut events: Vec<(u32, i64, i64, i32)> = Vec::new();
            // The fills own the bitset for the duration of this block; the
            // rect extraction below reads it immutably, so the borrow has to
            // end first.
            let work = {
                let mut grid = Grid {
                    bits: &mut bits,
                    words_per_row,
                    min_x,
                    min_y,
                    width,
                    work: 0,
                };
                for polygon in polygons {
                    // The span fill is only valid for a strictly convex, simple
                    // polygon; anything else — including the concave, collinear
                    // and self-intersecting fixtures the calibration and boundary
                    // tests feed straight into this function — keeps the general
                    // winding path. `convex_input` skips only this re-check, and
                    // only because `applicable` proved it for every polygon.
                    let convex = convex_input || strictly_convex_with(polygon);
                    match scratch.as_mut() {
                        Some(scratch) if convex => fill_convex(polygon, &mut grid, scratch)?,
                        _ => fill_winding(polygon, &mut grid, rule, &mut events)?,
                    }
                    // The fills charge once per polygon, so the cap is
                    // enforced here: at most one polygon's worth of work can
                    // run past the budget before this stops the scan.
                    if grid.exhausted() {
                        return Err(ScanError::Over(NotApplicable::TooMuchWork));
                    }
                }
                grid.work
            };

            // The scan runs inside the per-frame install callback, so an
            // allocation failure must fail the install rather than abort the
            // process: a bare `push` would call `handle_alloc_error`. The
            // lists below therefore grow through `push_checked`, which reports
            // failure as an error. No up-front reservation of the caps: they
            // are budgets, not expected sizes, and a real frame fills a few
            // hundred rectangles.
            let mut rects: Vec<[i32; 4]> = Vec::new();
            let mut active: Vec<(u32, u32, usize)> = Vec::new();
            let mut runs: Vec<(u32, u32)> = Vec::new();
            let mut pixels = 0_u64;
            for row in 0..height as u32 {
                runs.clear();
                let row_base = row as usize * words_per_row;
                let mut open: Option<u32> = None;
                for word_index in 0..words_per_row {
                    let base = (word_index * 64) as u32;
                    let word = bits[row_base + word_index];
                    // A run carried from the previous word ends at this
                    // word's start unless this word begins with a set bit.
                    // A run that fills a word's top bit ends exactly at the
                    // boundary as often as it continues into the next word.
                    if word & 1 == 0
                        && let Some(start) = open.take()
                    {
                        push_checked(&mut runs, (start, base))?;
                    }
                    if word == 0 {
                        continue;
                    }
                    let mut bit = 0_u32;
                    loop {
                        let shifted = word >> bit;
                        if shifted == 0 {
                            break;
                        }
                        let zeros = shifted.trailing_zeros();
                        // A carried run keeps its own start; only a fresh run
                        // records the position where these ones begin.
                        open.get_or_insert(base + bit + zeros);
                        let ones = (shifted >> zeros).trailing_ones();
                        let end = bit + zeros + ones;
                        if end == 64 {
                            // The run reaches this word's top bit; whether it
                            // continues is decided by the next word.
                            break;
                        }
                        let start = open.take().expect("run start was just set");
                        push_checked(&mut runs, (start, base + end))?;
                        bit = end;
                    }
                }
                if let Some(start) = open {
                    // Bits at or past `width` are never set, so a run carried
                    // past the last word ends at the row's width.
                    push_checked(&mut runs, (start, width as u32))?;
                }
                for &(left, right) in &runs {
                    pixels += u64::from(right - left);
                }
                // Vertical merge only when the complete run list is
                // identical; a partial overlap must split into new
                // rectangles, never widen or narrow the previous row.
                let identical = active.len() == runs.len()
                    && active
                        .iter()
                        .zip(&runs)
                        .all(|(active, run)| active.0 == run.0 && active.1 == run.1);
                if identical {
                    for &(_, _, rect_index) in &active {
                        rects[rect_index][3] = min_y + row as i32 + 1;
                    }
                } else {
                    active.clear();
                    for &(left, right) in &runs {
                        if rects.len() >= MAX_SCAN_RECTS {
                            return Err(ScanError::Over(NotApplicable::TooManyRects));
                        }
                        let rect_index = rects.len();
                        push_checked(
                            &mut rects,
                            [
                                min_x + left as i32,
                                min_y + row as i32,
                                min_x + right as i32,
                                min_y + row as i32 + 1,
                            ],
                        )?;
                        push_checked(&mut active, (left, right, rect_index))?;
                    }
                }
            }
            Ok(Scan {
                rects,
                pixels,
                work,
            })
        }

        /// One region from a scanned rectangle list, or an empty region when
        /// the fill is empty. The buffer is `Vec<u32>` so the RGNDATA header
        /// and RECT array start on their required four-byte alignment.
        pub(crate) fn region_from_scan(scan: &Scan) -> Result<GdiRegion, ScanError> {
            if scan.rects.is_empty() {
                return empty_region().map_err(ScanError::Invalid);
            }
            let payload = scan.rects.len().checked_mul(16).ok_or_else(|| {
                ScanError::Invalid("Scanline region byte length overflowed".to_owned())
            })?;
            let total = payload.checked_add(32).ok_or_else(|| {
                ScanError::Invalid("Scanline region byte length overflowed".to_owned())
            })?;
            if total > MAX_SCAN_BYTES {
                return Err(ScanError::Over(NotApplicable::TooManyBytes));
            }
            let mut words: Vec<u32> = Vec::new();
            words.try_reserve_exact(total / 4).map_err(|_| {
                ScanError::Invalid("Scanline region buffer allocation failed".to_owned())
            })?;
            let mut bound_left = i32::MAX;
            let mut bound_top = i32::MAX;
            let mut bound_right = i32::MIN;
            let mut bound_bottom = i32::MIN;
            for rect in &scan.rects {
                bound_left = bound_left.min(rect[0]);
                bound_top = bound_top.min(rect[1]);
                bound_right = bound_right.max(rect[2]);
                bound_bottom = bound_bottom.max(rect[3]);
            }
            words.extend_from_slice(&[
                32,
                RDH_RECTANGLES,
                scan.rects.len() as u32,
                payload as u32,
                bound_left as u32,
                bound_top as u32,
                bound_right as u32,
                bound_bottom as u32,
            ]);
            for rect in &scan.rects {
                words.extend_from_slice(&[
                    rect[0] as u32,
                    rect[1] as u32,
                    rect[2] as u32,
                    rect[3] as u32,
                ]);
            }
            #[cfg(test)]
            if region_tests::fail_call(region_tests::Call::ExtRegion) {
                return Err(ScanError::Invalid(
                    "ExtCreateRegion failed to create a GDI region".to_owned(),
                ));
            }
            // SAFETY: `words` holds exactly the RGNDATA header plus one
            // four-word RECT per rectangle, four-byte aligned, and stays alive
            // and unmodified for the call. `ExtCreateRegion` only reads it.
            let region = unsafe {
                ExtCreateRegion(
                    core::ptr::null(),
                    total as u32,
                    words.as_ptr().cast::<c_void>(),
                )
            };
            GdiRegion::from_raw(region, "ExtCreateRegion").map_err(ScanError::Invalid)
        }

        #[cfg(test)]
        pub(crate) fn scanline_region(
            polygons: &[Vec<[i32; 2]>],
            rule: FillRule,
        ) -> Result<GdiRegion, String> {
            let scan = scan_polygons(polygons, rule).map_err(ScanError::into_message)?;
            region_from_scan(&scan).map_err(ScanError::into_message)
        }

        #[cfg(test)]
        pub(crate) fn scanline_matches_chunked(
            polygons: &[Vec<[i32; 2]>],
            rule: FillRule,
        ) -> Result<bool, String> {
            let scanned = scanline_region(polygons, rule)?;
            let chunked = chunked_region(polygons, REGION_CHUNK_POLYGONS)?;
            regions_equal(&scanned, &chunked)
        }

        #[cfg(test)]
        pub(crate) fn scanline_matches_sequential(
            polygons: &[Vec<[i32; 2]>],
            rule: FillRule,
        ) -> Result<bool, String> {
            let scanned = scanline_region(polygons, rule)?;
            let sequential = unified_region_sequential(polygons)?;
            regions_equal(&scanned, &sequential)
        }

        #[cfg(test)]
        pub(crate) fn scanline_contains(
            polygons: &[Vec<[i32; 2]>],
            points: &[[i32; 2]],
            rule: FillRule,
        ) -> Result<Vec<bool>, String> {
            let region = scanline_region(polygons, rule)?;
            Ok(points
                .iter()
                .map(|point| {
                    // SAFETY: `region` is a live handle owned by this call and
                    // `PtInRegion` only reads it.
                    unsafe { PtInRegion(region.handle(), point[0], point[1]) != 0 }
                })
                .collect())
        }

        /// The rule that reproduces GDI exactly on every calibration shape.
        /// Fails if none or more than one candidate does: a tie means the
        /// battery cannot distinguish the rules and must be extended before
        /// the scanline path may be trusted. The failure names each
        /// candidate's first counterexample.
        #[cfg(test)]
        pub(crate) fn calibrate_fill_rule(
            cases: &[Vec<Vec<[i32; 2]>>],
        ) -> Result<FillRule, String> {
            let mut survivors = Vec::new();
            let mut misses = Vec::new();
            for rule in CANDIDATE_RULES {
                let mut first_miss = None;
                for polygons in cases {
                    let gdi = gdi_fill_rects(polygons)?;
                    let scanned = scan_polygons(polygons, rule).map_err(ScanError::into_message)?;
                    let mut rects = scanned.rects;
                    rects.sort_unstable();
                    if rects != gdi {
                        first_miss = Some(format!("shape={polygons:?} gdi={gdi:?} scan={rects:?}"));
                        break;
                    }
                }
                match first_miss {
                    None => survivors.push(rule),
                    Some(miss) => misses.push(format!("{rule:?}: {miss}")),
                }
            }
            match survivors.as_slice() {
                [rule] => Ok(*rule),
                [] => Err(format!(
                    "No scanline fill rule reproduces GDI on the calibration shapes.\n{}",
                    misses.join("\n")
                )),
                _ => Err(format!(
                    "{} fill rules reproduce GDI on the calibration shapes; the battery is ambiguous: {survivors:?}",
                    survivors.len()
                )),
            }
        }

        /// `GetRegionData` round trip: `ExtCreateRegion` must rebuild exactly
        /// the region GDI serialized, including the header layout.
        #[cfg(test)]
        pub(crate) fn probe_region_data_round_trip(
            polygons: &[Vec<[i32; 2]>],
        ) -> Result<bool, String> {
            let original = chunked_region(polygons, REGION_CHUNK_POLYGONS)?;
            let words = region_data_words(&original)?;
            // SAFETY: the buffer starts with the RGNDATA header GDI just
            // wrote; its `written` bytes are initialized and aligned.
            let copy = unsafe {
                ExtCreateRegion(
                    core::ptr::null(),
                    (words.len() * core::mem::size_of::<u32>()) as u32,
                    words.as_ptr().cast::<c_void>(),
                )
            };
            let copy = GdiRegion::from_raw(copy, "ExtCreateRegion")?;
            regions_equal(&original, &copy)
        }

        /// Serializes a live region into its exact `RGNDATA` bytes, in
        /// four-byte words so the header stays aligned. Also asserts the
        /// header size and rectangle count agree with the byte count.
        #[cfg(test)]
        pub(crate) fn region_data_words(region: &GdiRegion) -> Result<Vec<u32>, String> {
            // SAFETY: the region is live; a null buffer asks for the size.
            let bytes = unsafe { GetRegionData(region.handle(), 0, core::ptr::null_mut()) };
            if bytes < 32 {
                return Err(format!(
                    "GetRegionData returned {bytes} bytes, less than the 32-byte header"
                ));
            }
            let mut words: Vec<u32> = Vec::new();
            words
                .try_reserve_exact(bytes.div_ceil(4) as usize)
                .map_err(|_| "GetRegionData buffer allocation failed".to_owned())?;
            words.resize(bytes.div_ceil(4) as usize, 0);
            // SAFETY: the buffer holds at least `bytes` bytes and the region
            // is live; GDI writes at most `bytes` bytes into it.
            let written =
                unsafe { GetRegionData(region.handle(), bytes, words.as_mut_ptr().cast()) };
            if written != bytes {
                return Err(format!(
                    "GetRegionData wrote {written} of {bytes} requested bytes"
                ));
            }
            let header_size = words[0];
            let count = words[2];
            let payload = words[3];
            let expected = count
                .checked_mul(16)
                .ok_or_else(|| "RGNDATA payload size overflowed".to_owned())?;
            if header_size != 32 || payload != expected || bytes != 32 + expected {
                return Err(format!(
                    "RGNDATA header is inconsistent: size={header_size} count={count} \
                     payload={payload} bytes={bytes}"
                ));
            }
            words.truncate(bytes as usize / 4);
            Ok(words)
        }

        /// Decodes `RGNDATA` bytes into half-open rectangles without going
        /// through GDI, so a fixture can compare exact rectangle sets.
        #[cfg(test)]
        pub(crate) fn region_data_rects(words: &[u32]) -> Result<Vec<[i32; 4]>, String> {
            let count = *words
                .get(2)
                .ok_or_else(|| "RGNDATA is shorter than its header".to_owned())?
                as usize;
            let rects = words
                .get(8..)
                .ok_or_else(|| "RGNDATA has no rectangle array".to_owned())?;
            if rects.len() < count * 4 {
                return Err(format!(
                    "RGNDATA has {} rectangle words, fewer than {count} rectangles need",
                    rects.len()
                ));
            }
            Ok((0..count)
                .map(|index| {
                    let rect = &rects[index * 4..index * 4 + 4];
                    [
                        rect[0] as i32,
                        rect[1] as i32,
                        rect[2] as i32,
                        rect[3] as i32,
                    ]
                })
                .collect())
        }

        /// GDI's exact filled rectangles for `polygons`, normalized to
        /// half-open, sorted, pairwise-disjoint rectangles.
        #[cfg(test)]
        pub(crate) fn gdi_fill_rects(polygons: &[Vec<[i32; 2]>]) -> Result<Vec<[i32; 4]>, String> {
            let region = chunked_region(polygons, REGION_CHUNK_POLYGONS)?;
            let words = region_data_words(&region)?;
            let mut rects = region_data_rects(&words)?;
            rects.sort_unstable();
            Ok(rects)
        }

        /// The area of a rectangle list as a pixel set, used to compare the
        /// scanned fill with GDI's rectangles independent of rectangle
        /// decomposition. Both sides must already be disjoint.
        #[cfg(test)]
        pub(crate) fn rect_pixels(rects: &[[i32; 4]]) -> u64 {
            rects
                .iter()
                .map(|rect| {
                    u64::from((rect[2] - rect[0]).max(0) as u32)
                        * u64::from((rect[3] - rect[1]).max(0) as u32)
                })
                .sum()
        }
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
            PolyPoly,
            ExtRegion,
        }

        #[derive(Clone, Copy, Default)]
        struct State {
            live: usize,
            peak: usize,
            calls: [usize; 5],
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

        pub(super) fn call_count(call: Call) -> usize {
            state().calls[call as usize]
        }

        pub(super) fn live_regions() -> usize {
            state().live
        }

        pub(super) fn reset() {
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

        /// Every rectangle GDI actually filled, decoded from its own
        /// `RGNDATA`, so fill-rule calibration never relies on our assumption.
        fn gdi_rects(polygons: &[Vec<[i32; 2]>]) -> Vec<[i32; 4]> {
            super::scan::gdi_fill_rects(polygons).unwrap()
        }

        fn scan_rects(polygons: &[Vec<[i32; 2]>], rule: super::scan::FillRule) -> Vec<[i32; 4]> {
            let scan = super::scan::scan_polygons(polygons, rule).unwrap();
            let mut rects = scan.rects;
            rects.sort_unstable();
            rects
        }

        /// A half-open rectangle converted to the pixel set GDI reports, so
        /// an off-by-one in either direction shows up as a distinct rect.
        fn pixel_set(rects: &[[i32; 4]]) -> std::collections::BTreeSet<(i32, i32)> {
            let mut set = std::collections::BTreeSet::new();
            for rect in rects {
                for y in rect[1]..rect[3] {
                    for x in rect[0]..rect[2] {
                        set.insert((x, y));
                    }
                }
            }
            set
        }

        #[test]
        fn region_data_round_trip_preserves_region() {
            let cases = vec![
                vec![],
                vec![rectangle(0, 0, 10, 10)],
                vec![rectangle(0, 0, 10, 10), rectangle(5, 5, 10, 10)],
                vec![rectangle(-100, -100, 8, 8), rectangle(100, 100, 8, 8)],
                vec![vec![[0, 0], [1, 30], [0, 30]]],
            ];
            for polygons in cases {
                reset();
                assert!(
                    super::scan::probe_region_data_round_trip(&polygons).unwrap(),
                    "GetRegionData -> ExtCreateRegion changed the region for {polygons:?}"
                );
                assert_eq!(state().live, 0);
            }
        }

        /// Shapes chosen to expose the endpoint rules: axis-aligned, both
        /// diagonal directions, slanted edges crossing pixel centers in both
        /// directions, vertices exactly on scanlines, shared edges with mixed
        /// winding, touching shapes with a gap, and negative coordinates.
        fn calibration_cases() -> Vec<Vec<Vec<[i32; 2]>>> {
            vec![
                vec![rectangle(0, 0, 10, 10)],
                vec![rectangle(-4, -3, 7, 5)],
                vec![vec![[0, 0], [10, 0], [10, 10]]],
                vec![vec![[0, 0], [10, 10], [0, 10]]],
                vec![vec![[0, 10], [10, 0], [10, 10]]],
                vec![vec![[0, 0], [11, 6], [3, 11]]],
                vec![vec![[0, 0], [9, 4], [1, 9]]],
                vec![vec![[11, 0], [11, 6], [0, 3]]],
                // A diamond: vertices exactly on scanlines, edges at
                // fractional x crossings.
                vec![vec![[5, 0], [10, 5], [5, 10], [0, 5]]],
                vec![vec![[6, 1], [11, 6], [6, 11], [1, 6]]],
                // Two triangles sharing the edge x = 5 with opposite
                // winding: the union is the whole rectangle.
                vec![
                    vec![[0, 0], [5, 0], [5, 10]],
                    vec![[5, 0], [10, 0], [5, 10]],
                ],
                // Touching squares with a one-pixel gap (ear/leg spacing).
                vec![rectangle(0, 0, 4, 20), rectangle(5, 0, 4, 20)],
                // Overlapping shapes whose union fill must use OR, not XOR.
                vec![rectangle(0, 0, 8, 8), rectangle(4, 4, 8, 8)],
                vec![vec![[0, 0], [30, 7], [3, 30], [17, 13]]],
                // Concave L: the notch must stay empty under nonzero
                // winding, not be closed by a simplified hull.
                vec![vec![[0, 0], [20, 0], [20, 8], [8, 8], [8, 20], [0, 20]]],
                // Self-intersecting star: GDI's winding fill leaves the
                // centre filled; a convex-only assumption would differ.
                vec![vec![
                    [10, 0],
                    [12, 7],
                    [20, 7],
                    [14, 12],
                    [16, 20],
                    [10, 15],
                    [4, 20],
                    [6, 12],
                    [0, 7],
                    [8, 7],
                ]],
            ]
        }

        #[test]
        fn gdi_fill_rule_is_calibrated_by_measurement() {
            reset();
            let rule = super::scan::calibrate_fill_rule(&calibration_cases()).unwrap();
            // Pin the measured rule so a future GDI/Windows change surfaces
            // as an explicit failure instead of silent pixel drift.
            assert_eq!(
                rule,
                super::scan::MEASURED_RULE,
                "GDI's measured fill rule changed"
            );
            for polygons in calibration_cases() {
                let gdi = gdi_rects(&polygons);
                let scanned = scan_rects(&polygons, rule);
                assert_eq!(gdi, scanned, "rectangles differ for {polygons:?}");
            }
            assert_eq!(state().live, 0);
        }

        #[test]
        fn gdi_fill_uses_union_not_xor() {
            reset();
            let rule = super::scan::calibrate_fill_rule(&calibration_cases()).unwrap();
            let overlapping = vec![rectangle(0, 0, 8, 8), rectangle(4, 4, 8, 8)];
            let gdi = gdi_rects(&overlapping);
            let fill = pixel_set(&gdi);
            assert!(fill.contains(&(5, 5)), "overlap must stay filled");
            assert_eq!(fill.len(), 112, "union area must be 112 pixels");
            let scan = super::scan::scan_polygons(&overlapping, rule).unwrap();
            assert_eq!(pixel_set(&scan.rects), fill);
            assert_eq!(
                scan.pixels,
                super::scan::rect_pixels(&gdi),
                "the scanned pixel count must match GDI's filled area"
            );
            assert_eq!(state().live, 0);
        }

        #[test]
        fn convex_span_fill_matches_gdi_on_convex_polygons() {
            reset();
            let rule = super::scan::calibrate_fill_rule(&calibration_cases()).unwrap();
            // Every strictly convex case in the calibration battery, plus
            // slanted edges whose crossings are never integral and a shape
            // whose vertices sit exactly on scanlines.
            let mut cases: Vec<Vec<Vec<[i32; 2]>>> = vec![
                vec![rectangle(0, 0, 10, 10)],
                vec![rectangle(-4, -3, 7, 5)],
                vec![vec![[0, 0], [10, 0], [10, 10]]],
                vec![vec![[0, 0], [10, 10], [0, 10]]],
                vec![vec![[0, 10], [10, 0], [10, 10]]],
                vec![vec![[0, 0], [11, 6], [3, 11]]],
                vec![vec![[0, 0], [9, 4], [1, 9]]],
                vec![vec![[11, 0], [11, 6], [0, 3]]],
                vec![vec![[5, 0], [10, 5], [5, 10], [0, 5]]],
                vec![vec![[6, 1], [11, 6], [6, 11], [1, 6]]],
                vec![vec![[0, 0], [30, 7], [3, 30], [17, 13]]],
                // A one-pixel-wide sliver and a nearly horizontal ramp.
                vec![vec![[0, 0], [1, 0], [1, 40], [0, 40]]],
                vec![vec![[0, 0], [41, 1], [41, 2], [0, 2]]],
                // Overlap: each polygon is convex, the union must be OR.
                vec![rectangle(0, 0, 8, 8), rectangle(4, 4, 8, 8)],
                vec![
                    vec![[0, 0], [40, 0], [0, 40]],
                    vec![[20, 20], [60, 20], [20, 60]],
                ],
            ];
            // Fixed-seed convex polygons, including negative coordinates.
            let mut seed = 0x2545_F491_4F6C_DD1D_u64;
            fn next(seed: &mut u64, limit: i32) -> i32 {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                (*seed % limit as u64) as i32 - limit / 4
            }
            for _ in 0..24 {
                let mut polygons = Vec::new();
                for _ in 0..4 {
                    let x = next(&mut seed, 60);
                    let y = next(&mut seed, 50);
                    let w = 1 + next(&mut seed, 30).abs();
                    let h = 1 + next(&mut seed, 25).abs();
                    polygons.push(if seed & 1 == 0 {
                        vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
                    } else {
                        vec![[x, y], [x + w, y], [x + w / 2, y + h]]
                    });
                }
                cases.push(polygons);
            }
            for polygons in cases {
                let gdi = gdi_rects(&polygons);
                let span = super::scan::scan_convex_measured(&polygons, rule).unwrap();
                let mut span_rects = span.rects;
                span_rects.sort_unstable();
                assert_eq!(gdi, span_rects, "convex span fill differs for {polygons:?}");
                assert_eq!(state().live, 0);
            }
        }

        #[test]
        fn convex_span_and_winding_paths_are_interchangeable() {
            reset();
            let rule = super::scan::calibrate_fill_rule(&calibration_cases()).unwrap();
            // The convex path must match the general path exactly on every
            // input both can accept, so which one a polygon takes can never
            // change the region.
            let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
            fn next(seed: &mut u64, limit: i32) -> i32 {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                (*seed % limit as u64) as i32 - limit / 5
            }
            let mut cases: Vec<Vec<Vec<[i32; 2]>>> = calibration_cases()
                .into_iter()
                .filter(|polygons| {
                    polygons
                        .iter()
                        .all(|p| super::scan::is_strictly_convex_simple(p))
                })
                .collect();
            for _ in 0..24 {
                let mut polygons = Vec::new();
                for _ in 0..5 {
                    let x = next(&mut seed, 200);
                    let y = next(&mut seed, 160);
                    let w = 1 + next(&mut seed, 70).abs();
                    let h = 1 + next(&mut seed, 60).abs();
                    polygons.push(if seed & 1 == 0 {
                        vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
                    } else {
                        vec![[x, y], [x + w, y], [x + w / 2, y + h]]
                    });
                }
                cases.push(polygons);
            }
            for polygons in cases {
                let convex = super::scan::scan_convex_measured(&polygons, rule).unwrap();
                let general = super::scan::scan_polygons(&polygons, rule).unwrap();
                assert_eq!(
                    convex.rects, general.rects,
                    "convex and winding fills differ for {polygons:?}"
                );
                assert_eq!(convex.pixels, general.pixels);
            }
            assert_eq!(state().live, 0);
        }

        #[test]
        fn single_pixel_difference_is_detected() {
            reset();
            let rule = super::scan::calibrate_fill_rule(&calibration_cases()).unwrap();
            let polygons = vec![rectangle(0, 0, 6, 6)];
            let mut scan = super::scan::scan_polygons(&polygons, rule).unwrap();
            scan.rects[0][2] -= 1;
            let scanned = super::scan::region_from_scan(&scan).unwrap();
            let gdi = chunked_region(&polygons, REGION_CHUNK_POLYGONS).unwrap();
            assert!(
                !regions_equal(&scanned, &gdi).unwrap(),
                "one missing pixel must fail the set comparison"
            );
            drop(scanned);
            drop(gdi);
            assert_eq!(state().live, 0);
        }

        #[test]
        fn scanline_matches_gdi_on_boundary_and_random_polygons() {
            reset();
            let rule = super::scan::calibrate_fill_rule(&calibration_cases()).unwrap();
            let mut cases: Vec<Vec<Vec<[i32; 2]>>> = calibration_cases();
            // Deterministic pseudo-random convex quads and triangles over a
            // range that includes negative coordinates and slanted edges.
            let mut seed = 0x2545_F491_4F6C_DD1D_u64;
            fn next(seed: &mut u64, limit: i32) -> i32 {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                (*seed % limit as u64) as i32 - limit / 4
            }
            for _ in 0..24 {
                let mut polygons = Vec::new();
                for _ in 0..6 {
                    let x = next(&mut seed, 90);
                    let y = next(&mut seed, 70);
                    let w = 1 + next(&mut seed, 40).abs();
                    let h = 1 + next(&mut seed, 30).abs();
                    if seed & 1 == 0 {
                        polygons.push(vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]]);
                    } else {
                        polygons.push(vec![[x, y], [x + w, y], [x + w / 2, y + h]]);
                    }
                }
                cases.push(polygons);
            }
            for polygons in cases {
                assert!(
                    super::scan::scanline_matches_chunked(&polygons, rule).unwrap(),
                    "scanline differs from chunk-41 for {polygons:?}"
                );
                assert!(
                    super::scan::scanline_matches_sequential(&polygons, rule).unwrap(),
                    "scanline differs from the sequential oracle for {polygons:?}"
                );
                assert_eq!(state().live, 0);
            }
        }

        #[test]
        fn scanline_region_contains_matches_gdi_points() {
            reset();
            let rule = super::scan::calibrate_fill_rule(&calibration_cases()).unwrap();
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
                rectangle(50, 50, 10, 10),
            ];
            let points: Vec<[i32; 2]> = (0..60)
                .step_by(3)
                .flat_map(|x| (0..60).step_by(7).map(move |y| [x, y]))
                .collect();
            let scanned = super::scan::scanline_contains(&polygons, &points, rule).unwrap();
            let gdi = hit_region_contains(&polygons, &points).unwrap();
            assert_eq!(scanned, gdi);
            assert_eq!(state().live, 0);
        }

        #[test]
        fn scanline_rejects_invalid_and_over_budget_input() {
            reset();
            let rule = super::scan::MEASURED_RULE;
            // Malformed input is a real error, never a silent fallback.
            assert!(matches!(
                super::scan::scan_polygons(&[vec![[0, 0], [1, 1]]], rule),
                Err(super::scan::ScanError::Invalid(_))
            ));
            assert!(matches!(
                super::scan::scan_polygons(&[vec![[0, 0], [1 << 26, 0], [0, 10]]], rule),
                Err(super::scan::ScanError::Invalid(_))
            ));
            // Budget overruns decline to the old path.
            let too_wide = vec![vec![[0, 0], [super::scan::MAX_SCAN_SPAN + 1, 0], [0, 1]]];
            assert!(matches!(
                super::scan::scan_polygons(&too_wide, rule),
                Err(super::scan::ScanError::Over(
                    super::scan::NotApplicable::SpanTooLarge
                ))
            ));
            // A triangle past the span cap declines before any allocation.
            let too_tall = vec![vec![[0, 0], [100_000, 0], [0, 16_384]]];
            assert!(matches!(
                super::scan::scan_polygons(&too_tall, rule),
                Err(super::scan::ScanError::Over(
                    super::scan::NotApplicable::SpanTooLarge
                ))
            ));
            // Many tall, thin triangles inside the span and area caps still
            // exceed the total-work cap; the scan must stop instead of
            // walking every edge-row again.
            let heavy: Vec<Vec<[i32; 2]>> = (0..2_000)
                .map(|_| vec![[0, 0], [3_000, 0], [0, 16_384]])
                .collect();
            assert!(matches!(
                super::scan::scan_polygons(&heavy, rule),
                Err(super::scan::ScanError::Over(
                    super::scan::NotApplicable::TooMuchWork
                ))
            ));
            assert_eq!(state().live, 0);
        }

        /// The byte cap has to hold for whatever list `region_from_scan` is
        /// handed, not only for the one the scan produces: the scan's
        /// rectangle cap rejects first at exactly the same byte count, so
        /// without this the `TooManyBytes` branch could never be reached.
        #[test]
        fn region_from_scan_rejects_a_list_over_the_byte_budget() {
            reset();
            let over = super::scan::Scan {
                rects: vec![[0, 0, 1, 1]; super::scan::MAX_SCAN_RECTS + 1],
                pixels: 0,
                work: 0,
            };
            assert!(matches!(
                super::scan::region_from_scan(&over),
                Err(super::scan::ScanError::Over(
                    super::scan::NotApplicable::TooManyBytes
                ))
            ));
            drop(over);
            // Exactly at the cap must still build: the limit is a ceiling, not
            // an off-by-one exclusion.
            let at_cap = super::scan::Scan {
                rects: vec![[0, 0, 1, 1]; super::scan::MAX_SCAN_RECTS],
                pixels: 0,
                work: 0,
            };
            drop(super::scan::region_from_scan(&at_cap).unwrap());
            assert_eq!(state().live, 0);
        }

        #[test]
        fn applicability_requires_simple_convex_polygons() {
            reset();
            use super::scan::{NotApplicable, applicable, is_strictly_convex_simple};
            let square = vec![[0, 0], [10, 0], [10, 10], [0, 10]];
            let triangle = vec![[0, 0], [10, 0], [0, 10]];
            assert!(is_strictly_convex_simple(&square));
            assert!(is_strictly_convex_simple(&triangle));
            // Reversing the order keeps it simple and convex.
            let mut reversed = square.clone();
            reversed.reverse();
            assert!(is_strictly_convex_simple(&reversed));

            // Regular {5/2} and {7/3} star polygons over a radius-100 circle.
            // Every turn of both is the same sign, so the local-sign test
            // accepts them; only the turning number sees the difference. The
            // coordinates are exact and no triple is collinear, which was
            // checked before they were committed here.
            const PENTAGRAM: [[i32; 2]; 5] = [[0, 100], [59, -81], [-95, 31], [95, 31], [-59, -81]];
            const HEPTAGRAM: [[i32; 2]; 7] = [
                [0, 100],
                [43, -90],
                [-78, 62],
                [97, -22],
                [-97, -22],
                [78, 62],
                [-43, -90],
            ];

            // Concave, self-intersecting, and a star over a convex vertex set:
            // all decline, none is partially scanned.
            for polygon in [
                vec![[0, 0], [20, 0], [20, 8], [8, 8], [8, 20], [0, 20]],
                vec![[0, 0], [10, 10], [10, 0], [0, 10]],
                PENTAGRAM.to_vec(),
                // Collinear and repeated vertices are outside the fast
                // path's argument even though GDI fills them.
                vec![[0, 0], [1, 1], [2, 2]],
                vec![[0, 0], [0, 0], [0, 0]],
            ] {
                assert!(
                    !is_strictly_convex_simple(&polygon),
                    "{polygon:?} must not pass the simple-convex gate"
                );
            }

            // The turning number is what separates a pentagram from a
            // pentagon, so it has to reject in both winding directions: the
            // same star walked backwards winds twice the other way. A star
            // wound three times is the same trap at a different winding
            // number, so a pass that only rejected "two turns" would still be
            // wrong.
            assert!(!is_strictly_convex_simple(&PENTAGRAM));
            let backward: Vec<[i32; 2]> = PENTAGRAM.iter().rev().copied().collect();
            assert!(!is_strictly_convex_simple(&backward));
            assert!(!is_strictly_convex_simple(&HEPTAGRAM));
            let backward: Vec<[i32; 2]> = HEPTAGRAM.iter().rev().copied().collect();
            assert!(!is_strictly_convex_simple(&backward));

            // A simple non-convex polygon whose every turn is one sign cannot
            // exist, but a chain that only *locally* looks convex can: the
            // same vertex set in star order has all turns equal and a winding
            // of two, which is exactly the case a local-sign test misses.
            let star_of_squares = vec![
                [0, 0],
                [9, 0],
                [9, 9],
                [0, 9],
                [3, 3],
                [6, 3],
                [6, 6],
                [3, 6],
            ];
            assert!(!is_strictly_convex_simple(&star_of_squares));

            let mut frame = vec![square.clone(); super::scan::MIN_SCAN_POLYGONS];
            assert_eq!(applicable(&frame), Ok(()));
            frame.pop();
            assert_eq!(applicable(&frame), Err(NotApplicable::TooFewPolygons));
            frame.push(vec![[0, 0], [20, 0], [20, 8], [8, 8], [8, 20], [0, 20]]);
            assert_eq!(applicable(&frame), Err(NotApplicable::NonConvex));
            assert_eq!(state().live, 0);
        }

        #[test]
        fn scanline_ext_create_region_failure_is_fail_closed() {
            reset();
            let polygons = vec![rectangle(0, 0, 10, 10)];
            let _guard = FailureGuard::new(Call::ExtRegion, 1);
            assert!(super::scan::scanline_region(&polygons, super::scan::MEASURED_RULE).is_err());
            drop(_guard);
            assert_eq!(state().live, 0);
        }

        /// Repeated builds must leave Windows' own GDI object count where it
        /// started. The self-maintained counter proves every owned region was
        /// released; this reads `GetGuiResources` so a missed `DeleteObject`
        /// cannot be masked by a counter that was never incremented.
        ///
        /// Process-wide, so it must run alone: other tests in this binary
        /// create and drop regions concurrently and would move the count
        /// between the baseline and the checks. `ignored` by default for
        /// that reason; the resource gate runs it as its own process.
        #[test]
        #[ignore = "process-wide GDI counter; run alone with --test-threads=1"]
        fn repeated_builds_do_not_grow_the_process_gdi_object_count() {
            reset();
            let polygons = dense_convex_grid(super::scan::MIN_SCAN_POLYGONS);
            // Warm-up: the first call initializes lazily-allocated GDI state
            // that would otherwise look like a leak on the first round.
            drop(super::scan::scanline_region(&polygons, super::scan::MEASURED_RULE).unwrap());
            drop(super::scan::scanline_region(&polygons, super::scan::MEASURED_RULE).unwrap());
            let baseline = super::gdi_object_count()
                .unwrap_or_else(|error| panic!("GetGuiResources failed with Win32 error {error}"));
            // A whole process may legitimately own zero GDI objects, so a
            // non-zero baseline is not the control. The control is that a
            // live region is visible: hold one and require the count to rise.
            // Without this the leak loop could pass on an API that reports
            // nothing at all for this process.
            let held = super::scan::scanline_region(&polygons, super::scan::MEASURED_RULE).unwrap();
            let while_held = super::gdi_object_count()
                .unwrap_or_else(|error| panic!("GetGuiResources failed with Win32 error {error}"));
            assert!(
                while_held > baseline,
                "a live region must be visible to GetGuiResources: baseline {baseline}, held {while_held}"
            );
            drop(held);
            let after_held = super::gdi_object_count()
                .unwrap_or_else(|error| panic!("GetGuiResources failed with Win32 error {error}"));
            assert_eq!(
                after_held, baseline,
                "dropping the live region must return the count to baseline"
            );
            for round in 0..240 {
                drop(super::scan::scanline_region(&polygons, super::scan::MEASURED_RULE).unwrap());
                let now = super::gdi_object_count().unwrap_or_else(|error| {
                    panic!("GetGuiResources failed with Win32 error {error} on round {round}")
                });
                assert_eq!(
                    now, baseline,
                    "GDI objects grew on round {round}: {baseline} -> {now}"
                );
            }
            assert_eq!(state().live, 0);
        }

        /// The old batched path has the same obligation: it creates and
        /// combines many regions per frame, so a leak there would be far
        /// larger than in the single-region fast path.
        ///
        /// Process-wide like the fast-path check, so it also runs alone.
        #[test]
        #[ignore = "process-wide GDI counter; run alone with --test-threads=1"]
        fn repeated_chunked_builds_do_not_grow_the_process_gdi_object_count() {
            reset();
            let polygons = dense_convex_grid(super::scan::MIN_SCAN_POLYGONS);
            drop(chunked_region(&polygons, REGION_CHUNK_POLYGONS).unwrap());
            drop(chunked_region(&polygons, REGION_CHUNK_POLYGONS).unwrap());
            let baseline = super::gdi_object_count()
                .unwrap_or_else(|error| panic!("GetGuiResources failed with Win32 error {error}"));
            let held = chunked_region(&polygons, REGION_CHUNK_POLYGONS).unwrap();
            let while_held = super::gdi_object_count()
                .unwrap_or_else(|error| panic!("GetGuiResources failed with Win32 error {error}"));
            assert!(
                while_held > baseline,
                "a live region must be visible to GetGuiResources: baseline {baseline}, held {while_held}"
            );
            drop(held);
            for round in 0..8 {
                drop(chunked_region(&polygons, REGION_CHUNK_POLYGONS).unwrap());
                let now = super::gdi_object_count().unwrap_or_else(|error| {
                    panic!("GetGuiResources failed with Win32 error {error} on round {round}")
                });
                assert_eq!(
                    now, baseline,
                    "GDI objects grew on chunked round {round}: {baseline} -> {now}"
                );
            }
            assert_eq!(state().live, 0);
        }

        /// A grid of one-pixel squares: strictly convex, simple and above the
        /// fast path's density floor, so the dispatcher must take the
        /// scanline arm.
        fn dense_convex_grid(count: usize) -> Vec<Vec<[i32; 2]>> {
            (0..count)
                .map(|index| {
                    let x = (index % 256) as i32 * 3;
                    let y = (index / 256) as i32 * 3;
                    rectangle(x, y, 1, 1)
                })
                .collect()
        }

        #[test]
        fn dense_convex_frames_take_the_scanline_path() {
            reset();
            let polygons = dense_convex_grid(super::scan::MIN_SCAN_POLYGONS);
            let region = region_for_polygons(&polygons).unwrap();
            drop(region);
            assert_eq!(
                call_count(Call::PolyPoly),
                0,
                "dense convex input must not build GDI batches"
            );
            assert_eq!(
                call_count(Call::ExtRegion),
                1,
                "the fast path must build the region with exactly one ExtCreateRegion"
            );
            assert_eq!(state().live, 0);
        }

        #[test]
        fn dense_non_convex_frames_fall_back_to_batches() {
            reset();
            let mut polygons = dense_convex_grid(super::scan::MIN_SCAN_POLYGONS);
            polygons.push(vec![[0, 0], [20, 0], [20, 8], [8, 8], [8, 20], [0, 20]]);
            let region = region_for_polygons(&polygons).unwrap();
            drop(region);
            assert!(
                call_count(Call::PolyPoly) > 0,
                "a frame with one concave polygon must use the batched build"
            );
            assert_eq!(
                call_count(Call::ExtRegion),
                0,
                "the fast path must decline the whole frame, never scan a subset"
            );
            assert_eq!(state().live, 0);
        }

        #[test]
        fn dense_frame_over_a_budget_cap_falls_back_without_erroring() {
            reset();
            let mut polygons = dense_convex_grid(super::scan::MIN_SCAN_POLYGONS);
            // One polygon wider than the fast path's span cap: a decline, not
            // a failure, so production still installs a region.
            polygons.push(vec![[0, 0], [16_385, 0], [16_385, 1], [0, 1]]);
            let region = region_for_polygons(&polygons).unwrap();
            drop(region);
            assert!(
                call_count(Call::PolyPoly) > 0,
                "a budget overrun must fall back to the batched build"
            );
            assert_eq!(state().live, 0);
        }

        #[test]
        fn fast_path_errors_are_not_hidden_as_a_fallback() {
            reset();
            let polygons = dense_convex_grid(super::scan::MIN_SCAN_POLYGONS);
            let _guard = FailureGuard::new(Call::ExtRegion, 1);
            assert!(
                region_for_polygons(&polygons).is_err(),
                "an ExtCreateRegion failure must surface, not fall back to batches"
            );
            assert_eq!(
                call_count(Call::PolyPoly),
                0,
                "a real error must not be retried through the old path"
            );
            assert_eq!(state().live, 0);
        }

        #[test]
        fn production_union_matches_sequential_oracle() {
            // Exercises the production dispatcher through the same hook the
            // app tests use. Every case here is low density or non-convex, so
            // the dispatcher must take the chunk-41 path; the dense convex
            // arm is covered by `dense_convex_frames_take_the_scanline_path`
            // and by the app-level Eous gate probe.
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
                // Both GDI coordinate range corners, one corner per case: a
                // single multi-polygon batch rejects the full-range y-spread
                // of mixing both corners (measured, fail-closed), so each
                // pair keeps its batch-local spread small while still
                // covering the extreme coordinates.
                vec![
                    rectangle(GDI_MIN_COORD, GDI_MIN_COORD, 16, 16),
                    rectangle(GDI_MIN_COORD + 32, GDI_MIN_COORD + 32, 16, 16),
                ],
                vec![
                    rectangle(GDI_MAX_COORD - 16, GDI_MAX_COORD - 16, 16, 16),
                    rectangle(GDI_MAX_COORD - 32, GDI_MAX_COORD - 32, 16, 16),
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
        fn chunked_union_matches_sequential_oracle() {
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
            let mut mixed_winding = rectangle(5, 5, 10, 10);
            mixed_winding.reverse();
            let cases = vec![
                vec![],
                vec![square.clone()],
                vec![square.clone(), square.clone(), rectangle(5, 5, 10, 10)],
                vec![concave, rectangle(50, 50, 10, 10)],
                vec![
                    vec![[0, 0], [1, 30], [0, 30]],
                    vec![[1, 0], [2, 30], [1, 30]],
                ],
                vec![vec![[0, 0], [1, 1], [2, 2]], vec![[0, 0], [0, 0], [0, 0]]],
                vec![square.clone(), mixed_winding],
                // Core multi-batch shapes: 7 polygons over chunks < 41 spans
                // several batches in every rotation, with two overlapping
                // neighbours crossing batch boundaries in the base order.
                (0..7)
                    .map(|index| rectangle(index * 4, (index % 3) * 4, 10, 10))
                    .collect(),
            ];
            for chunk in [1_usize, 2, 3, 5, 10_512] {
                for mut polygons in cases.clone() {
                    reset();
                    let balanced = chunked_region(&polygons, chunk).unwrap();
                    let sequential = unified_region_sequential(&polygons).unwrap();
                    assert!(
                        regions_equal(&balanced, &sequential).unwrap(),
                        "chunk={chunk} raw arrangement differs"
                    );
                    drop(balanced);
                    drop(sequential);
                    assert_eq!(state().live, 0);
                    if !polygons.is_empty() {
                        polygons.rotate_left(1);
                        assert!(
                            probe_region_equivalence(
                                &polygons,
                                super::super::ProbedPlan::Chunked(chunk)
                            )
                            .unwrap(),
                            "chunk={chunk} rotated arrangement differs"
                        );
                    }
                    polygons.reverse();
                    for polygon in &mut polygons {
                        polygon.reverse();
                    }
                    assert!(
                        probe_region_equivalence(
                            &polygons,
                            super::super::ProbedPlan::Chunked(chunk)
                        )
                        .unwrap(),
                        "chunk={chunk} reversed arrangement differs"
                    );
                    assert_eq!(state().live, 0);
                }
            }
        }

        #[test]
        fn chunked_union_has_bounded_peak_and_exact_stress_equivalence() {
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
            let chunk = REGION_CHUNK_POLYGONS;
            for count in [0_usize, 1, 40, 41, 42, 81, 82, 83, 10_512] {
                reset();
                let region = chunked_region(&polygons[..count], chunk).unwrap();
                let stats = state();
                let batches = count.div_ceil(chunk);
                let bound = if batches < 2 {
                    1
                } else {
                    (usize::BITS - batches.leading_zeros()) as usize + 1
                };
                assert_eq!(stats.live, 1);
                assert!(
                    stats.peak <= bound,
                    "N={count}: peak {} > {bound}",
                    stats.peak
                );
                assert_eq!(stats.calls[Call::Polygon as usize], 0);
                assert_eq!(stats.calls[Call::PolyPoly as usize], batches);
                assert_eq!(stats.calls[Call::Union as usize], batches.saturating_sub(1));
                assert_eq!(
                    stats.calls[Call::Empty as usize],
                    if count == 0 { 1 } else { batches - 1 }
                );
                drop(region);
                assert_eq!(state().live, 0);
            }
            assert!(hit_region_matches_sequential_oracle(&polygons).unwrap());
            assert_eq!(state().live, 0);
        }

        #[test]
        fn chunked_rejects_invalid_polygon_without_leaking() {
            // The invalid polygon sits in the third batch: the first two
            // batches are built, the third is rejected during validation
            // before any native call, and nothing stays owned.
            let mut polygons: Vec<_> = (0..100).map(|x| rectangle(x * 12, 0, 8, 8)).collect();
            polygons[82] = vec![[0, 0], [1, 1]];
            reset();
            assert!(chunked_region(&polygons, 41).is_err());
            let stats = state();
            assert_eq!(stats.calls[Call::Polygon as usize], 0);
            assert_eq!(stats.calls[Call::PolyPoly as usize], 2);
            // The first two batches merged before the third was rejected.
            assert_eq!(stats.calls[Call::Union as usize], 1);
            assert_eq!(stats.live, 0);
        }

        #[test]
        fn chunked_injected_native_failures_release_all_owned_regions() {
            // 9 polygons in 3 batches of 3: each batch folds immediately, so
            // the failures land on PolyPoly calls 1..=3, Union calls 1..=2
            // and the Empty destination allocations inside those unions.
            let polygons: Vec<_> = (0..9).map(|x| rectangle(x * 12, 0, 8, 8)).collect();
            for (call, ordinals) in [
                (Call::PolyPoly, &[1, 2, 3][..]),
                (Call::Empty, &[1, 2][..]),
                (Call::Union, &[1, 2][..]),
            ] {
                for &ordinal in ordinals {
                    {
                        let _failure = FailureGuard::new(call, ordinal);
                        assert!(chunked_region(&polygons, 3).is_err(), "{call:?} #{ordinal}");
                        assert_eq!(state().calls[call as usize], ordinal);
                        assert_eq!(state().live, 0, "{call:?} #{ordinal}");
                    }
                    assert!(chunked_region(&polygons, 3).is_ok());
                    assert_eq!(state().live, 0);
                }
            }
            let _failure = FailureGuard::new(Call::Empty, 1);
            assert!(chunked_region(&[], 3).is_err());
            assert_eq!(state().live, 0);
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

        /// Phase-0 decision-gate probe for the batched (single-call) region
        /// install, on the synthetic clustered fixture.
        ///
        /// Run explicitly, in release, with output:
        /// `cargo test --release --locked --offline --lib platform -- --ignored --nocapture`
        ///
        /// Outcome (2026-10-15, evidence `.artifacts/perf-batch-20261015-230238/`):
        /// the whole-frame single `CreatePolyPolygonRgn` is rejected — 11.5x
        /// slower than the balanced fold at 10512 polygons and superlinear in
        /// the batch's polygon count — while one batch per instance
        /// (~164 polygons) measured 0.60x of the production-ordered balanced
        /// fold with exact equivalence. Boundaries observed: multi-polygon
        /// batches reject a y-spread >= 2^24 (fail-closed), and ~10512
        /// simultaneously live regions exceed the process GDI handle quota.
        /// Follow-up (2026-10-16, `region_batch_gate_probe` in the app tests,
        /// evidence `.artifacts/perf-perbatch-20261016-015856/`): on real
        /// benchmark geometry the registered optimum was 41-polygon chunks at
        /// 0.332x, adopted as the production `REGION_CHUNK_POLYGONS`; this
        /// probe stays as the synthetic-fixture reproduction harness. It never
        /// rides along with the regular suite (ignored), asserts only the
        /// exactness of the batch (hard failure), and prints the timing
        /// evidence for the decision made outside the test.
        #[test]
        #[ignore = "phase-0 micro-benchmark; run explicitly with --ignored"]
        fn poly_poly_batch_probe() {
            use std::time::{Duration, Instant};

            fn time(mut work: impl FnMut()) -> Duration {
                let start = Instant::now();
                work();
                start.elapsed()
            }

            /// Andrew's monotone chain hull (probe-local); returns the convex
            /// hull with collinear points removed.
            fn hull(points: &mut Vec<[i32; 2]>) -> Vec<[i32; 2]> {
                points.sort();
                points.dedup();
                if points.len() <= 2 {
                    return points.clone();
                }
                let cross = |o: [i32; 2], a: [i32; 2], b: [i32; 2]| -> i64 {
                    let abx = i64::from(a[0]) - i64::from(o[0]);
                    let aby = i64::from(a[1]) - i64::from(o[1]);
                    let acx = i64::from(b[0]) - i64::from(o[0]);
                    let acy = i64::from(b[1]) - i64::from(o[1]);
                    abx * acy - aby * acx
                };
                let mut out: Vec<[i32; 2]> = Vec::with_capacity(points.len() * 2);
                for &point in points.iter() {
                    while out.len() >= 2
                        && cross(out[out.len() - 2], out[out.len() - 1], point) <= 0
                    {
                        out.pop();
                    }
                    out.push(point);
                }
                let threshold = out.len() + 1;
                for &point in points.iter().rev().skip(1) {
                    while out.len() >= threshold
                        && cross(out[out.len() - 2], out[out.len() - 1], point) <= 0
                    {
                        out.pop();
                    }
                    out.push(point);
                }
                out.pop();
                out
            }

            /// ~10512 convex 5..=8-point polygons (the measured 64-instance
            /// polygon count) clustered like 64 character instances over a
            /// monitor-sized area; deterministic LCG as in the stress test.
            fn probe_fixture() -> Vec<Vec<[i32; 2]>> {
                let mut seed = 20_261_015_u32;
                let mut next = move || {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    seed
                };
                let mut polygons = Vec::with_capacity(10_512);
                for index in 0..10_512_usize {
                    let cluster = index % 64;
                    let cx = ((cluster % 8) as i32) * 200 + (next() % 60) as i32;
                    let cy = ((cluster / 8) as i32) * 125 + (next() % 40) as i32;
                    // First three points span a guaranteed non-degenerate
                    // triangle ((30+)*(30+)) − 20*20 ≥ 500 > 0), the rest are
                    // jittered inside the cluster.
                    let mut points = vec![
                        [cx, cy],
                        [cx + 30 + (next() % 15) as i32, cy + (next() % 20) as i32],
                        [cx + (next() % 20) as i32, cy + 30 + (next() % 15) as i32],
                    ];
                    for _ in 3..5 + (next() % 4) {
                        points.push([cx + (next() % 41) as i32, cy + (next() % 41) as i32]);
                    }
                    polygons.push(hull(&mut points));
                }
                polygons
            }

            fn pcts(mut samples: Vec<Duration>) -> (f64, f64) {
                samples.sort();
                let ms = |d: Duration| d.as_secs_f64() * 1000.0;
                let p50 = ms(samples[samples.len() / 2]);
                let p95 = {
                    let index = (samples.len() * 95 / 100).min(samples.len() - 1);
                    ms(samples[index])
                };
                (p50, p95)
            }

            reset();
            let fixture = probe_fixture();
            let total_points: usize = fixture.iter().map(Vec::len).sum();
            println!(
                "PROBE fixture polygons={} points={} avg_points={:.2}",
                fixture.len(),
                total_points,
                total_points as f64 / fixture.len() as f64
            );

            // Hard assert: the single-call batch is the exact same region as
            // the independent sequential oracle, for the probe fixture and
            // for small adversarial cases (rotation, reversal, mixed winding,
            // degenerate polygons, extreme coordinates). Every case is
            // attempted and reported before failing, so one run maps the whole
            // incompatibility surface.
            {
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
                let mut mixed_winding = rectangle(5, 5, 10, 10);
                mixed_winding.reverse();
                let mut small = vec![
                    vec![],
                    vec![square.clone()],
                    vec![
                        square.clone(),
                        rectangle(5, 5, 10, 10),
                        rectangle(10, 0, 10, 10),
                    ],
                    vec![concave, rectangle(50, 50, 10, 10)],
                    vec![
                        vec![[0, 0], [1, 30], [0, 30]],
                        vec![[1, 0], [2, 30], [1, 30]],
                    ],
                    // Collinear triple and three identical points: no area.
                    vec![vec![[0, 0], [1, 1], [2, 2]], vec![[0, 0], [0, 0], [0, 0]]],
                    // Both GDI coordinate range corners, one corner per case:
                    // a single batch rejects the full-range spread of mixing
                    // both corners (see the acceptance lab below), so each
                    // pair keeps its spread small while still covering the
                    // extreme coordinates.
                    vec![
                        rectangle(GDI_MIN_COORD, GDI_MIN_COORD, 16, 16),
                        rectangle(GDI_MIN_COORD + 32, GDI_MIN_COORD + 32, 16, 16),
                    ],
                    vec![
                        rectangle(GDI_MAX_COORD - 16, GDI_MAX_COORD - 16, 16, 16),
                        rectangle(GDI_MAX_COORD - 32, GDI_MAX_COORD - 32, 16, 16),
                    ],
                    // Mixed winding: raw WINDING fill would cancel in the
                    // overlap; canonicalization must keep the union.
                    vec![square.clone(), mixed_winding],
                ];
                /// Probes one arrangement: batch vs sequential oracle. A
                /// failure is recorded, not panicked, so one run maps the whole
                /// incompatibility surface; a failing arrangement reports
                /// growth-prefix verdicts (and, when small, per-polygon
                /// verdicts through both paths) to find the minimal failing
                /// sub-batch.
                fn probe_arrangement(
                    index: usize,
                    label: &str,
                    polygons: &[Vec<[i32; 2]>],
                    failures: &mut Vec<String>,
                ) {
                    let total_points: usize = polygons.iter().map(Vec::len).sum();
                    let verdict = batched_region(polygons).and_then(|batched| {
                        let sequential = unified_region_sequential(polygons)?;
                        regions_equal(&batched, &sequential)
                    });
                    let summary = match &verdict {
                        Ok(true) => "equivalence=OK".to_owned(),
                        Ok(false) => "equivalence=DIFF".to_owned(),
                        Err(error) => format!("error={error}"),
                    };
                    println!(
                        "PROBE case={index} {label} polygons={} points={total_points} {summary}",
                        polygons.len()
                    );
                    match &verdict {
                        Ok(true) => return,
                        Ok(false) => {
                            failures.push(format!("case={index} {label}: regions differ"));
                        }
                        Err(error) => {
                            failures.push(format!("case={index} {label}: {error}"));
                        }
                    }
                    let mut step = 1_usize;
                    let mut sweep = Vec::new();
                    while step < polygons.len() {
                        sweep.push(step);
                        step *= 2;
                    }
                    sweep.push(polygons.len());
                    for prefix in sweep {
                        let sub = &polygons[..prefix];
                        let points: usize = sub.iter().map(Vec::len).sum();
                        let verdict = match batched_region(sub) {
                            Ok(region) => {
                                drop(region);
                                "ok".to_owned()
                            }
                            Err(error) => format!("Err: {error}"),
                        };
                        println!("PROBE case={index} prefix={prefix} points={points} -> {verdict}");
                    }
                    if polygons.len() <= 16 {
                        for (position, polygon) in polygons.iter().enumerate() {
                            let single = std::slice::from_ref(polygon);
                            let batch = match batched_region(single) {
                                Ok(region) => {
                                    drop(region);
                                    "ok".to_owned()
                                }
                                Err(error) => format!("Err: {error}"),
                            };
                            let sequential = match unified_region(single) {
                                Ok(region) => {
                                    drop(region);
                                    "ok".to_owned()
                                }
                                Err(error) => format!("Err: {error}"),
                            };
                            println!(
                                "PROBE case={index} polygon={position} points={} batch={batch} sequential={sequential}",
                                polygon.len()
                            );
                        }
                    }
                }

                let mut failures: Vec<String> = Vec::new();
                for (index, polygons) in small.iter_mut().enumerate() {
                    probe_arrangement(index, "raw", polygons.as_slice(), &mut failures);
                    if !polygons.is_empty() {
                        polygons.rotate_left(1);
                        probe_arrangement(index, "rotated", polygons.as_slice(), &mut failures);
                        for polygon in polygons.iter_mut() {
                            polygon.reverse();
                        }
                        probe_arrangement(index, "reversed", polygons.as_slice(), &mut failures);
                    }
                }
                probe_arrangement(small.len(), "fixture", fixture.as_slice(), &mut failures);
                assert!(
                    failures.is_empty(),
                    "PROBE equivalence failures: {failures:#?}"
                );
                println!("PROBE equivalence=OK");

                // Acceptance lab (print-only, no assertion): the batch call
                // rejects some multi-polygon inputs that the sequential path
                // unions fine, even though every polygon is individually
                // acceptable. The probes narrow the condition (diagonal spread,
                // x-only spread, y-only spread, full range). Production
                // coordinates are window-local (a few thousand pixels), orders
                // of magnitude below anything failing here, and a rejection is
                // fail-closed (Err, never a wrong region).
                let verdict = |result: Result<GdiRegion, String>| match result {
                    Ok(region) => {
                        drop(region);
                        "ok"
                    }
                    Err(_) => "Err",
                };
                let side = |x: i32, y: i32| rectangle(x, y, 16, 16);
                let min = GDI_MIN_COORD;
                let lab_cases: Vec<(&str, Vec<Vec<[i32; 2]>>)> = vec![
                    (
                        "same-corner",
                        vec![side(min, min), side(min + 48, min + 48)],
                    ),
                    (
                        "diag-2^22",
                        vec![side(min, min), side(min + (1 << 22), min + (1 << 22))],
                    ),
                    (
                        "diag-2^23",
                        vec![side(min, min), side(min + (1 << 23), min + (1 << 23))],
                    ),
                    (
                        "diag-2^23+2^22",
                        vec![
                            side(min, min),
                            side(min + (1 << 23) + (1 << 22), min + (1 << 23) + (1 << 22)),
                        ],
                    ),
                    (
                        "diag-2^24",
                        vec![side(min, min), side(min + (1 << 24), min + (1 << 24))],
                    ),
                    (
                        "span-x-2^25",
                        vec![side(min, min), side(min + (1 << 25), min)],
                    ),
                    (
                        "span-y-2^25",
                        vec![side(min, min), side(min, min + (1 << 25))],
                    ),
                    (
                        "span-x-2^26",
                        vec![side(min, min), side(min + (1 << 26), min)],
                    ),
                    (
                        "span-y-2^26",
                        vec![side(min, min), side(min, min + (1 << 26))],
                    ),
                    (
                        "full-range",
                        vec![side(min, min), side(GDI_MAX_COORD - 16, GDI_MAX_COORD - 16)],
                    ),
                ];
                for (label, pair) in &lab_cases {
                    println!(
                        "PROBE lab {label} batch={} sequential={}",
                        verdict(batched_region(pair)),
                        verdict(unified_region_sequential(pair))
                    );
                }
                let huge = vec![rectangle(min, min, (1 << 27) - 48, 16)];
                println!(
                    "PROBE lab single-rect-full-range batched={} sequential={}",
                    verdict(batched_region(&huge)),
                    verdict(unified_region_sequential(&huge))
                );
            }

            // Measured-what-we-claim: the timed runs must be the real paths.
            for &count in &[2616_usize, 5232, 10512] {
                reset();
                drop(unified_region(&fixture[..count]).unwrap());
                let stats = state();
                assert_eq!(stats.calls[Call::Polygon as usize], count);
                assert_eq!(stats.calls[Call::Union as usize], count - 1);
                assert_eq!(stats.calls[Call::Empty as usize], count - 1);
                assert_eq!(stats.live, 0);
                reset();
                drop(batched_region(&fixture[..count]).unwrap());
                let stats = state();
                assert_eq!(stats.calls[Call::PolyPoly as usize], 1);
                assert_eq!(stats.calls[Call::Polygon as usize], 0);
                assert_eq!(stats.calls[Call::Union as usize], 0);
                assert_eq!(stats.calls[Call::Empty as usize], 0);
                assert_eq!(stats.peak, 1);
                assert_eq!(stats.live, 0);
            }
            reset();

            const ROUNDS: usize = 20;
            for &count in &[2616_usize, 5232, 10512] {
                let polygons = &fixture[..count];
                let mut balanced = Vec::with_capacity(ROUNDS);
                let mut batched = Vec::with_capacity(ROUNDS);
                // Warm both paths once (page-in, first-touch allocator effects).
                drop(unified_region(polygons).unwrap());
                drop(batched_region(polygons).unwrap());
                for round in 0..ROUNDS {
                    let run_balanced = || {
                        time(|| {
                            let region = unified_region(polygons).unwrap();
                            drop(region);
                        })
                    };
                    let run_batched = || {
                        time(|| {
                            let region = batched_region(polygons).unwrap();
                            drop(region);
                        })
                    };
                    if round % 2 == 0 {
                        balanced.push(run_balanced());
                        batched.push(run_batched());
                    } else {
                        batched.push(run_batched());
                        balanced.push(run_balanced());
                    }
                }
                let (balanced_p50, balanced_p95) = pcts(balanced);
                let (batched_p50, batched_p95) = pcts(batched);
                println!(
                    "PROBE n={count} A_balanced p50={balanced_p50:.3}ms p95={balanced_p95:.3}ms"
                );
                println!(
                    "PROBE n={count} B_batched  p50={batched_p50:.3}ms p95={batched_p95:.3}ms"
                );
                println!(
                    "PROBE n={count} ratio p50={:.3} p95={:.3}",
                    batched_p50 / balanced_p50,
                    batched_p95 / balanced_p95
                );
            }

            // Spatial-batching analysis (exploratory, print-only): the whole-
            // fixture batch is superlinear in the polygon count, but a batch
            // confined to one tight cluster may still pay off, since clusters
            // have a small bounding box and few edges per scanline. Partition
            // the fixture into K spatial groups (bands of its 8x8 cluster
            // grid), time the K batch calls alone and with a carry-slot fold
            // of the K regions, and check the folded region against the
            // sequential oracle. K=1 is the B path already timed above.
            fn cluster_groups(fixture: &[Vec<[i32; 2]>], groups: usize) -> Vec<Vec<Vec<[i32; 2]>>> {
                let mut out: Vec<Vec<Vec<[i32; 2]>>> = (0..groups).map(|_| Vec::new()).collect();
                for (index, polygon) in fixture.iter().enumerate() {
                    // Fixture layout: polygon `index` belongs to cluster
                    // `index % 64`; clusters form an 8x8 grid of 200x125 cells.
                    out[(index % 64) / (64 / groups)].push(polygon.clone());
                }
                out
            }

            fn merge_regions(regions: Vec<GdiRegion>) -> Result<GdiRegion, String> {
                let mut slots: [Option<GdiRegion>; usize::BITS as usize] =
                    core::array::from_fn(|_| None);
                for region in regions {
                    let mut carry = region;
                    let mut level = 0;
                    while let Some(occupant) = slots[level].take() {
                        carry = union_regions(occupant, carry)?;
                        level += 1;
                    }
                    slots[level] = Some(carry);
                }
                let mut merged: Option<GdiRegion> = None;
                for slot in &mut slots {
                    let Some(region) = slot.take() else {
                        continue;
                    };
                    merged = Some(match merged {
                        Some(accumulated) => union_regions(accumulated, region)?,
                        None => region,
                    });
                }
                match merged {
                    Some(region) => Ok(region),
                    None => empty_region(),
                }
            }

            let mut spatial_failures: Vec<usize> = Vec::new();
            let oracle = unified_region_sequential(&fixture).unwrap();
            const SPATIAL_ROUNDS: usize = 7;
            for &group_count in &[16_usize, 64] {
                let groups = cluster_groups(&fixture, group_count);
                let mut batch_only = Vec::new();
                let mut batch_merge = Vec::new();
                let mut grouped_merge = Vec::new();
                for _ in 0..SPATIAL_ROUNDS {
                    batch_only.push(time(|| {
                        let regions: Vec<GdiRegion> = groups
                            .iter()
                            .map(|group| batched_region(group).unwrap())
                            .collect();
                        drop(regions);
                    }));
                    batch_merge.push(time(|| {
                        let regions: Vec<GdiRegion> = groups
                            .iter()
                            .map(|group| batched_region(group).unwrap())
                            .collect();
                        drop(merge_regions(regions).unwrap());
                    }));
                    // Grouped balanced: the same partition through the
                    // per-polygon balanced path instead of one batch call, to
                    // separate what the batch call saves from what the
                    // partition alone (locality, fewer large merges) saves.
                    grouped_merge.push(time(|| {
                        let regions: Vec<GdiRegion> = groups
                            .iter()
                            .map(|group| unified_region(group).unwrap())
                            .collect();
                        drop(merge_regions(regions).unwrap());
                    }));
                }
                let (only_p50, _) = pcts(batch_only);
                let (batch_p50, _) = pcts(batch_merge);
                let (grouped_p50, _) = pcts(grouped_merge);
                let batch_merged = merge_regions(
                    groups
                        .iter()
                        .map(|group| batched_region(group).unwrap())
                        .collect(),
                )
                .unwrap();
                let batch_equal = regions_equal(&batch_merged, &oracle).unwrap();
                drop(batch_merged);
                let grouped_merged = merge_regions(
                    groups
                        .iter()
                        .map(|group| unified_region(group).unwrap())
                        .collect(),
                )
                .unwrap();
                let grouped_equal = regions_equal(&grouped_merged, &oracle).unwrap();
                drop(grouped_merged);
                println!(
                    "PROBE clusters K={group_count} batch_only p50={only_p50:.3}ms \
                     batch+merge p50={batch_p50:.3}ms grouped+merge p50={grouped_p50:.3}ms \
                     equivalence={} grouped_equivalence={}",
                    if batch_equal { "OK" } else { "DIFF" },
                    if grouped_equal { "OK" } else { "DIFF" }
                );
                if !batch_equal || !grouped_equal {
                    spatial_failures.push(group_count);
                }
            }
            // Scaling calibration (print-only, exploratory): the batch call's
            // cost separates into a polygon-count term and an extent term.
            // Sweep each with the other fixed, using the same generator in a
            // single cell. The production shape is ~164 polygons per character
            // in a ~240x135px cell, so these numbers decide how the fine
            // partitions above scale to production-sized instances.
            fn single_cell(count: usize, width: i32, height: i32) -> Vec<Vec<[i32; 2]>> {
                let mut seed = 20_261_015_u32;
                let mut next = move || {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    seed
                };
                let mut polygons = Vec::with_capacity(count);
                for _ in 0..count {
                    let cx = (next() % width as u32) as i32;
                    let cy = (next() % height as u32) as i32;
                    let mut points = vec![
                        [cx, cy],
                        [cx + 30 + (next() % 15) as i32, cy + (next() % 20) as i32],
                        [cx + (next() % 20) as i32, cy + 30 + (next() % 15) as i32],
                    ];
                    for _ in 3..5 + (next() % 4) {
                        points.push([cx + (next() % 41) as i32, cy + (next() % 41) as i32]);
                    }
                    polygons.push(hull(&mut points));
                }
                polygons
            }
            for &(label, count, width, height) in &[
                ("count-164-cell-240x135", 164_usize, 240, 135),
                ("count-328-cell-240x135", 328, 240, 135),
                ("count-656-cell-240x135", 656, 240, 135),
                ("count-1312-cell-240x135", 1312, 240, 135),
                ("spread-120x68-count-164", 164, 120, 68),
                ("spread-480x270-count-164", 164, 480, 270),
                ("spread-960x540-count-164", 164, 960, 540),
                ("spread-480x270-count-328", 328, 480, 270),
            ] {
                let cell = single_cell(count, width, height);
                let mut samples = Vec::new();
                for _ in 0..SPATIAL_ROUNDS {
                    samples.push(time(|| {
                        drop(batched_region(&cell).unwrap());
                    }));
                }
                let (p50, p95) = pcts(samples);
                println!(
                    "PROBE calib {label} batch p50={p50:.3}ms p95={p95:.3}ms per_poly={:.3}us",
                    p50 * 1000.0 / count as f64
                );
            }

            // Same polygons, cluster-major input order, unchanged balanced
            // path: does issuing same-cluster polygons consecutively (the
            // production frame's natural order) by itself shrink the merge
            // cost versus the interleaved order of the A measurement?
            let mut order: Vec<usize> = (0..fixture.len()).collect();
            order.sort_by_key(|&index| (index % 64, index / 64));
            let cluster_major: Vec<Vec<[i32; 2]>> =
                order.iter().map(|&index| fixture[index].clone()).collect();
            let mut reordered = Vec::new();
            for _ in 0..SPATIAL_ROUNDS {
                reordered.push(time(|| {
                    drop(unified_region(&cluster_major).unwrap());
                }));
            }
            let (reordered_p50, _) = pcts(reordered);
            let reordered_region = unified_region(&cluster_major).unwrap();
            let reordered_equal = regions_equal(&reordered_region, &oracle).unwrap();
            drop(reordered_region);
            println!(
                "PROBE order interleaved_p50={:.3}ms cluster_major_p50={reordered_p50:.3}ms equivalence={}",
                pcts(
                    (0..SPATIAL_ROUNDS)
                        .map(|_| time(|| {
                            drop(unified_region(&fixture).unwrap());
                        }))
                        .collect()
                )
                .0,
                if reordered_equal { "OK" } else { "DIFF" }
            );
            if !reordered_equal {
                spatial_failures.push(0);
            }
            assert!(
                spatial_failures.is_empty(),
                "PROBE spatial equivalence failures at K={spatial_failures:?}"
            );
            drop(oracle);

            // Coarse attribution: leaf creation alone vs the flatten step.
            // Chunked so the live-region count stays inside the process GDI
            // quota (~10k handles): creating all 10512 leaves at once would
            // fail, which is also why the balanced path's bounded liveness
            // matters. Per-leaf create cost is what this measures.
            let leaves = time(|| {
                for chunk in fixture.chunks(1000) {
                    let regions: Vec<GdiRegion> =
                        chunk.iter().map(|p| polygon_region(p).unwrap()).collect();
                    drop(regions);
                }
            });
            let flatten = time(|| {
                let (points, counts, polygons) = batch_points(&fixture).unwrap();
                assert_eq!(polygons as usize, fixture.len());
                assert_eq!(points.len(), total_points);
                assert_eq!(counts.len(), fixture.len());
            });
            println!(
                "PROBE attrib leaves_only={:.3}ms batch_points(validate+flatten)={:.3}ms",
                leaves.as_secs_f64() * 1000.0,
                flatten.as_secs_f64() * 1000.0
            );

            println!("PROBE GATE thresholds adopt<=0.50 low=0.50..0.80 stop>=0.80 at n=10512");
            assert_eq!(state().live, 0, "probe leaked regions");
        }
    }
}
