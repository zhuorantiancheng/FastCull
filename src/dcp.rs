//! Bounded, read-only DNG camera profile support. No Adobe code or profiles are
//! distributed. HSV tables follow DNG 1.6 section ProfileHueSatMapEncoding:
//! only V is sRGB-encoded for a three-dimensional table; correction is applied
//! in that encoded HSV domain, then V is decoded. A 2.5D table ignores encoding.
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub type Matrix3 = [[f64; 3]; 3];
const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_CELLS: usize = 1_048_576;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Linear,
    Srgb,
}

#[derive(Clone, Debug)]
pub struct HsvMap {
    /// Hue, saturation, value counts. Saturation is the fastest-changing index.
    pub dims: [usize; 3],
    /// Hue shift in degrees, saturation multiplier, value multiplier.
    pub data: Vec<[f32; 3]>,
    pub encoding: Encoding,
}
impl HsvMap {
    pub fn new(dims: [usize; 3], data: Vec<[f32; 3]>, encoding: Encoding) -> Result<Self> {
        ensure!(
            dims[0] > 0 && dims[1] >= 2 && dims[2] > 0,
            "DCP HSV 表尺寸无效"
        );
        let count = dims[0]
            .checked_mul(dims[1])
            .and_then(|n| n.checked_mul(dims[2]));
        ensure!(
            count.is_some_and(|n| n <= MAX_CELLS && n == data.len()),
            "DCP HSV 表长度与尺寸不符"
        );
        ensure!(
            data.iter().all(|v| v.iter().all(|x| x.is_finite())
                && v[0].abs() <= 360.
                && (0. ..=10000.).contains(&v[1])
                && (0. ..=10000.).contains(&v[2])),
            "DCP HSV 表含无效数值"
        );
        // The DNG format requires value scale 1 at zero saturation. Refuse a
        // malformed map rather than turning an achromatic input into a tone edit.
        for v in 0..dims[2] {
            for h in 0..dims[0] {
                ensure!(
                    (data[(v * dims[0] + h) * dims[1]][2] - 1.).abs() <= 0.0001,
                    "DCP HSV 灰轴明度增益须为 1"
                );
            }
        }
        Ok(Self {
            dims,
            data,
            encoding,
        })
    }
    pub fn sample(&self, h: f32, s: f32, v: f32) -> [f32; 3] {
        if !h.is_finite() || !s.is_finite() || !v.is_finite() {
            return [0., 1., 1.];
        }
        let [nh, ns, nv] = self.dims;
        let hue = h.rem_euclid(360.) / 360. * nh as f32;
        let h0 = hue.floor() as usize % nh;
        let h1 = (h0 + 1) % nh;
        let fh = hue.fract();
        let sat = s.clamp(0., 1.) * (ns - 1) as f32;
        let s0 = (sat.floor() as usize).min(ns - 2);
        let fs = sat - s0 as f32;
        let value = v.clamp(0., 1.) * (nv - 1) as f32;
        let v0 = if nv == 1 {
            0
        } else {
            (value.floor() as usize).min(nv - 2)
        };
        let v1 = (v0 + 1).min(nv - 1);
        let fv = value - v0 as f32;
        let at = |hi, si, vi| self.data[(vi * nh + hi) * ns + si];
        let lerp =
            |a: [f32; 3], b: [f32; 3], t: f32| std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t);
        let layer = |vi| {
            lerp(
                lerp(at(h0, s0, vi), at(h1, s0, vi), fh),
                lerp(at(h0, s0 + 1, vi), at(h1, s0 + 1, vi), fh),
                fs,
            )
        };
        lerp(layer(v0), layer(v1), fv)
    }
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        if !rgb.iter().all(|x| x.is_finite()) {
            return [0.; 3];
        }
        let mut hsv = rgb_to_hsv(rgb);
        if hsv[1] <= 1e-7 || hsv[2] <= 0. {
            return rgb;
        }
        let encoded = self.encoding == Encoding::Srgb && self.dims[2] > 1;
        if encoded {
            hsv[2] = srgb_encode(hsv[2]);
        }
        let correction = self.sample(hsv[0], hsv[1], hsv[2]);
        hsv[0] = (hsv[0] + correction[0]).rem_euclid(360.);
        hsv[1] = (hsv[1] * correction[1]).clamp(0., 1.);
        // Do not clamp highlight V to 1. The table indexing clamps, but the
        // original headroom remains in the floating point working image.
        hsv[2] *= correction[2];
        if encoded {
            hsv[2] = srgb_decode(hsv[2]);
        }
        hsv_to_rgb(hsv)
    }
    pub fn interpolate(&self, other: &Self, weight_first: f32) -> Result<Self> {
        ensure!(
            self.dims == other.dims && self.encoding == other.encoding,
            "DCP 双光源 HSV 表尺寸或编码不一致"
        );
        let w = weight_first.clamp(0., 1.);
        let data = self
            .data
            .iter()
            .zip(&other.data)
            .map(|(a, b)| std::array::from_fn(|c| a[c] * w + b[c] * (1. - w)))
            .collect();
        Self::new(self.dims, data, self.encoding)
    }
}
pub fn srgb_encode(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1. / 2.4) - 0.055
    }
}
pub fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
pub fn rgb_to_hsv(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    if max <= 0. || delta <= 1e-12 {
        return [0., 0., max];
    }
    let h = if max == r {
        (g - b) / delta
    } else if max == g {
        (b - r) / delta + 2.
    } else {
        (r - g) / delta + 4.
    };
    [(h * 60.).rem_euclid(360.), (delta / max).clamp(0., 1.), max]
}
pub fn hsv_to_rgb(hsv: [f32; 3]) -> [f32; 3] {
    let [h, s, v] = hsv;
    let h = h.rem_euclid(360.) / 60.;
    let c = v * s;
    let x = c * (1. - (h.rem_euclid(2.) - 1.).abs());
    let m = v - c;
    let rgb = match h as u32 {
        0 => [c, x, 0.],
        1 => [x, c, 0.],
        2 => [0., c, x],
        3 => [0., x, c],
        4 => [x, 0., c],
        _ => [c, 0., x],
    };
    rgb.map(|x| x + m)
}

#[derive(Clone, Debug)]
pub struct ToneCurve {
    pub points: Vec<[f32; 2]>,
    tangents: Vec<f32>,
}
impl ToneCurve {
    /// A monotone cubic interpolant avoids overshoot between profile knots.
    pub fn new(points: Vec<[f32; 2]>) -> Result<Self> {
        ensure!((2..=65536).contains(&points.len()), "DCP 色调曲线点数无效");
        ensure!(
            points
                .iter()
                .all(|p| p.iter().all(|v| v.is_finite() && (0. ..=1.).contains(v))),
            "DCP 色调曲线值超出范围"
        );
        ensure!(
            points
                .windows(2)
                .all(|p| p[1][0] > p[0][0] && p[1][1] >= p[0][1]),
            "DCP 色调曲线必须递增"
        );
        ensure!(
            points[0][0] == 0. && points.last().unwrap()[0] == 1.,
            "DCP 色调曲线须覆盖 0–1"
        );
        let n = points.len();
        let slopes: Vec<_> = points
            .windows(2)
            .map(|p| (p[1][1] - p[0][1]) / (p[1][0] - p[0][0]))
            .collect();
        let mut tangents = vec![0.; n];
        tangents[0] = slopes[0];
        tangents[n - 1] = slopes[n - 2];
        for i in 1..n - 1 {
            let (a, b) = (slopes[i - 1], slopes[i]);
            if a > 0. && b > 0. {
                let h0 = points[i][0] - points[i - 1][0];
                let h1 = points[i + 1][0] - points[i][0];
                let w1 = 2. * h1 + h0;
                let w2 = h1 + 2. * h0;
                tangents[i] = (w1 + w2) / (w1 / a + w2 / b);
            }
        }
        Ok(Self { points, tangents })
    }
    pub fn eval(&self, x: f32) -> f32 {
        if !x.is_finite() || x <= self.points[0][0] {
            return self.points[0][1];
        }
        let last = self.points.len() - 1;
        if x >= self.points[last][0] {
            return self.points[last][1];
        }
        let i = self.points.partition_point(|p| p[0] <= x) - 1;
        let [x0, y0] = self.points[i];
        let [x1, y1] = self.points[i + 1];
        let h = x1 - x0;
        let t = (x - x0) / h;
        let y = (2. * t * t * t - 3. * t * t + 1.) * y0
            + (t * t * t - 2. * t * t + t) * h * self.tangents[i]
            + (-2. * t * t * t + 3. * t * t) * y1
            + (t * t * t - t * t) * h * self.tangents[i + 1];
        y.clamp(y0, y1)
    }
}

#[derive(Clone, Debug)]
pub struct Profile {
    pub name: String,
    pub model: Option<String>,
    pub color: [Matrix3; 2],
    pub forward: [Option<Matrix3>; 2],
    pub illuminants: [f64; 2],
    pub dual_illuminant: bool,
    pub calibration: [Option<Matrix3>; 2],
    pub analog_balance: [f64; 3],
    pub camera_calibration_signature: Option<String>,
    pub profile_calibration_signature: Option<String>,
    pub hue_sat: [Option<HsvMap>; 2],
    pub look: Option<HsvMap>,
    pub tone: Option<ToneCurve>,
    pub baseline_exposure: f32,
    pub black_render: Option<u32>,
    pub fingerprint: u64,
    pub path: Option<PathBuf>,
    pub requested_name: Option<String>,
    pub fallback: bool,
    pub warning: Option<String>,
}
impl Profile {
    /// Reciprocal-temperature interpolation, clamped at calibration illuminants.
    pub fn weight(&self, temperature: f64) -> f64 {
        if !self.dual_illuminant || !temperature.is_finite() || temperature <= 0. {
            return 1.;
        }
        let [a, b] = self.illuminants;
        let denominator = 1. / a - 1. / b;
        if denominator.abs() < 1e-12 {
            return 1.;
        }
        ((1. / temperature - 1. / b) / denominator).clamp(0., 1.)
    }
    pub fn hue_sat_at(&self, temperature: f64) -> Result<Option<HsvMap>> {
        match (&self.hue_sat[0], &self.hue_sat[1]) {
            (Some(a), Some(b)) => Ok(Some(a.interpolate(b, self.weight(temperature) as f32)?)),
            (Some(a), None) => Ok(Some(a.clone())),
            (None, Some(b)) => Ok(Some(b.clone())),
            _ => Ok(None),
        }
    }
}

#[derive(Clone, Copy)]
struct Entry {
    kind: u16,
    count: usize,
    offset: usize,
}
struct Reader<'a> {
    bytes: &'a [u8],
    little: bool,
    tags: BTreeMap<u16, Entry>,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self> {
        ensure!(
            (8..=MAX_FILE as usize).contains(&bytes.len()),
            "DCP 文件尺寸超出限制"
        );
        let little = match &bytes[..2] {
            b"II" => true,
            b"MM" => false,
            _ => bail!("DCP 字节序无效"),
        };
        let mut r = Self {
            bytes,
            little,
            tags: BTreeMap::new(),
        };
        ensure!(
            matches!(r.u16(2)?, 42 | 0x4352),
            "DCP/TIFF 标记无效（不支持 BigTIFF）"
        );
        let offset = r.u32(4)? as usize;
        ensure!(offset >= 8, "DCP IFD 偏移无效");
        let count = r.u16(offset)? as usize;
        ensure!(count <= 4096, "DCP IFD 条目过多");
        let end = offset
            .checked_add(2)
            .and_then(|n| n.checked_add(count * 12))
            .and_then(|n| n.checked_add(4))
            .context("DCP IFD 大小溢出")?;
        ensure!(end <= bytes.len(), "DCP IFD 超出文件范围");
        for i in 0..count {
            let p = offset + 2 + i * 12;
            let tag = r.u16(p)?;
            let kind = r.u16(p + 2)?;
            let len = r.u32(p + 4)? as usize;
            let size = match kind {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                5 | 10 | 12 => 8,
                _ => {
                    ensure!(
                        !matches!(tag, 50708 | 50721..=50724 | 50727 | 50778 | 50779 | 50931 | 50932 | 50936..=50940 | 50964 | 50965 | 50981 | 50982 | 51107..=51110),
                        "DCP 标签 {tag} 使用不支持的数据类型 {kind}"
                    );
                    continue;
                }
            };
            let total = len.checked_mul(size).context("DCP 标签大小溢出")?;
            let data = if total <= 4 {
                p + 8
            } else {
                r.u32(p + 8)? as usize
            };
            ensure!(
                data.checked_add(total).is_some_and(|n| n <= bytes.len()),
                "DCP 标签 {tag} 数据越界"
            );
            ensure!(
                r.tags
                    .insert(
                        tag,
                        Entry {
                            kind,
                            count: len,
                            offset: data
                        }
                    )
                    .is_none(),
                "DCP 含重复标签 {tag}"
            );
        }
        Ok(r)
    }
    fn slice<const N: usize>(&self, p: usize) -> Result<[u8; N]> {
        self.bytes
            .get(p..p.checked_add(N).context("DCP 偏移溢出")?)
            .context("DCP 数据截断")?
            .try_into()
            .context("DCP 数据长度不符")
    }
    fn u16(&self, p: usize) -> Result<u16> {
        let b = self.slice(p)?;
        Ok(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }
    fn u32(&self, p: usize) -> Result<u32> {
        let b = self.slice(p)?;
        Ok(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }
    fn u64(&self, p: usize) -> Result<u64> {
        let b = self.slice(p)?;
        Ok(if self.little {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }
    fn text(&self, tag: u16) -> Result<Option<String>> {
        let Some(e) = self.tags.get(&tag) else {
            return Ok(None);
        };
        ensure!(
            matches!(e.kind, 2 | 7) && e.count <= 65536,
            "DCP 文本标签 {tag} 类型或长度无效"
        );
        let bytes = &self.bytes[e.offset..e.offset + e.count];
        let end = bytes.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
        let bytes = &bytes[..end];
        ensure!(!bytes.contains(&0), "DCP 文本标签 {tag} 含内嵌空字符");
        Ok(Some(
            std::str::from_utf8(bytes)
                .context("DCP 文本不是 UTF-8")?
                .trim()
                .to_owned(),
        ))
    }
    fn numbers(&self, tag: u16, max: usize) -> Result<Option<Vec<f64>>> {
        let Some(e) = self.tags.get(&tag) else {
            return Ok(None);
        };
        ensure!(e.count <= max, "DCP 标签 {tag} 元素过多");
        let mut out = Vec::with_capacity(e.count);
        for i in 0..e.count {
            let n = match e.kind {
                1 => self.bytes[e.offset + i] as f64,
                3 => self.u16(e.offset + i * 2)? as f64,
                4 => self.u32(e.offset + i * 4)? as f64,
                9 => (self.u32(e.offset + i * 4)? as i32) as f64,
                5 | 10 => {
                    let p = e.offset + i * 8;
                    let (a, b) = if e.kind == 5 {
                        (self.u32(p)? as f64, self.u32(p + 4)? as f64)
                    } else {
                        (
                            (self.u32(p)? as i32) as f64,
                            (self.u32(p + 4)? as i32) as f64,
                        )
                    };
                    ensure!(b != 0., "DCP 标签 {tag} 分母为零");
                    a / b
                }
                11 => f32::from_bits(self.u32(e.offset + i * 4)?) as f64,
                12 => f64::from_bits(self.u64(e.offset + i * 8)?),
                _ => bail!("DCP 数值标签 {tag} 类型不支持"),
            };
            ensure!(n.is_finite(), "DCP 标签 {tag} 包含非有限数值");
            out.push(n);
        }
        Ok(Some(out))
    }
    fn scalar(&self, tag: u16) -> Result<Option<f64>> {
        let Some(v) = self.numbers(tag, 1)? else {
            return Ok(None);
        };
        ensure!(v.len() == 1, "DCP 标签 {tag} 须为标量");
        Ok(Some(v[0]))
    }
    fn matrix(&self, tag: u16) -> Result<Option<Matrix3>> {
        let Some(v) = self.numbers(tag, 9)? else {
            return Ok(None);
        };
        ensure!(
            v.len() == 9 && v.iter().all(|n| n.abs() <= 1000.),
            "DCP 只支持三通道色彩矩阵"
        );
        let m: Matrix3 = std::array::from_fn(|i| std::array::from_fn(|j| v[i * 3 + j]));
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        ensure!(det.abs() > 1e-10, "DCP 矩阵不可逆");
        Ok(Some(m))
    }
    fn encoding(&self, tag: u16) -> Result<Encoding> {
        match self.scalar(tag)?.unwrap_or(0.) {
            0. => Ok(Encoding::Linear),
            1. => Ok(Encoding::Srgb),
            _ => bail!("DCP HSV 编码不支持"),
        }
    }
    fn map(&self, dims_tag: u16, data_tag: u16, encoding: Encoding) -> Result<Option<HsvMap>> {
        let (dims, values) = (
            self.numbers(dims_tag, 3)?,
            self.numbers(data_tag, MAX_CELLS * 3)?,
        );
        match (dims, values) {
            (None, None) => Ok(None),
            (Some(dims), Some(values)) => {
                ensure!(
                    dims.len() == 3
                        && dims
                            .iter()
                            .all(|v| *v >= 1. && v.fract() == 0. && *v <= MAX_CELLS as f64),
                    "DCP HSV 表维度无效"
                );
                ensure!(values.len() % 3 == 0, "DCP HSV 数据不是三元组");
                let data = values
                    .chunks_exact(3)
                    .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32])
                    .collect();
                Ok(Some(HsvMap::new(
                    [dims[0] as usize, dims[1] as usize, dims[2] as usize],
                    data,
                    encoding,
                )?))
            }
            (Some(_), None) => Ok(None), // A second map is optional even with shared dimensions.
            _ => bail!("DCP HSV 数据缺少尺寸"),
        }
    }
}

pub fn parse(bytes: &[u8]) -> Result<Profile> {
    let r = Reader::new(bytes)?;
    let first = r.matrix(50721)?.context("DCP 缺少 ColorMatrix1")?;
    let second = r.matrix(50722)?;
    let dual_illuminant = second.is_some();
    let illum1 = illuminant_temperature(
        r.scalar(50778)?
            .context("DCP 缺少 CalibrationIlluminant1")?,
    )?;
    let illum2 = if dual_illuminant {
        illuminant_temperature(
            r.scalar(50779)?
                .context("双光源 DCP 缺少 CalibrationIlluminant2")?,
        )?
    } else {
        illum1
    };
    let mut analog_balance = [1.; 3];
    if let Some(v) = r.numbers(50727, 3)? {
        ensure!(
            v.len() == 3 && v.iter().all(|v| *v > 0. && *v <= 1000.),
            "DCP AnalogBalance 无效"
        );
        analog_balance.copy_from_slice(&v);
    }
    let hue_encoding = r.encoding(51107)?;
    let hue_sat = [
        r.map(50937, 50938, hue_encoding)?,
        r.map(50937, 50939, hue_encoding)?,
    ];
    ensure!(
        !r.tags.contains_key(&50937) || hue_sat[0].is_some(),
        "DCP HSV 表缺少 Data1"
    );
    if let (Some(a), Some(b)) = (&hue_sat[0], &hue_sat[1]) {
        ensure!(
            a.dims == b.dims && a.encoding == b.encoding,
            "DCP 双光源 HSV 表不匹配"
        );
    }
    let look = r.map(50981, 50982, r.encoding(51108)?)?;
    ensure!(
        !r.tags.contains_key(&50981) || look.is_some(),
        "DCP Look 表缺少数据"
    );
    let tone = if let Some(points) = r.numbers(50940, 131072)? {
        ensure!(points.len() % 2 == 0, "DCP 色调曲线坐标不成对");
        Some(ToneCurve::new(
            points
                .chunks_exact(2)
                .map(|v| [v[0] as f32, v[1] as f32])
                .collect(),
        )?)
    } else {
        None
    };
    let baseline = r.scalar(51109)?.unwrap_or(0.);
    ensure!(
        (-10. ..=10.).contains(&baseline),
        "DCP 基线曝光超出支持范围"
    );
    let black_render = match r.scalar(51110)? {
        None => None,
        Some(0.) => Some(0),
        Some(1.) => Some(1),
        _ => bail!("DCP 默认黑位模式不支持"),
    };
    // FNV-1a is stable across processes, unlike the standard randomized hasher.
    let fingerprint = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    });
    Ok(Profile {
        name: r
            .text(50936)?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Unnamed DCP".into()),
        model: r.text(50708)?.filter(|s| !s.is_empty()),
        color: [first, second.unwrap_or(first)],
        forward: [r.matrix(50964)?, r.matrix(50965)?],
        illuminants: [illum1, illum2],
        dual_illuminant,
        calibration: [r.matrix(50723)?, r.matrix(50724)?],
        analog_balance,
        camera_calibration_signature: r.text(50931)?,
        profile_calibration_signature: r.text(50932)?,
        hue_sat,
        look,
        tone,
        baseline_exposure: baseline as f32,
        black_render,
        fingerprint,
        path: None,
        requested_name: None,
        fallback: false,
        warning: None,
    })
}
fn illuminant_temperature(code: f64) -> Result<f64> {
    ensure!(code.fract() == 0., "DCP 光源代码无效");
    Ok(match code as u32 {
        1 | 4 | 9 | 20 => 5500.,
        2 => 4230.,
        3 | 17 => 2850.,
        10 | 21 => 6500.,
        11 | 22 => 7500.,
        12 => 6400.,
        13 => 5050.,
        14 => 4150.,
        15 => 3520.,
        18 => 4874.,
        19 => 6774.,
        23 => 5000.,
        24 => 3200.,
        _ => bail!("DCP 标定光源 {code} 暂不支持"),
    })
}
pub fn read(path: &Path) -> Result<Profile> {
    let metadata = fs::metadata(path).with_context(|| format!("读取 DCP：{}", path.display()))?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_FILE,
        "DCP 不是普通文件或超出 64 MiB 限制"
    );
    // Recheck the actual bounded read: the file could grow after metadata().
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)?;
    let mut profile = parse(&bytes).with_context(|| format!("解析 DCP：{}", path.display()))?;
    profile.path = Some(path.to_path_buf());
    Ok(profile)
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn model_matches(actual: &str, expected: &str) -> bool {
    let (a, b) = (normalize(actual), normalize(expected));
    a == b || (a.len() >= 3 && b.len() >= 3 && (a.ends_with(&b) || b.ends_with(&a)))
}
fn roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = std::env::var_os("FASTCULL_DCP_DIR") {
        roots.push(PathBuf::from(dir));
    }
    #[cfg(windows)]
    {
        if let Some(dir) = std::env::var_os("APPDATA") {
            roots.push(PathBuf::from(dir).join("Adobe/CameraRaw/CameraProfiles"));
        }
        if let Some(dir) = std::env::var_os("PROGRAMDATA") {
            roots.push(PathBuf::from(dir).join("Adobe/CameraRaw/CameraProfiles"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(
                PathBuf::from(home)
                    .join("Library/Application Support/Adobe/CameraRaw/CameraProfiles"),
            );
        }
        roots.push(PathBuf::from(
            "/Library/Application Support/Adobe/CameraRaw/CameraProfiles",
        ));
    }
    roots
}
fn find_in_roots(roots: &[PathBuf], model: &str, name: &str) -> Result<Option<Profile>> {
    ensure!(
        !model.is_empty() && model.len() <= 512 && name.len() <= 512,
        "DCP 机型或名称无效"
    );
    ensure!(
        !model.contains(['/', '\\', ':']) && !name.contains(['/', '\\', ':']),
        "DCP 机型或名称含路径字符"
    );
    let suffix = normalize(name);
    // Bound directory traversal; never follow nested links or scan a photo tree.
    for root in roots {
        let mut directories = vec![
            root.clone(),
            root.join("Adobe Standard"),
            root.join("Camera"),
            root.join("Camera").join(model),
        ];
        if let Ok(entries) = fs::read_dir(root.join("Camera")) {
            for entry in entries.take(4096).flatten() {
                let metadata = entry.file_type()?;
                if metadata.is_dir()
                    && !metadata.is_symlink()
                    && model_matches(&entry.file_name().to_string_lossy(), model)
                {
                    directories.push(entry.path());
                }
            }
        }
        for directory in directories {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            let mut matches = Vec::new();
            for entry in entries.take(16384).flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_none_or(|e| !e.eq_ignore_ascii_case("dcp"))
                    || !entry.file_type()?.is_file()
                {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let normalized = normalize(stem);
                let Some(prefix) = normalized.strip_suffix(&suffix) else {
                    continue;
                };
                if model_matches(prefix, model) {
                    matches.push(path);
                }
            }
            matches.sort();
            for path in matches {
                let profile = read(&path)?;
                if normalize(&profile.name) != suffix {
                    continue;
                }
                if profile
                    .model
                    .as_deref()
                    .is_none_or(|actual| model_matches(actual, model))
                {
                    return Ok(Some(profile));
                }
            }
        }
    }
    Ok(None)
}
/// Find only installed files. Explicit overrides are validated rather than
/// silently ignored. Enhanced XMP profiles may fall back to Adobe Standard,
/// with the requested name and fallback status retained for an honest UI label.
pub fn find(model: &str, name: &str) -> Result<Option<Profile>> {
    if let Some(path) = std::env::var_os("FASTCULL_DCP_PROFILE") {
        let mut profile = read(Path::new(&path))?;
        ensure!(
            profile
                .model
                .as_deref()
                .is_none_or(|actual| model_matches(actual, model)),
            "指定 DCP 不适用于当前机型"
        );
        profile.requested_name = Some(name.into());
        return Ok(Some(profile));
    }
    let requested = if name.trim().is_empty() {
        "Adobe Standard"
    } else {
        name.trim()
    };
    find_requested(&roots(), model, requested)
}
fn find_requested(roots: &[PathBuf], model: &str, requested: &str) -> Result<Option<Profile>> {
    if let Some(mut profile) = find_in_roots(roots, model, requested)? {
        profile.requested_name = Some(requested.into());
        return Ok(Some(profile));
    }
    if !requested.eq_ignore_ascii_case("Adobe Standard")
        && let Some(mut profile) = find_in_roots(roots, model, "Adobe Standard")?
    {
        profile.requested_name = Some(requested.into());
        profile.fallback = true;
        profile.warning = Some(format!(
            "未执行 {requested}；当前使用 Adobe Standard 基础 DCP"
        ));
        return Ok(Some(profile));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(little: bool) -> Vec<u8> {
        let mut out = vec![0u8; 8 + 2 + 4 * 12 + 4];
        out[..2].copy_from_slice(if little { b"II" } else { b"MM" });
        let u16b = |v: u16| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let u32b = |v: u32| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        out[2..4].copy_from_slice(&u16b(0x4352));
        out[4..8].copy_from_slice(&u32b(8));
        out[8..10].copy_from_slice(&u16b(4));
        let matrix_offset = out.len();
        for x in [1f32, 0., 0., 0., 1., 0., 0., 0., 1.] {
            out.extend_from_slice(&u32b(x.to_bits()));
        }
        let name_offset = out.len();
        out.extend_from_slice(b"Test DCP\0");
        for (i, (tag, kind, count, value)) in [
            (50721u16, 11u16, 9u32, matrix_offset as u32),
            (50778, 3, 1, 21),
            (50936, 2, 9, name_offset as u32),
            (51109, 11, 1, 0.25f32.to_bits()),
        ]
        .into_iter()
        .enumerate()
        {
            let p = 10 + i * 12;
            out[p..p + 2].copy_from_slice(&u16b(tag));
            out[p + 2..p + 4].copy_from_slice(&u16b(kind));
            out[p + 4..p + 8].copy_from_slice(&u32b(count));
            if kind == 3 {
                out[p + 8..p + 10].copy_from_slice(&u16b(value as u16));
            } else {
                out[p + 8..p + 12].copy_from_slice(&u32b(value));
            }
        }
        out
    }
    #[test]
    fn profiles_parse_both_orders_and_preserve_exposure_and_fingerprint() {
        for little in [true, false] {
            let bytes = fixture(little);
            let p = parse(&bytes).unwrap();
            assert_eq!(p.name, "Test DCP");
            assert_eq!(p.color[0], [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]]);
            assert_eq!(p.baseline_exposure, 0.25);
            assert_eq!(p.illuminants, [6500.; 2]);
            assert!(!p.dual_illuminant);
            assert_eq!(p.fingerprint, parse(&bytes).unwrap().fingerprint);
        }
    }
    #[test]
    fn malformed_offsets_duplicates_nonfinite_and_incomplete_matrices_fail() {
        let pristine = fixture(true);
        for length in 0..pristine.len() {
            assert!(parse(&pristine[..length]).is_err());
        }
        let mut b = pristine.clone();
        b[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&b).is_err());
        let mut b = pristine.clone();
        b[22..24].copy_from_slice(&50721u16.to_le_bytes());
        assert!(parse(&b).is_err());
        let mut b = pristine.clone();
        let pos = u32::from_le_bytes(b[18..22].try_into().unwrap()) as usize;
        b[pos..pos + 4].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
        assert!(parse(&b).is_err());
        let mut b = pristine;
        b[14..18].copy_from_slice(&8u32.to_le_bytes());
        assert!(parse(&b).is_err());
    }
    fn identity_map(encoding: Encoding) -> HsvMap {
        HsvMap::new([4, 3, 2], vec![[0., 1., 1.]; 24], encoding).unwrap()
    }
    #[test]
    fn identity_gray_and_headroom_survive_both_table_encodings() {
        for encoding in [Encoding::Linear, Encoding::Srgb] {
            let map = identity_map(encoding);
            for rgb in [[0.2, 0.4, 0.1], [0.25; 3], [1.5, 0.2, 0.4], [0.; 3]] {
                let out = map.apply(rgb);
                assert!(
                    out.iter().zip(rgb).all(|(a, b)| (*a - b).abs() < 0.00001),
                    "{out:?} vs {rgb:?}"
                );
            }
        }
    }
    #[test]
    fn hsv_tables_use_saturation_fastest_and_wrap_hue() {
        let mut data = vec![[0., 1., 1.]; 8];
        data[1] = [30., 1., 2.];
        data[3] = [-30., 1., 4.];
        data[5] = [60., 1., 6.];
        data[7] = [-60., 1., 8.];
        let map = HsvMap::new([2, 2, 2], data, Encoding::Linear).unwrap();
        assert_eq!(map.sample(0., 1., 0.), [30., 1., 2.]);
        assert_eq!(map.sample(180., 1., 1.), [-60., 1., 8.]);
        assert_eq!(map.sample(90., 1., 0.5), [0., 1., 5.]);
        assert_eq!(map.sample(-90., 1., 0.5), map.sample(270., 1., 0.5));
        assert!((map.sample(359.999, 1., 0.5)[0] - map.sample(0.001, 1., 0.5)[0]).abs() < 0.01);
    }
    #[test]
    fn srgb_encoding_is_applied_only_to_value_and_ignored_for_2d_tables() {
        let mut map = identity_map(Encoding::Srgb);
        for v in 0..2 {
            for h in 0..4 {
                for s in 1..3 {
                    map.data[(v * 4 + h) * 3 + s][2] = 0.5;
                }
            }
        }
        let out = map.apply([0.18, 0., 0.]);
        assert!((out[0] - srgb_decode(srgb_encode(0.18) * 0.5)).abs() < 1e-6);
        assert_eq!([out[1], out[2]], [0.; 2]);
        let a = HsvMap::new([1, 2, 1], vec![[0., 1., 1.], [0., 1., 0.5]], Encoding::Srgb).unwrap();
        assert!((a.apply([0.18, 0., 0.])[0] - 0.09).abs() < 1e-6);
    }
    #[test]
    fn monotone_tone_curve_never_overshoots_and_rejects_nonmonotone_points() {
        let curve = ToneCurve::new(vec![[0., 0.], [0.2, 0.4], [0.8, 0.6], [1., 1.]]).unwrap();
        let mut previous = 0.;
        for i in 0..=1000 {
            let y = curve.eval(i as f32 / 1000.);
            assert!(y >= previous && y <= 1.);
            previous = y;
        }
        assert!((curve.eval(0.2) - 0.4).abs() < 1e-6);
        assert_eq!(curve.eval(2.), 1.);
        assert!(ToneCurve::new(vec![[0., 0.], [0.5, 0.8], [0.6, 0.7], [1., 1.]]).is_err());
    }
    #[test]
    fn profile_search_respects_model_and_make_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("Sony ILCE-6700 Test DCP.dcp"), fixture(true)).unwrap();
        let found = find_in_roots(&[root.to_path_buf()], "ILCE-6700", "Test DCP")
            .unwrap()
            .unwrap();
        assert_eq!(found.name, "Test DCP");
        assert!(found.path.is_some());
        assert!(
            find_in_roots(&[root.to_path_buf()], "Nikon D3S", "Test DCP")
                .unwrap()
                .is_none()
        );
        assert!(find_in_roots(&[root.to_path_buf()], "../outside", "Test DCP").is_err());
    }
    #[test]
    fn unsupported_enhanced_profile_is_an_explicit_standard_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let mut bytes = fixture(true);
        let text = b"Adobe Standard\0";
        let offset = bytes.len() as u32;
        bytes.extend_from_slice(text);
        let entry = 10 + 2 * 12;
        bytes[entry + 4..entry + 8].copy_from_slice(&(text.len() as u32).to_le_bytes());
        bytes[entry + 8..entry + 12].copy_from_slice(&offset.to_le_bytes());
        fs::write(
            directory.path().join("Sony ILCE-6700 Adobe Standard.dcp"),
            bytes,
        )
        .unwrap();
        let roots = [directory.path().to_path_buf()];
        let p = find_requested(&roots, "ILCE-6700", "Adobe Color")
            .unwrap()
            .unwrap();
        assert_eq!(p.name, "Adobe Standard");
        assert_eq!(p.requested_name.as_deref(), Some("Adobe Color"));
        assert!(p.fallback && p.warning.as_deref().unwrap().contains("未执行 Adobe Color"));
        assert!(
            !find_requested(&roots, "ILCE-6700", "Adobe Standard")
                .unwrap()
                .unwrap()
                .fallback
        );
    }
    #[test]
    fn unknown_encoding_and_unbounded_files_or_tables_are_rejected() {
        let mut bytes = fixture(true);
        let entry = 10 + 3 * 12;
        bytes[entry..entry + 2].copy_from_slice(&51107u16.to_le_bytes());
        bytes[entry + 8..entry + 12].copy_from_slice(&2f32.to_bits().to_le_bytes());
        assert!(parse(&bytes).is_err());
        assert!(HsvMap::new([usize::MAX, 2, 1], Vec::new(), Encoding::Linear).is_err());
        assert!(
            HsvMap::new(
                [1, 2, 1],
                vec![[0., 1., 1.], [0., f32::NAN, 1.]],
                Encoding::Linear
            )
            .is_err()
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.dcp");
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_FILE + 1)
            .unwrap();
        assert!(read(&path).is_err());
    }
}
