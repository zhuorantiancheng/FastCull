//! Seek directly to DNG IFDs and baseline JPEG previews, never to sensor pixels.
//! The fast path needs no native library. Unusual layouts/encodings use LibRaw.
use crate::arw::{ArwInfo, BitmapRef, CaptureMetadata, JpegRef};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

const MAX_METADATA: usize = 4 * 1024 * 1024;
const MAX_JPEG: u64 = 128 * 1024 * 1024;

struct Reader {
    file: File,
    size: u64,
    little: bool,
    remaining: usize,
    first: u32,
}
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    value: [u8; 4],
}
impl Reader {
    fn open(path: &Path) -> Result<Self> {
        Self::from_file(File::open(path)?)
    }
    fn from_file(mut file: File) -> Result<Self> {
        let size = file.metadata()?.len();
        let mut head = [0; 8];
        file.read_exact(&mut head)?;
        ensure!(matches!(&head[..2], b"II" | b"MM"), "not TIFF/DNG");
        let mut r = Self {
            file,
            size,
            little: &head[..2] == b"II",
            remaining: MAX_METADATA,
            first: 0,
        };
        ensure!(
            r.short(&head[2..]) == 42,
            "DNG fast path requires classic TIFF"
        );
        r.first = r.long(&head[4..]);
        Ok(r)
    }
    fn short(&self, b: &[u8]) -> u16 {
        let b = [b[0], b[1]];
        if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    }
    fn long(&self, b: &[u8]) -> u32 {
        let b = [b[0], b[1], b[2], b[3]];
        if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    }
    fn bytes(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        ensure!(
            len <= self.remaining
                && offset
                    .checked_add(len as u64)
                    .is_some_and(|end| end <= self.size),
            "DNG metadata outside bounds"
        );
        self.remaining -= len;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut b = vec![0; len];
        self.file.read_exact(&mut b)?;
        Ok(b)
    }
    fn ifd(&mut self, offset: u32) -> Result<(Vec<Entry>, u32)> {
        ensure!(offset >= 8, "invalid DNG IFD offset");
        let n = self.bytes(offset as u64, 2)?;
        let n = self.short(&n) as usize;
        ensure!(n <= 4096, "too many DNG IFD entries");
        let b = self.bytes(offset as u64 + 2, n * 12 + 4)?;
        let entries = b[..n * 12]
            .chunks_exact(12)
            .map(|e| Entry {
                tag: self.short(e),
                kind: self.short(&e[2..]),
                count: self.long(&e[4..]),
                value: e[8..12].try_into().unwrap(),
            })
            .collect();
        Ok((entries, self.long(&b[n * 12..])))
    }
    fn data(&mut self, e: &Entry, unit: usize, max: u32) -> Result<Vec<u8>> {
        ensure!(e.count <= max, "DNG tag exceeds limit");
        let len = (e.count as usize)
            .checked_mul(unit)
            .context("DNG count overflow")?;
        if len <= 4 {
            Ok(e.value[..len].to_vec())
        } else {
            self.bytes(self.long(&e.value) as u64, len)
        }
    }
    fn numbers(&mut self, e: &Entry, max: u32) -> Result<Vec<f64>> {
        let unit = match e.kind {
            3 => 2,
            4 | 13 => 4,
            5 => 8,
            _ => anyhow::bail!("unsupported DNG number"),
        };
        let b = self.data(e, unit, max)?;
        b.chunks_exact(unit)
            .map(|b| {
                Ok(match e.kind {
                    3 => self.short(b) as f64,
                    5 => {
                        let den = self.long(&b[4..]);
                        ensure!(den != 0, "zero DNG denominator");
                        self.long(b) as f64 / den as f64
                    }
                    _ => self.long(b) as f64,
                })
            })
            .collect()
    }
    fn number(&self, e: &Entry) -> Option<u32> {
        if e.count != 1 {
            return None;
        }
        // Scalar SHORT/LONG values are always inline; no allocation or I/O.
        match e.kind {
            3 => Some(self.short(&e.value) as u32),
            4 => Some(self.long(&e.value)),
            _ => None,
        }
    }
    fn text(&mut self, e: &Entry) -> Option<String> {
        if e.kind != 2 {
            return None;
        }
        let b = self.data(e, 1, 4096).ok()?;
        let s = std::str::from_utf8(&b)
            .ok()?
            .trim_matches(['\0', ' ', '\r', '\n']);
        (!s.is_empty()).then(|| s.to_owned())
    }
}

/// Embed an XMP packet without decoding or rewriting any image data.
///
/// Classic TIFF offsets are absolute, so all original bytes remain in place. A
/// new packet and IFD0 are appended to a same-directory temporary copy; only its
/// header's first-IFD pointer is changed before atomic replacement. Unsupported
/// BigTIFF/64-bit layouts and malformed directories are rejected before copying.
pub fn write_xmp(path: &Path, packet: &str) -> Result<()> {
    ensure!(
        !packet.is_empty() && packet.len() <= MAX_METADATA,
        "DNG XMP packet exceeds limit"
    );
    ensure!(
        crate::xmp::patch(packet, &crate::xmp::Updates::default())? == packet,
        "DNG XMP packet requires namespace repair"
    );
    let link_metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        link_metadata.is_file() && !link_metadata.file_type().is_symlink(),
        "DNG must be a regular file"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    // Deny writers for the complete transaction, while allowing atomic rename.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0x0000_0001 | 0x0000_0004); // FILE_SHARE_READ | DELETE
    }
    let mut r = Reader::from_file(options.open(path)?)?;
    ensure!(
        r.size <= u32::MAX as u64,
        "DNG larger than classic TIFF offset range"
    );
    let before = r.file.metadata()?;
    ensure!(!before.permissions().readonly(), "DNG is read-only");
    let (mut entries, next) = r.ifd(r.first)?;
    validate_write_ifds(&mut r, &entries, next)?;
    let existing = entries.iter().position(|entry| entry.tag == 700);
    ensure!(
        existing.is_some() || entries.len() < 4096,
        "too many DNG IFD entries"
    );
    if let Some(index) = existing {
        ensure!(
            matches!(entries[index].kind, 1 | 7),
            "unsupported DNG XMP tag type"
        );
    }
    let packet_offset = r
        .size
        .checked_add(r.size & 1)
        .context("DNG offset overflow")?;
    let packet_end = packet_offset
        .checked_add(packet.len() as u64)
        .context("DNG offset overflow")?;
    let ifd_offset = packet_end
        .checked_add(packet_end & 1)
        .context("DNG offset overflow")?;
    let count = entries.len() + usize::from(existing.is_none());
    ensure!(
        packet.len() + 2 + count * 12 + 4 <= MAX_METADATA,
        "DNG XMP and directory exceed metadata read limit"
    );
    let final_size = ifd_offset
        .checked_add(2 + count as u64 * 12 + 4)
        .context("DNG offset overflow")?;
    ensure!(
        final_size <= u32::MAX as u64,
        "DNG metadata exceeds classic TIFF offset range"
    );
    let xmp = Entry {
        tag: 700,
        kind: 1,
        count: packet.len() as u32,
        value: encode_long(packet_offset as u32, r.little),
    };
    if let Some(index) = existing {
        entries[index] = xmp;
    } else {
        let index = entries
            .iter()
            .position(|entry| entry.tag > 700)
            .unwrap_or(entries.len());
        entries.insert(index, xmp);
    }
    let mut directory = Vec::with_capacity(2 + entries.len() * 12 + 4);
    directory.extend_from_slice(&encode_short(entries.len() as u16, r.little));
    for entry in entries {
        directory.extend_from_slice(&encode_short(entry.tag, r.little));
        directory.extend_from_slice(&encode_short(entry.kind, r.little));
        directory.extend_from_slice(&encode_long(entry.count, r.little));
        directory.extend_from_slice(&entry.value);
    }
    directory.extend_from_slice(&encode_long(next, r.little));
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".fastcull-dng-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    r.file.seek(SeekFrom::Start(0))?;
    let copied = std::io::copy(
        &mut BufReader::with_capacity(1024 * 1024, &mut r.file),
        temporary.as_file_mut(),
    )?;
    ensure!(copied == r.size, "DNG changed during copy");
    if packet_offset != r.size {
        temporary.write_all(&[0])?;
    }
    temporary.write_all(packet.as_bytes())?;
    if ifd_offset != packet_end {
        temporary.write_all(&[0])?;
    }
    temporary.write_all(&directory)?;
    temporary.seek(SeekFrom::Start(4))?;
    temporary.write_all(&encode_long(ifd_offset as u32, r.little))?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.as_file().set_permissions(before.permissions())?;
    let now = std::fs::metadata(path)?;
    ensure!(
        now.len() == before.len() && now.modified()? == before.modified()?,
        "DNG changed before metadata commit"
    );
    // Windows' replacement operation needs the destination read handle closed,
    // even when DELETE sharing is allowed. The source was held against writes
    // throughout validation and copying; release it only for the atomic commit.
    drop(r);
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("atomically replace DNG metadata")?;
    Ok(())
}

fn encode_short(value: u16, little: bool) -> [u8; 2] {
    if little {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    }
}
fn encode_long(value: u32, little: bool) -> [u8; 4] {
    if little {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    }
}
fn tag_width(kind: u16) -> Result<u64> {
    Ok(match kind {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => anyhow::bail!("unsupported DNG TIFF field type {kind}"),
    })
}
fn validate_write_ifds(r: &mut Reader, root: &[Entry], next: u32) -> Result<()> {
    let mut active = HashSet::new();
    let mut complete = HashSet::new();
    active.insert(r.first);
    let mut stack = vec![(r.first, true)];
    let mut children = validate_write_entries(r, root)?;
    children.push(next);
    stack.extend(
        children
            .into_iter()
            .filter(|offset| *offset != 0)
            .map(|offset| (offset, false)),
    );
    while let Some((offset, exiting)) = stack.pop() {
        if exiting {
            active.remove(&offset);
            complete.insert(offset);
            continue;
        }
        ensure!(!active.contains(&offset), "cyclic DNG IFD graph");
        if complete.contains(&offset) {
            continue;
        }
        ensure!(active.len() + complete.len() < 256, "too many DNG IFDs");
        active.insert(offset);
        let (entries, next) = r.ifd(offset)?;
        let mut children = validate_write_entries(r, &entries)?;
        children.push(next);
        stack.push((offset, true));
        stack.extend(
            children
                .into_iter()
                .filter(|offset| *offset != 0)
                .map(|offset| (offset, false)),
        );
    }
    Ok(())
}
fn validate_write_entries(r: &mut Reader, entries: &[Entry]) -> Result<Vec<u32>> {
    let mut tags = HashSet::new();
    let mut children = Vec::new();
    for entry in entries {
        ensure!(tags.insert(entry.tag), "duplicate DNG tag {}", entry.tag);
        let length = u64::from(entry.count) * tag_width(entry.kind)?;
        if length > 4 {
            let start = u64::from(r.long(&entry.value));
            ensure!(
                start >= 8 && start.checked_add(length).is_some_and(|end| end <= r.size),
                "DNG tag {} outside bounds",
                entry.tag
            );
        }
        if matches!(entry.tag, 0x014a | 0x8769 | 0x8825 | 0xa005) {
            ensure!(
                matches!(entry.kind, 4 | 13),
                "invalid DNG directory pointer type"
            );
            ensure!(
                entry.tag == 0x014a || entry.count == 1,
                "invalid DNG directory pointer count"
            );
            children.extend(r.numbers(entry, 256)?.into_iter().map(|value| value as u32));
        }
    }
    for (offset_tag, length_tag) in [(0x0111, 0x0117), (0x0144, 0x0145), (0x0201, 0x0202)] {
        let offsets = entries.iter().find(|entry| entry.tag == offset_tag);
        let lengths = entries.iter().find(|entry| entry.tag == length_tag);
        if offsets.is_none() && lengths.is_none() {
            continue;
        }
        let offsets = offsets.context("missing DNG image offsets")?;
        let lengths = lengths.context("missing DNG image byte counts")?;
        ensure!(
            matches!(offsets.kind, 3 | 4)
                && matches!(lengths.kind, 3 | 4)
                && offsets.count == lengths.count,
            "invalid DNG image offsets/byte counts"
        );
        let offsets = r.numbers(offsets, (MAX_METADATA / 4) as u32)?;
        let lengths = r.numbers(lengths, (MAX_METADATA / 4) as u32)?;
        for (offset, length) in offsets.into_iter().zip(lengths) {
            let (offset, length) = (offset as u64, length as u64);
            ensure!(
                offset >= 8 && offset.checked_add(length).is_some_and(|end| end <= r.size),
                "DNG image data outside bounds"
            );
        }
    }
    Ok(children)
}

pub(crate) fn read(path: &Path) -> Result<ArwInfo> {
    let mut r = Reader::open(path)?;
    let mut queue = vec![r.first];
    let mut seen = HashSet::new();
    let mut previews = Vec::new();
    let mut bitmap_thumbnail: Option<BitmapRef> = None;
    let mut capture = CaptureMetadata::default();
    let mut orientation = 1;
    let (mut raw_size, mut crop_size, mut exif_width, mut exif_height) = (None, None, None, None);
    let (mut original_time, mut digitized_time, mut file_time) = (None, None, None);
    while let Some(offset) = queue.pop() {
        if offset == 0 || !seen.insert(offset) {
            continue;
        }
        ensure!(seen.len() <= 256, "too many DNG IFDs");
        let (entries, next) = r.ifd(offset)?;
        let (mut subfile, mut compression, mut photometric, mut samples) = (0, 0, 0, 0);
        let (mut rgb8, mut planar) = (false, 1);
        let (mut width, mut height, mut jo, mut jl, mut strip, mut strip_len) =
            (None, None, None, None, None, None);
        let mut crop = None;
        for e in &entries {
            match e.tag {
                0x00fe => subfile = r.number(e).unwrap_or(0),
                0x0100 => width = r.number(e),
                0x0101 => height = r.number(e),
                0x0103 => compression = r.number(e).unwrap_or(0),
                0x0106 => photometric = r.number(e).unwrap_or(0),
                0x0115 => samples = r.number(e).unwrap_or(0),
                0x011c => planar = r.number(e).unwrap_or(0),
                0x0102 if e.kind == 3 && e.count == 3 => {
                    rgb8 = r.numbers(e, 3).is_ok_and(|v| v == [8., 8., 8.]);
                }
                0x0111 => strip = r.number(e),
                0x0117 => strip_len = r.number(e),
                0x0201 => jo = r.number(e),
                0x0202 => jl = r.number(e),
                0x0112 if offset == r.first => orientation = r.number(e).unwrap_or(1) as u16,
                0x014a | 0x8769 if matches!(e.kind, 4 | 13) => {
                    ensure!(
                        e.count <= 256 && queue.len() + e.count as usize <= 256,
                        "too many DNG sub-IFDs"
                    );
                    queue.extend(r.numbers(e, 256)?.into_iter().map(|n| n as u32));
                }
                0xc620 if e.count == 2 => {
                    if let Ok(v) = r.numbers(e, 2) {
                        crop = dimensions(Some(v[0].round() as u32), Some(v[1].round() as u32));
                    }
                }
                0xa002 => exif_width = r.number(e),
                0xa003 => exif_height = r.number(e),
                0x010f => capture.make = r.text(e).or(capture.make),
                0x0110 => capture.camera = r.text(e).or(capture.camera),
                0xa434 => capture.lens = r.text(e).or(capture.lens),
                0x9003 => original_time = r.text(e).or(original_time),
                0x9004 => digitized_time = r.text(e).or(digitized_time),
                0x0132 => file_time = r.text(e).or(file_time),
                0x8827 => {
                    capture.iso = r
                        .numbers(e, 16)
                        .ok()
                        .and_then(|v| v.first().map(|n| *n as u32))
                }
                0x829d | 0x829a | 0x920a if e.count == 1 => {
                    let value = r
                        .numbers(e, 1)
                        .ok()
                        .and_then(|v| v.first().copied())
                        .filter(|n| *n > 0.);
                    match e.tag {
                        0x829d => capture.aperture = value,
                        0x829a => capture.shutter = value,
                        _ => capture.focal = value,
                    }
                }
                _ => {}
            }
        }
        if subfile == 0 {
            raw_size = dimensions(width, height).or(raw_size);
            crop_size = crop.or(crop_size);
        }
        // Only rendered, reduced-resolution RGB/YCbCr IFDs are previews.
        // Never mistake lossy/lossless JPEG sensor strips, masks or depth maps for photos.
        if subfile == 1 {
            if compression == 1
                && photometric == 2
                && samples == 3
                && planar == 1
                && rgb8
                && let (Some((w, h)), Some(offset), Some(len)) =
                    (dimensions(width, height), strip, strip_len)
                && w.max(h) <= 1024
                && u64::from(w) * u64::from(h) * 3 == u64::from(len)
                && u64::from(offset) + u64::from(len) <= r.size
                && bitmap_thumbnail
                    .as_ref()
                    .is_none_or(|b| w * h < b.width * b.height)
            {
                bitmap_thumbnail = Some(BitmapRef {
                    offset: offset as u64,
                    width: w,
                    height: h,
                });
            }
            let range = jo.zip(jl).or_else(|| {
                (compression == 7 && matches!(photometric, 2 | 6) && samples == 3)
                    .then_some(strip.zip(strip_len))
                    .flatten()
            });
            if let Some((offset, len)) = range {
                let (offset, len) = (offset as u64, len as u64);
                if (4..=MAX_JPEG).contains(&len)
                    && offset.checked_add(len).is_some_and(|end| end <= r.size)
                    && let Ok((w, h)) = crate::jpeg::dimensions(&mut r.file, offset, len)
                {
                    previews.push(((w, h), JpegRef { offset, len }));
                }
            }
        }
        queue.push(next);
    }
    ensure!((1..=8).contains(&orientation), "invalid DNG orientation");
    previews.sort_by_key(|((w, h), _)| u64::from(*w) * u64::from(*h));
    // RGB-only DNGs still avoid native metadata parsing and can draw an instant
    // placeholder. Regular previews/full resolution keep the native fallback.
    let native_size = dimensions(exif_width, exif_height)
        .or(crop_size)
        .or(raw_size);
    let largest_size = previews
        .last()
        .map(|p| p.0)
        .or(native_size)
        .context("no DNG image dimensions")?;
    ensure!(
        !previews.is_empty() || (bitmap_thumbnail.is_some() && native_size.is_some()),
        "no DNG preview"
    );
    let jpeg = previews
        .last()
        .map(|p| p.1.clone())
        .unwrap_or(JpegRef { offset: 0, len: 0 });
    let thumbnail = previews
        .first()
        .map(|p| p.1.clone())
        .unwrap_or_else(|| jpeg.clone());
    let preview = previews
        .iter()
        .find(|((w, h), _)| (*w).max(*h) >= 200)
        .map(|p| p.1.clone())
        .unwrap_or_else(|| jpeg.clone());
    capture.captured_at = original_time.or(digitized_time).or(file_time);
    Ok(ArwInfo {
        jpeg,
        preview,
        thumbnail,
        bitmap_thumbnail,
        orientation,
        summary: capture.summary(),
        capture,
        raw_size: previews.is_empty().then_some(native_size).flatten(),
        image_size: native_size.or(Some(largest_size)),
    })
}
fn dimensions(w: Option<u32>, h: Option<u32>) -> Option<(u32, u32)> {
    w.zip(h)
        .filter(|(w, h)| crate::jpeg::valid_dimensions(*w, *h))
}

pub(crate) fn decode_thumbnail(path: &Path, bitmap: &BitmapRef) -> Result<crate::imaging::Pixels> {
    ensure!(
        crate::jpeg::valid_dimensions(bitmap.width, bitmap.height)
            && bitmap.width.max(bitmap.height) <= 1024,
        "DNG thumbnail too large"
    );
    let mut r = Reader::open(path)?;
    let bytes = r.bytes(
        bitmap.offset,
        bitmap.width as usize * bitmap.height as usize * 3,
    )?;
    let mut rgba = Vec::with_capacity(bytes.len() / 3 * 4);
    for p in bytes.chunks_exact(3) {
        rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    Ok(crate::imaging::Pixels {
        width: bitmap.width as usize,
        height: bitmap.height as usize,
        rgba,
    })
}

/// DNG editors normally embed XMP in IFD0. Sidecar overrides are handled by xmp.
pub(crate) fn read_xmp(path: &Path) -> Result<Option<String>> {
    let mut r = Reader::open(path)?;
    let (entries, _) = r.ifd(r.first)?;
    let Some(e) = entries.iter().find(|e| e.tag == 0x02bc) else {
        return Ok(None);
    };
    ensure!(matches!(e.kind, 1 | 7), "unsupported DNG XMP type");
    let b = r.data(e, 1, MAX_METADATA as u32)?;
    Ok(Some(
        std::str::from_utf8(&b)?.trim_end_matches('\0').to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging::{self, Level};
    use std::fs;

    fn short(v: u16, little: bool) -> [u8; 2] {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }
    fn long(v: u32, little: bool) -> [u8; 4] {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }
    fn ifd(b: &mut [u8], off: usize, entries: &[(u16, u16, u32, u32)], little: bool) {
        b[off..off + 2].copy_from_slice(&short(entries.len() as u16, little));
        for (i, &(tag, kind, count, value)) in entries.iter().enumerate() {
            let e = off + 2 + i * 12;
            b[e..e + 2].copy_from_slice(&short(tag, little));
            b[e + 2..e + 4].copy_from_slice(&short(kind, little));
            b[e + 4..e + 8].copy_from_slice(&long(count, little));
            if kind == 3 && count == 1 {
                b[e + 8..e + 10].copy_from_slice(&short(value as u16, little));
            } else {
                b[e + 8..e + 12].copy_from_slice(&long(value, little));
            }
        }
    }
    fn fixture(little: bool) -> Vec<u8> {
        let jpg = include_bytes!("../tests/fixtures/preview-3x2.jpg");
        let tiny = include_bytes!("../tests/fixtures/tiny.jpg");
        let mut padded = vec![255, 216, 255, 239];
        padded.extend(60002u16.to_be_bytes());
        padded.resize(60006, 0); // Smallest resolution, largest compressed byte length.
        padded.extend_from_slice(&tiny[2..]);
        let mut b = vec![0; 2 * 1024 * 1024 + 256];
        b[..2].copy_from_slice(if little { b"II" } else { b"MM" });
        b[2..4].copy_from_slice(&short(42, little));
        b[4..8].copy_from_slice(&long(8, little));
        ifd(
            &mut b,
            8,
            &[
                (0xfe, 4, 1, 1),
                (0x103, 3, 1, 7),
                (0x106, 3, 1, 6),
                (0x115, 3, 1, 3),
                (0x111, 4, 1, 4096),
                (0x117, 4, 1, jpg.len() as u32),
                (0x112, 3, 1, 6),
                (0x14a, 13, 2, 256),
            ],
            little,
        );
        b[256..260].copy_from_slice(&long(1024 * 1024, little));
        b[260..264].copy_from_slice(&long(2 * 1024 * 1024, little));
        ifd(
            &mut b,
            1024 * 1024,
            &[
                (0xfe, 4, 1, 1),
                (0x201, 4, 1, 8192),
                (0x202, 4, 1, padded.len() as u32),
            ],
            little,
        );
        ifd(
            &mut b,
            2 * 1024 * 1024,
            &[
                (0xfe, 4, 1, 0),
                (0x100, 4, 1, 6200),
                (0x101, 4, 1, 4100),
                (0xc620, 5, 2, 512),
            ],
            little,
        );
        for (i, v) in [6000, 1, 4000, 1].into_iter().enumerate() {
            b[512 + i * 4..516 + i * 4].copy_from_slice(&long(v, little));
        }
        b[4096..4096 + jpg.len()].copy_from_slice(jpg);
        b[8192..8192 + padded.len()].copy_from_slice(&padded);
        b
    }

    #[test]
    fn seeks_far_subifds_and_selects_by_pixels_in_both_byte_orders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("远处预览.DnG");
        for little in [true, false] {
            fs::write(&path, fixture(little)).unwrap();
            let info = ArwInfo::read(&path).unwrap();
            assert!(info.raw_size.is_none(), "must use the Rust fast path");
            assert_eq!(info.jpeg.offset, 4096);
            assert_eq!(info.thumbnail.offset, 8192);
            assert!(info.thumbnail.len > info.jpeg.len);
            assert_eq!(info.orientation, 6);
            assert_eq!(info.image_size, Some((6000, 4000)));
            let thumb = imaging::decode_level(&path, &info, Level::Thumb).unwrap();
            assert_eq!((thumb.width, thumb.height), (120, 80));
            let fast = imaging::decode_level(&path, &info, Level::FastThumb).unwrap();
            assert_eq!((fast.width, fast.height), (16, 12));
            let fit = imaging::decode_level(&path, &info, Level::Fit(50)).unwrap();
            assert_eq!((fit.width, fit.height), (50, 33));
        }
    }

    #[test]
    fn excludes_sensor_strips_and_mask_ifds_and_skips_broken_previews() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mask.dng");
        let mut b = fixture(true);
        // Remove the picture candidate; a valid JPEG in a sensor IFD is not a preview.
        b[18..22].copy_from_slice(&0u32.to_le_bytes());
        fs::write(&path, &b).unwrap();
        assert_eq!(read(&path).unwrap().jpeg.offset, 8192);
        b[1024 * 1024 + 10..1024 * 1024 + 14].copy_from_slice(&65537u32.to_le_bytes());
        fs::write(&path, &b).unwrap();
        assert!(read(&path).is_err());
        let mut b = fixture(true);
        b[4096] = 0; // Invalid primary SOI must not hide the remaining preview.
        fs::write(&path, &b).unwrap();
        assert_eq!(read(&path).unwrap().jpeg.offset, 8192);
    }

    #[test]
    fn bounded_metadata_cycles_offsets_and_huge_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bounds.dng");
        let mut b = fixture(true);
        b[256..260].copy_from_slice(&8u32.to_le_bytes()); // SubIFD cycle.
        fs::write(&path, &b).unwrap();
        assert!(read(&path).is_ok());
        let mut reader = Reader::open(&path).unwrap();
        let _ = reader.ifd(reader.first).unwrap();
        assert!(MAX_METADATA - reader.remaining < 4096);
        assert!(reader.bytes(reader.size - 1, 2).is_err());
        assert!(reader.bytes(0, MAX_METADATA + 1).is_err());
        // The SubIFDs array must be bounded before allocating or reading it.
        b[10 + 7 * 12 + 4..10 + 7 * 12 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&path, &b).unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn uncompressed_rgb_thumbnail_is_read_without_jpeg_or_sensor_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rgb.dng");
        let mut b = fixture(true);
        ifd(
            &mut b,
            1024 * 1024,
            &[
                (0xfe, 4, 1, 1),
                (0x100, 4, 1, 2),
                (0x101, 4, 1, 1),
                (0x102, 3, 3, 768),
                (0x103, 3, 1, 1),
                (0x106, 3, 1, 2),
                (0x115, 3, 1, 3),
                (0x111, 4, 1, 800),
                (0x117, 4, 1, 6),
            ],
            true,
        );
        b[768..774].copy_from_slice(&[8, 0, 8, 0, 8, 0]);
        b[800..806].copy_from_slice(&[255, 0, 0, 0, 255, 0]);
        fs::write(&path, &b).unwrap();
        let info = read(&path).unwrap();
        let p = imaging::decode_level(&path, &info, Level::FastThumb).unwrap();
        assert_eq!((p.width, p.height), (2, 1));
        assert_eq!(p.rgba, [255, 0, 0, 255, 0, 255, 0, 255]);
        // RGB-only containers retain the small placeholder and native fallback.
        b[18..22].copy_from_slice(&0u32.to_le_bytes());
        fs::write(&path, &b).unwrap();
        let info = read(&path).unwrap();
        assert!(info.raw_size.is_some());
        assert!(info.bitmap_thumbnail.is_some());
        assert_eq!(info.jpeg.len, 0);
        assert!(imaging::decode_level(&path, &info, Level::FastThumb).is_ok());
        // 16-bit RGB cannot be interpreted as RGB8.
        b[768..774].copy_from_slice(&[16, 0, 16, 0, 16, 0]);
        fs::write(&path, &b).unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn embedded_xmp_is_loaded_and_seeded_into_sidecar_without_touching_dng() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edited.DNG");
        let packet = r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmp:Rating="3" crs:Exposure2012="0.5" crs:HasCrop="True" crs:CropLeft="0.1" crs:CropTop="0.2" crs:CropRight="0.9" crs:CropBottom="0.8" crs:CropAngle="0" crs:Contrast2012="23"/>"#;
        for little in [true, false] {
            let mut b = vec![0; 128];
            b[..2].copy_from_slice(if little { b"II" } else { b"MM" });
            b[2..4].copy_from_slice(&short(42, little));
            b[4..8].copy_from_slice(&long(8, little));
            ifd(&mut b, 8, &[(0x2bc, 1, packet.len() as u32, 128)], little);
            b.extend_from_slice(packet.as_bytes());
            let path = path.with_file_name(format!("edited-{little}.DNG"));
            fs::write(&path, &b).unwrap();
            let before = crate::xmp::read(&path).unwrap();
            assert_eq!(before.rating, Some(3));
            assert_eq!(before.exposure, Some(500000));
            assert!(before.crop.is_some());
            crate::xmp::write(
                &path,
                &crate::xmp::Updates {
                    rating: Some(0),
                    ..Default::default()
                },
            )
            .unwrap();
            let after = crate::xmp::read(&path).unwrap();
            assert_eq!(after.rating, Some(0));
            assert_eq!(after.crop, before.crop);
            assert_eq!(after.exposure, before.exposure);
            assert!(
                fs::read_to_string(path.with_extension("xmp"))
                    .unwrap()
                    .contains("crs:Contrast2012=\"23\"")
            );
            assert_eq!(fs::read(&path).unwrap(), b);
        }
    }

    fn writable_fixture(little: bool, xmp: bool) -> Vec<u8> {
        let old_packet = crate::xmp::template();
        let mut bytes = vec![0; 2048 + old_packet.len()];
        bytes[..2].copy_from_slice(if little { b"II" } else { b"MM" });
        bytes[2..4].copy_from_slice(&short(42, little));
        bytes[4..8].copy_from_slice(&long(8, little));
        let mut entries = vec![
            (256, 4, 1, 16),
            (273, 4, 2, 256),
            (279, 4, 2, 264),
            (274, 3, 1, 6), // Preserve unsorted entries and inline padding too.
            (330, 13, 1, 384),
            (65000, 7, 9, 768),
        ];
        if xmp {
            entries.push((700, 7, old_packet.len() as u32, 2048));
        }
        ifd(&mut bytes, 8, &entries, little);
        let next = 10 + entries.len() * 12;
        bytes[next..next + 4].copy_from_slice(&long(512, little));
        bytes[10 + 3 * 12 + 10..10 + 3 * 12 + 12].copy_from_slice(&[0xa5, 0x5a]);
        for (index, value) in [1024, 1060, 16, 24].into_iter().enumerate() {
            bytes[256 + index * 4..260 + index * 4].copy_from_slice(&long(value, little));
        }
        ifd(
            &mut bytes,
            384,
            &[(273, 4, 1, 1100), (279, 4, 1, 16)],
            little,
        );
        ifd(&mut bytes, 512, &[(65001, 11, 1, 0x7fc1_2345)], little);
        bytes[768..777].copy_from_slice(b"DxO\0\xffdata");
        for (index, byte) in bytes[1024..1200].iter_mut().enumerate() {
            *byte = (index * 31) as u8;
        }
        bytes[2048..].copy_from_slice(old_packet.as_bytes());
        bytes
    }

    #[test]
    fn atomically_embeds_xmp_without_rewriting_pixels_unknown_tags_or_other_ifds() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("原生元数据.DNG");
        let packet = crate::xmp::patch(
            &crate::xmp::template(),
            &crate::xmp::Updates {
                exposure: Some(750_000),
                ..Default::default()
            },
        )
        .unwrap();
        for little in [true, false] {
            for existing in [true, false] {
                let before = writable_fixture(little, existing);
                fs::write(&path, &before).unwrap();
                let mut old = Reader::open(&path).unwrap();
                let (old_entries, old_next) = old.ifd(old.first).unwrap();
                drop(old);
                write_xmp(&path, &packet).unwrap();
                assert_eq!(read_xmp(&path).unwrap().as_deref(), Some(packet.as_str()));
                let after = fs::read(&path).unwrap();
                assert_eq!(&after[..4], &before[..4]);
                // This stronger invariant covers every strip, tile, sub-IFD,
                // private payload and obsolete metadata in the original file.
                assert_eq!(&after[8..before.len()], &before[8..]);
                let mut current = Reader::open(&path).unwrap();
                assert!(u64::from(current.first) >= before.len() as u64);
                let (new_entries, new_next) = current.ifd(current.first).unwrap();
                assert_eq!(new_next, old_next);
                assert_eq!(
                    new_entries.iter().filter(|entry| entry.tag == 700).count(),
                    1
                );
                for old in old_entries.into_iter().filter(|entry| entry.tag != 700) {
                    let new = new_entries
                        .iter()
                        .find(|entry| entry.tag == old.tag)
                        .unwrap();
                    assert_eq!(
                        (new.kind, new.count, new.value),
                        (old.kind, old.count, old.value)
                    );
                }
                drop(current);
                // A later save must follow the latest directory and retain the
                // original offset graph rather than adding duplicate XMP tags.
                write_xmp(&path, &crate::xmp::template()).unwrap();
                assert_eq!(read_xmp(&path).unwrap(), Some(crate::xmp::template()));
            }
        }
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn malformed_dng_or_xmp_never_changes_the_source_or_leaves_a_temp_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("拒绝写入.dng");
        let packet = crate::xmp::template();
        for little in [true, false] {
            let mut cases = Vec::new();
            let mut bytes = writable_fixture(little, false);
            bytes[2..4].copy_from_slice(&short(43, little)); // BigTIFF.
            cases.push(bytes);
            let mut bytes = writable_fixture(little, false);
            bytes[10 + 5 * 12..12 + 5 * 12].copy_from_slice(&short(273, little)); // Duplicate.
            cases.push(bytes);
            let mut bytes = writable_fixture(little, false);
            bytes[10 + 5 * 12 + 8..10 + 5 * 12 + 12].copy_from_slice(&long(u32::MAX, little));
            cases.push(bytes); // External private payload outside file.
            let mut bytes = writable_fixture(little, false);
            bytes[256..260].copy_from_slice(&long(u32::MAX - 4, little));
            cases.push(bytes); // Pixel strip outside file.
            let mut bytes = writable_fixture(little, false);
            bytes[10 + 5 * 12 + 2..10 + 5 * 12 + 4].copy_from_slice(&short(16, little));
            cases.push(bytes); // 64-bit field type in classic TIFF.
            let mut bytes = writable_fixture(little, false);
            bytes[10 + 4 * 12 + 8..10 + 4 * 12 + 12].copy_from_slice(&long(8, little));
            cases.push(bytes); // Cyclic sub-IFD graph.
            let mut bytes = writable_fixture(little, false);
            bytes[10 + 6 * 12..14 + 6 * 12].copy_from_slice(&long(u32::MAX, little));
            cases.push(bytes); // Broken next-IFD pointer.
            for bytes in cases {
                fs::write(&path, &bytes).unwrap();
                assert!(write_xmp(&path, &packet).is_err());
                assert_eq!(fs::read(&path).unwrap(), bytes);
                assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
            }
            let bytes = writable_fixture(little, true);
            fs::write(&path, &bytes).unwrap();
            for invalid in ["", "<rdf:Description>", "<bad/>"] {
                assert!(write_xmp(&path, invalid).is_err());
                assert_eq!(fs::read(&path).unwrap(), bytes);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_blocked_atomic_commit_leaves_source_and_directory_unchanged() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("busy.dng");
        let original = writable_fixture(true, true);
        fs::write(&path, &original).unwrap();
        // Simulate another application keeping the destination open without
        // delete sharing. Temporary output may be complete but cannot commit.
        let held = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert!(write_xmp(&path, &crate::xmp::template()).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        drop(held);
        let permissions = fs::metadata(&path).unwrap().permissions();
        let mut read_only = permissions.clone();
        read_only.set_readonly(true);
        fs::set_permissions(&path, read_only).unwrap();
        assert!(write_xmp(&path, &crate::xmp::template()).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        fs::set_permissions(&path, permissions).unwrap();
    }

    #[test]
    #[ignore = "requires FASTCULL_DNG_EMBED_FIXTURE pointing to a private test copy"]
    fn private_real_dng_metadata_roundtrip_preserves_all_original_payload_bytes() {
        let fixture = std::env::var_os("FASTCULL_DNG_EMBED_FIXTURE")
            .expect("set FASTCULL_DNG_EMBED_FIXTURE to a PRIVATE test copy");
        let fixture = Path::new(&fixture);
        let source_metadata = fs::metadata(fixture).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("embed-roundtrip.dng");
        fs::copy(fixture, &target).unwrap();
        let packet = crate::xmp::packet(&target)
            .unwrap()
            .unwrap_or_else(crate::xmp::template);
        let packet = crate::xmp::patch(
            &packet,
            &crate::xmp::Updates {
                exposure: Some(750_000),
                white_balance: Some(crate::xmp::WhiteBalance::Custom {
                    temperature: 4850,
                    tint: 17,
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let began = std::time::Instant::now();
        write_xmp(&target, &packet).unwrap();
        eprintln!(
            "embedded {} bytes in {:.3}s",
            source_metadata.len(),
            began.elapsed().as_secs_f64()
        );
        assert_eq!(read_xmp(&target).unwrap().as_deref(), Some(packet.as_str()));
        let values = crate::xmp::parse_values(&packet).unwrap();
        assert_eq!(values.exposure, Some(750_000));
        assert_eq!(
            values
                .white_balance
                .as_ref()
                .and_then(crate::xmp::WhiteBalanceSettings::edit),
            Some(crate::xmp::WhiteBalance::Custom {
                temperature: 4850,
                tint: 17
            })
        );
        let mut original = File::open(fixture).unwrap();
        let mut updated = File::open(&target).unwrap();
        let mut first = [0; 8];
        let mut second = [0; 8];
        original.read_exact(&mut first).unwrap();
        updated.read_exact(&mut second).unwrap();
        assert_eq!(first[..4], second[..4]);
        let mut remaining = source_metadata.len() - 8;
        let mut left = vec![0; 1024 * 1024];
        let mut right = vec![0; left.len()];
        while remaining > 0 {
            let count = remaining.min(left.len() as u64) as usize;
            original.read_exact(&mut left[..count]).unwrap();
            updated.read_exact(&mut right[..count]).unwrap();
            assert_eq!(left[..count], right[..count], "original bytes changed");
            remaining -= count as u64;
        }
        let after = fs::metadata(fixture).unwrap();
        assert_eq!(after.len(), source_metadata.len());
        assert_eq!(
            after.modified().unwrap(),
            source_metadata.modified().unwrap()
        );
    }
}
