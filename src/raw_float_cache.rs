//! Versioned local preview cache. All image/color state is data, never an input
//! profile filename. The format uses little-endian f32/f64 without quantizing
//! highlights; writes commit atomically only after the whole frame is valid.
use crate::{
    dcp::{Encoding, HsvMap},
    raw_color::{Frame, GreySampler, Recipe},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    io::{BufReader, BufWriter, Cursor, Read, Write},
    path::Path,
};

const MAGIC: &[u8; 16] = b"FC_FLOAT_RAW_V1\0";
const HEADER: u64 = 40;
const MAX_METADATA: usize = 32 * 1024 * 1024;
const MAX_PIXELS: usize = 160_000_000;
const MAX_STRING: usize = 65536;
const MAX_CURVE: usize = 65536;
const MAX_MAP_CELLS: usize = 1_048_576;
const PIXEL_CHUNK: usize = 8192;

fn pixel_count(size: [usize; 2]) -> Result<usize> {
    let count = size[0].checked_mul(size[1]).context("线性缓存尺寸溢出")?;
    ensure!(
        size[0] > 0 && size[1] > 0 && count <= MAX_PIXELS,
        "线性缓存尺寸超出限制"
    );
    Ok(count)
}
fn grey_count(size: [usize; 2]) -> Result<usize> {
    ensure!(
        size.iter().all(|v| (1..=128).contains(v)),
        "灰点缓存尺寸超出限制"
    );
    Ok(size[0] * size[1])
}
fn finite32(value: f32) -> Result<f32> {
    ensure!(value.is_finite(), "线性缓存包含非有限数值");
    Ok(value)
}
fn finite64(value: f64) -> Result<f64> {
    ensure!(value.is_finite(), "线性缓存包含非有限数值");
    Ok(value)
}
struct Encoder(Vec<u8>);
impl Encoder {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }
    fn u32(&mut self, value: usize) -> Result<()> {
        self.0
            .extend_from_slice(&u32::try_from(value)?.to_le_bytes());
        Ok(())
    }
    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn f32(&mut self, value: f32) -> Result<()> {
        self.0.extend_from_slice(&finite32(value)?.to_le_bytes());
        Ok(())
    }
    fn f64(&mut self, value: f64) -> Result<()> {
        self.0.extend_from_slice(&finite64(value)?.to_le_bytes());
        Ok(())
    }
    fn text(&mut self, value: &str) -> Result<()> {
        ensure!(value.len() <= MAX_STRING, "线性缓存文本过长");
        self.u32(value.len())?;
        self.0.extend_from_slice(value.as_bytes());
        Ok(())
    }
    fn text_option(&mut self, value: Option<&str>) -> Result<()> {
        self.u8(u8::from(value.is_some()));
        if let Some(value) = value {
            self.text(value)?;
        }
        Ok(())
    }
    fn number_option(&mut self, value: Option<f64>) -> Result<()> {
        self.u8(u8::from(value.is_some()));
        if let Some(value) = value {
            self.f64(value)?;
        }
        Ok(())
    }
    fn matrix(&mut self, value: [[f64; 3]; 3]) -> Result<()> {
        for row in value {
            for v in row {
                self.f64(v)?;
            }
        }
        Ok(())
    }
    fn map(&mut self, value: Option<&HsvMap>) -> Result<()> {
        self.u8(u8::from(value.is_some()));
        let Some(map) = value else {
            return Ok(());
        };
        let [h, s, v] = map.dims;
        ensure!(
            h > 0
                && s >= 2
                && v > 0
                && h.checked_mul(s)
                    .and_then(|n| n.checked_mul(v))
                    .is_some_and(|n| n <= MAX_MAP_CELLS && n == map.data.len()),
            "线性缓存 HSV 表尺寸无效"
        );
        for dimension in map.dims {
            self.u32(dimension)?;
        }
        self.u8(match map.encoding {
            Encoding::Linear => 0,
            Encoding::Srgb => 1,
        });
        // Use the validated public constructor on both read and write. Metadata
        // maps are small compared with the pixel array and never share it.
        HsvMap::new(map.dims, map.data.clone(), map.encoding)?;
        for row in &map.data {
            for &v in row {
                self.f32(v)?;
            }
        }
        Ok(())
    }
}
struct Decoder<'a>(Cursor<&'a [u8]>);
impl Decoder<'_> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut data = [0; N];
        self.0.read_exact(&mut data).context("线性缓存元数据截断")?;
        Ok(data)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes::<1>()?[0])
    }
    fn flag(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => bail!("线性缓存布尔标记无效"),
        }
    }
    fn u32(&mut self) -> Result<usize> {
        Ok(u32::from_le_bytes(self.bytes()?) as usize)
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }
    fn f32(&mut self) -> Result<f32> {
        finite32(f32::from_le_bytes(self.bytes()?))
    }
    fn f64(&mut self) -> Result<f64> {
        finite64(f64::from_le_bytes(self.bytes()?))
    }
    fn text(&mut self) -> Result<String> {
        let len = self.u32()?;
        let start = self.0.position() as usize;
        ensure!(
            len <= MAX_STRING
                && start
                    .checked_add(len)
                    .is_some_and(|end| end <= self.0.get_ref().len()),
            "线性缓存文本截断或过长"
        );
        let value = std::str::from_utf8(&self.0.get_ref()[start..start + len])
            .context("线性缓存文本不是 UTF-8")?
            .to_owned();
        self.0.set_position((start + len) as u64);
        Ok(value)
    }
    fn text_option(&mut self) -> Result<Option<String>> {
        if self.flag()? {
            Ok(Some(self.text()?))
        } else {
            Ok(None)
        }
    }
    fn number_option(&mut self) -> Result<Option<f64>> {
        if self.flag()? {
            Ok(Some(self.f64()?))
        } else {
            Ok(None)
        }
    }
    fn matrix(&mut self) -> Result<[[f64; 3]; 3]> {
        let mut out = [[0.; 3]; 3];
        for row in &mut out {
            for value in row {
                *value = self.f64()?;
            }
        }
        Ok(out)
    }
    fn map(&mut self) -> Result<Option<HsvMap>> {
        if !self.flag()? {
            return Ok(None);
        }
        let dims = [self.u32()?, self.u32()?, self.u32()?];
        let count = dims[0]
            .checked_mul(dims[1])
            .and_then(|n| n.checked_mul(dims[2]))
            .context("线性缓存 HSV 表大小溢出")?;
        ensure!(
            dims[0] > 0 && dims[1] >= 2 && dims[2] > 0 && count <= MAX_MAP_CELLS,
            "线性缓存 HSV 表超出限制"
        );
        let encoding = match self.u8()? {
            0 => Encoding::Linear,
            1 => Encoding::Srgb,
            _ => bail!("线性缓存 HSV 编码无效"),
        };
        let remaining = self.0.get_ref().len() - self.0.position() as usize;
        ensure!(
            count.checked_mul(12).is_some_and(|n| n <= remaining),
            "线性缓存 HSV 数据截断"
        );
        let mut data = Vec::new();
        data.try_reserve_exact(count)?;
        for _ in 0..count {
            data.push([self.f32()?, self.f32()?, self.f32()?]);
        }
        Ok(Some(HsvMap::new(dims, data, encoding)?))
    }
}

fn encode_metadata(frame: &Frame) -> Result<Vec<u8>> {
    let mut e = Encoder(Vec::new());
    e.text(&frame.profile)?;
    e.text_option(frame.warning.as_deref())?;
    e.u64(frame.profile_fingerprint);
    e.text(&frame.make)?;
    e.text(&frame.model)?;
    e.text(&frame.requested_profile)?;
    e.number_option(frame.temperature)?;
    e.number_option(frame.tint)?;
    e.f32(frame.recipe.baseline_exposure)?;
    e.map(frame.recipe.look.as_ref())?;
    ensure!(
        (2..=MAX_CURVE).contains(&frame.recipe.curve.len()),
        "线性缓存色调曲线长度无效"
    );
    e.u32(frame.recipe.curve.len())?;
    for &value in &frame.recipe.curve {
        e.f32(value)?;
    }
    let grey = &frame.grey;
    let count = grey_count(grey.size)?;
    ensure!(grey.samples.len() == count, "线性缓存灰点采样格长度不符");
    e.u32(grey.size[0])?;
    e.u32(grey.size[1])?;
    for sample in &grey.samples {
        for &v in sample {
            e.f32(v)?;
        }
    }
    for uv in grey.uvs {
        for v in uv {
            e.f32(v)?;
        }
    }
    ensure!(
        grey.applied_mul.iter().all(|x| x.is_finite() && *x > 0.),
        "线性缓存白平衡增益无效"
    );
    for v in grey.applied_mul {
        e.f64(v)?;
    }
    e.matrix(grey.native_matrix)?;
    e.u8(u8::from(grey.profile_color.is_some()));
    if let Some(color) = grey.profile_color {
        for m in color {
            e.matrix(m)?;
        }
    }
    ensure!(
        grey.profile_illuminants
            .iter()
            .all(|x| x.is_finite() && *x > 0.),
        "线性缓存标定光源无效"
    );
    for v in grey.profile_illuminants {
        e.f64(v)?;
    }
    e.u8(u8::from(grey.profile_dual));
    for m in grey.calibration {
        e.matrix(m)?;
    }
    ensure!(e.0.len() <= MAX_METADATA, "线性缓存元数据超出限制");
    Ok(e.0)
}
fn decode_metadata(bytes: &[u8], size: [usize; 2]) -> Result<Frame> {
    ensure!(bytes.len() <= MAX_METADATA, "线性缓存元数据超出限制");
    let mut d = Decoder(Cursor::new(bytes));
    let profile = d.text()?;
    let warning = d.text_option()?;
    let profile_fingerprint = d.u64()?;
    let make = d.text()?;
    let model = d.text()?;
    let requested_profile = d.text()?;
    let temperature = d.number_option()?;
    let tint = d.number_option()?;
    let baseline_exposure = d.f32()?;
    let look = d.map()?;
    let curve_len = d.u32()?;
    ensure!(
        (2..=MAX_CURVE).contains(&curve_len),
        "线性缓存色调曲线长度无效"
    );
    let mut curve = Vec::new();
    curve.try_reserve_exact(curve_len)?;
    for _ in 0..curve_len {
        curve.push(d.f32()?);
    }
    let grey_size = [d.u32()?, d.u32()?];
    let grey_count = grey_count(grey_size)?;
    let mut samples = Vec::new();
    samples.try_reserve_exact(grey_count)?;
    for _ in 0..grey_count {
        samples.push([d.f32()?, d.f32()?, d.f32()?]);
    }
    let mut uvs = [[0.; 2]; 4];
    for uv in &mut uvs {
        for value in uv {
            *value = d.f32()?;
        }
    }
    let applied_mul = [d.f64()?, d.f64()?, d.f64()?];
    ensure!(
        applied_mul.iter().all(|x| *x > 0.),
        "线性缓存白平衡增益无效"
    );
    let native_matrix = d.matrix()?;
    let profile_color = if d.flag()? {
        Some([d.matrix()?, d.matrix()?])
    } else {
        None
    };
    let profile_illuminants = [d.f64()?, d.f64()?];
    ensure!(
        profile_illuminants.iter().all(|x| *x > 0.),
        "线性缓存标定光源无效"
    );
    let profile_dual = d.flag()?;
    let calibration = [d.matrix()?, d.matrix()?];
    ensure!(
        d.0.position() == bytes.len() as u64,
        "线性缓存元数据含多余尾部数据"
    );
    Ok(Frame {
        size,
        pixels: Vec::new(),
        recipe: Recipe {
            look,
            curve,
            baseline_exposure,
        },
        grey: GreySampler {
            size: grey_size,
            samples,
            uvs,
            applied_mul,
            native_matrix,
            profile_color,
            profile_illuminants,
            profile_dual,
            calibration,
        },
        temperature,
        tint,
        profile,
        warning,
        profile_fingerprint,
        make,
        model,
        requested_profile,
    })
}

pub fn write(path: &Path, frame: &Frame) -> Result<()> {
    let count = pixel_count(frame.size)?;
    ensure!(frame.pixels.len() == count, "线性缓存像素数量不符");
    let metadata = encode_metadata(frame)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".fc-float-")
        .tempfile_in(parent)?;
    {
        let mut output = BufWriter::with_capacity(1024 * 1024, temporary.as_file_mut());
        output.write_all(MAGIC)?;
        output.write_all(&(metadata.len() as u64).to_le_bytes())?;
        output.write_all(&(count as u64).to_le_bytes())?;
        output.write_all(&u32::try_from(frame.size[0])?.to_le_bytes())?;
        output.write_all(&u32::try_from(frame.size[1])?.to_le_bytes())?;
        output.write_all(&metadata)?;
        let mut buffer = Vec::with_capacity(PIXEL_CHUNK * 16);
        for chunk in frame.pixels.chunks(PIXEL_CHUNK) {
            buffer.clear();
            for pixel in chunk {
                for &v in pixel {
                    ensure!(
                        v.is_finite() && v.abs() <= 65504.,
                        "线性缓存像素超出浮点显示范围"
                    );
                    buffer.extend_from_slice(&v.to_le_bytes());
                }
            }
            output.write_all(&buffer)?;
        }
        output.flush()?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}
pub fn read(path: &Path) -> Result<Frame> {
    let file = fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    ensure!(file_len >= HEADER, "线性缓存头部截断");
    let mut input = BufReader::with_capacity(1024 * 1024, file);
    let mut magic = [0u8; 16];
    input.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "线性缓存版本或文件类型不匹配");
    let mut bytes8 = [0; 8];
    input.read_exact(&mut bytes8)?;
    let metadata_len = u64::from_le_bytes(bytes8);
    input.read_exact(&mut bytes8)?;
    let declared_count = u64::from_le_bytes(bytes8);
    let mut bytes4 = [0; 4];
    input.read_exact(&mut bytes4)?;
    let width = u32::from_le_bytes(bytes4) as usize;
    input.read_exact(&mut bytes4)?;
    let height = u32::from_le_bytes(bytes4) as usize;
    let size = [width, height];
    let count = pixel_count(size)?;
    ensure!(
        declared_count == count as u64 && metadata_len <= MAX_METADATA as u64,
        "线性缓存计数超出限制或与尺寸不符"
    );
    let expected = HEADER
        .checked_add(metadata_len)
        .and_then(|n| n.checked_add((count as u64) * 16))
        .context("线性缓存长度溢出")?;
    ensure!(
        file_len == expected,
        "线性缓存长度与声明不符（截断或尾部垃圾）"
    );
    let mut metadata = vec![0; metadata_len as usize];
    input.read_exact(&mut metadata)?;
    let mut frame = decode_metadata(&metadata, size)?;
    frame.pixels.try_reserve_exact(count)?;
    let mut buffer = vec![0u8; PIXEL_CHUNK * 16];
    while frame.pixels.len() < count {
        let chunk = (count - frame.pixels.len()).min(PIXEL_CHUNK);
        input
            .read_exact(&mut buffer[..chunk * 16])
            .context("线性缓存像素截断")?;
        for pixel in buffer[..chunk * 16].chunks_exact(16) {
            let mut values = [0.; 4];
            for (i, v) in values.iter_mut().enumerate() {
                *v = f32::from_le_bytes(pixel[i * 4..i * 4 + 4].try_into().unwrap());
                ensure!(
                    v.is_finite() && v.abs() <= 65504.,
                    "线性缓存像素包含无效浮点数"
                );
            }
            frame.pixels.push(values);
        }
    }
    let mut trailing = [0];
    ensure!(input.read(&mut trailing)? == 0, "线性缓存在读取期间增长");
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: [[f64; 3]; 3] = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
    fn sample() -> Frame {
        Frame {
            size: [2, 2],
            pixels: vec![
                [-0.125, 1.25, 3.5, 1.],
                [0., 0.1, 0.2, 1.],
                [-0., 1e-30, 2., 1.],
                [0.3, 0.4, 0.5, 1.],
            ],
            recipe: Recipe {
                look: Some(
                    HsvMap::new(
                        [1, 2, 1],
                        vec![[0., 1., 1.], [10., 0.8, 1.1]],
                        Encoding::Srgb,
                    )
                    .unwrap(),
                ),
                curve: vec![0., 0.1, 0.8, 1.],
                baseline_exposure: 0.125,
            },
            grey: GreySampler {
                size: [2, 2],
                samples: vec![[0.2, 0.3, 0.4]; 4],
                uvs: [[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
                applied_mul: [2.125, 1., 1.75],
                native_matrix: ID,
                profile_color: Some([ID; 2]),
                profile_illuminants: [2850., 6500.],
                profile_dual: true,
                calibration: [ID; 2],
            },
            temperature: Some(5500.123456789),
            tint: Some(-12.3456789),
            profile: "../descriptive label only".into(),
            warning: Some("基础配置文件回退".into()),
            profile_fingerprint: u64::MAX - 11,
            make: "Sony".into(),
            model: "ILCE-6700".into(),
            requested_profile: "Adobe Color".into(),
        }
    }
    #[test]
    fn hdr_frame_roundtrip_preserves_float_bits_recipe_and_grey_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.fcr");
        let frame = sample();
        write(&path, &frame).unwrap();
        let decoded = read(&path).unwrap();
        assert_eq!(decoded.size, frame.size);
        for (a, b) in decoded
            .pixels
            .iter()
            .flatten()
            .zip(frame.pixels.iter().flatten())
        {
            assert_eq!(a.to_bits(), b.to_bits());
        }
        assert_eq!(
            encode_metadata(&decoded).unwrap(),
            encode_metadata(&frame).unwrap()
        );
        assert_eq!(
            decoded.temperature.unwrap().to_bits(),
            frame.temperature.unwrap().to_bits()
        );
        assert_eq!(decoded.grey.samples, frame.grey.samples);
        assert_eq!(decoded.profile, "../descriptive label only");
        assert_eq!(
            decoded.recipe.look.as_ref().unwrap().apply([0.2, 0.3, 0.1]),
            frame.recipe.look.as_ref().unwrap().apply([0.2, 0.3, 0.1])
        );
    }
    #[test]
    fn truncated_metadata_and_trailing_metadata_are_rejected() {
        let metadata = encode_metadata(&sample()).unwrap();
        for len in 0..metadata.len() {
            assert!(
                decode_metadata(&metadata[..len], [2, 2]).is_err(),
                "accepted prefix {len}"
            );
        }
        let mut invalid = metadata.clone();
        invalid.push(0);
        assert!(decode_metadata(&invalid, [2, 2]).is_err());
        let mut invalid = metadata;
        invalid[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_metadata(&invalid, [2, 2]).is_err());
    }
    #[test]
    fn corrupt_counts_nonfinite_pixels_and_tail_garbage_never_load() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.fcr");
        write(&path, &sample()).unwrap();
        let good = fs::read(&path).unwrap();
        let mut bad = good.clone();
        bad[24..32].copy_from_slice(&5u64.to_le_bytes());
        fs::write(&path, bad).unwrap();
        assert!(read(&path).is_err());
        let mut bad = good.clone();
        bad[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&path, bad).unwrap();
        assert!(read(&path).is_err());
        let mut bad = good.clone();
        bad.extend_from_slice(b"tail");
        fs::write(&path, bad).unwrap();
        assert!(read(&path).is_err());
        let mut bad = good.clone();
        bad.pop();
        fs::write(&path, bad).unwrap();
        assert!(read(&path).is_err());
        let mut bad = good;
        let n = bad.len();
        bad[n - 4..].copy_from_slice(&f32::NAN.to_le_bytes());
        fs::write(&path, bad).unwrap();
        assert!(read(&path).is_err());
    }
    #[test]
    fn invalid_write_preserves_the_existing_cache_and_unrelated_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.fcr");
        let unrelated = directory.path().join("keep.txt");
        fs::write(&unrelated, b"untouched").unwrap();
        write(&path, &sample()).unwrap();
        let before = fs::read(&path).unwrap();
        let mut frame = sample();
        frame.pixels[2][0] = f32::INFINITY;
        assert!(write(&path, &frame).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut frame = sample();
        frame.size = [160_000_001, 1];
        assert!(write(&path, &frame).is_err());
        let mut frame = sample();
        frame.grey.size = [129, 1];
        assert!(write(&path, &frame).is_err());
        let mut frame = sample();
        frame.recipe.curve = vec![0.; MAX_CURVE + 1];
        assert!(write(&path, &frame).is_err());
        assert_eq!(fs::read(unrelated).unwrap(), b"untouched");
    }
    #[test]
    fn bulk_io_crosses_chunk_boundaries_and_atomically_replaces_a_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bulk.fcr");
        let count = PIXEL_CHUNK + 17;
        let mut frame = sample();
        frame.size = [count, 1];
        frame.pixels = (0..count)
            .map(|i| [i as f32 / 4096. - 0.125, 1.25, 2.5, 1.])
            .collect();
        write(&path, &frame).unwrap();
        assert_eq!(read(&path).unwrap().pixels, frame.pixels);
        frame.pixels[count - 1][0] = 3.75;
        frame.profile_fingerprint = 123;
        write(&path, &frame).unwrap();
        let replaced = read(&path).unwrap();
        assert_eq!(replaced.profile_fingerprint, 123);
        assert_eq!(replaced.pixels, frame.pixels);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
