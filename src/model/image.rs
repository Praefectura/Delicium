//! Base64 + PNG decoding for the embedded image table of `0.2` documents.
//!
//! The pipeline is strictly bounded: the Base64 decode buffer is sized by an
//! estimate computed from the input length, the PNG payload must fit
//! [`LoadLimits::max_png_decoder_bytes`], decoded dimensions and RGBA8 bytes
//! are checked *before* the output buffer is allocated, and a per-model
//! running total is checked against [`LoadLimits::max_total_image_bytes`].
//!
//! Decoding accepts ordinary non-animated PNGs of at most 8 bits per sample
//! and expands palette/gray/transparency to straight RGBA8; 16-bit and APNG
//! inputs are rejected. CRC/Adler checks stay enabled, text and ICC chunks
//! are ignored (no color transform: samples are treated as sRGB), and the
//! full stream is verified through `next_frame` plus `finish`, so truncated
//! or corrupt files never decode.
//!
//! After decoding, RGB is premultiplied by `alpha / 255` **in linear space**
//! and re-encoded to sRGB, matching how the renderer samples an
//! `Rgba8UnormSrgb` texture into linear space: transparent texels become
//! exactly black, so bilinear filtering cannot bleed dark edges. The same
//! pass records the alpha flags and the global maximum alpha used later for
//! the conservative whole-model visibility test.

use std::io::Cursor;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use png::{BitDepth, ColorType, Decoder, Limits, Transformations};

use crate::model::error::ModelError;
use crate::model::limits::LoadLimits;
use crate::model::types::ModelImage;

/// One decoded image plus the metadata the compiler derives from it.
pub(super) struct DecodedImage {
    /// The decoded image itself.
    pub image: ModelImage,
    /// Global maximum alpha over all texels, normalized to `[0, 1]`.
    ///
    /// Only an upper bound of actual coverage is needed downstream, so this
    /// is computed from the straight alpha before any color work.
    pub max_alpha: f32,
}

/// Decodes one embedded image.
///
/// `total_used` is the accumulated RGBA8 size of the images decoded before
/// this one; the budget check happens before the output buffer exists, so an
/// over-budget model never allocates the decoded pixels. `ctx` names the
/// JSON location (`images[i]`).
pub(super) fn decode_model_image(
    id: &str,
    png_base64: &str,
    limits: &LoadLimits,
    total_used: usize,
    ctx: &str,
) -> Result<DecodedImage, ModelError> {
    let base64_ctx = format!("{ctx}.png_base64");

    // Canonical padding reduces the library's conservative estimate by up
    // to two bytes. The engine still validates the alphabet and padding.
    let padding = png_base64
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'=')
        .take(2)
        .count();
    let estimate = base64::decoded_len_estimate(png_base64.len()).saturating_sub(padding);
    if estimate > limits.max_png_decoder_bytes {
        return Err(ModelError::LimitExceeded {
            what: "png image bytes",
            actual: estimate as u64,
            max: limits.max_png_decoder_bytes as u64,
        });
    }
    let mut encoded = Vec::new();
    encoded.try_reserve_exact(estimate).map_err(|_| {
        ModelError::invalid(&base64_ctx, "could not allocate the Base64 decode buffer")
    })?;
    encoded.resize(estimate, 0);
    let written = STANDARD
        .decode_slice(png_base64, &mut encoded)
        .map_err(|err| ModelError::invalid(&base64_ctx, format!("invalid Base64: {err}")))?;
    encoded.truncate(written);

    if encoded.len() > limits.max_png_decoder_bytes {
        return Err(ModelError::LimitExceeded {
            what: "png image bytes",
            actual: encoded.len() as u64,
            max: limits.max_png_decoder_bytes as u64,
        });
    }

    let mut decoder = Decoder::new_with_limits(
        Cursor::new(encoded.as_slice()),
        Limits {
            bytes: limits.max_png_decoder_bytes,
        },
    );
    decoder.set_transformations(Transformations::EXPAND | Transformations::ALPHA);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    decoder.ignore_checksums(false);
    let mut reader = decoder
        .read_info()
        .map_err(|err| ModelError::invalid(ctx, format!("invalid PNG: {err}")))?;

    let (width, height) = reader.info().size();
    let bit_depth = reader.info().bit_depth;
    let animated = reader.info().animation_control.is_some();
    if bit_depth == BitDepth::Sixteen {
        return Err(ModelError::invalid(ctx, "16-bit PNGs are not supported"));
    }
    if animated {
        return Err(ModelError::invalid(
            ctx,
            "animated (APNG) images are not supported",
        ));
    }

    let max_dim = limits.max_image_dimension;
    if width == 0 || height == 0 || width > max_dim || height > max_dim {
        return Err(ModelError::invalid(
            ctx,
            format!("image size {width}x{height} is outside the supported range (1..={max_dim})"),
        ));
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(ModelError::SizeOverflow {
            what: "image pixels",
        })?;
    let byte_len = pixels.checked_mul(4).ok_or(ModelError::SizeOverflow {
        what: "image bytes",
    })?;
    if byte_len > limits.max_image_bytes {
        return Err(ModelError::LimitExceeded {
            what: "image bytes",
            actual: byte_len as u64,
            max: limits.max_image_bytes as u64,
        });
    }
    let total = total_used
        .checked_add(byte_len)
        .ok_or(ModelError::SizeOverflow {
            what: "total image bytes",
        })?;
    if total > limits.max_total_image_bytes {
        return Err(ModelError::LimitExceeded {
            what: "total image bytes",
            actual: total as u64,
            max: limits.max_total_image_bytes as u64,
        });
    }

    // The output buffer is allocated only after every size check passed.
    let mut data = Vec::new();
    data.try_reserve_exact(byte_len)
        .map_err(|_| ModelError::invalid(ctx, "could not allocate the image output buffer"))?;
    data.resize(byte_len, 0);
    let output = reader
        .next_frame(&mut data)
        .map_err(|err| ModelError::invalid(ctx, format!("PNG frame decode failed: {err}")))?;
    if output.width != width || output.height != height || output.bit_depth != BitDepth::Eight {
        return Err(ModelError::invalid(
            ctx,
            "PNG frame does not match its header",
        ));
    }
    reader.finish().map_err(|err| {
        ModelError::invalid(ctx, format!("PNG end-of-stream verification failed: {err}"))
    })?;

    match output.color_type {
        ColorType::Rgba => {}
        ColorType::GrayscaleAlpha => expand_gray_alpha_in_place(&mut data, pixels),
        other => {
            return Err(ModelError::invalid(
                ctx,
                format!("unsupported PNG output color type {other:?}"),
            ));
        }
    }

    let mut max_alpha = 0u8;
    let mut opaque = true;
    let mut fully_transparent = true;
    for texel in data.as_chunks_mut::<4>().0 {
        let alpha = texel[3];
        max_alpha = max_alpha.max(alpha);
        opaque &= alpha == 255;
        fully_transparent &= alpha == 0;
        if alpha == 255 {
            // Premultiplying by 1.0 in linear space and re-encoding is an
            // identity; keeping the byte avoids a needless round trip.
            continue;
        }
        if alpha == 0 {
            // Fully transparent texels are premultiplied to exact zero so the
            // renderer's filtering can never blend hidden color in.
            texel[0] = 0;
            texel[1] = 0;
            texel[2] = 0;
            continue;
        }
        let factor = alpha as f32 / 255.0;
        for channel in &mut texel[..3] {
            let linear = srgb_to_linear(*channel as f32 / 255.0) * factor;
            *channel = linear_to_srgb_byte(linear);
        }
    }

    Ok(DecodedImage {
        image: ModelImage {
            id: id.to_string(),
            width,
            height,
            rgba: Arc::from(data),
            opaque,
            fully_transparent,
        },
        max_alpha: max_alpha as f32 / 255.0,
    })
}

/// Expands a straight gray+alpha stream to RGBA8 in place, walking from the
/// end so the wider texels never overwrite source bytes that are still to be
/// read.
fn expand_gray_alpha_in_place(data: &mut [u8], pixels: usize) {
    for i in (0..pixels).rev() {
        let gray = data[2 * i];
        let alpha = data[2 * i + 1];
        data[4 * i] = gray;
        data[4 * i + 1] = gray;
        data[4 * i + 2] = gray;
        data[4 * i + 3] = alpha;
    }
}

/// The sRGB transfer function (IEC 61966-2-1), byte fraction to linear.
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// The inverse sRGB transfer function, linear to byte fraction.
fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

/// Linear `[0, 1]` to an sRGB byte with round-to-nearest and saturation.
fn linear_to_srgb_byte(value: f32) -> u8 {
    (linear_to_srgb(value.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8
}
