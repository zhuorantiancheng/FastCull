//! Scene-referred local RAW development. Camera/profile processing stays in
//! floating point; exposure, Look and tone are deferred to the display recipe.
use crate::{
    dcp::{self, HsvMap, Profile, ToneCurve},
    native_raw::LinearDeveloped,
    white_balance::{self as wb, Matrix},
    xmp::{Values, WhiteBalance},
};
use anyhow::{Context, Result, ensure};
use eframe::egui::Vec2;

pub const PIPELINE_VERSION: &str = "scene-prophoto-v1";
pub const PROPHOTO_TO_XYZ: Matrix = [
    [0.7976749, 0.1351917, 0.0313534],
    [0.2880402, 0.7118741, 0.0000857],
    [0., 0., 0.82521],
];
const SRGB_TO_XYZ: Matrix = [
    [0.4124564, 0.3575761, 0.1804375],
    [0.2126729, 0.7151522, 0.072175],
    [0.0193339, 0.119192, 0.9503041],
];

#[derive(Clone)]
pub struct Recipe {
    pub look: Option<HsvMap>,
    pub curve: Vec<f32>,
    pub baseline_exposure: f32,
}
pub struct Frame {
    pub size: [usize; 2],
    pub pixels: Vec<[f32; 4]>,
    pub recipe: Recipe,
    pub grey: GreySampler,
    pub temperature: Option<f64>,
    pub tint: Option<f64>,
    pub profile: String,
    pub warning: Option<String>,
    pub profile_fingerprint: u64,
    pub make: String,
    pub model: String,
    pub requested_profile: String,
}
/// Source-camera samples, before HSM, Look, exposure, tone or display encoding.
#[derive(Clone)]
pub struct GreySampler {
    pub size: [usize; 2],
    pub samples: Vec<[f32; 3]>,
    pub uvs: [[f32; 2]; 4],
    pub applied_mul: [f64; 3],
    pub native_matrix: Matrix,
    pub profile_color: Option<[Matrix; 2]>,
    pub profile_illuminants: [f64; 2],
    pub profile_dual: bool,
    pub calibration: [Matrix; 2],
}
impl GreySampler {
    pub fn estimate(&self, uv: [f32; 2]) -> Result<WhiteBalance> {
        ensure!(
            uv.iter().all(|x| x.is_finite() && (0. ..=1.).contains(x)),
            "取样点须位于照片内"
        );
        let left = std::array::from_fn::<_, 2, _>(|c| {
            self.uvs[0][c] + (self.uvs[3][c] - self.uvs[0][c]) * uv[1]
        });
        let right = std::array::from_fn::<_, 2, _>(|c| {
            self.uvs[1][c] + (self.uvs[2][c] - self.uvs[1][c]) * uv[1]
        });
        let p = std::array::from_fn::<_, 2, _>(|c| left[c] + (right[c] - left[c]) * uv[0]);
        let [w, h] = self.size;
        ensure!(
            w > 0 && h > 0 && self.samples.len() == w * h,
            "灰点采样格无效"
        );
        let x = ((p[0] * w as f32) as usize).min(w - 1);
        let y = ((p[1] * h as f32) as usize).min(h - 1);
        let valid = |rgb: [f32; 3]| {
            (0..3).all(|c| {
                let sensor = f64::from(rgb[c]) / self.applied_mul[c];
                sensor.is_finite() && sensor > 0.0001 && sensor < 0.9999
            })
        };
        ensure!(
            valid(self.samples[y * w + x]),
            "灰点过暗或已剪切，请重新取样"
        );
        let mut sum = [0.; 3];
        let mut count = 0;
        let mut total = 0;
        for sy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
            for sx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                let rgb = self.samples[sy * w + sx];
                total += 1;
                if valid(rgb) {
                    for c in 0..3 {
                        sum[c] += f64::from(rgb[c]) / self.applied_mul[c];
                    }
                    count += 1;
                }
            }
        }
        ensure!(
            count >= 3 && count * 2 > total,
            "灰点区域过暗或无效，请选择较均匀的中性区域"
        );
        let neutral = sum.map(|x| x / count as f64);
        let mut temperature = 6500.;
        let mut tint = 0.;
        for _ in 0..20 {
            let matrix = if let Some(color) = self.profile_color {
                let w = weight(temperature, self.profile_illuminants, self.profile_dual);
                wb::mul(
                    wb::interpolate(self.calibration[0], self.calibration[1], w),
                    wb::interpolate(color[0], color[1], w),
                )
            } else {
                self.native_matrix
            };
            let xyz = wb::apply(wb::inverse(matrix).context("白平衡矩阵不可逆")?, neutral);
            let (next, t) =
                wb::temperature_tint(xyz).context("该取样点超出白平衡范围，请改选中性灰区域")?;
            let converged = (next - temperature).abs() < 0.05;
            temperature = next;
            tint = t;
            if converged {
                break;
            }
        }
        let result = WhiteBalance::Custom {
            temperature: temperature.round() as u32,
            tint: tint.round() as i32,
        };
        ensure!(result.valid(), "灰点白平衡超出可编辑范围");
        Ok(result)
    }
}
pub fn weight(t: f64, illuminants: [f64; 2], dual: bool) -> f64 {
    let [a, b] = illuminants;
    if !dual || (1. / a - 1. / b).abs() < 1e-12 {
        return 1.;
    }
    ((1. / t - 1. / b) / (1. / a - 1. / b)).clamp(0., 1.)
}
fn camera_calibration(profile: &Profile, w: f64) -> Matrix {
    let matches = profile.camera_calibration_signature == profile.profile_calibration_signature;
    let cc = if matches {
        wb::interpolate(
            profile.calibration[0].unwrap_or(wb::IDENTITY),
            profile.calibration[1].unwrap_or(wb::IDENTITY),
            w,
        )
    } else {
        wb::IDENTITY
    };
    wb::mul(wb::diagonal(profile.analog_balance), cc)
}
pub fn find_profile(make: &str, model: &str, name: &str) -> Result<Option<Profile>> {
    dcp::find(&format!("{make} {model}"), name)
}
pub fn requested_profile(values: &Values) -> &str {
    values.camera_profile.as_deref().unwrap_or("Adobe Standard")
}
/// Sparse monotone approximation of the public classic SDR rendering curve.
/// Numerical landmarks sampled from Adobe's public dng_render.cpp; this local
/// baseline is not a reproduction of current Lightroom ProcessVersion tone.
pub fn default_tone() -> ToneCurve {
    ToneCurve::new(vec![
        [0., 0.],
        [0.01, 0.0083],
        [0.03, 0.0377472],
        [0.06, 0.0987448],
        [0.12, 0.2468688],
        [0.18, 0.3880856],
        [0.3, 0.597212],
        [0.5, 0.80486],
        [0.75, 0.93986],
        [0.9, 0.982656],
        [1., 1.],
    ])
    .expect("compiled monotone SDR curve")
}
fn normalized_forward(m: Matrix) -> Result<Matrix> {
    let white = wb::apply(m, [1.; 3]);
    ensure!(
        white.iter().all(|x| x.is_finite() && *x > 1e-7),
        "DCP 前向矩阵白点无效"
    );
    Ok(wb::mul(
        wb::diagonal(std::array::from_fn(|i| wb::D50[i] / white[i])),
        m,
    ))
}
pub fn prepare(
    source: LinearDeveloped,
    values: &Values,
    max_edge: u32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Frame> {
    let name = requested_profile(values).to_string();
    let profile = find_profile(&source.make, &source.model, &name)?;
    prepare_using_profile(source, values, max_edge, profile, cancelled)
}
pub fn prepare_using_profile(
    mut source: LinearDeveloped,
    values: &Values,
    max_edge: u32,
    profile: Option<Profile>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Frame> {
    ensure!(
        source.width > 0
            && source.height > 0
            && source.width.checked_mul(source.height) == Some(source.rgb.len()),
        "线性相机图像尺寸不符"
    );
    let name = requested_profile(values).to_string();
    let native: Matrix = std::array::from_fn(|i| source.camera_matrix[i].map(f64::from));
    let applied: [f64; 3] = std::array::from_fn(|i| f64::from(source.applied_mul[i]));
    ensure!(
        applied.iter().all(|x| x.is_finite() && *x > 0.),
        "RAW 白平衡增益无效"
    );
    let explicit = values
        .white_balance
        .as_ref()
        .filter(|w| w.mode.as_deref() != Some("As Shot"))
        .and_then(|w| {
            w.temperature
                .map(|t| (f64::from(t), f64::from(w.tint.unwrap_or(0))))
        });
    let mut temperature = explicit
        .map(|v| v.0)
        .or(source.temperature)
        .unwrap_or(6500.);
    let mut tint = explicit.map(|v| v.1).or(source.tint).unwrap_or(0.);
    let mut white_known =
        explicit.is_some() || (source.temperature.is_some() && source.tint.is_some());
    // Resolve As Shot against the selected profile, retaining its camera gains.
    if explicit.is_none()
        && let Some(profile) = &profile
    {
        let neutral = applied.map(|x| 1. / x);
        for _ in 0..20 {
            let w = profile.weight(temperature);
            let cm = wb::mul(
                camera_calibration(profile, w),
                wb::interpolate(profile.color[0], profile.color[1], w),
            );
            let xyz = wb::apply(wb::inverse(cm).context("DCP 色彩矩阵不可逆")?, neutral);
            let Some((t, new_tint)) = wb::temperature_tint(xyz) else {
                break;
            };
            let done = (t - temperature).abs() < 0.05;
            temperature = t;
            tint = new_tint;
            white_known = true;
            if done {
                break;
            }
        }
    }
    let mut correction = [1.; 3];
    let mut hsm = None;
    let xyz_to_prophoto = wb::inverse(PROPHOTO_TO_XYZ).unwrap();
    let (
        matrix,
        recipe,
        profile_name,
        warning,
        fingerprint,
        profile_color,
        illuminants,
        dual,
        calibrations,
    ) = if let Some(profile) = &profile {
        let w = profile.weight(temperature);
        let cc = camera_calibration(profile, w);
        let cm = wb::mul(cc, wb::interpolate(profile.color[0], profile.color[1], w));
        let inv_cc = wb::inverse(cc).context("DCP 校准矩阵不可逆")?;
        let desired = if let Some((t, tint)) = explicit {
            let response = wb::apply(cm, wb::xyz(t, tint).context("白平衡白点无效")?);
            ensure!(
                response.iter().all(|x| x.is_finite() && *x > 1e-7),
                "DCP 白点超出相机响应范围"
            );
            let gains = response.map(|x| 1. / x);
            let gains = gains.map(|x| x / gains[1]);
            correction = std::array::from_fn(|i| gains[i] / applied[i]);
            gains
        } else {
            applied
        };
        let neutral = desired.map(|x| 1. / x);
        let to_xyz = match (profile.forward[0], profile.forward[1]) {
            (Some(a), Some(b)) => {
                let fm = normalized_forward(wb::interpolate(a, b, w))?;
                let ref_neutral = wb::apply(inv_cc, neutral);
                ensure!(
                    ref_neutral.iter().all(|x| *x > 1e-7),
                    "DCP 参考相机白点无效"
                );
                wb::mul(
                    wb::mul(
                        wb::mul(fm, wb::diagonal(ref_neutral.map(|x| 1. / x))),
                        inv_cc,
                    ),
                    wb::diagonal(neutral),
                )
            }
            (Some(fm), None) | (None, Some(fm)) => {
                let ref_neutral = wb::apply(inv_cc, neutral);
                ensure!(
                    ref_neutral.iter().all(|x| *x > 1e-7),
                    "DCP 参考相机白点无效"
                );
                wb::mul(
                    wb::mul(
                        wb::mul(
                            normalized_forward(fm)?,
                            wb::diagonal(ref_neutral.map(|x| 1. / x)),
                        ),
                        inv_cc,
                    ),
                    wb::diagonal(neutral),
                )
            }
            _ => {
                let inverse = wb::inverse(cm).context("DCP 色彩矩阵不可逆")?;
                let white = wb::apply(inverse, neutral);
                normalized_forward(wb::mul(
                    wb::mul(
                        wb::adapt(white, wb::D50).context("DCP 白点适应失败")?,
                        inverse,
                    ),
                    wb::diagonal(neutral),
                ))?
            }
        };
        hsm = profile.hue_sat_at(temperature)?;
        let curve = profile.tone.clone().unwrap_or_else(default_tone);
        (
            wb::mul(xyz_to_prophoto, to_xyz),
            Recipe {
                look: profile.look.clone(),
                curve: (0..4096).map(|i| curve.eval(i as f32 / 4095.)).collect(),
                baseline_exposure: source.baseline_exposure as f32 + profile.baseline_exposure,
            },
            format!("{} · {}", profile.name, source.model),
            profile.warning.clone(),
            profile.fingerprint,
            Some(profile.color),
            profile.illuminants,
            profile.dual_illuminant,
            [
                camera_calibration(profile, 1.),
                camera_calibration(profile, 0.),
            ],
        )
    } else {
        let matrix: Matrix = std::array::from_fn(|i| {
            std::array::from_fn(|j| f64::from(source.camera_to_srgb[i][j]))
        });
        let curve = default_tone();
        (
            wb::mul(
                wb::mul(
                    wb::mul(xyz_to_prophoto, wb::adapt(wb::D65, wb::D50).unwrap()),
                    SRGB_TO_XYZ,
                ),
                matrix,
            ),
            Recipe {
                look: None,
                curve: (0..4096).map(|i| curve.eval(i as f32 / 4095.)).collect(),
                baseline_exposure: source.baseline_exposure as f32,
            },
            "相机矩阵 · 本地 SDR".to_string(),
            Some(format!(
                "未找到 {} 的 {name} 配置文件，使用相机矩阵",
                source.model
            )),
            0,
            None,
            [2850., 6500.],
            false,
            [wb::IDENTITY; 2],
        )
    };
    ensure!(
        matrix.iter().flatten().all(|x| x.is_finite()),
        "显影色彩矩阵无效"
    );
    let size = Vec2::new(source.width as f32, source.height as f32);
    let crop = values.crop.unwrap_or_default();
    let orientation = values.orientation.unwrap_or(source.orientation);
    let geometry = crop.geometry(size, orientation);
    let uvs = geometry.uvs(size, orientation);
    let sample_size = [source.width.min(128), source.height.min(128)];
    let mut samples = Vec::with_capacity(sample_size[0] * sample_size[1]);
    for y in 0..sample_size[1] {
        for x in 0..sample_size[0] {
            let sx = (2 * x + 1) * source.width / (2 * sample_size[0]);
            let sy = (2 * y + 1) * source.height / (2 * sample_size[1]);
            samples.push(source.rgb[sy * source.width + sx]);
        }
    }
    let grey = GreySampler {
        size: sample_size,
        samples,
        uvs: uvs.map(|p| [p.x, p.y]),
        applied_mul: applied,
        native_matrix: native,
        profile_color,
        profile_illuminants: illuminants,
        profile_dual: dual,
        calibration: calibrations,
    };
    let dimensions = geometry.rect.size();
    let scale = if max_edge == 0 {
        1.
    } else {
        (max_edge as f32 / dimensions.max_elem()).min(1.)
    };
    let output = [
        (dimensions.x * scale).round().max(1.) as usize,
        (dimensions.y * scale).round().max(1.) as usize,
    ];
    let count = output[0]
        .checked_mul(output[1])
        .context("线性预览尺寸溢出")?;
    ensure!(count <= 160_000_000, "线性预览超出像素限制");
    let direct = !crop.enabled && orientation == 1 && output == [source.width, source.height];
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(count)?;
    for y in 0..output[1] {
        if y % 32 == 0 {
            ensure!(!cancelled(), "线性显影已取消");
        }
        let fy = (y as f32 + 0.5) / output[1] as f32;
        let left = uvs[0].lerp(uvs[3], fy);
        let right = uvs[1].lerp(uvs[2], fy);
        for x in 0..output[0] {
            let rgb = if direct {
                source.rgb[y * source.width + x]
            } else {
                let p = left.lerp(right, (x as f32 + 0.5) / output[0] as f32);
                let sx = (p.x * source.width as f32 - 0.5).clamp(0., (source.width - 1) as f32);
                let sy = (p.y * source.height as f32 - 0.5).clamp(0., (source.height - 1) as f32);
                let (x0, y0) = (sx as usize, sy as usize);
                let (x1, y1) = (
                    (x0 + 1).min(source.width - 1),
                    (y0 + 1).min(source.height - 1),
                );
                let (dx, dy) = (sx - x0 as f32, sy - y0 as f32);
                std::array::from_fn(|c| {
                    (source.rgb[y0 * source.width + x0][c] * (1. - dx)
                        + source.rgb[y0 * source.width + x1][c] * dx)
                        * (1. - dy)
                        + (source.rgb[y1 * source.width + x0][c] * (1. - dx)
                            + source.rgb[y1 * source.width + x1][c] * dx)
                            * dy
                })
            };
            let mut out = wb::apply(
                matrix,
                std::array::from_fn(|i| f64::from(rgb[i]) * correction[i]),
            )
            .map(|x| x as f32);
            if let Some(map) = &hsm {
                out = map.apply(out);
            }
            ensure!(
                out.iter().all(|x| x.is_finite() && x.abs() < 65504.),
                "显影结果超出浮点显示范围"
            );
            pixels.push([out[0], out[1], out[2], 1.]);
        }
    }
    source.rgb.clear();
    Ok(Frame {
        size: output,
        pixels,
        recipe,
        grey,
        temperature: white_known.then_some(temperature),
        tint: white_known.then_some(tint),
        profile: profile_name,
        warning,
        profile_fingerprint: fingerprint,
        make: source.make,
        model: source.model,
        requested_profile: name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tone_landmarks_are_monotone_and_do_not_change_white_or_black() {
        let c = default_tone();
        assert_eq!(c.eval(0.), 0.);
        assert_eq!(c.eval(1.), 1.);
        assert!((c.eval(0.18) - 0.3880856).abs() < 1e-6);
        for i in 0..4095 {
            assert!(c.eval((i + 1) as f32 / 4095.) >= c.eval(i as f32 / 4095.));
        }
    }
    #[test]
    fn normalized_forward_keeps_neutral_white_and_rejects_singular_white() {
        let fm = [
            [0.4861, 0.3051, 0.1731],
            [0.2813, 0.6649, 0.0537],
            [0.1277, 0.0027, 0.6947],
        ];
        let white = wb::apply(normalized_forward(fm).unwrap(), [1.; 3]);
        assert_eq!(white, wb::D50);
        assert!(normalized_forward([[0.; 3]; 3]).is_err());
    }
    fn fixture() -> LinearDeveloped {
        LinearDeveloped {
            width: 4,
            height: 2,
            rgb: vec![[1.5, 1.5, 1.5]; 8],
            orientation: 1,
            temperature: Some(5500.),
            tint: Some(0.),
            camera_multipliers: [2., 1., 1.5, 1.],
            applied_mul: [2., 1., 1.5, 1.],
            camera_matrix: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [0.; 3]],
            camera_to_srgb: [[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]],
            baseline_exposure: 0.,
            make: "Fixture".into(),
            model: "Fixture".into(),
            open_ms: 0.,
            unpack_ms: 0.,
            process_ms: 0.,
        }
    }
    #[test]
    fn floating_geometry_keeps_headroom_and_defers_exposure() {
        let values = Values {
            exposure: Some(-2_000_000),
            orientation: Some(6),
            crop: Some(crate::crop::Crop {
                enabled: true,
                right: 500_000,
                ..Default::default()
            }),
            ..Default::default()
        };
        let frame = prepare_using_profile(fixture(), &values, 0, None, &|| false).unwrap();
        assert_eq!(frame.size, [2, 2]);
        assert!(
            frame.pixels.iter().all(|p| p[..3].iter().all(|v| *v > 1.4)),
            "camera headroom was clipped or exposure baked twice"
        );
        assert_eq!(frame.recipe.baseline_exposure, 0.);
        assert!(prepare_using_profile(fixture(), &values, 0, None, &|| true).is_err());
    }
    #[test]
    fn unknown_camera_white_balance_is_not_reported_as_a_default() {
        let mut source = fixture();
        source.temperature = None;
        source.tint = None;
        let frame = prepare_using_profile(source, &Values::default(), 0, None, &|| false).unwrap();
        assert_eq!((frame.temperature, frame.tint), (None, None));
    }
    #[test]
    fn grey_picker_uses_sensor_response_and_shared_white_model() {
        let t = 4850.;
        let tint = 17.;
        let xyz = wb::xyz(t, tint).unwrap();
        let camera_response = xyz.map(|v| v * 0.1);
        let initial = [2., 1., 1.5];
        let sampler = GreySampler {
            size: [3, 3],
            samples: vec![std::array::from_fn(|i| (camera_response[i] * initial[i]) as f32); 9],
            uvs: [[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
            applied_mul: initial,
            native_matrix: wb::IDENTITY,
            profile_color: None,
            profile_illuminants: [2850., 6500.],
            profile_dual: false,
            calibration: [wb::IDENTITY; 2],
        };
        let actual = sampler.estimate([0.5, 0.5]).unwrap();
        if let WhiteBalance::Custom {
            temperature,
            tint: actual_tint,
        } = actual
        {
            assert!((temperature as f64 - t).abs() < 2.);
            assert!((actual_tint - tint as i32).abs() <= 1);
        } else {
            panic!("grey estimate is not Custom");
        }
        let mut clipped = sampler.clone();
        clipped.samples.fill([2.; 3]);
        assert!(clipped.estimate([0.5, 0.5]).is_err());
        for (channel, gain) in initial.iter().enumerate() {
            let mut clipped = sampler.clone();
            clipped.samples[4][channel] = *gain as f32;
            assert!(
                clipped.estimate([0.5, 0.5]).is_err(),
                "single-channel sensor clipping must be rejected"
            );
        }
        let mut mixed = sampler.clone();
        for index in [0, 1, 2, 3, 5] {
            mixed.samples[index][1] = 1.;
        }
        assert!(
            mixed.estimate([0.5, 0.5]).is_err(),
            "mostly clipped neighbourhood must be rejected"
        );
    }
}
