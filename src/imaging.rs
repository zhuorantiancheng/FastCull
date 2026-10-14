use crate::arw::ArwInfo;
use anyhow::{Context, Result, ensure};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};
use std::path::Path;
use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
use zune_jpeg::JpegDecoder;

pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Thumb,
    Fit(u32),
    Full,
}
pub fn decode(path: &Path, full: bool) -> Result<(ArwInfo, Pixels)> {
    let (info, pixels) = decode_sensor(path, full)?;
    let pixels = orient(pixels, info.orientation);
    Ok((info, pixels))
}
/// GUI uses sensor pixels and transforms texture coordinates on GPU, without a full-image copy.
pub fn decode_sensor(path: &Path, full: bool) -> Result<(ArwInfo, Pixels)> {
    let info = ArwInfo::read(path)?;
    let pixels = decode_level(path, &info, if full { Level::Full } else { Level::Thumb })?;
    Ok((info, pixels))
}
pub fn decode_level(path: &Path, info: &ArwInfo, level: Level) -> Result<Pixels> {
    decode_level_with_geometry(path, info, level, &mut |_| {})
}
/// Publish actual unrotated output dimensions before expensive pixel decoding.
pub fn decode_level_with_geometry(
    path: &Path,
    info: &ArwInfo,
    level: Level,
    geometry: &mut dyn FnMut([usize; 2]),
) -> Result<Pixels> {
    if info.raw_size.is_some() {
        return crate::native_raw::decode_with_geometry(path, level, geometry);
    }
    if crate::formats::is_jpeg(path) {
        ensure!(
            std::fs::metadata(path)?.len() <= 128 * 1024 * 1024,
            "JPEG too large"
        );
        return decode_jpeg_bytes_with_geometry(&std::fs::read(path)?, level, geometry);
    }
    // Sony models and in-camera crop modes also have different output sizes.
    // At 100%, let the native decoder compare actual JPEG dimensions with this
    // file's active image area, instead of assuming the largest byte stream is
    // full resolution. Keep the bounded preview reader for legacy TIFF files
    // that are viewable but not recognized as a sensor RAW by LibRaw.
    if level == Level::Full
        && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("arw"))
        && crate::native_raw::read(path).is_ok()
    {
        return crate::native_raw::decode_with_geometry(path, level, geometry);
    }
    let primary = match level {
        // Camera icon JPEGs can be padded to 4:3 even for a 3:2 photo.
        // Downscale the picture preview so crop UVs describe picture pixels,
        // not that padded canvas. The scaled JPEG path bounds decode cost.
        Level::Thumb | Level::Fit(_) => &info.preview,
        Level::Full => &info.jpeg,
    };
    // A damaged small JPEG must not make a valid larger preview inaccessible.
    match decode_jpeg(path, primary, level, geometry) {
        Ok(p) => Ok(p),
        Err(_) if primary.offset != info.jpeg.offset => {
            decode_jpeg(path, &info.jpeg, level, geometry)
        }
        Err(e) => Err(e),
    }
}
fn decode_jpeg(
    path: &Path,
    jpeg: &crate::arw::JpegRef,
    level: Level,
    geometry: &mut dyn FnMut([usize; 2]),
) -> Result<Pixels> {
    let bytes = ArwInfo::read_jpeg(path, jpeg)?;
    decode_jpeg_bytes_with_geometry(&bytes, level, geometry)
}
#[cfg(test)]
pub(crate) fn decode_jpeg_bytes(bytes: &[u8], level: Level) -> Result<Pixels> {
    decode_jpeg_bytes_with_geometry(bytes, level, &mut |_| {})
}
pub(crate) fn decode_jpeg_bytes_with_geometry(
    bytes: &[u8],
    level: Level,
    geometry: &mut dyn FnMut([usize; 2]),
) -> Result<Pixels> {
    let mut decoder = JpegDecoder::new_with_options(
        ZCursor::new(bytes),
        DecoderOptions::default()
            .jpeg_set_out_colorspace(ColorSpace::RGBA)
            .set_max_width(16384)
            .set_max_height(16384),
    );
    decoder
        .decode_headers()
        .map_err(|e| anyhow::anyhow!("JPEG header: {e}"))?;
    let (w, h) = decoder
        .dimensions()
        .context("JPEG dimensions unavailable")?;
    ensure!(
        w.checked_mul(h).is_some_and(|n| n <= 160_000_000),
        "JPEG exceeds 160 MP safety limit"
    );
    geometry([w, h]);
    let edge = match level {
        Level::Thumb => 200,
        Level::Fit(edge) => edge as usize,
        Level::Full => usize::MAX,
    };
    if edge > 0
        && w.max(h) / 2 >= edge
        && matches!(
            decoder.input_colorspace(),
            Some(ColorSpace::YCbCr | ColorSpace::RGB | ColorSpace::Luma)
        )
    {
        let ratio = edge as f64 / w.max(h) as f64;
        let tw = (w as f64 * ratio).round().max(1.) as u32;
        let th = (h as f64 * ratio).round().max(1.) as u32;
        if let Some(pixels) = crate::native_raw::scaled_jpeg(bytes, tw, th) {
            return resize_to(pixels, tw, th, false);
        }
        // Missing/unsupported OS codec or malformed JPEG: keep the existing
        // bounded Rust decoder as the authoritative fallback.
    }
    let rgba = decoder
        .decode()
        .map_err(|e| anyhow::anyhow!("JPEG decode: {e}"))?;
    let pixels = Pixels {
        width: w,
        height: h,
        rgba,
    };
    resize_level(pixels, level)
}
pub(crate) fn resize_level(pixels: Pixels, level: Level) -> Result<Pixels> {
    match level {
        // JPEG and LibRaw output are opaque. Avoid a full-sized temporary
        // buffer and two alpha passes when reducing photographic previews.
        Level::Thumb => resize(pixels, 200, false),
        Level::Fit(edge) => resize(pixels, edge, false),
        Level::Full => Ok(pixels),
    }
}
pub fn thumbnail(p: Pixels, edge: u32) -> Result<Pixels> {
    resize(p, edge, true)
}
fn resize(p: Pixels, edge: u32, alpha: bool) -> Result<Pixels> {
    let ratio = (edge as f64 / p.width.max(p.height) as f64).min(1.0);
    let w = (p.width as f64 * ratio).round().max(1.0) as u32;
    let h = (p.height as f64 * ratio).round().max(1.0) as u32;
    resize_to(p, w, h, alpha)
}
fn resize_to(p: Pixels, w: u32, h: u32, alpha: bool) -> Result<Pixels> {
    if w as usize == p.width && h as usize == p.height {
        return Ok(p);
    }
    let src = Image::from_vec_u8(p.width as u32, p.height as u32, p.rgba, PixelType::U8x4)?;
    let mut dst = Image::new(w, h, PixelType::U8x4);
    Resizer::new().resize(
        &src,
        &mut dst,
        Some(
            &ResizeOptions::new()
                .use_alpha(alpha)
                .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        ),
    )?;
    Ok(Pixels {
        width: w as usize,
        height: h as usize,
        rgba: dst.into_vec(),
    })
}
pub fn orient(p: Pixels, o: u16) -> Pixels {
    if o == 1 {
        return p;
    }
    let swap = o >= 5;
    let (w, h) = if swap {
        (p.height, p.width)
    } else {
        (p.width, p.height)
    };
    let mut rgba = vec![0; p.rgba.len()];
    // Cache-blocked transpose: sequential rows in both source and destination within each tile.
    for ty in (0..p.height).step_by(32) {
        for tx in (0..p.width).step_by(32) {
            for y in ty..(ty + 32).min(p.height) {
                for x in tx..(tx + 32).min(p.width) {
                    let (dx, dy) = match o {
                        2 => (p.width - 1 - x, y),
                        3 => (p.width - 1 - x, p.height - 1 - y),
                        4 => (x, p.height - 1 - y),
                        5 => (y, x),
                        6 => (p.height - 1 - y, x),
                        7 => (p.height - 1 - y, p.width - 1 - x),
                        8 => (y, p.width - 1 - x),
                        _ => (x, y),
                    };
                    rgba[(dy * w + dx) * 4..(dy * w + dx + 1) * 4]
                        .copy_from_slice(&p.rgba[(y * p.width + x) * 4..(y * p.width + x + 1) * 4]);
                }
            }
        }
    }
    Pixels {
        width: w,
        height: h,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_resize_matches_alpha_aware_resize_exactly() {
        let full =
            decode_jpeg_bytes(include_bytes!("../tests/fixtures/tiny.jpg"), Level::Full).unwrap();
        let reference = thumbnail(
            Pixels {
                width: full.width,
                height: full.height,
                rgba: full.rgba.clone(),
            },
            7,
        )
        .unwrap();
        let fast = resize(full, 7, false).unwrap();
        assert_eq!(fast.rgba, reference.rgba);
    }
    #[test]
    fn native_scaled_jpeg_retains_color_and_opaque_alpha() {
        let bytes = include_bytes!("../tests/fixtures/tiny.jpg");
        let Some(p) = crate::native_raw::scaled_jpeg(bytes, 8, 6) else {
            assert!(
                std::env::var_os("FASTCULL_NATIVE_TEST").is_none(),
                "native scaled JPEG unavailable"
            );
            return;
        };
        assert!(p.width >= 8 && p.height >= 6);
        assert!(p.rgba.chunks_exact(4).all(|p| p[3] == 255));
        let p = resize_to(p, 8, 6, false).unwrap();
        let original = decode_jpeg_bytes(bytes, Level::Full).unwrap();
        let reference = thumbnail(original, 8).unwrap();
        let mean_error = p
            .rgba
            .iter()
            .zip(&reference.rgba)
            .map(|(a, b)| a.abs_diff(*b) as usize)
            .sum::<usize>() as f64
            / p.rgba.len() as f64;
        assert!(mean_error < 12., "scaled JPEG color error {mean_error}");
        assert!(crate::native_raw::scaled_jpeg(b"broken jpeg", 8, 6).is_none());
    }
}
