//! CPU rasterization of a layer mask polygon into an R8 coverage texture.
//!
//! The mask is rasterized once, at asset registration time, in model-local coordinates:
//! it does not follow the runtime mesh deformation. The shader maps the deformed local
//! point through the recorded bounds to a mask UV.

use std::cmp::Ordering;

/// Number of texels per mask edge (square texture).
pub const MASK_SIZE: u32 = 128;

/// Maximum polygon points accepted for one mask.
pub const MASK_MAX_POINTS: usize = 1024;

/// Supersampling factor per axis (2 => 4 samples per texel) for edge antialiasing.
const SUPERSAMPLE: u32 = 2;

/// Margin added around the polygon bounds so linear filtering does not clamp the edge.
const PAD_FRACTION: f32 = 0.06;

/// A rasterized mask plus the local-space mapping used by the shader.
#[derive(Debug)]
pub struct MaskRaster {
    /// Square texture edge length in texels.
    pub size: u32,
    /// `size * size` R8 coverage values.
    pub data: Vec<u8>,
    /// Local-space minimum (xy) of the texture area.
    pub min: [f32; 2],
    /// Local-space extent (xy) of the texture area, strictly positive.
    pub extent: [f32; 2],
}

/// Rasterizes `points` (model-local, even-odd fill) into a coverage texture.
///
/// Returns a human-readable error for degenerate or invalid polygons; the caller wraps it
/// into `RenderError::InvalidAsset`.
pub fn rasterize_mask(points: &[[f32; 2]]) -> Result<MaskRaster, String> {
    if points.len() < 3 {
        return Err(format!(
            "mask polygon needs at least 3 points, got {}",
            points.len()
        ));
    }
    if points.len() > MASK_MAX_POINTS {
        return Err(format!(
            "mask polygon has {} points, the limit is {MASK_MAX_POINTS}",
            points.len()
        ));
    }
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for (index, point) in points.iter().enumerate() {
        if !point[0].is_finite() || !point[1].is_finite() {
            return Err(format!("mask point {index} is not finite"));
        }
        min[0] = min[0].min(point[0]);
        min[1] = min[1].min(point[1]);
        max[0] = max[0].max(point[0]);
        max[1] = max[1].max(point[1]);
    }
    let raw_extent = [max[0] - min[0], max[1] - min[1]];
    if raw_extent[0].partial_cmp(&0.0) != Some(Ordering::Greater)
        || raw_extent[1].partial_cmp(&0.0) != Some(Ordering::Greater)
    {
        return Err(format!(
            "mask polygon bounds are degenerate: extent [{}, {}]",
            raw_extent[0], raw_extent[1]
        ));
    }

    // Signed area (shoelace) guards genuinely empty polygons.
    let mut area = 0.0f64;
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        let cross = f64::from(a[0]) * f64::from(b[1]) - f64::from(b[0]) * f64::from(a[1]);
        area += cross;
    }
    if (area.abs() * 0.5).partial_cmp(&1e-9) != Some(Ordering::Greater) {
        return Err("mask polygon encloses no area".into());
    }

    let pad = [
        raw_extent[0] * PAD_FRACTION + 1e-4,
        raw_extent[1] * PAD_FRACTION + 1e-4,
    ];
    let min = [min[0] - pad[0], min[1] - pad[1]];
    let extent = [raw_extent[0] + 2.0 * pad[0], raw_extent[1] + 2.0 * pad[1]];
    if !extent[0].is_finite() || !extent[1].is_finite() || extent[0] <= 0.0 || extent[1] <= 0.0 {
        return Err("mask polygon bounds overflowed".into());
    }

    let size = MASK_SIZE;
    let texels = (size * size) as usize;
    let mut data = vec![0u8; texels];
    let step = 1.0 / (SUPERSAMPLE * SUPERSAMPLE) as f32;
    let offset = 0.5 / SUPERSAMPLE as f32;
    for y in 0..size {
        for x in 0..size {
            let mut hits = 0u32;
            for sub_y in 0..SUPERSAMPLE {
                for sub_x in 0..SUPERSAMPLE {
                    let texel = [
                        x as f32 + offset + sub_x as f32 / SUPERSAMPLE as f32,
                        y as f32 + offset + sub_y as f32 / SUPERSAMPLE as f32,
                    ];
                    let local = [
                        min[0] + texel[0] / size as f32 * extent[0],
                        min[1] + texel[1] / size as f32 * extent[1],
                    ];
                    if point_inside_even_odd(local, points) {
                        hits += 1;
                    }
                }
            }
            let coverage = hits as f32 * step;
            data[(y * size + x) as usize] = (coverage * 255.0 + 0.5) as u8;
        }
    }

    Ok(MaskRaster {
        size,
        data,
        min,
        extent,
    })
}

/// Even-odd rule point-in-polygon test.
fn point_inside_even_odd(point: [f32; 2], points: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[j];
        let crosses = (a[1] > point[1]) != (b[1] > point[1]);
        if crosses {
            let x_at = (b[0] - a[0]) * (point[1] - a[1]) / (b[1] - a[1]) + a[0];
            if point[0] < x_at {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_guards_reject_nonfinite_degenerate_and_overflowing_bounds() {
        let points = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        assert!(rasterize_mask(&points).is_ok());
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut invalid = points;
            invalid[0][0] = value;
            assert!(rasterize_mask(&invalid).is_err());
        }
        assert!(rasterize_mask(&[[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]]).is_err());
        assert!(rasterize_mask(&[[-f32::MAX, 0.0], [f32::MAX, 0.0], [0.0, 1.0]]).is_err());
    }
}
