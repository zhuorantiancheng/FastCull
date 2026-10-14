use fastcull::{
    crop::Crop,
    handoff::Manifest,
    session::Session,
    state::Item,
    xmp::{self, Updates},
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
    assert_eq!(m.entries[0].revisions, [1, 0, 0, 0, 1, 1]);
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
    session.attach_manifest(file.clone()).unwrap();
    let receipt = file.with_extension("stars-request");
    assert!(!receipt.exists());
    session.close().unwrap();
    assert!(!receipt.exists());
    assert!(Manifest::read(&file).unwrap().closed);
    session.request_sync().unwrap();
    assert_eq!(fs::read_to_string(receipt).unwrap(), "exit-test\nall\n");
    assert!(!file.with_extension("folders").exists());
}
