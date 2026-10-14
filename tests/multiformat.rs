use fastcull::{
    arw::ArwInfo,
    formats,
    imaging::{self, Level},
    jpeg, state,
    xmp::{self, Updates},
};
use std::{fs, path::Path};
const TINY: &[u8] = include_bytes!("fixtures/tiny.jpg");
#[test]
fn jpeg_edits_preserve_pixels_and_other_metadata_and_never_use_raw_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("同名.JPG");
    fs::write(&path, TINY).unwrap();
    let raw_sidecar = path.with_extension("xmp");
    fs::write(&raw_sidecar, "raw sidecar unchanged").unwrap();
    let before = imaging::decode(&path, true).unwrap().1;
    let cached = ArwInfo::read(&path).unwrap();
    let original = r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmp:Rating="2" crs:Contrast2012="23"/>"#;
    jpeg::write_packet(&path, None, original).unwrap();
    let suffix = |path: &Path| {
        let mut f = fs::File::open(path).unwrap();
        let h = jpeg::header(&mut f).unwrap();
        fs::read(path).unwrap()[h.image_start as usize..].to_vec()
    };
    let compressed = suffix(&path);
    xmp::write(
        &path,
        &Updates {
            rating: Some(5),
            label: Some("Blue".into()),
            exposure: Some(333333),
            ..Default::default()
        },
    )
    .unwrap();
    let values = xmp::read(&path).unwrap();
    assert_eq!(values.rating, Some(5));
    assert_eq!(values.exposure, Some(333333));
    assert!(
        jpeg::read_packet(&path)
            .unwrap()
            .unwrap()
            .contains("crs:Contrast2012=\"23\"")
    );
    assert_eq!(suffix(&path), compressed);
    let after = imaging::decode_level(&path, &cached, Level::Full).unwrap();
    assert_eq!((after.width, after.height), (16, 12));
    assert_eq!(after.rgba, before.rgba);
    assert_eq!(
        fs::read_to_string(raw_sidecar).unwrap(),
        "raw sidecar unchanged"
    );
    assert!(
        state::apply_one(&state::Item {
            path: path.clone(),
            updates: Updates {
                rating: Some(0),
                label: Some(String::new()),
                exposure: None,
                ..Default::default()
            }
        })
        .unwrap()
    );
    assert_eq!(xmp::read(&path).unwrap().rating, Some(0));
    assert_eq!(suffix(&path), compressed);
}
#[test]
fn jpeg_exif_orientation_and_camera_metadata_survive_xmp_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Exif.jpeg");
    let mut tiff = b"II\x2a\0\x08\0\0\0".to_vec();
    tiff.extend(2u16.to_le_bytes());
    // Make text outside the IFD, and an inline Orientation value.
    for (tag, typ, count, value) in [(0x010fu16, 2u16, 6u32, 38u32), (0x0112, 3, 1, 6)] {
        tiff.extend(tag.to_le_bytes());
        tiff.extend(typ.to_le_bytes());
        tiff.extend(count.to_le_bytes());
        tiff.extend(value.to_le_bytes());
    }
    tiff.extend(0u32.to_le_bytes());
    tiff.extend(b"Canon\0");
    let payload = [b"Exif\0\0".as_slice(), &tiff].concat();
    let data = [
        TINY[..2].to_vec(),
        vec![255, 225],
        ((payload.len() + 2) as u16).to_be_bytes().to_vec(),
        payload.clone(),
        TINY[2..].to_vec(),
    ]
    .concat();
    fs::write(&path, &data).unwrap();
    let info = ArwInfo::read(&path).unwrap();
    assert_eq!(info.orientation, 6);
    assert_eq!(info.capture.make.as_deref(), Some("Canon"));
    let oriented = imaging::decode(&path, true).unwrap().1;
    assert_eq!((oriented.width, oriented.height), (12, 16));
    xmp::write(
        &path,
        &Updates {
            rating: Some(4),
            ..Default::default()
        },
    )
    .unwrap();
    let header = jpeg::header(&mut fs::File::open(&path).unwrap()).unwrap();
    assert!(header.app1.iter().any(|s| s.payload == payload));
    assert_eq!(imaging::decode(&path, true).unwrap().1.rgba, oriented.rgba);
    xmp::write(
        &path,
        &Updates {
            orientation: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(ArwInfo::read(&path).unwrap().orientation, 3);
    assert_eq!(xmp::read(&path).unwrap().orientation, Some(3));
}
#[test]
fn jpeg_ambiguous_extended_and_corrupt_metadata_are_preserved_on_failed_edits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("protected.jpg");
    let ext = b"http://ns.adobe.com/xmp/extension/\0retained bytes";
    let data = [
        TINY[..2].to_vec(),
        vec![255, 225],
        ((ext.len() + 2) as u16).to_be_bytes().to_vec(),
        ext.to_vec(),
        TINY[2..].to_vec(),
    ]
    .concat();
    fs::write(&path, &data).unwrap();
    assert!(
        xmp::write(
            &path,
            &Updates {
                rating: Some(4),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), data);
    for data in [
        vec![],
        vec![255, 216, 255, 225, 0, 1],
        vec![255, 216, 255, 225, 255, 255, 0],
    ] {
        fs::write(&path, &data).unwrap();
        assert!(
            xmp::write(
                &path,
                &Updates {
                    rating: Some(4),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), data);
    }
}
#[test]
fn moving_raw_and_matching_jpeg_keeps_independent_metadata_and_refresh_rules() {
    use fastcull::relocating::{self, Journal};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    let raw = source.join("same.NEF");
    let jpg = source.join("same.JPG");
    fs::write(&raw, b"raw").unwrap();
    fs::write(&jpg, TINY).unwrap();
    fs::write(raw.with_extension("xmp"), b"RAW metadata").unwrap();
    xmp::write(
        &jpg,
        &Updates {
            rating: Some(4),
            ..Default::default()
        },
    )
    .unwrap();
    let items = relocating::prepare(&[raw.clone(), jpg.clone()], &[0, 1], &target).unwrap();
    let mut journal = Journal::create(&dir.path().join("journals"), &items).unwrap();
    // Move the JPEG first: the RAW sidecar must remain at source.
    let item = relocating::refresh(&items[1], &jpg).unwrap();
    relocating::execute(&item, &mut journal).unwrap();
    assert_eq!(
        fs::read(raw.with_extension("xmp")).unwrap(),
        b"RAW metadata"
    );
    assert_eq!(xmp::read(&target.join("same.JPG")).unwrap().rating, Some(4));
    let item = relocating::refresh(&items[0], &raw).unwrap();
    relocating::execute(&item, &mut journal).unwrap();
    journal.finish().unwrap();
    assert_eq!(fs::read(target.join("same.xmp")).unwrap(), b"RAW metadata");
}
#[test]
fn supported_formats_include_raw_families_and_jpeg_but_exclude_video_sidecars_and_png() {
    for ext in formats::EXTENSIONS {
        assert!(formats::supported(Path::new(&format!(
            "photo.{}",
            ext.to_uppercase()
        ))));
    }
    for ext in [
        "mp4", "xmp", "acr", "png", "heic", "txt", "cr3", "raf", "dng", "rw2", "orf",
    ] {
        assert!(!formats::supported(Path::new(&format!("photo.{ext}"))));
    }
}
#[test]
fn real_raw_samples_use_largest_preview_then_original_resolution_if_needed() {
    let Some(folder) = std::env::var_os("FASTCULL_RAW_TEST_DIR") else {
        return;
    };
    let mut checked = 0;
    for path in fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| formats::supported(p) && !formats::is_jpeg(p))
    {
        let info = ArwInfo::read(&path).unwrap();
        let (w, h) = info.raw_size.expect("native format fixture");
        let fit = imaging::decode_level(&path, &info, Level::Fit(800)).unwrap();
        assert!(fit.width.max(fit.height) <= 800);
        let full = imaging::decode_level(&path, &info, Level::Full).unwrap();
        assert!(
            full.width + 2 >= w as usize && full.height + 2 >= h as usize,
            "{} {}x{} versus {w}x{h}",
            path.display(),
            full.width,
            full.height
        );
        assert_eq!(full.rgba.len(), full.width * full.height * 4);
        checked += 1;
    }
    assert!(
        checked >= 2,
        "expected real samples from at least Canon and Nikon"
    );
}
