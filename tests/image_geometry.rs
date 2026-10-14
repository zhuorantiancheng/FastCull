use fastcull::{arw::ArwInfo, jpeg};
use std::{fs, io::Seek};

fn tiff(little: bool, typ: u16, w: u32, h: u32) -> Vec<u8> {
    let mut b = vec![0; 128];
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
    b[..2].copy_from_slice(if little { b"II" } else { b"MM" });
    b[2..4].copy_from_slice(&u16b(42));
    b[4..8].copy_from_slice(&u32b(8));
    b[8..10].copy_from_slice(&u16b(4));
    let jpg = include_bytes!("fixtures/preview-3x2.jpg");
    for (i, (tag, ty, v)) in [
        (0x0201, 4, 128),
        (0x0202, 4, jpg.len() as u32),
        (0xa002, typ, w),
        (0xa003, typ, h),
    ]
    .into_iter()
    .enumerate()
    {
        let e = 10 + i * 12;
        b[e..e + 2].copy_from_slice(&u16b(tag));
        b[e + 2..e + 4].copy_from_slice(&u16b(ty));
        b[e + 4..e + 8].copy_from_slice(&u32b(1));
        if ty == 3 {
            b[e + 8..e + 10].copy_from_slice(&u16b(v as u16));
        } else {
            b[e + 8..e + 12].copy_from_slice(&u32b(v));
        }
    }
    b.extend_from_slice(jpg);
    b
}

#[test]
fn original_dimensions_are_available_before_raw_preview_decode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("camera.ARW");
    for little in [true, false] {
        for typ in [3, 4] {
            fs::write(&path, tiff(little, typ, 6192, 4128)).unwrap();
            let info = ArwInfo::read(&path).unwrap();
            assert_eq!(info.image_size, Some((6192, 4128)));
            let pixels =
                fastcull::imaging::decode_level(&path, &info, fastcull::imaging::Level::Fit(50))
                    .unwrap();
            assert_eq!((pixels.width, pixels.height), (50, 33));
            // Invalid/missing EXIF size falls back to bounded embedded SOF.
            fs::write(&path, tiff(little, typ, 0, 0)).unwrap();
            assert_eq!(ArwInfo::read(&path).unwrap().image_size, Some((120, 80)));
        }
    }
}

#[test]
fn jpeg_uses_actual_sof_despite_stale_exif_dimensions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edited.jpg");
    let jpg = include_bytes!("fixtures/tiny.jpg");
    let mut app = b"Exif\0\0".to_vec();
    app.extend_from_slice(&tiff(true, 4, 6192, 4128));
    let mut b = vec![255, 216, 255, 225];
    b.extend_from_slice(&((app.len() + 2) as u16).to_be_bytes());
    b.extend_from_slice(&app);
    b.extend_from_slice(&jpg[2..]);
    fs::write(&path, b).unwrap();
    assert_eq!(ArwInfo::read(&path).unwrap().image_size, Some((16, 12)));
}

#[test]
fn jpeg_size_reader_respects_embedded_range_and_stops_at_scan_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("range.bin");
    let jpg = include_bytes!("fixtures/tiny.jpg");
    fs::write(&path, [b"prefix".as_slice(), jpg.as_slice()].concat()).unwrap();
    let mut file = fs::File::open(&path).unwrap();
    assert_eq!(
        jpeg::dimensions(&mut file, 6, jpg.len() as u64).unwrap(),
        (16, 12)
    );
    assert!(jpeg::dimensions(&mut file, 6, 4).is_err());
    assert!(jpeg::dimensions(&mut file, u64::MAX, 10).is_err());
    drop(file);
    // A fake SOF after SOS must never be interpreted as metadata.
    fs::write(
        &path,
        [b"\xff\xd8\xff\xda".as_slice(), jpg.as_slice()].concat(),
    )
    .unwrap();
    let mut file = fs::File::open(&path).unwrap();
    assert!(jpeg::dimensions(&mut file, 0, jpg.len() as u64 + 4).is_err());
    assert_eq!(file.stream_position().unwrap(), 4);
}
