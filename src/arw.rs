//! Bounded classic-TIFF traversal. Never scan compressed sensor data for JPEG markers.
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const HEAD: usize = 512 * 1024;
const MAX_JPEG: u64 = 128 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct JpegRef {
    pub offset: u64,
    pub len: u64,
}
#[derive(Clone, Debug)]
pub struct BitmapRef {
    pub offset: u64,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug)]
pub struct ArwInfo {
    pub jpeg: JpegRef,
    pub preview: JpegRef,
    pub thumbnail: JpegRef,
    /// Bounded, uncompressed RGB8 DNG thumbnail, in sensor orientation.
    pub bitmap_thumbnail: Option<BitmapRef>,
    pub orientation: u16,
    pub summary: String,
    pub capture: CaptureMetadata,
    /// Present when regular previews require LibRaw; before orientation.
    pub raw_size: Option<(u32, u32)>,
    /// Stable, unrotated picture dimensions for fitted layout, before previews
    /// are rounded down or the full-resolution pixels finish decoding.
    pub image_size: Option<(u32, u32)>,
}
#[derive(Clone, Debug, Default)]
pub struct CaptureMetadata {
    pub make: Option<String>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub captured_at: Option<String>,
    pub iso: Option<u32>,
    pub aperture: Option<f64>,
    pub shutter: Option<f64>,
    pub focal: Option<f64>,
}
struct Tiff<'a> {
    data: &'a [u8],
    little: bool,
}
impl Tiff<'_> {
    fn u16(&self, o: usize) -> Result<u16> {
        let v: [u8; 2] = self
            .data
            .get(o..o.checked_add(2).context("offset overflow")?)
            .context("truncated TIFF u16")?
            .try_into()?;
        Ok(if self.little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        })
    }
    fn u32(&self, o: usize) -> Result<u32> {
        let v: [u8; 4] = self
            .data
            .get(o..o.checked_add(4).context("offset overflow")?)
            .context("truncated TIFF u32")?
            .try_into()?;
        Ok(if self.little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        })
    }
    fn number(&self, o: usize, typ: u16) -> Result<u32> {
        match typ {
            3 => Ok(self.u16(o)? as u32),
            4 => self.u32(o),
            _ => bail!("unsupported TIFF number type"),
        }
    }
    fn rational(&self, o: usize) -> Result<f64> {
        let den = self.u32(o + 4)?;
        ensure!(den != 0, "zero EXIF denominator");
        Ok(self.u32(o)? as f64 / den as f64)
    }
    fn text(&self, o: usize, count: usize) -> Option<String> {
        if count == 0 || count > 4096 {
            return None;
        }
        let text = std::str::from_utf8(self.data.get(o..o.checked_add(count)?)?)
            .ok()?
            .trim_matches(['\0', ' ', '\r', '\n']);
        (!text.is_empty()).then(|| text.to_owned())
    }
}
impl ArwInfo {
    pub fn read(path: &Path) -> Result<Self> {
        if crate::formats::is_jpeg(path) {
            return Self::read_jpeg_info(path);
        }
        if crate::formats::is_dng(path) {
            return crate::dng::read(path).or_else(|e| {
                crate::native_raw::read(path)
                    .with_context(|| format!("DNG JPEG preview unavailable: {e}"))
            });
        }
        if crate::formats::supported(path)
            && !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("arw"))
        {
            return crate::native_raw::read(path);
        }
        let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let size = file.metadata()?.len();
        let mut head = Vec::with_capacity(HEAD);
        (&mut file).take(HEAD as u64).read_to_end(&mut head)?;
        let mut info = Self::parse(&head, size).or_else(|e| {
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("arw"))
            {
                crate::native_raw::read(path)
                    .with_context(|| format!("TIFF preview unavailable: {e}"))
            } else {
                Err(e)
            }
        })?;
        if info.image_size.is_none() {
            info.image_size =
                crate::jpeg::dimensions(&mut file, info.jpeg.offset, info.jpeg.len).ok();
        }
        Ok(info)
    }
    pub fn parse(head: &[u8], size: u64) -> Result<Self> {
        Self::parse_tiff(head, size, None)
    }
    fn read_jpeg_info(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        ensure!(size <= MAX_JPEG, "JPEG too large");
        let header = crate::jpeg::header(&mut file)?;
        let whole = JpegRef {
            offset: 0,
            len: size,
        };
        let mut info = header
            .app1
            .iter()
            .find(|s| s.payload.starts_with(b"Exif\0\0"))
            .and_then(|s| Self::parse_tiff(&s.payload[6..], size, Some(whole.clone())).ok())
            .unwrap_or_else(|| Self {
                jpeg: whole.clone(),
                preview: whole.clone(),
                thumbnail: whole.clone(),
                bitmap_thumbnail: None,
                orientation: 1,
                summary: String::new(),
                capture: CaptureMetadata::default(),
                raw_size: None,
                image_size: None,
            });
        info.jpeg = whole.clone();
        info.preview = whole.clone();
        info.thumbnail = whole;
        // JPEG SOF describes the actual image; EXIF dimensions can be stale.
        info.image_size = crate::jpeg::dimensions(&mut file, 0, size).ok();
        Ok(info)
    }
    fn parse_tiff(head: &[u8], size: u64, fallback: Option<JpegRef>) -> Result<Self> {
        ensure!(head.len() >= 8, "truncated TIFF header");
        let little = match &head[..2] {
            b"II" => true,
            b"MM" => false,
            _ => bail!("not TIFF/ARW"),
        };
        let t = Tiff { data: head, little };
        ensure!(
            t.u16(2)? == 42,
            "unsupported TIFF magic (BigTIFF unsupported)"
        );
        let first = t.u32(4)? as usize;
        let mut queue = vec![first];
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        let mut orientation = 1;
        let mut image_size = None;
        let mut iso = None;
        let (mut aperture, mut shutter, mut focal) = (None, None, None);
        let mut capture = CaptureMetadata::default();
        let (mut original_time, mut digitized_time, mut file_time) = (None, None, None);
        while let Some(off) = queue.pop() {
            if off == 0 || !seen.insert(off) {
                continue;
            }
            ensure!(seen.len() <= 256, "too many IFDs");
            let n = t.u16(off)? as usize;
            let end = off.checked_add(2 + n * 12).context("IFD overflow")?;
            ensure!(end + 4 <= head.len(), "IFD outside metadata window");
            let (mut jo, mut jl) = (None, None);
            let (mut image_width, mut image_height) = (None, None);
            for i in 0..n {
                let e = off + 2 + i * 12;
                let (tag, typ, cnt) = (t.u16(e)?, t.u16(e + 2)?, t.u32(e + 4)? as usize);
                let unit = match typ {
                    1 | 2 | 7 => 1,
                    3 => 2,
                    4 | 9 => 4,
                    5 | 10 => 8,
                    _ => continue,
                };
                let bytes = cnt.checked_mul(unit).context("TIFF count overflow")?;
                let vo = if bytes <= 4 {
                    e + 8
                } else {
                    t.u32(e + 8)? as usize
                };
                // Unknown entries can point to the RAW strip. Only read recognized metadata.
                match tag {
                    0x0112 if off == first && typ == 3 && cnt == 1 => orientation = t.u16(vo)?,
                    0x0201 if cnt == 1 && typ == 4 => jo = Some(t.u32(vo)? as u64),
                    0x0202 if cnt == 1 && typ == 4 => jl = Some(t.u32(vo)? as u64),
                    0xa002 if cnt == 1 && matches!(typ, 3 | 4) => {
                        image_width = Some(t.number(vo, typ)?)
                    }
                    0xa003 if cnt == 1 && matches!(typ, 3 | 4) => {
                        image_height = Some(t.number(vo, typ)?)
                    }
                    0x014a | 0x8769 | 0x8825 if typ == 4 => {
                        ensure!(cnt <= 256, "too many sub-IFDs");
                        for j in 0..cnt {
                            queue.push(t.u32(vo + j * 4)? as usize);
                        }
                    }
                    0x8827 if cnt >= 1 && (typ == 3 || typ == 4) => iso = Some(t.number(vo, typ)?),
                    0x829d if typ == 5 => aperture = t.rational(vo).ok(),
                    0x829a if typ == 5 => shutter = t.rational(vo).ok(),
                    0x920a if typ == 5 => focal = t.rational(vo).ok(),
                    0x010f if typ == 2 => capture.make = t.text(vo, cnt).or(capture.make),
                    0x0110 if typ == 2 => capture.camera = t.text(vo, cnt).or(capture.camera),
                    0xa434 if typ == 2 => capture.lens = t.text(vo, cnt).or(capture.lens),
                    0x9003 if typ == 2 => original_time = t.text(vo, cnt).or(original_time),
                    0x9004 if typ == 2 => digitized_time = t.text(vo, cnt).or(digitized_time),
                    0x0132 if typ == 2 => file_time = t.text(vo, cnt).or(file_time),
                    _ => {}
                }
            }
            if image_size.is_none()
                && let (Some(w), Some(h)) = (image_width, image_height)
                && crate::jpeg::valid_dimensions(w, h)
            {
                image_size = Some((w, h));
            }
            if let (Some(offset), Some(len)) = (jo, jl) {
                ensure!(
                    (4..=MAX_JPEG).contains(&len)
                        && offset.checked_add(len).is_some_and(|x| x <= size),
                    "JPEG range outside file"
                );
                candidates.push(JpegRef { offset, len });
            }
            queue.push(t.u32(end)? as usize);
        }
        ensure!((1..=8).contains(&orientation), "invalid TIFF orientation");
        candidates.sort_by_key(|j| j.len);
        if candidates.is_empty()
            && let Some(jpeg) = fallback
        {
            candidates.push(jpeg);
        }
        let jpeg = candidates.last().context("no embedded JPEG found")?.clone();
        let thumbnail = candidates.first().unwrap().clone();
        // Prefer the medium preview over a 160x120 thumbnail for grid use.
        let preview = candidates
            .iter()
            .find(|j| j.len >= 64 * 1024)
            .unwrap_or(&jpeg)
            .clone();
        let mut summary = Vec::new();
        if let Some(v) = iso {
            summary.push(format!("ISO{v}"));
        }
        if let Some(v) = aperture {
            summary.push(format!("f/{v:.1}"));
        }
        if let Some(v) = shutter.filter(|v| *v > 0.0) {
            summary.push(if v < 1.0 {
                format!("1/{:.0}s", 1.0 / v)
            } else {
                format!("{v:.1}s")
            });
        }
        if let Some(v) = focal {
            summary.push(format!("{v:.0}mm"));
        }
        Ok(Self {
            jpeg,
            preview,
            thumbnail,
            bitmap_thumbnail: None,
            orientation,
            summary: summary.join("  "),
            raw_size: None,
            image_size,
            capture: CaptureMetadata {
                captured_at: original_time.or(digitized_time).or(file_time),
                iso,
                aperture,
                shutter,
                focal,
                ..capture
            },
        })
    }
    pub fn read_jpeg(path: &Path, jpeg: &JpegRef) -> Result<Vec<u8>> {
        ensure!(jpeg.len <= MAX_JPEG, "JPEG too large");
        let mut f = File::open(path)?;
        ensure!(
            jpeg.offset
                .checked_add(jpeg.len)
                .is_some_and(|x| x <= f.metadata().map(|m| m.len()).unwrap_or(0)),
            "JPEG truncated"
        );
        f.seek(SeekFrom::Start(jpeg.offset))?;
        let mut data = vec![0; jpeg.len as usize];
        f.read_exact(&mut data)?;
        ensure!(data.starts_with(&[0xff, 0xd8]), "invalid JPEG SOI");
        Ok(data)
    }
}
impl CaptureMetadata {
    pub fn summary(&self) -> String {
        let mut out = Vec::new();
        if let Some(v) = self.iso {
            out.push(format!("ISO{v}"));
        }
        if let Some(v) = self.aperture {
            out.push(format!("f/{v:.1}"));
        }
        if let Some(v) = self.shutter {
            out.push(if v < 1. {
                format!("1/{:.0}s", 1. / v)
            } else {
                format!("{v:.1}s")
            });
        }
        if let Some(v) = self.focal {
            out.push(format!("{v:.0}mm"));
        }
        out.join("  ")
    }
}
