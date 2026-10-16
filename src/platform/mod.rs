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

    #[cfg(test)]
    pub(super) fn hit_region_contains(
        polygons: &[Vec<[i32; 2]>],
        points: &[[i32; 2]],
    ) -> Result<Vec<bool>, String> {
        let region = chunked_region(polygons, REGION_CHUNK_POLYGONS)?;
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
        // The production path: the hook mirrors what `set_hit_region`
        // installs, so the test entry point tracks production exactly.
        let production = chunked_region(polygons, REGION_CHUNK_POLYGONS)?;
        let sequential = unified_region_sequential(polygons)?;
        regions_equal(&production, &sequential)
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
        let region = chunked_region(polygons, REGION_CHUNK_POLYGONS)?;
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
            PolyPoly,
        }

        #[derive(Clone, Copy, Default)]
        struct State {
            live: usize,
            peak: usize,
            calls: [usize; 4],
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

        #[test]
        fn production_union_matches_sequential_oracle() {
            // Exercises the production path through the same hook the app
            // tests use (`hit_region_matches_sequential_oracle` builds
            // `chunked_region(.., REGION_CHUNK_POLYGONS)`); every case here
            // fits in one batch, so this covers the production leaf and fold,
            // while `chunked_union_matches_sequential_oracle` covers
            // multi-batch unions.
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
