use eframe::egui::{Pos2, Rect, Vec2};
use fastcull::{
    crop::{Crop, Geometry, oriented_size},
    relocating,
    xmp::{self, Updates},
};
use std::fs;

#[test]
fn adobe_diagonal_coordinates_decode_to_a_leveled_rectangle() {
    // Independent 30-degree example: a 400x200 rectangle centered on (500,400).
    // The rotated diagonal is (-123.205,-186.603)..(123.205,186.603).
    let c = Crop {
        enabled: true,
        left: 376795,
        top: 266747,
        right: 623205,
        bottom: 733253,
        angle: 30_000_000,
    };
    let g = c.geometry(Vec2::new(1000., 800.), 1);
    assert!((g.rect.width() - 400.).abs() < 0.01);
    assert!((g.rect.height() - 200.).abs() < 0.01);
    assert!((g.rect.center() - Pos2::new(500., 400.)).length() < 0.01);
}

#[test]
fn all_eight_orientations_ratios_and_angles_roundtrip_without_black_edges() {
    for size in [Vec2::new(6000., 4000.), Vec2::new(3000., 5000.)] {
        for o in 1..=8 {
            let display = oriented_size(size, o);
            for angle in [-45., -17.25, 0., 32.4, 45.] {
                for ratio in [0.3, 1., 1.5, 16. / 9., 4.] {
                    let mut g = Geometry {
                        rect: Rect::from_center_size((display * 0.43).to_pos2(), display * 0.85),
                        angle,
                    };
                    g.set_ratio(ratio, display);
                    let c = Crop::from_geometry(g, size, o);
                    assert!(c.valid(), "{c:?}");
                    let back = c.geometry(size, o);
                    assert!(
                        (back.rect.min - g.rect.min).length() < 0.02,
                        "{o} {angle} {g:?} {back:?}"
                    );
                    assert!((back.rect.max - g.rect.max).length() < 0.02);
                    assert!((back.rect.width() / back.rect.height() - ratio).abs() < 0.0001);
                    for uv in c.raw_uvs(size) {
                        assert!(
                            uv.x >= -0.000002
                                && uv.x <= 1.000002
                                && uv.y >= -0.000002
                                && uv.y <= 1.000002,
                            "{uv:?}"
                        );
                    }
                }
            }
        }
    }
}

fn sample_crop() -> Crop {
    Crop {
        enabled: true,
        left: 100000,
        top: 200000,
        right: 900000,
        bottom: 800000,
        angle: 0,
    }
}

#[test]
fn load_adobe_crop_from_raw_sidecar_and_jpeg_embedded_xmp_without_rewriting() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "adobe.ARW",
        "adobe.CR2",
        "adobe.NEF",
        "adobe.DNG",
        "adobe.jpg",
    ] {
        let path = dir.path().join(name);
        let jpeg = fastcull::formats::is_jpeg(&path);
        fs::write(
            &path,
            if jpeg {
                include_bytes!("fixtures/tiny.jpg").as_slice()
            } else {
                b"RAW pixels"
            },
        )
        .unwrap();
        // Adobe-shaped metadata, independent of the culler's XMP writer.
        let packet = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" crs:HasCrop="True" crs:CropLeft="0.1" crs:CropTop="0.2" crs:CropRight="0.9" crs:CropBottom="0.8" crs:CropAngle="0" tiff:Orientation="6"/></rdf:RDF></x:xmpmeta>"#;
        if jpeg {
            fastcull::jpeg::write_packet(&path, None, packet).unwrap();
        } else {
            fs::write(path.with_extension("xmp"), packet).unwrap();
        }
        let before = fs::read(&path).unwrap();
        let values = xmp::read(&path).unwrap();
        assert_eq!(values.crop, Some(sample_crop()));
        assert_eq!(values.orientation, Some(6));
        let geometry = values.crop.unwrap().geometry(Vec2::new(6000., 4000.), 6);
        assert!((geometry.rect.width() - 2400.).abs() < 0.01);
        assert!((geometry.rect.height() - 4800.).abs() < 0.01);
        assert_eq!(fs::read(&path).unwrap(), before);
        if !jpeg {
            assert_eq!(
                fs::read_to_string(path.with_extension("xmp")).unwrap(),
                packet
            );
        }
    }
}

#[test]
fn crop_xmp_keeps_other_develop_settings_and_supports_namespace_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("photo.ARW");
    fs::write(&raw, b"original RAW").unwrap();
    let original = r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:c="http://ns.adobe.com/camera-raw-settings/1.0/" c:Contrast2012="23" c:CropLeft='0.0'><c:HasCrop>False</c:HasCrop><c:CropAngle>0.0</c:CropAngle><c:ToneCurvePV2012><rdf:Seq><rdf:li>0, 7</rdf:li></rdf:Seq></c:ToneCurvePV2012></rdf:Description>"#;
    fs::write(raw.with_extension("xmp"), original).unwrap();
    let crop = sample_crop();
    xmp::write(
        &raw,
        &Updates {
            crop: Some(crop),
            ..Default::default()
        },
    )
    .unwrap();
    let out = fs::read_to_string(raw.with_extension("xmp")).unwrap();
    assert!(out.contains("c:Contrast2012=\"23\""));
    assert!(out.contains("<rdf:li>0, 7</rdf:li>"));
    assert!(out.contains("c:CropLeft='0.100000'"));
    assert!(out.contains("<c:HasCrop>True</c:HasCrop>"));
    assert!(!out.contains("crs:"));
    assert_eq!(xmp::read(&raw).unwrap().crop, Some(crop));
    xmp::write(
        &raw,
        &Updates {
            rating: Some(5),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(xmp::read(&raw).unwrap().crop, Some(crop));
    xmp::write(
        &raw,
        &Updates {
            crop: Some(Crop::default()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(xmp::read(&raw).unwrap().crop, Some(Crop::default()));
    assert_eq!(xmp::read(&raw).unwrap().rating, Some(5));
    assert_eq!(fs::read(&raw).unwrap(), b"original RAW");
}

#[test]
fn crop_does_not_rewrite_adobe_snapshot_develop_parameters() {
    let snapshot = r#"<crs:Snapshots><rdf:Bag><rdf:li><rdf:Description crs:HasCrop="True" crs:CropLeft="0.25" crs:CropTop="0.25" crs:CropRight="0.75" crs:CropBottom="0.75" crs:CropAngle="3.0"/></rdf:li></rdf:Bag></crs:Snapshots>"#;
    let text = format!(
        r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">{snapshot}</rdf:Description>"#
    );
    let updated = xmp::patch(
        &text,
        &Updates {
            crop: Some(sample_crop()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(updated.contains(snapshot));
    assert!(updated.contains("crs:CropLeft=\"0.100000\""));
}

#[test]
fn invalid_crop_or_broken_xml_never_overwrites_original() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("a.NEF");
    let path = raw.with_extension("xmp");
    fs::write(&path, "<broken>").unwrap();
    assert!(
        xmp::write(
            &raw,
            &Updates {
                crop: Some(sample_crop()),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "<broken>");
    let bad = Crop {
        left: -1,
        ..sample_crop()
    };
    assert!(
        xmp::patch(
            r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>"#,
            &Updates {
                crop: Some(bad),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn crop_moves_with_raw_and_jpeg_and_allows_edits_after_two_moves() {
    for extension in ["ARW", "CR2", "NEF", "DNG", "JPG"] {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(format!("测试.{extension}"));
        let bytes: &[u8] = if extension == "JPG" {
            include_bytes!("fixtures/tiny.jpg").as_slice()
        } else if extension == "DNG" {
            b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0"
        } else {
            b"original RAW"
        };
        fs::write(&src, bytes).unwrap();
        let dest = dir.path().join("一");
        let again = dir.path().join("二");
        fs::create_dir(&dest).unwrap();
        fs::create_dir(&again).unwrap();
        // Plan before creating an XMP: refresh must carry the newly written crop too.
        let planned = relocating::prepare(std::slice::from_ref(&src), &[0], &dest)
            .unwrap()
            .remove(0);
        xmp::write(
            &src,
            &Updates {
                crop: Some(sample_crop()),
                ..Default::default()
            },
        )
        .unwrap();
        let mut path = src.clone();
        for (i, target) in [&dest, &again].into_iter().enumerate() {
            let item = if i == 0 {
                relocating::refresh(&planned, &path).unwrap()
            } else {
                relocating::prepare(std::slice::from_ref(&path), &[0], target)
                    .unwrap()
                    .remove(0)
            };
            let mut journal = relocating::Journal::create(
                &dir.path().join("journals"),
                std::slice::from_ref(&item),
            )
            .unwrap();
            relocating::execute(&item, &mut journal).unwrap();
            assert!(!path.exists());
            path = item.to;
            assert_eq!(xmp::read(&path).unwrap().crop, Some(sample_crop()));
            xmp::write(
                &path,
                &Updates {
                    rating: Some(3),
                    orientation: Some(6),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert!(!src.with_extension("xmp").exists());
        assert_eq!(xmp::read(&path).unwrap().orientation, Some(6));
        xmp::write(
            &path,
            &Updates {
                crop: Some(Crop::default()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(xmp::read(&path).unwrap().crop, Some(Crop::default()));
        if extension == "JPG" {
            // Compressed JPEG scan bytes must remain byte-for-byte unchanged.
            let after = fs::read(&path).unwrap();
            let scan = |b: &[u8]| b.windows(2).position(|v| v == [0xff, 0xda]).unwrap();
            assert_eq!(&after[scan(&after)..], &bytes[scan(bytes)..]);
            assert!(!path.with_extension("xmp").exists());
        } else {
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
}

#[test]
fn destination_orphan_crop_sidecar_blocks_move() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.ARW");
    fs::write(&src, b"raw").unwrap();
    let dest = dir.path().join("destination");
    fs::create_dir(&dest).unwrap();
    let orphan = dest.join("a.xmp");
    fs::write(&orphan, "keep crop").unwrap();
    assert!(relocating::prepare(std::slice::from_ref(&src), &[0], &dest).is_err());
    assert_eq!(fs::read_to_string(orphan).unwrap(), "keep crop");
    assert!(src.exists());
}
