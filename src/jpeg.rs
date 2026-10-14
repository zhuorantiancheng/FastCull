//! Read JPEG metadata before SOS; never scan entropy-coded image data.
use anyhow::{Result, bail, ensure};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
pub const XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
pub(crate) fn valid_dimensions(w: u32, h: u32) -> bool {
    (1..=16384).contains(&w)
        && (1..=16384).contains(&h)
        && u64::from(w) * u64::from(h) <= 160_000_000
}
/// Read only bounded JPEG headers, including an embedded JPEG at a TIFF offset.
/// Never scan RAW strips or JPEG entropy data to determine fitted geometry.
pub fn dimensions(file: &mut File, offset: u64, len: u64) -> Result<(u32, u32)> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| anyhow::anyhow!("JPEG range overflow"))?;
    ensure!(
        end <= file.metadata()?.len() && len >= 4,
        "truncated JPEG range"
    );
    let limit = end.min(offset.saturating_add(4 * 1024 * 1024));
    file.seek(SeekFrom::Start(offset))?;
    let mut pair = [0u8; 2];
    file.read_exact(&mut pair)?;
    ensure!(pair == [255, 216], "not JPEG");
    loop {
        ensure!(
            file.stream_position()? + 2 <= limit,
            "JPEG header exceeds range"
        );
        file.read_exact(&mut pair)?;
        ensure!(pair[0] == 255, "invalid JPEG marker");
        while pair[1] == 255 {
            ensure!(
                file.stream_position()? < limit,
                "JPEG padding exceeds range"
            );
            file.read_exact(&mut pair[1..])?;
        }
        let marker = pair[1];
        ensure!(
            !matches!(marker, 0xda | 0xd9),
            "JPEG dimensions missing before image data"
        );
        if marker == 1 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        ensure!(
            file.stream_position()? + 2 <= limit,
            "truncated JPEG segment"
        );
        file.read_exact(&mut pair)?;
        let length = u16::from_be_bytes(pair) as u64;
        ensure!(length >= 2, "invalid JPEG segment length");
        let next = file.stream_position()? + length - 2;
        ensure!(next <= limit, "JPEG segment outside range");
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            ensure!(length >= 8, "truncated JPEG SOF");
            let mut sof = [0u8; 5];
            file.read_exact(&mut sof)?;
            let h = u16::from_be_bytes([sof[1], sof[2]]) as u32;
            let w = u16::from_be_bytes([sof[3], sof[4]]) as u32;
            ensure!(valid_dimensions(w, h), "invalid JPEG dimensions");
            return Ok((w, h));
        }
        file.seek(SeekFrom::Start(next))?;
    }
}
pub struct Segment {
    pub start: u64,
    pub end: u64,
    pub payload: Vec<u8>,
}
pub struct Header {
    pub app1: Vec<Segment>,
    pub image_start: u64,
    pub extended: bool,
}
pub fn header(file: &mut File) -> Result<Header> {
    file.seek(SeekFrom::Start(0))?;
    let size = file.metadata()?.len();
    let mut soi = [0u8; 2];
    file.read_exact(&mut soi)?;
    ensure!(soi == [255, 216], "not a JPEG file");
    let mut app1 = Vec::new();
    let mut extended = false;
    loop {
        let start = file.stream_position()?;
        ensure!(start <= 4 * 1024 * 1024, "JPEG metadata exceeds 4 MB limit");
        let mut marker = [0u8; 2];
        file.read_exact(&mut marker)?;
        ensure!(marker[0] == 255, "invalid JPEG marker");
        while marker[1] == 255 {
            file.read_exact(&mut marker[1..])?;
        }
        if marker[1] == 218 || marker[1] == 217 {
            return Ok(Header {
                app1,
                image_start: start,
                extended,
            });
        }
        if marker[1] == 1 || (208..=215).contains(&marker[1]) {
            continue;
        }
        let mut bytes = [0u8; 2];
        file.read_exact(&mut bytes)?;
        let len = u16::from_be_bytes(bytes) as u64;
        ensure!(len >= 2, "invalid JPEG segment length");
        let end = file
            .stream_position()?
            .checked_add(len - 2)
            .ok_or_else(|| anyhow::anyhow!("JPEG segment overflow"))?;
        ensure!(end <= size, "truncated JPEG metadata");
        if marker[1] == 225 {
            let mut payload = vec![0; len as usize - 2];
            file.read_exact(&mut payload)?;
            extended |= payload.starts_with(b"http://ns.adobe.com/xmp/extension/\0");
            app1.push(Segment {
                start,
                end,
                payload,
            });
        } else {
            file.seek(SeekFrom::Start(end))?;
        }
    }
}
impl Header {
    pub fn xmp(&self) -> Result<Option<&Segment>> {
        let mut packets = self
            .app1
            .iter()
            .filter(|s| s.payload.starts_with(XMP_HEADER));
        let first = packets.next();
        ensure!(
            packets.next().is_none(),
            "JPEG has multiple standard XMP packets; preserved without editing"
        );
        Ok(first)
    }
}
pub fn read_packet(path: &Path) -> Result<Option<String>> {
    let header = header(&mut File::open(path)?)?;
    let Some(packet) = header.xmp()? else {
        return Ok(None);
    };
    Ok(Some(
        std::str::from_utf8(&packet.payload[XMP_HEADER.len()..])?.to_owned(),
    ))
}
pub fn write_packet(path: &Path, original: Option<&str>, packet: &str) -> Result<()> {
    write_packet_with_orientation(path, original, packet, None)
}
pub(crate) fn write_packet_with_orientation(
    path: &Path,
    original: Option<&str>,
    packet: &str,
    orientation: Option<u16>,
) -> Result<()> {
    use std::io::Write;
    let mut file = File::open(path)?;
    let header = header(&mut file)?;
    ensure!(
        !header.extended,
        "JPEG 含扩展 XMP，已保留原文件；请使用宿主修改元数据"
    );
    let segment = header.xmp()?;
    let existing = segment
        .map(|s| std::str::from_utf8(&s.payload[XMP_HEADER.len()..]))
        .transpose()?;
    ensure!(
        existing == original,
        "JPEG XMP changed while saving; retry after host finishes"
    );
    let payload = XMP_HEADER.len() + packet.len();
    ensure!(
        payload + 2 <= u16::MAX as usize,
        "JPEG XMP packet exceeds APP1 limit"
    );
    let metadata = file.metadata()?;
    let (start, end) = segment.map_or((2, 2), |s| (s.start, s.end));
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".fastcull-")
        .suffix(".jpg.tmp")
        .tempfile_in(parent)?;
    file.seek(SeekFrom::Start(0))?;
    ensure!(
        std::io::copy(&mut (&mut file).take(start), &mut tmp)? == start,
        "JPEG source changed"
    );
    tmp.write_all(&[255, 225])?;
    tmp.write_all(&((payload + 2) as u16).to_be_bytes())?;
    tmp.write_all(XMP_HEADER)?;
    tmp.write_all(packet.as_bytes())?;
    file.seek(SeekFrom::Start(end))?;
    ensure!(
        std::io::copy(&mut file, &mut tmp)? == metadata.len() - end,
        "JPEG source changed"
    );
    if let Some(orientation) = orientation {
        for segment in &header.app1 {
            if let Some((offset, little)) = exif_orientation(segment) {
                let delta = 4 + payload as i64 - (end - start) as i64;
                let position = if offset >= end {
                    (offset as i64 + delta) as u64
                } else {
                    offset
                };
                tmp.as_file_mut().seek(SeekFrom::Start(position))?;
                tmp.write_all(&if little {
                    orientation.to_le_bytes()
                } else {
                    orientation.to_be_bytes()
                })?;
            }
        }
    }
    tmp.as_file().sync_all()?;
    let now = std::fs::metadata(path)?;
    if now.len() != metadata.len()
        || now.modified()? != metadata.modified()?
        || read_packet(path)?.as_deref() != original
    {
        bail!("JPEG changed while saving; retry after host finishes");
    }
    drop(file);
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn exif_orientation(segment: &Segment) -> Option<(u64, bool)> {
    let bytes = segment.payload.strip_prefix(b"Exif\0\0")?;
    let little = match bytes.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b: [u8; 2] = bytes.get(o..o.checked_add(2)?)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b: [u8; 4] = bytes.get(o..o.checked_add(4)?)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let first = u32_at(4)? as usize;
    let count = u16_at(first)? as usize;
    if first.checked_add(2 + count * 12)? > bytes.len() {
        return None;
    }
    for i in 0..count {
        let entry = first + 2 + i * 12;
        if u16_at(entry)? == 0x0112 && u16_at(entry + 2)? == 3 && u32_at(entry + 4)? == 1 {
            return Some((
                segment.end - segment.payload.len() as u64 + 6 + (entry + 8) as u64,
                little,
            ));
        }
    }
    None
}
