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

/// Largest per-vertex screen-space movement the installed region may lag
/// behind the frame, in physical pixels. Movement is measured against the
/// geometry of the last successful build, so lag cannot accumulate.
pub(super) const REGION_MOVE_EPSILON_PX: f32 = 4.0;

// A cached region may lag by REGION_MOVE_EPSILON_PX, so the padding has to be
// larger to keep a positive margin around the drawn geometry at all times.
const _: () = assert!(REGION_MOVE_EPSILON_PX < HIT_PADDING_PX);

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
    fn changed_since(&self, previous: &Self) -> bool {
        if self.asset != previous.asset
            || self.instance != previous.instance
            || self.polys.len() != previous.polys.len()
        {
            return true;
        }
        self.polys
            .iter()
            .zip(&previous.polys)
            .any(|(current, previous)| {
                if current.len() != previous.len() {
                    return true;
                }
                current.iter().zip(previous).any(|(current, previous)| {
                    let dx = current[0] - previous[0];
                    let dy = current[1] - previous[1];
                    dx * dx + dy * dy > REGION_MOVE_EPSILON_PX * REGION_MOVE_EPSILON_PX
                })
            })
    }
}

/// The geometry a pixel region was last built from.
///
/// The raw screen-space polygons and the padded integer polygons are kept
/// apart: [`HitGeometry::changed_since`] compares the raw geometry, so a
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
    pub(super) fn from_frame_with_view(
        simulation: &Simulation,
        frame: &RenderFrame,
        size: [u32; 2],
        view: SceneView,
    ) -> Result<Self, String> {
        let width = size[0].max(1) as f32;
        let height = size[1].max(1) as f32;
        let mut instances = Vec::with_capacity(frame.instances.len());
        let mut polygons = Vec::new();
        #[cfg(all(test, target_os = "windows"))]
        let mut instance_polygon_ends = Vec::with_capacity(frame.instances.len());
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
            for poly in &polys {
                // Pad first: clipping the padded polygon keeps the full
                // padding everywhere except beyond the window edge, where
                // there is nothing left to intercept.
                let padded = pad_polygon(poly, HIT_PADDING_PX);
                let clipped = clip_to_client(&padded, width, height);
                let region = region_polygon(&clipped, width, height);
                if region.len() >= 3 {
                    polygons.push(region);
                }
            }
            instances.push(InstanceOutline {
                asset,
                instance: draw.instance(),
                polys,
            });
            #[cfg(all(test, target_os = "windows"))]
            instance_polygon_ends.push(polygons.len());
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

    /// `true` when the region built from this geometry no longer matches
    /// `previous` (the geometry of the last successful build): a client
    /// resize, a scene-view change, any topology change (instances, asset or
    /// instance identity, polygon or vertex counts), or a vertex that moved
    /// more than [`REGION_MOVE_EPSILON_PX`] on screen.
    pub(super) fn changed_since(&self, previous: &Self) -> bool {
        if self.size != previous.size
            || self.view != previous.view
            || self.instances.len() != previous.instances.len()
        {
            return true;
        }
        self.instances
            .iter()
            .zip(&previous.instances)
            .any(|(current, previous)| current.changed_since(previous))
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
            push_triangle(&mut polys, corners, size, view, width, height)
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
    for triangle in skeleton.indices.as_chunks::<3>().0 {
        let mut corners = [[0.0f32; 3]; 3];
        for (corner, index) in triangle.iter().enumerate() {
            let vertex = skeleton.vertices.get(*index as usize).ok_or_else(|| {
                format!(
                    "asset {asset} references vertex {index} but only {} exist",
                    skeleton.vertices.len()
                )
            })?;
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
            corners[corner] = [world.x, world.y, world.z];
        }
        push_triangle(&mut polys, corners, size, view, width, height)
            .map_err(|error| format!("asset {asset}: {error}"))?;
    }
    Ok(polys)
}

/// Clips one world-space triangle to the camera's depth slab, projects it and
/// appends the visible, non-degenerate screen-space polygon to `outlines`.
fn push_triangle(
    outlines: &mut Vec<Vec<[f32; 2]>>,
    triangle: [[f32; 3]; 3],
    size: [u32; 2],
    view: SceneView,
    width: f32,
    height: f32,
) -> Result<(), String> {
    let Some(visible) = clip_depth(&triangle) else {
        return Ok(());
    };
    let mut projected = Vec::with_capacity(visible.len());
    for point in &visible {
        let screen = world_to_screen_with([point[0], point[1]], size, view);
        if !screen[0].is_finite() || !screen[1].is_finite() {
            return Err("a vertex projects to a non-finite screen position".to_owned());
        }
        projected.push(screen);
    }
    let projected = clip_to_client(&projected, width, height);
    if projected.len() < 3 {
        return Ok(());
    }
    if doubled_area(&projected) == 0.0 {
        // Exactly zero-area projections are invisible. Thin but non-zero
        // slivers are kept on purpose.
        return Ok(());
    }
    outlines.push(projected);
    Ok(())
}

/// Clips a triangle against the camera's near and far planes in world space,
/// returning `None` when nothing of it is visible.
fn clip_depth(triangle: &[[f32; 3]; 3]) -> Option<Vec<[f32; 3]>> {
    let poly = clip_polygon3(triangle, |point| DEPTH_NEAR_Z - point[2]);
    if poly.len() < 3 {
        return None;
    }
    let poly = clip_polygon3(&poly, |point| point[2] - DEPTH_FAR_Z);
    if poly.len() < 3 {
        return None;
    }
    Some(poly)
}

/// Sutherland-Hodgman clip of a convex polygon against one 3D half-plane,
/// keeping the side where `side(point) >= 0` and interpolating crossing edges.
fn clip_polygon3(poly: &[[f32; 3]], side: impl Fn(&[f32; 3]) -> f32) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(poly.len() + 1);
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
    out
}

/// Sutherland-Hodgman clip of a convex polygon against the client rectangle.
/// Vertices are never clamped here: clipping keeps the true silhouette.
fn clip_to_client(poly: &[[f32; 2]], width: f32, height: f32) -> Vec<[f32; 2]> {
    let poly = clip_polygon2(poly, |point| point[0]);
    let poly = clip_polygon2(&poly, |point| width - point[0]);
    let poly = clip_polygon2(&poly, |point| point[1]);
    clip_polygon2(&poly, |point| height - point[1])
}

/// Sutherland-Hodgman clip of a convex polygon against one 2D half-plane,
/// keeping the side where `side(point) >= 0` and interpolating crossing edges.
fn clip_polygon2(poly: &[[f32; 2]], side: impl Fn([f32; 2]) -> f32) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(poly.len() + 1);
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
    out
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

/// Minkowski sum of a convex polygon with the axis-aligned square
/// `[-pad, pad]^2`, as the convex hull of every vertex offset by the four
/// square corners. The result has exactly `pad` pixels of margin along each
/// axis and no unbounded miter spikes on sharp corners.
fn pad_polygon(poly: &[[f32; 2]], pad: f32) -> Vec<[f32; 2]> {
    let mut points = Vec::with_capacity(poly.len() * 4);
    for &[x, y] in poly {
        points.push([x - pad, y - pad]);
        points.push([x + pad, y - pad]);
        points.push([x - pad, y + pad]);
        points.push([x + pad, y + pad]);
    }
    convex_hull(&mut points)
}

/// Rounds a padded polygon to integer client coordinates without shrinking
/// it: every vertex contributes its unit-cell corners, whose convex hull
/// contains the float polygon (each vertex lies inside its own cell), so no
/// visible margin is lost to rounding. `poly` is already clipped to the
/// client, so the final clamp is only a safety net against float drift.
fn region_polygon(poly: &[[f32; 2]], width: f32, height: f32) -> Vec<[i32; 2]> {
    let mut points = Vec::with_capacity(poly.len() * 4);
    for &[x, y] in poly {
        let (x0, x1) = (x.floor(), x.ceil());
        let (y0, y1) = (y.floor(), y.ceil());
        points.push([x0, y0]);
        points.push([x1, y0]);
        points.push([x0, y1]);
        points.push([x1, y1]);
    }
    convex_hull(&mut points)
        .into_iter()
        .map(|point| {
            [
                point[0].clamp(0.0, width) as i32,
                point[1].clamp(0.0, height) as i32,
            ]
        })
        .collect()
}

/// Andrew's monotone chain convex hull with collinear points removed. All
/// coordinates must be finite.
fn convex_hull(points: &mut Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    points.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    points.dedup();
    if points.len() <= 2 {
        return points.clone();
    }
    let mut hull: Vec<[f32; 2]> = Vec::with_capacity(points.len() * 2);
    for &point in points.iter() {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0.0 {
            hull.pop();
        }
        hull.push(point);
    }
    let threshold = hull.len() + 1;
    for &point in points.iter().rev().skip(1) {
        while hull.len() >= threshold
            && cross(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0.0
        {
            hull.pop();
        }
        hull.push(point);
    }
    hull.pop();
    hull
}

fn cross(origin: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - origin[0]) * (b[1] - origin[1]) - (a[1] - origin[1]) * (b[0] - origin[0])
}
