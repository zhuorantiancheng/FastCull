use fastcull::{
    arw::ArwInfo,
    imaging::{Pixels, orient},
    state,
    xmp::{self, Updates},
};
use std::{fs, process::Command};

#[test]
fn exposure_is_namespace_resolved_and_only_changed_field_is_exported() {
    use fastcull::handoff::Manifest;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("曝光.ARW");
    let original = r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:camera="http://ns.adobe.com/camera-raw-settings/1.0/" camera:Exposure2012="0.100000" camera:Contrast2012="23"/>"#;
    fs::write(raw.with_extension("xmp"), original).unwrap();
    let u = Updates {
        exposure: Some(433333),
        ..Default::default()
    };
    xmp::write(&raw, &u).unwrap();
    let text = fs::read_to_string(raw.with_extension("xmp")).unwrap();
    assert!(text.contains("camera:Exposure2012=\"0.433333\""));
    assert!(text.contains("camera:Contrast2012=\"23\""));
    assert!(!text.contains("crs:Exposure2012"));
    let mut m = Manifest::new("exposure-test".into(), std::slice::from_ref(&raw)).unwrap();
    m.record(0, &u);
    let path = dir.path().join("session.txt");
    m.write(&path).unwrap();
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .starts_with("FASTCULL_SESSION_V2")
    );
    let m = Manifest::read(&path).unwrap();
    let rows = m.rows_since(0);
    let v = rows[0].1.as_ref().unwrap();
    assert_eq!(v.exposure, Some(433333));
    assert!(v.rating.is_none() && v.flag.is_none() && v.label.is_none());
    assert!(m.rows_since(1).is_empty());
    assert!(
        xmp::patch(
            &text,
            &Updates {
                exposure: Some(5_000_001),
                ..Default::default()
            }
        )
        .is_err()
    );
    fs::write(
        raw.with_extension("xmp"),
        original.replace("camera:Exposure2012=\"0.100000\"", ""),
    )
    .unwrap();
    assert!(m.rows_since(0)[0].1.is_err()); // Never acknowledge a missing changed field.
}

#[test]
fn stars_acknowledgement_is_independent_of_other_fields_and_later_stars() {
    use fastcull::handoff::Manifest;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("stars.ARW");
    let mut m = Manifest::new("stars-session".into(), std::slice::from_ref(&raw)).unwrap();
    let star = Updates {
        rating: Some(4),
        ..Default::default()
    };
    xmp::write(&raw, &star).unwrap();
    m.record(0, &star);
    m.record(
        0,
        &Updates {
            label: Some("Blue".into()),
            ..Default::default()
        },
    );
    assert!(m.rows_since(0)[0].1.is_err()); // Label was not persisted.
    let stars = m.stars_since(0);
    assert_eq!(stars[0].1.as_ref().unwrap().rating, Some(4));
    assert!(stars[0].1.as_ref().unwrap().label.is_none());
    assert!(m.stars_since(2).is_empty());
    xmp::write(
        &raw,
        &Updates {
            label: Some("Blue".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let remaining = m.rows_with_stars_ack(0, 2);
    let v = remaining[0].1.as_ref().unwrap();
    assert!(v.rating.is_none());
    assert_eq!(v.label.as_deref(), Some("Blue"));
    let later = Updates {
        rating: Some(5),
        ..Default::default()
    };
    xmp::write(&raw, &later).unwrap();
    m.record(0, &later);
    assert_eq!(
        m.rows_with_stars_ack(0, 2)[0].1.as_ref().unwrap().rating,
        Some(5)
    );
}

#[test]
fn thumbnail_uses_picture_preview_instead_of_padded_camera_icon() {
    use fastcull::{
        arw::JpegRef,
        imaging::{self, Level},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preview.ARW");
    let icon = include_bytes!("fixtures/tiny.jpg"); // 4:3 camera icon canvas
    let preview = include_bytes!("fixtures/preview-3x2.jpg");
    fs::write(&path, [icon.as_slice(), preview.as_slice()].concat()).unwrap();
    let full = JpegRef {
        offset: icon.len() as u64,
        len: preview.len() as u64,
    };
    let info = ArwInfo {
        jpeg: full.clone(),
        bitmap_thumbnail: None,
        preview: full,
        thumbnail: JpegRef {
            offset: 0,
            len: icon.len() as u64,
        },
        orientation: 1,
        summary: String::new(),
        capture: Default::default(),
        raw_size: None,
        image_size: None,
    };
    let p = imaging::decode_level(&path, &info, Level::Thumb).unwrap();
    assert_eq!((p.width, p.height), (120, 80));
    assert!(p.rgba.chunks_exact(4).all(|p| p[1] > 140));
    let broken = ArwInfo {
        preview: JpegRef { offset: 0, len: 4 },
        ..info
    };
    let p = imaging::decode_level(&path, &broken, Level::Thumb).unwrap();
    assert_eq!((p.width, p.height), (120, 80));
}

#[test]
fn fit_preview_is_bounded_but_full_keeps_actual_pixels_and_recovers_bad_thumbnail() {
    use fastcull::{
        arw::JpegRef,
        imaging::{self, Level},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.ARW");
    let jpeg = include_bytes!("fixtures/tiny.jpg");
    let mut data = vec![0xff, 0xd8, 0, 0];
    data.extend_from_slice(jpeg);
    fs::write(&path, data).unwrap();
    let info = ArwInfo {
        raw_size: None,
        bitmap_thumbnail: None,
        image_size: None,
        jpeg: JpegRef {
            offset: 4,
            len: jpeg.len() as u64,
        },
        preview: JpegRef {
            offset: 4,
            len: jpeg.len() as u64,
        },
        thumbnail: JpegRef { offset: 0, len: 4 },
        orientation: 1,
        summary: String::new(),
        capture: Default::default(),
    };
    let fit = imaging::decode_level(&path, &info, Level::Fit(8)).unwrap();
    assert_eq!((fit.width, fit.height), (8, 6));
    let full = imaging::decode_level(&path, &info, Level::Full).unwrap();
    assert_eq!((full.width, full.height), (16, 12));
    assert!(imaging::decode_level(&path, &info, Level::Thumb).is_ok());
}
#[test]
fn repaired_metadata_can_retry_without_reinitializing_successful_user_edits() {
    use fastcull::session::Session;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retry.ARW");
    fs::write(path.with_extension("xmp"), "<broken>").unwrap();
    let session = Session::new(vec![state::Item {
        path: path.clone(),
        updates: Updates {
            rating: Some(2),
            ..Default::default()
        },
    }]);
    assert!(session.initialize(0).is_err());
    assert_eq!(session.stats().error, 1);
    fs::write(path.with_extension("xmp"), xmp_text("xmp:Rating='1'")).unwrap();
    assert!(session.retry(0).is_ok());
    assert_eq!(session.stats().error, 0);
    xmp::write(
        &path,
        &Updates {
            rating: Some(5),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(session.retry(0).is_ok());
    assert_eq!(xmp::read(&path).unwrap().rating, Some(5));
}
#[test]
fn session_exports_only_newly_edited_fields_and_handles_empty_values() {
    use fastcull::handoff::Manifest;
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("照片 ARW.ARW");
    let b = dir.path().join("untouched.ARW");
    xmp::write(
        &a,
        &Updates {
            rating: Some(4),
            label: Some("".into()),
            flag: Some("pick".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let file = dir.path().join("session.txt");
    let mut m = Manifest::new("test-id".into(), &[a.clone(), b]).unwrap();
    m.record(
        0,
        &Updates {
            label: Some("".into()),
            ..Default::default()
        },
    );
    m.write(&file).unwrap();
    let rows = Manifest::read(&file).unwrap().rows_since(0);
    assert_eq!(rows.len(), 1);
    let v = rows[0].1.as_ref().unwrap();
    assert_eq!(v.label.as_deref(), Some(""));
    assert_eq!(v.rating, None);
    assert_eq!(v.flag, None);
    let acknowledged = m.revision;
    m.record(
        0,
        &Updates {
            rating: Some(4),
            ..Default::default()
        },
    );
    let rows = m.rows_since(acknowledged);
    let v = rows[0].1.as_ref().unwrap();
    assert_eq!(v.rating, Some(4));
    assert_eq!(v.label, None);
    assert!(m.rows_since(m.revision).is_empty());
    fs::write(a.with_extension("xmp"), "<broken>").unwrap();
    assert!(m.rows_since(0)[0].1.is_err());
}
#[test]
fn bridge_encoding_roundtrips_unicode_newlines_and_delimiters() {
    use fastcull::handoff::{decode, encode};
    for s in ["", "C:/照片|空间/a.ARW", "标签 & \"引号\"\t\n"] {
        assert_eq!(decode(&encode(s)).unwrap(), s);
    }
    for s in ["x", "xx", "-", "ff"] {
        assert!(decode(s).is_err());
    }
}

fn xmp_text(fields: &str) -> String {
    format!(
        "<?xpacket begin=\"\u{feff}\"?>\r\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" crs:Exposure2012='+1.25' {fields}><crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 7</rdf:li></rdf:Seq></crs:ToneCurvePV2012></rdf:Description></rdf:RDF></x:xmpmeta>\r\n<?xpacket end=\"w\"?>"
    )
}
#[test]
fn patch_preserves_every_unrelated_byte() {
    let text = xmp_text("xmp:Rating='1' xmp:Label=\"Red\"");
    let out = xmp::patch(
        &text,
        &Updates {
            rating: Some(5),
            label: Some("A & \"B\"".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        out,
        text.replace("xmp:Rating='1'", "xmp:Rating='5'")
            .replace("xmp:Label=\"Red\"", "xmp:Label=\"A &amp; &quot;B&quot;\"")
    );
}
#[test]
fn element_style_and_namespace_insertion_are_valid() {
    let text = xmp_text("").replace(
        "<crs:ToneCurvePV2012>",
        "<xmp:Rating>2</xmp:Rating><xmp:Label>Red</xmp:Label><crs:ToneCurvePV2012>",
    );
    let out = xmp::patch(
        &text,
        &Updates {
            rating: Some(0),
            label: Some("".into()),
            flag: Some("".into()),
            orientation: Some(1),
            exposure: None,
            white_balance: None,
            basic: Default::default(),
            crop: None,
        },
    )
    .unwrap();
    assert!(out.contains("<xmp:Rating>0</xmp:Rating>"));
    assert!(out.contains("<xmp:Label></xmp:Label>"));
    assert!(out.contains("xmlns:sr=\"urn:zrtc:syncratings:1/\""));
    assert!(out.contains("xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\""));
    assert!(out.contains("crs:Exposure2012='+1.25'"));
}
#[test]
fn namespace_aliases_are_resolved_without_rewriting_tags() {
    let text = xmp_text("xmp:Rating='2'")
        .replace("xmlns:xmp=", "xmlns:m=")
        .replace("xmp:Rating", "m:Rating");
    let out = xmp::patch(
        &text,
        &Updates {
            rating: Some(4),
            label: Some("Blue".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(out.contains("m:Rating='4'"));
    assert!(out.contains("m:Label=\"Blue\""));
}
#[test]
fn legacy_unbound_bridge_attribute_is_patched_not_duplicated() {
    // Python prototype inserted sr:Flag without declaring xmlns:sr in existing LR sidecars.
    let text = xmp_text("xmp:Rating='1' xmp:Label=\"\" sr:Flag=\"\"");
    let out = xmp::patch(
        &text,
        &Updates {
            rating: Some(4),
            label: Some("Red".into()),
            flag: Some("pick".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out.matches("sr:Flag=").count(), 1);
    assert!(out.contains("sr:Flag=\"pick\""));
    assert!(out.contains("xmlns:sr=\"urn:zrtc:syncratings:1/\""));
    assert!(out.contains("crs:Exposure2012='+1.25'"));
    assert!(out.contains(
        "<crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 7</rdf:li></rdf:Seq></crs:ToneCurvePV2012>"
    ));
}
#[test]
fn legacy_missing_namespaces_are_repaired_even_for_rating_only() {
    let text = xmp_text("sr:Flag='reject' tiff:Orientation='6'");
    let out = xmp::patch(
        &text,
        &Updates {
            rating: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(out.contains("xmlns:sr=\"urn:zrtc:syncratings:1/\""));
    assert!(out.contains("xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\""));
    assert!(out.contains("sr:Flag='reject' tiff:Orientation='6'"));
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("legacy.ARW");
    fs::write(raw.with_extension("xmp"), text).unwrap();
    let v = xmp::read(&raw).unwrap();
    assert_eq!(v.flag.as_deref(), Some("reject"));
    assert_eq!(v.orientation, Some(6));
}
#[test]
fn genuine_duplicate_attributes_are_rejected_without_overwrite() {
    let text = xmp_text("xmlns:sr='urn:zrtc:syncratings:1/' sr:Flag='pick' sr:Flag='reject'");
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("duplicate.ARW");
    fs::write(raw.with_extension("xmp"), &text).unwrap();
    assert!(
        xmp::write(
            &raw,
            &Updates {
                flag: Some("".into()),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(raw.with_extension("xmp")).unwrap(), text);
}
#[test]
fn malformed_sidecar_is_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("照片.ARW");
    let sidecar = raw.with_extension("xmp");
    for bad in [
        "<broken>",
        "<rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><bad></rdf:Description>",
        "\u{fffd}not xml",
    ] {
        fs::write(&sidecar, bad).unwrap();
        assert!(
            xmp::write(
                &raw,
                &Updates {
                    rating: Some(4),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&sidecar).unwrap(), bad);
    }
    fs::write(&sidecar, [0xff, 0xfe, 0x21]).unwrap();
    assert!(
        xmp::write(
            &raw,
            &Updates {
                rating: Some(4),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_eq!(fs::read(&sidecar).unwrap(), [0xff, 0xfe, 0x21]);
}
#[test]
fn roundtrip_and_clear_are_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("中文 空格.ARW");
    xmp::write(
        &raw,
        &Updates {
            rating: Some(5),
            label: Some("Purple".into()),
            flag: Some("pick".into()),
            orientation: Some(6),
            exposure: None,
            white_balance: None,
            basic: Default::default(),
            crop: None,
        },
    )
    .unwrap();
    let v = xmp::read(&raw).unwrap();
    assert_eq!(v.rating, Some(5));
    assert_eq!(v.orientation, Some(6));
    xmp::write(
        &raw,
        &Updates {
            rating: Some(0),
            label: Some(String::new()),
            flag: Some(String::new()),
            ..Default::default()
        },
    )
    .unwrap();
    let v = xmp::read(&raw).unwrap();
    assert_eq!(v.rating, Some(0));
    assert_eq!(v.label.as_deref(), Some(""));
    assert_eq!(v.flag.as_deref(), Some(""));
    assert_eq!(v.orientation, Some(6));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}
#[test]
fn state_missing_and_empty_fields_are_distinct() {
    let v = state::parse("\u{feff}# comment\na.ARW\nb.ARW|||\nc.ARW|3|Red|-1\n").unwrap();
    assert!(v[0].updates.empty());
    assert_eq!(v[1].updates.rating, Some(0));
    assert_eq!(v[1].updates.flag.as_deref(), Some(""));
    assert_eq!(v[2].updates.flag.as_deref(), Some("reject"));
    assert!(state::parse("a.ARW|6").is_err());
    assert!(state::parse("a.ARW|3|Red|9").is_err());
}
#[test]
fn apply_zero_does_not_create_sidecar_and_missing_does_not_clear() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("照片.ARW");
    let items = state::parse(&format!("{}|0||0", raw.display())).unwrap();
    let stats = state::apply(&items);
    assert_eq!(stats.kept, 1);
    assert!(!raw.with_extension("xmp").exists());
    xmp::write(
        &raw,
        &Updates {
            rating: Some(4),
            label: Some("Blue".into()),
            flag: Some("pick".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let stats = state::apply(&state::parse(&format!("{}|||", raw.display())).unwrap());
    assert_eq!(stats.wrote, 1);
    assert_eq!(xmp::read(&raw).unwrap().rating, Some(0));
    let stats = state::apply(&state::parse(&raw.to_string_lossy()).unwrap());
    assert_eq!(stats.kept, 1);
}
#[test]
fn cli_reports_errors_without_interaction() {
    let dir = tempfile::tempdir().unwrap();
    let statefile = dir.path().join("状态.txt");
    let raw = dir.path().join("照片.ARW");
    fs::write(&statefile, format!("{}|3|Green|1\n", raw.display())).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fastcull"))
        .arg("--apply-state")
        .arg(&statefile)
        .env("FASTCULL_LOG_DIR", dir.path().join("logs"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("RESULT wrote=1 kept=0 error=0")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_fastcull"))
        .arg("--apply-state")
        .arg(dir.path().join("缺失 文件.txt"))
        .env("FASTCULL_LOG_DIR", dir.path().join("logs"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stdout).unwrap();
    assert!(error.contains("ERROR") && error.contains("缺失 文件.txt"));
}

fn fixture(little: bool) -> Vec<u8> {
    let mut b = vec![0; 256];
    b[..2].copy_from_slice(if little { b"II" } else { b"MM" });
    fn u16(b: &mut [u8], o: usize, v: u16, l: bool) {
        b[o..o + 2].copy_from_slice(&if l { v.to_le_bytes() } else { v.to_be_bytes() });
    }
    fn u32(b: &mut [u8], o: usize, v: u32, l: bool) {
        b[o..o + 4].copy_from_slice(&if l { v.to_le_bytes() } else { v.to_be_bytes() });
    }
    u16(&mut b, 2, 42, little);
    u32(&mut b, 4, 8, little);
    u16(&mut b, 8, 4, little);
    // Length intentionally comes BEFORE offset. SubIFDs is an array of two pointers.
    for (i, (tag, typ, cnt, value)) in [
        (0x0202, 4, 1, 20),
        (0x0201, 4, 1, 200),
        (0x0112, 3, 1, 6),
        (0x014a, 4, 2, 100),
    ]
    .into_iter()
    .enumerate()
    {
        let o = 10 + 12 * i;
        u16(&mut b, o, tag, little);
        u16(&mut b, o + 2, typ, little);
        u32(&mut b, o + 4, cnt, little);
        if typ == 3 {
            u16(&mut b, o + 8, value as u16, little);
        } else {
            u32(&mut b, o + 8, value, little);
        }
    }
    u32(&mut b, 58, 8, little); // deliberate IFD cycle
    u32(&mut b, 100, 112, little);
    u32(&mut b, 104, 146, little);
    u16(&mut b, 112, 0, little);
    u32(&mut b, 114, 0, little);
    u16(&mut b, 146, 2, little);
    for (i, (tag, value)) in [(0x0201, 220), (0x0202, 30)].into_iter().enumerate() {
        let o = 148 + i * 12;
        u16(&mut b, o, tag, little);
        u16(&mut b, o + 2, 4, little);
        u32(&mut b, o + 4, 1, little);
        u32(&mut b, o + 8, value, little);
    }
    b
}
#[test]
fn ifd_pairs_array_cycle_endianness_and_orientation() {
    for little in [true, false] {
        let b = fixture(little);
        let info = ArwInfo::parse(&b, b.len() as u64).unwrap();
        assert_eq!(info.jpeg.offset, 220);
        assert_eq!(info.jpeg.len, 30);
        assert_eq!(info.orientation, 6);
    }
}

#[test]
fn structured_capture_metadata_is_bounded_and_original_capture_time_wins() {
    let mut b = vec![0u8; 2048];
    b[0..4].copy_from_slice(&[b'I', b'I', 42, 0]);
    b[4..8].copy_from_slice(&8u32.to_le_bytes());
    let fields = [
        (0x0201u16, 4u16, 1u32, 1800u32),
        (0x0202, 4, 1, 20),
        (0x010f, 2, 5, 500),
        (0x0110, 2, 10, 520),
        (0xa434, 2, 10, 540),
        (0x0132, 2, 20, 560),
        (0x9003, 2, 20, 600),
        (0x8827, 3, 1, 5000),
        (0x829d, 5, 1, 640),
        (0x829a, 5, 1, 650),
        (0x920a, 5, 1, 660),
    ];
    b[8..10].copy_from_slice(&(fields.len() as u16).to_le_bytes());
    for (i, (tag, typ, count, value)) in fields.into_iter().enumerate() {
        let o = 10 + i * 12;
        b[o..o + 2].copy_from_slice(&tag.to_le_bytes());
        b[o + 2..o + 4].copy_from_slice(&typ.to_le_bytes());
        b[o + 4..o + 8].copy_from_slice(&count.to_le_bytes());
        b[o + 8..o + 12].copy_from_slice(&value.to_le_bytes());
    }
    b[500..505].copy_from_slice(b"SONY\0");
    b[520..530].copy_from_slice(b"ILCE-6700\0");
    b[540..550].copy_from_slice(b"Lens test\0");
    b[560..580].copy_from_slice(b"2026:10:12 10:00:00\0");
    b[600..620].copy_from_slice(b"2026:08:04 21:10:07\0");
    for (offset, n, d) in [(640, 28u32, 10u32), (650, 1, 250), (660, 70, 1)] {
        b[offset..offset + 4].copy_from_slice(&n.to_le_bytes());
        b[offset + 4..offset + 8].copy_from_slice(&d.to_le_bytes());
    }
    let c = ArwInfo::parse(&b, b.len() as u64).unwrap().capture;
    assert_eq!(c.camera.as_deref(), Some("ILCE-6700"));
    assert_eq!(c.lens.as_deref(), Some("Lens test"));
    assert_eq!(c.captured_at.as_deref(), Some("2026:08:04 21:10:07"));
    assert_eq!(c.iso, Some(5000));
    assert_eq!(c.aperture, Some(2.8));
    assert_eq!(c.shutter, Some(1. / 250.));
    assert_eq!(c.focal, Some(70.));
    // A bad optional string does not make the embedded JPEG inaccessible.
    b[10 + 4 * 12 + 8..10 + 4 * 12 + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    let info = ArwInfo::parse(&b, b.len() as u64).unwrap();
    assert!(info.capture.lens.is_none());
    assert_eq!(info.jpeg.offset, 1800);
}
#[test]
fn malformed_tiff_never_panics_or_reads_raw_strips() {
    let b = fixture(true);
    for n in 0..b.len() {
        let result = std::panic::catch_unwind(|| ArwInfo::parse(&b[..n], n as u64));
        assert!(result.is_ok());
    }
    let mut corrupt = b.clone();
    corrupt[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(ArwInfo::parse(&corrupt, 256).is_err());
    for i in 0..256 {
        let mut corrupt = b.clone();
        corrupt[i] = 255;
        assert!(std::panic::catch_unwind(|| ArwInfo::parse(&corrupt, 256)).is_ok());
    }
}
#[test]
fn all_eight_pixel_orientations_match_tiff() {
    let source = || Pixels {
        width: 2,
        height: 3,
        rgba: (1..=6).flat_map(|i| [i, 0, 0, 255]).collect(),
    };
    let expected = [
        vec![1, 2, 3, 4, 5, 6],
        vec![2, 1, 4, 3, 6, 5],
        vec![6, 5, 4, 3, 2, 1],
        vec![5, 6, 3, 4, 1, 2],
        vec![1, 3, 5, 2, 4, 6],
        vec![5, 3, 1, 6, 4, 2],
        vec![6, 4, 2, 5, 3, 1],
        vec![2, 4, 6, 1, 3, 5],
    ];
    for o in 1..=8 {
        let p = orient(source(), o);
        assert_eq!(
            p.rgba.chunks_exact(4).map(|p| p[0]).collect::<Vec<_>>(),
            expected[o as usize - 1]
        );
        assert_eq!(p.width, if o >= 5 { 3 } else { 2 });
    }
}
#[test]
fn background_initialization_never_overwrites_a_subsequent_edit() {
    use fastcull::session::Session;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("session.ARW");
    let session = std::sync::Arc::new(Session::new(
        state::parse(&format!("{}|4|Red|1", raw.display())).unwrap(),
    ));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let s = session.clone();
            std::thread::spawn(move || s.initialize(0).unwrap())
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(session.stats().wrote, 1);
    xmp::write(
        &raw,
        &Updates {
            rating: Some(2),
            flag: Some("reject".into()),
            ..Default::default()
        },
    )
    .unwrap();
    session.initialize(0).unwrap();
    assert_eq!(xmp::read(&raw).unwrap().rating, Some(2));
    assert_eq!(xmp::read(&raw).unwrap().flag.as_deref(), Some("reject"));
}
#[test]
fn startup_failure_is_accounted_once_without_overwriting_sidecar() {
    use fastcull::session::Session;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("bad.ARW");
    fs::write(raw.with_extension("xmp"), "<broken>").unwrap();
    let session = Session::new(state::parse(&format!("{}|5||0", raw.display())).unwrap());
    assert!(session.initialize(0).is_err());
    assert!(session.initialize(0).is_err());
    assert_eq!(session.stats().error, 1);
    assert_eq!(
        fs::read_to_string(raw.with_extension("xmp")).unwrap(),
        "<broken>"
    );
}
#[test]
fn duplicate_raw_entries_use_last_lr_state_once() {
    use fastcull::session::Session;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("duplicate.ARW");
    let session = Session::new(
        state::parse(&format!(
            "{}|1|Red|1\n{}|4|Blue|-1",
            raw.display(),
            raw.display()
        ))
        .unwrap(),
    );
    session.initialize(0).unwrap();
    assert_eq!(xmp::read(&raw).unwrap().rating, Some(4));
    xmp::write(
        &raw,
        &Updates {
            rating: Some(2),
            ..Default::default()
        },
    )
    .unwrap();
    session.initialize(1).unwrap();
    assert_eq!(xmp::read(&raw).unwrap().rating, Some(2));
    assert_eq!(session.stats().wrote, 1);
    assert_eq!(session.stats().kept, 1);
}
