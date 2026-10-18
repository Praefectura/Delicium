//! Screen-space character outlines that back the native window hit region.
//!
//! The region is the union of every visible triangle of a frame, projected to
//! physical window pixels and padded by [`HIT_PADDING_PX`] along each axis. It
//! is a geometric outline of the rendered silhouette, not a copy of its
//! pixels: a fully transparent 2D layer contributes nothing, masks only shape
//! the shader's alpha, and anti-aliased coverage is not reproduced. Triangles
//! are built exactly as the renderer builds them (deformed 2D layers, skinned
//! 3D vertices) and projected with [`world_to_screen_with`], the inverse of
//! the picking transform.
//!
//! Clipping never moves vertices onto the window border: triangles are first
//! clipped against the fixed camera's near/far depth slab (world z in
//! `-90.0 ..= 9.9` maps to NDC z in `0 ..= 1`), then against the client
//! rectangle as polygons. Geometry outside the depth slab cannot intercept
//! input, and partially visible triangles keep their true silhouette.

#[cfg(all(test, target_os = "windows"))]
#[path = "hit_region_tests.rs"]
mod tests;

use std::cmp::Ordering;

#[cfg(all(test, target_os = "windows"))]
use std::cell::Cell;

use glam::{Mat4, Vec3};

use crate::{
    model::{CompiledModel, CompiledSkeleton3d, CompiledSprite2d, ModelData},
    renderer::{
        InstanceDraw, RenderAssetId, RenderFrame, RenderInstanceId, SceneView, world_to_screen_with,
    },
    simulation::Simulation,
};

/// Padding added around every projected triangle, in physical pixels.
///
/// The padding is the Minkowski sum with an axis-aligned square, so the
/// margin is exactly this many pixels along each axis (at most
/// `sqrt(2) * HIT_PADDING_PX` along a diagonal) and never an unbounded spike
/// on a sharp corner.
pub(super) const HIT_PADDING_PX: f32 = 8.0;

/// Movement dead zone, measured against the last successful installation.
pub(super) const REGION_MOVE_EPSILON_PX: f32 = 4.0;

/// Deferral must leave at least one pixel inside the padded native region.
/// Unlike the dead zone, this is a hard limit even under install throttling.
pub(super) const REGION_SAFE_LAG_PX: f32 = 7.0;
const _: () = assert!(REGION_MOVE_EPSILON_PX < REGION_SAFE_LAG_PX);
const _: () = assert!(REGION_SAFE_LAG_PX < HIT_PADDING_PX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RegionChange {
    Unchanged,
    WithinMargin,
    Immediate,
}

/// World-space z of the camera's near plane (NDC z = 0); see
/// [`crate::renderer::scene_projection`], an orthographic view at `z = 10`
/// looking down -Z with near/far depths `0.1`/`100`.
const DEPTH_NEAR_Z: f32 = 9.9;

/// World-space z of the camera's far plane (NDC z = 1).
const DEPTH_FAR_Z: f32 = -90.0;

/// One instance's contribution to the outline: the projected screen-space
/// triangles of the last successful build, after clipping and before padding.
#[derive(Debug)]
struct InstanceOutline {
    asset: RenderAssetId,
    instance: RenderInstanceId,
    polys: Vec<Vec<[f32; 2]>>,
}

impl InstanceOutline {
    fn change_since(&self, previous: &Self) -> RegionChange {
        if self.asset != previous.asset
            || self.instance != previous.instance
            || self.polys.len() != previous.polys.len()
        {
            return RegionChange::Immediate;
        }
        let mut change = RegionChange::Unchanged;
        for (current, previous) in self.polys.iter().zip(&previous.polys) {
            if current.len() != previous.len() {
                return RegionChange::Immediate;
            }
            for (current, previous) in current.iter().zip(previous) {
                let dx = current[0] - previous[0];
                let dy = current[1] - previous[1];
                let distance_sq = dx * dx + dy * dy;
                if distance_sq >= REGION_SAFE_LAG_PX * REGION_SAFE_LAG_PX {
                    return RegionChange::Immediate;
                }
                if distance_sq > REGION_MOVE_EPSILON_PX * REGION_MOVE_EPSILON_PX {
                    change = RegionChange::WithinMargin;
                }
            }
        }
        change
    }
}

/// The geometry a pixel region was last built from.
///
/// The raw screen-space polygons and the padded integer polygons are kept
/// apart: [`HitGeometry::change_since`] compares the raw geometry, so a
/// region is rebuilt on real movement instead of on rounding noise, and the
/// stored integer polygons are the ones actually installed.
#[derive(Debug)]
pub(super) struct HitGeometry {
    size: [u32; 2],
    view: SceneView,
    instances: Vec<InstanceOutline>,
    polygons: Vec<Vec<[i32; 2]>>,
    /// Exclusive end of every instance's polygons inside `polygons`, in
    /// frame order; probe-only metadata for the batched-install decision
    /// gate (see `.artifacts/perf-perbatch-20261016-015856/`).
    #[cfg(all(test, target_os = "windows"))]
    instance_polygon_ends: Vec<usize>,
}

impl HitGeometry {
    /// Collects the screen-space outline of every instance in `frame`.
    ///
    /// `size` is the physical client size the projection maps to. Broken
    /// assets (layer count mismatches, out-of-range indices, missing skin
    /// matrices) and non-finite deformations are reported as errors; the
    /// caller falls back instead of silently leaving holes in the region.
    /// [`HitGeometry::from_frame_with_view`] under the frozen [`LEGACY`] view,
    /// kept as the entry point of the region tests.
    #[cfg(all(test, target_os = "windows"))]
    pub(super) fn from_frame(
        simulation: &Simulation,
        frame: &RenderFrame,
        size: [u32; 2],
    ) -> Result<Self, String> {
        use crate::renderer::LEGACY;
        Self::from_frame_with_view(simulation, frame, size, LEGACY)
    }

    /// [`HitGeometry::from_frame`] under an explicit [`SceneView`]; the view
    /// decides the world-to-screen mapping the outlines are projected with.
    #[cfg(all(test, target_os = "windows"))]
    pub(super) fn from_frame_with_view(
        simulation: &Simulation,
        frame: &RenderFrame,
        size: [u32; 2],
        view: SceneView,
    ) -> Result<Self, String> {
        let mut geometry = Self::collect_frame_with_view(simulation, frame, size, view)?;
        geometry.materialize()?;
        Ok(geometry)
    }

    /// Collects only raw comparison geometry. Deferred and reused frames do
    /// not need padded integer polygons; only an installation materializes them.
    pub(super) fn collect_frame_with_view(
        simulation: &Simulation,
        frame: &RenderFrame,
        size: [u32; 2],
        view: SceneView,
    ) -> Result<Self, String> {
        let mut instances = Vec::new();
        try_reserve_vec(&mut instances, frame.instances.len(), "frame instances")?;
        let polygons = Vec::new();
        #[cfg(all(test, target_os = "windows"))]
        let mut instance_polygon_ends = Vec::new();
        #[cfg(all(test, target_os = "windows"))]
        try_reserve_vec(
            &mut instance_polygon_ends,
            frame.instances.len(),
            "instance polygon ends",
        )?;
        if size[0] == 0 || size[1] == 0 {
            // A zero-sized client shows nothing and must not intercept input.
            return Ok(Self {
                size,
                view,
                instances,
                polygons,
                #[cfg(all(test, target_os = "windows"))]
                instance_polygon_ends,
            });
        }
        for draw in &frame.instances {
            let (asset, polys) = match draw {
                InstanceDraw::Sprite2d {
                    asset,
                    transform,
                    layers,
                    ..
                } => {
                    let model = asset_model(simulation, *asset)?;
                    let ModelData::Sprite2d(sprite) = &model.data else {
                        return Err(format!(
                            "asset {asset} is drawn as a 2D sprite but is a {} model",
                            model.data.kind()
                        ));
                    };
                    (
                        *asset,
                        sprite_outline(*asset, sprite, transform, layers, size, view)?,
                    )
                }
                InstanceDraw::Skeleton3d {
                    asset,
                    transform,
                    skin_matrices,
                    ..
                } => {
                    let model = asset_model(simulation, *asset)?;
                    let ModelData::Skeleton3d(skeleton) = &model.data else {
                        return Err(format!(
                            "asset {asset} is drawn as a 3D skeleton but is a {} model",
                            model.data.kind()
                        ));
                    };
                    (
                        *asset,
                        skeleton_outline(*asset, skeleton, transform, skin_matrices, size, view)?,
                    )
                }
            };
            instances.push(InstanceOutline {
                asset,
                instance: draw.instance(),
                polys,
            });
        }
        Ok(Self {
            size,
            view,
            instances,
            polygons,
            #[cfg(all(test, target_os = "windows"))]
            instance_polygon_ends,
        })
    }

    /// Pads and integerizes only a candidate selected for installation.
    /// This is idempotent, including empty/zero-sized client geometry. On
    /// failure the partial polygons and instance ends are cleared, so a
    /// rejected candidate can never be installed.
    pub(super) fn materialize(&mut self) -> Result<(), String> {
        self.polygons.clear();
        #[cfg(all(test, target_os = "windows"))]
        self.instance_polygon_ends.clear();
        match self.materialize_into() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.polygons.clear();
                #[cfg(all(test, target_os = "windows"))]
                self.instance_polygon_ends.clear();
                Err(error)
            }
        }
    }

    fn materialize_into(&mut self) -> Result<(), String> {
        let mut scratch = MaterializeScratch::default();
        let mut raw_polygons = 0usize;
        for instance in &self.instances {
            raw_polygons = raw_polygons
                .checked_add(instance.polys.len())
                .ok_or_else(|| "hit region polygon count overflows usize".to_owned())?;
        }
        try_reserve_vec(&mut self.polygons, raw_polygons, "hit region polygons")?;
        #[cfg(all(test, target_os = "windows"))]
        try_reserve_vec(
            &mut self.instance_polygon_ends,
            self.instances.len(),
            "instance polygon ends",
        )?;
        let [width, height] = self.size.map(|n| n as f32);
        for instance in &self.instances {
            for poly in &instance.polys {
                // Pad before clipping, preserving the margin at window edges.
                pad_polygon_into(
                    poly,
                    HIT_PADDING_PX,
                    &mut scratch.candidates,
                    &mut scratch.hull,
                )?;
                let region = if client_fully_inside(&scratch.hull, width, height) {
                    // All four side expressions hold, so the four clip passes
                    // would only copy the points unchanged.
                    region_polygon_into(
                        &scratch.hull,
                        width,
                        height,
                        &mut scratch.candidates,
                        &mut scratch.region_hull,
                    )?
                } else {
                    clip_to_client_into(
                        &scratch.hull,
                        &mut scratch.clip_a,
                        &mut scratch.clip_b,
                        width,
                        height,
                    )?;
                    region_polygon_into(
                        &scratch.clip_b,
                        width,
                        height,
                        &mut scratch.candidates,
                        &mut scratch.region_hull,
                    )?
                };
                if region.len() >= 3 {
                    self.polygons.push(region);
                }
            }
            #[cfg(all(test, target_os = "windows"))]
            self.instance_polygon_ends.push(self.polygons.len());
        }
        Ok(())
    }

    /// Classifies movement against the last successfully installed geometry.
    /// Structural changes have no correspondence proof and cannot be deferred.
    pub(super) fn change_since(&self, previous: &Self) -> RegionChange {
        if self.size != previous.size
            || self.view != previous.view
            || self.instances.len() != previous.instances.len()
        {
            return RegionChange::Immediate;
        }
        let mut change = RegionChange::Unchanged;
        for (current, previous) in self.instances.iter().zip(&previous.instances) {
            match current.change_since(previous) {
                RegionChange::Immediate => return RegionChange::Immediate,
                RegionChange::WithinMargin => change = RegionChange::WithinMargin,
                RegionChange::Unchanged => {}
            }
        }
        change
    }

    #[cfg(all(test, target_os = "windows"))]
    pub(super) fn changed_since(&self, previous: &Self) -> bool {
        self.change_since(previous) != RegionChange::Unchanged
    }

    /// The padded integer polygons to install as the window region, in
    /// physical client coordinates.
    pub(super) fn polygons(&self) -> &[Vec<[i32; 2]>] {
        &self.polygons
    }

    /// Exclusive end of every instance's polygons in [`HitGeometry::polygons`],
    /// in frame order (probe-only; see the `instance_polygon_ends` field).
    #[cfg(all(test, target_os = "windows"))]
    pub(super) fn instance_polygon_ends(&self) -> &[usize] {
        &self.instance_polygon_ends
    }
}

fn asset_model(simulation: &Simulation, asset: RenderAssetId) -> Result<&CompiledModel, String> {
    simulation
        .asset(asset)
        .map(|model| model.as_ref())
        .ok_or_else(|| format!("asset {asset} is drawn but not registered in the simulation"))
}

/// Screen-space triangles of a 2D instance: every non-transparent layer's
/// deformed vertices grouped by the compiled triangle list.
fn sprite_outline(
    asset: RenderAssetId,
    sprite: &CompiledSprite2d,
    transform: &Mat4,
    layers: &[Vec<[f32; 2]>],
    size: [u32; 2],
    view: SceneView,
) -> Result<Vec<Vec<[f32; 2]>>, String> {
    if sprite.layers.len() != layers.len() {
        return Err(format!(
            "asset {asset} declares {} layers but the frame deformed {}",
            sprite.layers.len(),
            layers.len()
        ));
    }
    let width = size[0].max(1) as f32;
    let height = size[1].max(1) as f32;
    let mut polys = Vec::new();
    let mut scratch = TriangleScratch::default();
    for (layer, vertices) in sprite.layers.iter().zip(layers) {
        if layer.color[3] <= 0.0 || !layer.texture_visible {
            // A fully transparent layer draws nothing and must not intercept.
            continue;
        }
        if layer.indices.len() % 3 != 0 {
            return Err(format!(
                "asset {asset} layer '{}' has {} indices, not a triangle list",
                layer.name,
                layer.indices.len()
            ));
        }
        let triangles = layer.indices.len() / 3;
        try_reserve_vec(&mut polys, triangles, "sprite outlines")?;
        for triangle in layer.indices.as_chunks::<3>().0 {
            let mut corners = [[0.0f32; 3]; 3];
            for (corner, index) in triangle.iter().enumerate() {
                let vertex = vertices.get(*index as usize).ok_or_else(|| {
                    format!(
                        "asset {asset} layer '{}' references vertex {index} but only {} were deformed",
                        layer.name,
                        vertices.len()
                    )
                })?;
                let world = transform.transform_point3(Vec3::new(vertex[0], vertex[1], 0.0));
                if !world.is_finite() {
                    return Err(format!(
                        "asset {asset} layer '{}' deforms to a non-finite position",
                        layer.name
                    ));
                }
                corners[corner] = [world.x, world.y, world.z];
            }
            push_triangle_scratch(&mut polys, corners, size, view, width, height, &mut scratch)
                .map_err(|error| format!("asset {asset} layer '{}': {error}", layer.name))?;
        }
    }
    Ok(polys)
}

/// Screen-space triangles of a 3D instance: bind-space vertices skinned with
/// the frame's matrices (exactly as picking does), grouped by the compiled
/// triangle list and transformed by the instance root.
fn skeleton_outline(
    asset: RenderAssetId,
    skeleton: &CompiledSkeleton3d,
    transform: &Mat4,
    skin_matrices: &[Mat4],
    size: [u32; 2],
    view: SceneView,
) -> Result<Vec<Vec<[f32; 2]>>, String> {
    if !skeleton.visible {
        return Ok(Vec::new());
    }
    let width = size[0].max(1) as f32;
    let height = size[1].max(1) as f32;
    if !skeleton.indices.len().is_multiple_of(3) {
        return Err(format!(
            "asset {asset} has {} indices, not a triangle list",
            skeleton.indices.len()
        ));
    }
    let mut polys = Vec::new();
    try_reserve_vec(&mut polys, skeleton.indices.len() / 3, "skeleton outlines")?;
    // Per-instance, per-frame: shared triangle corners reuse the exact same
    // skinning sum. Lazy population preserves first-error order and never
    // evaluates an unreferenced vertex.
    let mut world_vertices = Vec::new();
    try_reserve_vec(
        &mut world_vertices,
        skeleton.vertices.len(),
        "lazy world vertices",
    )?;
    world_vertices.resize(skeleton.vertices.len(), None);
    let mut scratch = TriangleScratch::default();
    for triangle in skeleton.indices.as_chunks::<3>().0 {
        let mut corners = [[0.0f32; 3]; 3];
        for (corner, index) in triangle.iter().enumerate() {
            let cached = world_vertices.get_mut(*index as usize).ok_or_else(|| {
                format!(
                    "asset {asset} references vertex {index} but only {} exist",
                    skeleton.vertices.len()
                )
            })?;
            let world = if let Some(world) = *cached {
                world
            } else {
                let vertex = &skeleton.vertices[*index as usize];
                let bind = Vec3::from_array(vertex.position);
                let mut position = Vec3::ZERO;
                for (joint, weight) in vertex.joints.iter().zip(vertex.weights) {
                    let skin = skin_matrices.get(*joint as usize).ok_or_else(|| {
                        format!(
                            "asset {asset} vertex {index} references joint {joint} but only {} skin matrices are ready",
                            skin_matrices.len()
                        )
                    })?;
                    position += skin.transform_point3(bind) * weight;
                }
                let world = transform.transform_point3(position);
                if !world.is_finite() {
                    return Err(format!(
                        "asset {asset} vertex {index} skins to a non-finite position"
                    ));
                }
                let world = world.to_array();
                *cached = Some(world);
                world
            };
            corners[corner] = world;
        }
        push_triangle_scratch(&mut polys, corners, size, view, width, height, &mut scratch)
            .map_err(|error| format!("asset {asset}: {error}"))?;
    }
    Ok(polys)
}

/// A padded or integerization candidate together with its position in the
/// original push order. The ordinal reproduces the stable-sort result of the
/// old comparator with an allocation-free unstable sort.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    point: [f32; 2],
    key: u64,
    ordinal: usize,
}

/// Frame-local buffers reused across every polygon of one materialization.
#[derive(Default)]
struct MaterializeScratch {
    candidates: Vec<Candidate>,
    hull: Vec<[f32; 2]>,
    region_hull: Vec<[f32; 2]>,
    clip_a: Vec<[f32; 2]>,
    clip_b: Vec<[f32; 2]>,
}

/// Per-instance buffers reused across every triangle of one outline.
#[derive(Default)]
struct TriangleScratch {
    depth_a: Vec<[f32; 3]>,
    depth_b: Vec<[f32; 3]>,
    projected: Vec<[f32; 2]>,
    clip_a: Vec<[f32; 2]>,
    clip_b: Vec<[f32; 2]>,
}

#[cfg(all(test, target_os = "windows"))]
std::thread_local! {
    static RESERVE_FAILURE: Cell<Option<usize>> = const { Cell::new(None) };
    static RESERVE_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
}

/// Test-only allocation-failure injection: the `(after + 1)`-th actual
/// reserve attempt of this thread fails. Resets the attempt counter.
#[cfg(all(test, target_os = "windows"))]
pub(super) fn set_reserve_failure(after: Option<usize>) {
    RESERVE_FAILURE.with(|failure| failure.set(after));
    RESERVE_ATTEMPTS.with(|attempts| attempts.set(0));
}

/// Test-only count of actual reserve attempts since the last
/// [`set_reserve_failure`] call.
#[cfg(all(test, target_os = "windows"))]
pub(super) fn reserve_attempts() -> usize {
    RESERVE_ATTEMPTS.with(Cell::get)
}

#[cfg(all(test, target_os = "windows"))]
fn injected_reserve_failure() -> bool {
    let attempt = RESERVE_ATTEMPTS.with(|attempts| {
        let attempt = attempts.get() + 1;
        attempts.set(attempt);
        attempt
    });
    RESERVE_FAILURE.with(|failure| failure.get().is_some_and(|after| attempt > after))
}

/// Grows `vec` to fit `target` elements if it is smaller, and reports an
/// injected or real allocation failure. Only actual reserve calls count as
/// attempts.
fn reserve_capacity<T>(vec: &mut Vec<T>, target: usize, what: &str) -> Result<(), String> {
    if vec.capacity() >= target {
        return Ok(());
    }
    let additional = target.saturating_sub(vec.len());
    #[cfg(all(test, target_os = "windows"))]
    {
        if injected_reserve_failure() {
            return Err(format!(
                "injected allocation failure while reserving {what}"
            ));
        }
    }
    vec.try_reserve(additional)
        .map_err(|error| format!("failed to reserve {what}: {error}"))
}

fn try_reserve_vec<T>(vec: &mut Vec<T>, additional: usize, what: &str) -> Result<(), String> {
    let target = vec
        .len()
        .checked_add(additional)
        .ok_or_else(|| format!("{what} length overflows usize"))?;
    reserve_capacity(vec, target, what)
}

/// Clips one world-space triangle to the camera's depth slab, projects it and
/// appends the visible, non-degenerate screen-space polygon to `outlines`,
/// reusing the per-instance [`TriangleScratch`].
fn push_triangle_scratch(
    outlines: &mut Vec<Vec<[f32; 2]>>,
    triangle: [[f32; 3]; 3],
    size: [u32; 2],
    view: SceneView,
    width: f32,
    height: f32,
    scratch: &mut TriangleScratch,
) -> Result<(), String> {
    let TriangleScratch {
        depth_a,
        depth_b,
        projected,
        clip_a,
        clip_b,
    } = scratch;
    let depth_len = clip_depth_into(&triangle, depth_a, depth_b)?;
    if depth_len < 3 {
        return Ok(());
    }
    projected.clear();
    reserve_capacity(projected, depth_len, "projected triangle")?;
    for point in &depth_b[..depth_len] {
        let screen = world_to_screen_with([point[0], point[1]], size, view);
        if !screen[0].is_finite() || !screen[1].is_finite() {
            return Err("a vertex projects to a non-finite screen position".to_owned());
        }
        projected.push(screen);
    }
    if client_fully_inside(projected, width, height) {
        // All four side expressions hold, so clipping would only copy points.
        finish_triangle(outlines, projected)
    } else {
        clip_to_client_into(projected, clip_a, clip_b, width, height)?;
        finish_triangle(outlines, clip_b)
    }
}

fn finish_triangle(outlines: &mut Vec<Vec<[f32; 2]>>, clipped: &[[f32; 2]]) -> Result<(), String> {
    if clipped.len() < 3 {
        return Ok(());
    }
    if clipped
        .iter()
        .any(|p| !p[0].is_finite() || !p[1].is_finite())
    {
        return Err("a clipped outline vertex is not finite".to_owned());
    }
    if doubled_area(clipped) == 0.0 {
        // Exactly zero-area projections are invisible. Thin but non-zero
        // slivers are kept on purpose.
        return Ok(());
    }
    try_reserve_vec(outlines, 1, "triangle outlines")?;
    let mut owned = Vec::new();
    reserve_capacity(&mut owned, clipped.len(), "triangle outline")?;
    owned.extend_from_slice(clipped);
    outlines.push(owned);
    Ok(())
}

/// Clips a triangle against the camera's near and far planes in world space,
/// returning the visible polygon length in `out` (0 when nothing is visible).
fn clip_depth_into(
    triangle: &[[f32; 3]; 3],
    a: &mut Vec<[f32; 3]>,
    out: &mut Vec<[f32; 3]>,
) -> Result<usize, String> {
    clip_polygon3_into(triangle, a, |point| DEPTH_NEAR_Z - point[2])?;
    if a.len() < 3 {
        out.clear();
        return Ok(0);
    }
    clip_polygon3_into(a, out, |point| point[2] - DEPTH_FAR_Z)?;
    Ok(if out.len() < 3 { 0 } else { out.len() })
}

/// Sutherland-Hodgman clip of a convex polygon against one 3D half-plane,
/// keeping the side where `side(point) >= 0` and interpolating crossing edges.
fn clip_polygon3_into(
    poly: &[[f32; 3]],
    out: &mut Vec<[f32; 3]>,
    side: impl Fn(&[f32; 3]) -> f32,
) -> Result<(), String> {
    let target = poly
        .len()
        .checked_mul(2)
        .ok_or_else(|| "depth clip buffer overflows usize".to_owned())?;
    out.clear();
    reserve_capacity(out, target, "depth clip buffer")?;
    for (index, current) in poly.iter().enumerate() {
        let previous = &poly[(index + poly.len() - 1) % poly.len()];
        let current_side = side(current);
        let previous_side = side(previous);
        if current_side >= 0.0 {
            if previous_side < 0.0 {
                out.push(lerp3(
                    previous,
                    current,
                    previous_side / (previous_side - current_side),
                ));
            }
            out.push(*current);
        } else if previous_side >= 0.0 {
            out.push(lerp3(
                previous,
                current,
                previous_side / (previous_side - current_side),
            ));
        }
    }
    Ok(())
}

/// Whether all four original client side expressions hold for every point;
/// in that case the four clip passes only copy points.
fn client_fully_inside(poly: &[[f32; 2]], width: f32, height: f32) -> bool {
    poly.iter().all(|point| {
        point[0] >= 0.0 && width - point[0] >= 0.0 && point[1] >= 0.0 && height - point[1] >= 0.0
    })
}

/// Sutherland-Hodgman clip of a convex polygon against the client rectangle,
/// alternating between two reusable buffers. Vertices are never clamped here:
/// clipping keeps the true silhouette.
fn clip_to_client_into(
    poly: &[[f32; 2]],
    a: &mut Vec<[f32; 2]>,
    b: &mut Vec<[f32; 2]>,
    width: f32,
    height: f32,
) -> Result<(), String> {
    clip_polygon2_into(poly, a, |point| point[0])?;
    clip_polygon2_into(a, b, |point| width - point[0])?;
    clip_polygon2_into(b, a, |point| point[1])?;
    clip_polygon2_into(a, b, |point| height - point[1])?;
    Ok(())
}

/// Sutherland-Hodgman clip of a convex polygon against one 2D half-plane,
/// keeping the side where `side(point) >= 0` and interpolating crossing edges.
fn clip_polygon2_into(
    poly: &[[f32; 2]],
    out: &mut Vec<[f32; 2]>,
    side: impl Fn([f32; 2]) -> f32,
) -> Result<(), String> {
    let target = poly
        .len()
        .checked_mul(2)
        .ok_or_else(|| "client clip buffer overflows usize".to_owned())?;
    out.clear();
    reserve_capacity(out, target, "client clip buffer")?;
    for (index, &current) in poly.iter().enumerate() {
        let previous = poly[(index + poly.len() - 1) % poly.len()];
        let current_side = side(current);
        let previous_side = side(previous);
        if current_side >= 0.0 {
            if previous_side < 0.0 {
                out.push(lerp2(
                    previous,
                    current,
                    previous_side / (previous_side - current_side),
                ));
            }
            out.push(current);
        } else if previous_side >= 0.0 {
            out.push(lerp2(
                previous,
                current,
                previous_side / (previous_side - current_side),
            ));
        }
    }
    Ok(())
}

fn lerp2(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn lerp3(a: &[f32; 3], b: &[f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Twice the signed area of a polygon (shoelace); the sign follows winding.
fn doubled_area(poly: &[[f32; 2]]) -> f32 {
    let mut sum = 0.0;
    for (index, &[x, y]) in poly.iter().enumerate() {
        let [next_x, next_y] = poly[(index + 1) % poly.len()];
        sum += x * next_y - next_x * y;
    }
    sum
}

fn push_candidate(candidates: &mut Vec<Candidate>, point: [f32; 2]) -> Result<(), String> {
    if !point[0].is_finite() || !point[1].is_finite() {
        return Err(format!("hit region hull candidate {point:?} is not finite"));
    }
    let ordinal = candidates.len();
    candidates.push(Candidate {
        point,
        key: candidate_sort_key(point),
        ordinal,
    });
    Ok(())
}

// Finite numeric order; only the key canonicalizes zero, never the point bits.
fn candidate_sort_key(point: [f32; 2]) -> u64 {
    let [x, y] = point.map(|value| {
        let bits = if value == 0.0 { 0 } else { value.to_bits() };
        if bits & 0x8000_0000 != 0 {
            !bits
        } else {
            bits ^ 0x8000_0000
        }
    });
    (u64::from(x) << 32) | u64::from(y)
}

fn compare_candidates(a: &Candidate, b: &Candidate) -> Ordering {
    a.key.cmp(&b.key).then_with(|| a.ordinal.cmp(&b.ordinal))
}

/// Minkowski sum of a convex polygon with the axis-aligned square
/// `[-pad, pad]^2`, as the convex hull of every vertex offset by the four
/// square corners. The result has exactly `pad` pixels of margin along each
/// axis and no unbounded miter spikes on sharp corners.
fn pad_polygon_into(
    poly: &[[f32; 2]],
    pad: f32,
    candidates: &mut Vec<Candidate>,
    hull: &mut Vec<[f32; 2]>,
) -> Result<(), String> {
    let count = poly
        .len()
        .checked_mul(4)
        .ok_or_else(|| "hit region candidate count overflows usize".to_owned())?;
    candidates.clear();
    reserve_capacity(candidates, count, "hit region candidates")?;
    for &[x, y] in poly {
        for point in [
            [x - pad, y - pad],
            [x + pad, y - pad],
            [x - pad, y + pad],
            [x + pad, y + pad],
        ] {
            push_candidate(candidates, point)?;
        }
    }
    convex_hull_candidates_into(candidates, hull, "hit region padding hull")
}

/// Rounds a padded polygon to integer client coordinates without shrinking
/// it: every vertex contributes its unit-cell corners, whose convex hull
/// contains the float polygon (each vertex lies inside its own cell), so no
/// visible margin is lost to rounding. `poly` is already clipped to the
/// client, so the final clamp is only a safety net against float drift.
fn region_polygon_into(
    poly: &[[f32; 2]],
    width: f32,
    height: f32,
    candidates: &mut Vec<Candidate>,
    region_hull: &mut Vec<[f32; 2]>,
) -> Result<Vec<[i32; 2]>, String> {
    let count = poly
        .len()
        .checked_mul(4)
        .ok_or_else(|| "hit region candidate count overflows usize".to_owned())?;
    candidates.clear();
    reserve_capacity(candidates, count, "hit region candidates")?;
    for &[x, y] in poly {
        let (x0, x1) = (x.floor(), x.ceil());
        let (y0, y1) = (y.floor(), y.ceil());
        for point in [[x0, y0], [x1, y0], [x0, y1], [x1, y1]] {
            push_candidate(candidates, point)?;
        }
    }
    convex_hull_candidates_into(candidates, region_hull, "hit region hull")?;
    let mut region = Vec::new();
    reserve_capacity(&mut region, region_hull.len(), "hit region polygon")?;
    for point in region_hull.iter() {
        region.push([
            point[0].clamp(0.0, width) as i32,
            point[1].clamp(0.0, height) as i32,
        ]);
    }
    Ok(region)
}

/// Andrew's monotone chain convex hull with collinear points removed.
///
/// Candidates must be finite; non-finite ones are rejected as an invalid
/// intermediate because the old comparator was non-transitive for NaN.
/// Precomputed finite numeric keys and original ordinals reproduce the old
/// stable sort bit for bit without a temporary allocation.
fn convex_hull_candidates_into(
    points: &mut Vec<Candidate>,
    output: &mut Vec<[f32; 2]>,
    what: &str,
) -> Result<(), String> {
    points.sort_unstable_by(compare_candidates);
    points.dedup_by(|a, b| a.point == b.point);
    output.clear();
    if points.len() <= 2 {
        reserve_capacity(output, points.len(), what)?;
        for candidate in points.iter() {
            output.push(candidate.point);
        }
        return Ok(());
    }
    let peak = points
        .len()
        .checked_mul(2)
        .ok_or_else(|| format!("{what} size overflows usize"))?;
    reserve_capacity(output, peak, what)?;
    for candidate in points.iter() {
        let point = candidate.point;
        while output.len() >= 2
            && cross(output[output.len() - 2], output[output.len() - 1], point) <= 0.0
        {
            output.pop();
        }
        output.push(point);
    }
    let threshold = output.len() + 1;
    for candidate in points.iter().rev().skip(1) {
        let point = candidate.point;
        while output.len() >= threshold
            && cross(output[output.len() - 2], output[output.len() - 1], point) <= 0.0
        {
            output.pop();
        }
        output.push(point);
    }
    output.pop();
    Ok(())
}

fn cross(origin: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - origin[0]) * (b[1] - origin[1]) - (a[1] - origin[1]) * (b[0] - origin[0])
}

/// Test-only free helper preserving the original `push_triangle` signature.
#[cfg(all(test, target_os = "windows"))]
fn push_triangle(
    outlines: &mut Vec<Vec<[f32; 2]>>,
    triangle: [[f32; 3]; 3],
    size: [u32; 2],
    view: SceneView,
    width: f32,
    height: f32,
) -> Result<(), String> {
    let mut scratch = TriangleScratch::default();
    push_triangle_scratch(outlines, triangle, size, view, width, height, &mut scratch)
}

/// Test-only free helper preserving the original `clip_depth` signature.
#[cfg(all(test, target_os = "windows"))]
fn clip_depth(triangle: &[[f32; 3]; 3]) -> Option<Vec<[f32; 3]>> {
    let mut a = Vec::new();
    let mut b = Vec::new();
    let len = clip_depth_into(triangle, &mut a, &mut b).ok()?;
    if len < 3 {
        return None;
    }
    let mut visible = Vec::new();
    visible.extend_from_slice(&b[..len]);
    Some(visible)
}

/// Test-only free helper preserving the original `clip_to_client` signature.
#[cfg(all(test, target_os = "windows"))]
fn clip_to_client(poly: &[[f32; 2]], width: f32, height: f32) -> Vec<[f32; 2]> {
    let mut a = Vec::new();
    let mut b = Vec::new();
    clip_to_client_into(poly, &mut a, &mut b, width, height)
        .map(|()| b)
        .unwrap_or_default()
}

/// Test-only free helper preserving the original `pad_polygon` signature.
#[cfg(all(test, target_os = "windows"))]
fn pad_polygon(poly: &[[f32; 2]], pad: f32) -> Vec<[f32; 2]> {
    let mut candidates = Vec::new();
    let mut hull = Vec::new();
    pad_polygon_into(poly, pad, &mut candidates, &mut hull)
        .map(|()| hull)
        .unwrap_or_default()
}

/// Test-only free helper preserving the original `region_polygon` signature.
#[cfg(all(test, target_os = "windows"))]
fn region_polygon(poly: &[[f32; 2]], width: f32, height: f32) -> Vec<[i32; 2]> {
    let mut candidates = Vec::new();
    let mut hull = Vec::new();
    region_polygon_into(poly, width, height, &mut candidates, &mut hull).unwrap_or_default()
}

/// Test-only free helper preserving the original `convex_hull` signature.
#[cfg(all(test, target_os = "windows"))]
#[allow(dead_code, clippy::ptr_arg)] // original test-facing signature
fn convex_hull(points: &mut Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    let mut candidates = Vec::new();
    for &point in points.iter() {
        if push_candidate(&mut candidates, point).is_err() {
            return Vec::new();
        }
    }
    let mut hull = Vec::new();
    convex_hull_candidates_into(&mut candidates, &mut hull, "convex hull")
        .map(|()| hull)
        .unwrap_or_default()
}
