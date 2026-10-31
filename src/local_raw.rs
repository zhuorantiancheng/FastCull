//! Local, non-Adobe RAW development. LibRaw applies camera/DNG color matrices,
//! sensor-stage white balance and linear exposure; the UI owns the seven tone
//! controls. DCP looks, masks and Adobe's process algorithms are not reproduced.
use crate::{crop::Crop, imaging::Pixels, native_raw, xmp};
use anyhow::{Context, Result, ensure};
use eframe::egui::Vec2;
use std::{
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

pub struct Request {
    pub source: PathBuf,
    pub xmp: String,
    pub output: PathBuf,
    pub max_edge: u32,
}
pub struct SceneRequest {
    pub source: PathBuf,
    pub xmp: String,
    pub max_edge: u32,
}
pub struct SceneRendered {
    pub frame: crate::raw_color::Frame,
    pub timings: Timings,
}
/// High-precision editor path. No exposure, Look, tone or sRGB encoding is baked
/// into the returned ProPhoto pixels; those operations share one display recipe.
pub fn render_scene(request: &SceneRequest) -> Result<SceneRendered> {
    let started = Instant::now();
    let generation = GENERATION.load(Ordering::Acquire);
    let cancelled = || GENERATION.load(Ordering::Acquire) != generation;
    let values = if request.xmp.trim().is_empty() {
        xmp::Values::default()
    } else {
        xmp::parse_values(&request.xmp)?
    };
    let wb = values
        .white_balance
        .as_ref()
        .filter(|w| w.mode.as_deref() != Some("As Shot"));
    let temperature = wb.and_then(|w| w.temperature).map(f64::from);
    let tint = wb.and_then(|w| w.tint).map(f64::from).unwrap_or(0.);
    let decoded = native_raw::develop_linear(
        &request.source,
        &native_raw::DevelopSettings {
            max_edge: request.max_edge,
            temperature,
            tint,
            exposure: 0.,
        },
        &cancelled,
    )?;
    let mut timings = Timings {
        raw_open_ms: decoded.open_ms,
        metadata_ms: decoded.open_ms,
        unpack_ms: decoded.unpack_ms,
        process_ms: decoded.process_ms,
        ..Default::default()
    };
    let color_started = Instant::now();
    let frame = crate::raw_color::prepare(decoded, &values, request.max_edge, &cancelled)?;
    timings.profile_ms = color_started.elapsed().as_secs_f64() * 1000.;
    timings.wall_ms = started.elapsed().as_secs_f64() * 1000.;
    Ok(SceneRendered { frame, timings })
}
#[derive(Clone, Debug, Default)]
pub struct Timings {
    pub copy_ms: f64,
    pub metadata_ms: f64,
    pub raw_open_ms: f64,
    pub profile_ms: f64,
    pub resize_ms: f64,
    pub write_ms: f64,
    pub wall_ms: f64,
    pub unpack_ms: f64,
    pub process_ms: f64,
}
pub struct Rendered {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub profile: String,
    pub temperature: Option<f64>,
    pub tint: Option<f64>,
    pub timings: Timings,
}
static GENERATION: AtomicU64 = AtomicU64::new(0);
pub fn cancel_pending() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    native_raw::cancel_develop();
}

pub fn render(request: &Request) -> Result<Rendered> {
    let start = Instant::now();
    let generation = GENERATION.load(Ordering::Acquire);
    let cancelled = || GENERATION.load(Ordering::Acquire) != generation;
    ensure!(
        request
            .source
            .extension()
            .is_some_and(|e| ["arw", "nef", "dng"]
                .iter()
                .any(|s| e.eq_ignore_ascii_case(s))),
        "本地 RAW 显影仅支持 ARW、NEF、DNG"
    );
    let extension = request
        .output
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ensure!(
        matches!(extension.as_str(), "png" | "jpg" | "jpeg"),
        "RAW 输出必须是 PNG 或 JPEG"
    );
    let values = if request.xmp.trim().is_empty() {
        xmp::Values::default()
    } else {
        xmp::parse_values(&request.xmp)?
    };
    let wb = values.white_balance.as_ref();
    let temperature = wb
        .filter(|wb| wb.mode.as_deref() != Some("As Shot"))
        .and_then(|wb| wb.temperature)
        .map(f64::from);
    let tint = if temperature.is_some() {
        wb.and_then(|wb| wb.tint).map(f64::from).unwrap_or(0.0)
    } else {
        0.0
    };
    let exposure = f64::from(values.exposure.unwrap_or(0)) / 1_000_000.0;
    let decoded = native_raw::develop(
        &request.source,
        &native_raw::DevelopSettings {
            max_edge: request.max_edge,
            temperature,
            tint,
            exposure,
        },
        &cancelled,
    )?;
    ensure!(!cancelled(), "已取消本地 RAW 显影");
    let resize = Instant::now();
    let orientation = values.orientation.unwrap_or(decoded.orientation);
    let pixels = transform(
        decoded.pixels,
        values.crop.unwrap_or_default(),
        orientation,
        request.max_edge,
        &cancelled,
    )?;
    let resize_ms = resize.elapsed().as_secs_f64() * 1000.0;
    ensure!(!cancelled(), "已取消本地 RAW 显影");
    let parent = request.output.parent().context("RAW 输出缺少父目录")?;
    fs::create_dir_all(parent)?;
    let write = Instant::now();
    if extension == "png" {
        let mut output = BufWriter::new(fs::File::create(&request.output)?);
        let mut encoder = png::Encoder::new(&mut output, pixels.width as u32, pixels.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        encoder.set_source_srgb(png::SrgbRenderingIntent::RelativeColorimetric);
        encoder.write_header()?.write_image_data(&pixels.rgba)?;
        output.flush()?;
    } else {
        native_raw::write_jpeg(&request.output, &pixels)?;
    }
    Ok(Rendered {
        path: request.output.clone(),
        width: pixels.width as u32,
        height: pixels.height as u32,
        profile: "LibRaw camera matrix → sRGB (local WB model)".into(),
        temperature: decoded.temperature,
        tint: decoded.tint,
        timings: Timings {
            raw_open_ms: decoded.open_ms,
            metadata_ms: decoded.open_ms,
            unpack_ms: decoded.unpack_ms,
            process_ms: decoded.process_ms,
            resize_ms,
            write_ms: write.elapsed().as_secs_f64() * 1000.0,
            wall_ms: start.elapsed().as_secs_f64() * 1000.0,
            ..Timings::default()
        },
    })
}

fn transform(
    source: Pixels,
    crop: Crop,
    orientation: u16,
    max_edge: u32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Pixels> {
    let size = Vec2::new(source.width as f32, source.height as f32);
    let geometry = crop.geometry(size, orientation);
    let dimensions = geometry.rect.size();
    ensure!(
        dimensions.x > 0.0 && dimensions.y > 0.0,
        "无效 RAW 裁切区域"
    );
    let scale = if max_edge == 0 {
        1.0
    } else {
        (max_edge as f32 / dimensions.x.max(dimensions.y)).min(1.0)
    };
    let width = (dimensions.x * scale).round().max(1.0) as usize;
    let height = (dimensions.y * scale).round().max(1.0) as usize;
    ensure!(
        width.checked_mul(height).is_some_and(|n| n <= 160_000_000),
        "RAW 输出超出尺寸限制"
    );
    if !crop.enabled && orientation == 1 && width == source.width && height == source.height {
        return Ok(source);
    }
    let uv = geometry.uvs(size, orientation);
    let mut rgba = vec![0_u8; width * height * 4];
    for y in 0..height {
        if y % 64 == 0 {
            ensure!(!cancelled(), "已取消本地 RAW 几何处理");
        }
        let fy = (y as f32 + 0.5) / height as f32;
        let left = uv[0].lerp(uv[3], fy);
        let right = uv[1].lerp(uv[2], fy);
        for x in 0..width {
            let p = left.lerp(right, (x as f32 + 0.5) / width as f32);
            let sx = (p.x * source.width as f32 - 0.5).clamp(0.0, (source.width - 1) as f32);
            let sy = (p.y * source.height as f32 - 0.5).clamp(0.0, (source.height - 1) as f32);
            let x0 = sx as usize;
            let y0 = sy as usize;
            let x1 = (x0 + 1).min(source.width - 1);
            let y1 = (y0 + 1).min(source.height - 1);
            let dx = sx - x0 as f32;
            let dy = sy - y0 as f32;
            let out = (y * width + x) * 4;
            for c in 0..3 {
                let a = source.rgba[(y0 * source.width + x0) * 4 + c] as f32 * (1.0 - dx)
                    + source.rgba[(y0 * source.width + x1) * 4 + c] as f32 * dx;
                let b = source.rgba[(y1 * source.width + x0) * 4 + c] as f32 * (1.0 - dx)
                    + source.rgba[(y1 * source.width + x1) * 4 + c] as f32 * dx;
                rgba[out + c] = (a * (1.0 - dy) + b * dy).round() as u8;
            }
            rgba[out + 3] = 255;
        }
    }
    Ok(Pixels {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Pixels {
        Pixels {
            width: 4,
            height: 2,
            rgba: (0..8).flat_map(|v| [v * 20, 0, 0, 255]).collect(),
        }
    }
    #[test]
    fn geometry_applies_orientation_once() {
        let p = transform(sample(), Crop::default(), 6, 0, &|| false).unwrap();
        assert_eq!((p.width, p.height), (2, 4));
        assert_eq!(&p.rgba[..4], &[80, 0, 0, 255]);
    }
    #[test]
    fn crop_and_fit_share_one_sampling_pass() {
        let crop = Crop {
            enabled: true,
            right: 500_000,
            ..Crop::default()
        };
        let p = transform(sample(), crop, 1, 0, &|| false).unwrap();
        assert_eq!((p.width, p.height), (2, 2));
        assert_eq!(&p.rgba[..4], &[0, 0, 0, 255]);
    }
    #[test]
    fn geometry_cancellation_has_a_real_pixel_loop_seam() {
        assert!(transform(sample(), Crop::default(), 6, 0, &|| true).is_err());
    }
}
