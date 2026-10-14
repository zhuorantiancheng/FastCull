//! Bounded IFD0 reads: never scan the compressed RAW/image payload for metadata.
use std::{
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) fn read(path: &Path) -> Option<u8> {
    let mut file = File::open(path).ok()?;
    if crate::formats::is_jpeg(path) {
        let header = crate::jpeg::header(&mut file).ok()?;
        return header.app1.iter().find_map(|segment| {
            let bytes = segment.payload.strip_prefix(b"Exif\0\0")?;
            tiff(&mut Cursor::new(bytes), bytes.len() as u64)
        });
    }
    let size = file.metadata().ok()?.len();
    tiff(&mut file, size)
}

fn tiff(reader: &mut (impl Read + Seek), size: u64) -> Option<u8> {
    let mut header = [0; 8];
    reader.read_exact(&mut header).ok()?;
    let little = match &header[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let short = |b: &[u8]| {
        let v = [b[0], b[1]];
        if little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        }
    };
    let long = |b: &[u8]| {
        let v = [b[0], b[1], b[2], b[3]];
        if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        }
    };
    if short(&header[2..]) != 42 {
        return None;
    }
    let offset = long(&header[4..]) as u64;
    if offset < 8 || offset.checked_add(2)? > size {
        return None;
    }
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut count = [0; 2];
    reader.read_exact(&mut count).ok()?;
    let count = short(&count) as usize;
    if count > 1024 || offset.checked_add(2 + count as u64 * 12)? > size {
        return None;
    }
    let mut entries = vec![0; count * 12];
    reader.read_exact(&mut entries).ok()?;
    let mut rating = None;
    let mut packet = None;
    for entry in entries.chunks_exact(12) {
        let tag = short(entry);
        let kind = short(&entry[2..]);
        let count = long(&entry[4..]);
        if tag == 0x4746 && kind == 3 && count == 1 {
            let value = short(&entry[8..]);
            if value <= 5 {
                rating = Some(value as u8);
            }
        }
        if tag == 0x02bc && matches!(kind, 1 | 2 | 7) && (5..=1_048_576).contains(&count) {
            packet = Some((long(&entry[8..]) as u64, count as usize));
        }
    }
    if let Some((offset, count)) = packet
        && offset >= 8
        && offset
            .checked_add(count as u64)
            .is_some_and(|end| end <= size)
    {
        let mut bytes = vec![0; count];
        if reader.seek(SeekFrom::Start(offset)).is_ok()
            && reader.read_exact(&mut bytes).is_ok()
            && let Ok(text) = std::str::from_utf8(&bytes)
            && let Ok(values) = crate::xmp::parse_values(text.trim_end_matches('\0'))
            && values.rating.is_some()
        {
            return values.rating;
        }
    }
    rating
}
