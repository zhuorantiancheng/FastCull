//! A small sampling map and a neutral-point white-balance ESTIMATE.
//!
//! Samples are averaged in linear sRGB. Temperature follows the red/blue residual
//! against a Planck spectrum with representative 610/550/460 nm bands; tint uses
//! the remaining green/magenta residual. These are approximate display-RGB bands,
//! not the camera spectral sensitivities or a Lightroom eyedropper implementation.
//! A missing effective RAW white balance uses an explicitly flagged 6500 K / 0
//! solver initial guess. The result is editable and must be checked after the
//! local RAW renderer rerenders it; the picker never renders synchronously.
use crate::xmp::WhiteBalance;
use anyhow::{Result, ensure};
use eframe::egui::ColorImage;

const MAP_SIDE: usize = 128;
const INITIAL_TEMPERATURE: u32 = 6500;
const SECOND_RADIATION_CONSTANT: f64 = 1.438_776_877e-2;
const BANDS: [f64; 3] = [610e-9, 550e-9, 460e-9];
const TINT_SCALE: f64 = 100.;

#[derive(Clone, Debug)]
pub struct SampleMap {
    size: [usize; 2],
    rgba: Vec<[u8; 4]>,
}
fn linear(value: u8) -> f32 {
    let v = value as f32 / 255.;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn usable(rgb: [f32; 3]) -> bool {
    let luminance = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    rgb.into_iter()
        .all(|v| v.is_finite() && v > 0.001 && v < 0.956)
        && (0.005..=0.90).contains(&luminance)
}
fn pixel(rgba: [u8; 4]) -> Option<[f32; 3]> {
    if rgba[3] != 255 || rgba[..3].iter().any(|v| *v >= 250) {
        return None;
    }
    let rgb = [linear(rgba[0]), linear(rgba[1]), linear(rgba[2])];
    usable(rgb).then_some(rgb)
}
impl SampleMap {
    /// O(128²) center sampling, at most 64 KiB; never clones the full-size image.
    pub fn from_image(image: &ColorImage) -> Result<Self> {
        let [width, height] = image.size;
        ensure!(
            width > 0 && height > 0 && width.checked_mul(height) == Some(image.pixels.len()),
            "无法从无效图像建立白平衡采样格"
        );
        let size = [width.min(MAP_SIDE), height.min(MAP_SIDE)];
        let mut rgba = Vec::with_capacity(size[0] * size[1]);
        for y in 0..size[1] {
            let sy = (((2 * y + 1) as u128 * height as u128) / (2 * size[1]) as u128) as usize;
            for x in 0..size[0] {
                let sx = (((2 * x + 1) as u128 * width as u128) / (2 * size[0]) as u128) as usize;
                rgba.push(image.pixels[sy * width + sx].to_array());
            }
        }
        Ok(Self { size, rgba })
    }
    #[cfg(test)]
    pub fn size(&self) -> [usize; 2] {
        self.size
    }
    /// Normalized image coordinates. Returns the valid 3×3 neighborhood mean in
    /// LINEAR sRGB, with bounded neighborhoods at the image edges.
    pub fn sample(&self, uv: [f32; 2]) -> Result<[f32; 3]> {
        ensure!(
            uv.into_iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
            "白平衡取样点必须位于图像内"
        );
        let x = ((uv[0] * self.size[0] as f32) as usize).min(self.size[0] - 1);
        let y = ((uv[1] * self.size[1] as f32) as usize).min(self.size[1] - 1);
        ensure!(
            pixel(self.rgba[y * self.size[0] + x]).is_some(),
            "取样点过暗、过曝或无效，请选择未剪切的中性灰区域"
        );
        let mut sum = [0.; 3];
        let (mut count, mut total) = (0, 0);
        for sy in y.saturating_sub(1)..=(y + 1).min(self.size[1] - 1) {
            for sx in x.saturating_sub(1)..=(x + 1).min(self.size[0] - 1) {
                total += 1;
                if let Some(rgb) = pixel(self.rgba[sy * self.size[0] + sx]) {
                    for channel in 0..3 {
                        sum[channel] += rgb[channel];
                    }
                    count += 1;
                }
            }
        }
        ensure!(
            count * 2 >= total,
            "取样区域含太多暗部或剪切像素，请选择较均匀的灰色区域"
        );
        Ok(sum.map(|v| v / count as f32))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WhiteBalanceEstimate {
    pub white_balance: WhiteBalance,
    /// The starting white balance was unknown; 6500/0 was only a solver seed.
    pub used_initial_guess: bool,
}
fn planck_chroma(temperature: f64) -> [f64; 2] {
    let log = BANDS.map(|wavelength| {
        -5. * wavelength.ln()
            - (SECOND_RADIATION_CONSTANT / (wavelength * temperature))
                .exp_m1()
                .ln()
    });
    [log[0] - log[2], log[1] - (log[0] + log[2]) / 2.]
}
fn solve_temperature(red_blue: f64) -> f64 {
    let (mut low, mut high) = (2000., 50000.);
    if red_blue >= planck_chroma(low)[0] {
        return low;
    }
    if red_blue <= planck_chroma(high)[0] {
        return high;
    }
    for _ in 0..40 {
        let middle = (low + high) / 2.;
        if planck_chroma(middle)[0] > red_blue {
            low = middle;
        } else {
            high = middle;
        }
    }
    (low + high) / 2.
}
/// Estimate an absolute Custom setting from a linear-sRGB neutral sample. Supply
/// the CURRENT effective temperature/tint as Custom when those values are known,
/// including resolved As Shot values. Unknown As Shot is not assumed to be 6500 K.
pub fn estimate(rgb: [f32; 3], current: Option<WhiteBalance>) -> Result<WhiteBalanceEstimate> {
    ensure!(usable(rgb), "样点过暗、过曝或无效，无法估算白平衡");
    let (temperature, tint, used_initial_guess) = match current {
        Some(wb @ WhiteBalance::Custom { temperature, tint }) if wb.valid() => {
            (temperature, tint, false)
        }
        _ => (INITIAL_TEMPERATURE, 0, true),
    };
    let log = rgb.map(|v| (v as f64).ln());
    let red_blue = log[0] - log[2];
    let green_magenta = log[1] - (log[0] + log[2]) / 2.;
    let baseline = planck_chroma(temperature as f64);
    let next_temperature = if red_blue.abs() < 1e-7 {
        temperature as f64
    } else {
        solve_temperature(baseline[0] + red_blue)
    };
    let temperature_green = planck_chroma(next_temperature)[1] - baseline[1];
    let next_tint = (tint as f64 + TINT_SCALE * (green_magenta - temperature_green))
        .round()
        .clamp(-150., 150.) as i32;
    Ok(WhiteBalanceEstimate {
        white_balance: WhiteBalance::Custom {
            temperature: next_temperature.round().clamp(2000., 50000.) as u32,
            tint: next_tint,
        },
        used_initial_guess,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    #[test]
    fn neutral_sample_keeps_known_balance_and_unknown_baseline_is_flagged() {
        let wb = WhiteBalance::Custom {
            temperature: 4850,
            tint: 17,
        };
        let solved = estimate([0.2; 3], Some(wb)).unwrap();
        assert_eq!(solved.white_balance, wb);
        assert!(!solved.used_initial_guess);
        for unknown in [
            None,
            Some(WhiteBalance::AsShot),
            Some(WhiteBalance::Custom {
                temperature: 0,
                tint: 0,
            }),
        ] {
            let solved = estimate([0.2; 3], unknown).unwrap();
            assert!(solved.used_initial_guess);
            assert!(solved.white_balance.valid());
        }
    }
    #[test]
    fn temperature_and_tint_counter_the_observed_color_cast_within_limits() {
        let wb = Some(WhiteBalance::Custom {
            temperature: 6500,
            tint: 0,
        });
        let temperature = |rgb| match estimate(rgb, wb).unwrap().white_balance {
            WhiteBalance::Custom { temperature, .. } => temperature,
            _ => unreachable!(),
        };
        assert!(temperature([0.6, 0.5, 0.4]) < 6500);
        assert!(temperature([0.4, 0.5, 0.6]) > 6500);
        for (rgb, sign) in [([0.4, 0.6, 0.4], 1), ([0.6, 0.4, 0.6], -1)] {
            let WhiteBalance::Custom { tint, .. } = estimate(rgb, wb).unwrap().white_balance else {
                unreachable!()
            };
            assert_eq!(tint.signum(), sign);
        }
        assert!(
            estimate([0.8, 0.2, 0.01], wb)
                .unwrap()
                .white_balance
                .valid()
        );
    }
    #[test]
    fn sample_map_is_bounded_and_averages_linear_values_without_mutating_image() {
        let mut image = ColorImage::filled([1024, 512], Color32::from_rgb(128, 128, 128));
        let original = image.pixels[0];
        let map = SampleMap::from_image(&image).unwrap();
        assert_eq!(map.size(), [128, 128]);
        assert_eq!(map.rgba.len(), 16384);
        assert_eq!(image.pixels[0], original);
        for uv in [[0., 0.], [1., 1.], [0.5, 0.5]] {
            for v in map.sample(uv).unwrap() {
                assert!((v - linear(128)).abs() < 1e-6);
            }
        }
        image.pixels.clear();
        assert!(SampleMap::from_image(&image).is_err());
    }
    #[test]
    fn picker_rejects_black_clipped_transparent_and_invalid_coordinates() {
        for color in [
            Color32::BLACK,
            Color32::WHITE,
            Color32::from_rgb(255, 128, 128),
            Color32::TRANSPARENT,
        ] {
            let image = ColorImage::filled([3, 3], color);
            assert!(
                SampleMap::from_image(&image)
                    .unwrap()
                    .sample([0.5, 0.5])
                    .is_err()
            );
        }
        let map = SampleMap::from_image(&ColorImage::filled([3, 3], Color32::GRAY)).unwrap();
        for uv in [
            [f32::NAN, 0.5],
            [0.5, f32::INFINITY],
            [-0.1, 0.5],
            [1.1, 0.5],
        ] {
            assert!(map.sample(uv).is_err());
        }
        for rgb in [[0.; 3], [1.; 3], [f32::NAN, 0.2, 0.2], [-0.1, 0.2, 0.2]] {
            assert!(estimate(rgb, None).is_err());
        }
    }
    #[test]
    fn isolated_valid_pixel_is_not_mistaken_for_a_reliable_neutral_area() {
        let mut image = ColorImage::filled([3, 3], Color32::BLACK);
        image.pixels[4] = Color32::GRAY;
        assert!(
            SampleMap::from_image(&image)
                .unwrap()
                .sample([0.5, 0.5])
                .is_err()
        );
        image.pixels.fill(Color32::from_gray(128));
        image.pixels[0] = Color32::BLACK;
        let sampled = SampleMap::from_image(&image)
            .unwrap()
            .sample([0.5, 0.5])
            .unwrap();
        assert!(sampled.into_iter().all(|v| (v - linear(128)).abs() < 1e-6));
    }
}
