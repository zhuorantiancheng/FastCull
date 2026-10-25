use fastcull::{
    crop::Crop,
    handoff::Manifest,
    session::Session,
    state::Item,
    xmp::{self, BasicAdjustments, Updates, WhiteBalance},
};
use std::fs;

#[test]
fn crop_rotation_revisions_roundtrip_and_survive_stars_ack() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("photo.ARW");
    let file = dir.path().join("session.txt");
    let mut m = Manifest::new("edits".into(), std::slice::from_ref(&raw)).unwrap();
    let all = Updates {
        rating: Some(4),
        crop: Some(Crop {
            enabled: true,
            left: 100_000,
            top: 200_000,
            right: 800_000,
            bottom: 900_000,
            angle: 3_250_000,
        }),
        orientation: Some(6),
        ..Default::default()
    };
    xmp::write(&raw, &all).unwrap();
    m.record(0, &all);
    m.write(&file).unwrap();
    assert!(
        fs::read_to_string(&file)
            .unwrap()
            .starts_with("FASTCULL_SESSION_V3")
    );
    let m = Manifest::read(&file).unwrap();
    assert_eq!(m.entries[0].revisions, [1, 0, 0, 0, 1, 1, 0, 0]);
    let stars = m.stars_since(0);
    let v = stars[0].1.as_ref().unwrap();
    assert_eq!(v.rating, Some(4));
    assert!(v.crop.is_none() && v.orientation.is_none());
    assert_eq!(m.other_fields_since(0), 1);
    let rows = m.rows_with_stars_ack(0, 1);
    let v = rows[0].1.as_ref().unwrap();
    assert!(v.rating.is_none());
    assert_eq!(v.crop, all.crop);
    assert_eq!(v.orientation, Some(6));
    assert!(m.rows_since(1).is_empty());
}

#[test]
fn reset_crop_is_an_edit_and_missing_changed_fields_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("photo.ARW");
    let mut m = Manifest::new("reset".into(), std::slice::from_ref(&raw)).unwrap();
    let u = Updates {
        crop: Some(Crop::default()),
        ..Default::default()
    };
    xmp::write(&raw, &u).unwrap();
    m.record(0, &u);
    assert_eq!(m.revision, 1);
    assert_eq!(m.rows_since(0)[0].1.as_ref().unwrap().crop, u.crop);
    fs::write(
        raw.with_extension("xmp"),
        r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>"#,
    )
    .unwrap();
    assert!(m.rows_since(0)[0].1.is_err());
    let mut m = Manifest::new("rotate".into(), &[raw]).unwrap();
    m.record(
        0,
        &Updates {
            orientation: Some(6),
            ..Default::default()
        },
    );
    assert!(m.rows_since(0)[0].1.is_err());
}

#[test]
fn ordinary_exit_requests_all_only_when_explicitly_requested() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("photo.ARW");
    let file = dir.path().join("session.txt");
    Manifest::new("exit-test".into(), std::slice::from_ref(&raw))
        .unwrap()
        .write(&file)
        .unwrap();
    let session = Session::new(vec![Item {
        path: raw,
        updates: Default::default(),
    }]);
    assert_eq!(session.raw_settings_source(), None);
    session.attach_manifest(file.clone()).unwrap();
    assert_eq!(
        session.raw_settings_source(),
        Some((file.with_extension("raw-request"), "exit-test".into()))
    );
    let receipt = file.with_extension("stars-request");
    assert!(!receipt.exists());
    session.close().unwrap();
    assert!(!receipt.exists());
    assert!(Manifest::read(&file).unwrap().closed);
    session.request_sync().unwrap();
    assert_eq!(fs::read_to_string(receipt).unwrap(), "exit-test\nall\n");
    assert!(!file.with_extension("folders").exists());
}

#[test]
fn white_balance_is_one_revision_and_survives_stars_only_ack() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("白平衡.NEF");
    let file = dir.path().join("session.txt");
    let mut manifest = Manifest::new("wb-test".into(), &[raw.clone(), raw.clone()]).unwrap();
    let update = Updates {
        rating: Some(4),
        white_balance: Some(WhiteBalance::Custom {
            temperature: 4_850,
            tint: 17,
        }),
        ..Default::default()
    };
    xmp::write(&raw, &update).unwrap();
    manifest.record(0, &update);
    manifest.write(&file).unwrap();
    assert!(
        fs::read_to_string(&file)
            .unwrap()
            .starts_with("FASTCULL_SESSION_V4\t")
    );
    let manifest = Manifest::read(&file).unwrap();
    assert_eq!(manifest.entries[0].revisions, [1, 0, 0, 0, 0, 0, 1, 0]);
    assert_eq!(manifest.entries[0].revisions, manifest.entries[1].revisions);
    let stars = manifest.stars_since(0);
    assert_eq!(stars.len(), 1);
    assert_eq!(stars[0].1.as_ref().unwrap().rating, Some(4));
    assert!(stars[0].1.as_ref().unwrap().white_balance.is_none());
    assert_eq!(manifest.other_fields_since(0), 2);
    let rows = manifest.rows_with_stars_ack(0, 1);
    assert_eq!(rows.len(), 1);
    let values = rows[0].1.as_ref().unwrap();
    assert_eq!(values.rating, None);
    assert_eq!(
        values.white_balance.as_ref().unwrap().edit(),
        update.white_balance
    );
    assert!(manifest.rows_since(1).is_empty());
    assert_eq!(manifest.other_fields_since(1), 0);
}

#[test]
fn missing_or_unsupported_changed_white_balance_is_never_acknowledged() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("edit.DNG");
    let mut manifest = Manifest::new("wb-missing".into(), std::slice::from_ref(&raw)).unwrap();
    manifest.record(
        0,
        &Updates {
            white_balance: Some(WhiteBalance::AsShot),
            ..Default::default()
        },
    );
    let packet = |attrs: &str| {
        format!(
            r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" {attrs}/>"#
        )
    };
    for attrs in [
        "",
        r#"crs:WhiteBalance="Custom""#,
        r#"crs:WhiteBalance="Auto" crs:Temperature="5000" crs:Tint="0""#,
    ] {
        fs::write(raw.with_extension("xmp"), packet(attrs)).unwrap();
        assert!(manifest.rows_since(0)[0].1.is_err(), "{attrs}");
        // The unchanged, absent WB cannot poison a rating-only export.
        assert!(manifest.stars_since(0).is_empty());
    }
    xmp::write(
        &raw,
        &Updates {
            white_balance: Some(WhiteBalance::AsShot),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        manifest.rows_since(0)[0]
            .1
            .as_ref()
            .unwrap()
            .white_balance
            .as_ref()
            .unwrap()
            .edit(),
        Some(WhiteBalance::AsShot)
    );
}

#[test]
fn manifest_versions_remain_minimal_and_validate_v4_revision() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("edit.ARW");
    let file = dir.path().join("session.txt");
    for (version, revisions) in [
        (1, "1\t0\t0"),
        (2, "1\t0\t0\t1"),
        (3, "1\t0\t0\t1\t1\t1"),
        (4, "1\t0\t0\t1\t1\t1\t1"),
        (5, "1\t0\t0\t1\t1\t1\t1\t1\t1\t0\t0\t0\t0\t0\t0"),
    ] {
        let text = format!(
            "FASTCULL_SESSION_V{version}\tcompat\tactive\t1\n{}\t{revisions}\n",
            fastcull::handoff::encode(raw.to_str().unwrap())
        );
        fs::write(&file, &text).unwrap();
        let manifest = Manifest::read(&file).unwrap();
        manifest.write(&file).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), text);
    }
    fs::write(
        &file,
        "FASTCULL_SESSION_V4\tcompat\tactive\t1\n61\t0\t0\t0\t0\t0\t0\t2\n",
    )
    .unwrap();
    assert!(Manifest::read(&file).is_err());
}

#[test]
fn basic_group_preserves_zero_and_tracks_only_changed_children_across_acknowledgments() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("basic.ARW");
    let file = dir.path().join("session.txt");
    let mut manifest = Manifest::new("basics".into(), std::slice::from_ref(&raw)).unwrap();
    xmp::write(
        &raw,
        &Updates {
            basic: BasicAdjustments {
                contrast: Some(23),
                highlights: Some(44),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let first = Updates {
        rating: Some(5),
        basic: BasicAdjustments {
            contrast: Some(0),
            ..Default::default()
        },
        ..Default::default()
    };
    xmp::write(&raw, &first).unwrap();
    manifest.record(0, &first);
    manifest.write(&file).unwrap();
    assert!(
        fs::read_to_string(&file)
            .unwrap()
            .starts_with("FASTCULL_SESSION_V5\t")
    );
    let mut manifest = Manifest::read(&file).unwrap();
    assert_eq!(manifest.entries[0].basic_revisions, [1, 0, 0, 0, 0, 0, 0]);
    assert_eq!(manifest.entries[0].revisions[7], 1);
    let rows = manifest.rows_since(0);
    let values = rows[0].1.as_ref().unwrap();
    assert_eq!(values.basic, first.basic); // Existing, unchanged Highlights=44 stays absent.
    assert_eq!(
        manifest.stars_since(0)[0].1.as_ref().unwrap().basic,
        Default::default()
    );
    assert_eq!(
        manifest.rows_with_stars_ack(0, 1)[0]
            .1
            .as_ref()
            .unwrap()
            .basic,
        first.basic
    );
    assert_eq!(manifest.other_fields_since(0), 1);
    let second = Updates {
        basic: BasicAdjustments {
            whites: Some(-12),
            vibrance: Some(0),
            ..Default::default()
        },
        ..Default::default()
    };
    xmp::write(&raw, &second).unwrap();
    manifest.record(0, &second);
    assert_eq!(
        manifest.rows_since(1)[0].1.as_ref().unwrap().basic,
        second.basic
    );
    assert_eq!(manifest.entries[0].basic_revisions, [1, 0, 0, 2, 0, 2, 0]);
    manifest.write(&file).unwrap();
    assert_eq!(
        Manifest::read(&file).unwrap().entries[0].basic_revisions,
        manifest.entries[0].basic_revisions
    );
    assert!(manifest.rows_since(2).is_empty());
}

#[test]
fn missing_changed_basic_field_or_invalid_basic_revision_cannot_be_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("basic.NEF");
    let mut manifest = Manifest::new("missing-basic".into(), std::slice::from_ref(&raw)).unwrap();
    let update = Updates {
        basic: BasicAdjustments {
            saturation: Some(0),
            ..Default::default()
        },
        ..Default::default()
    };
    manifest.record(0, &update);
    xmp::write(
        &raw,
        &Updates {
            rating: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(manifest.rows_since(0)[0].1.is_err());
    let file = dir.path().join("session.txt");
    manifest.write(&file).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, text.replace("\t1\n", "\t2\n")).unwrap();
    assert!(Manifest::read(&file).is_err());
}
