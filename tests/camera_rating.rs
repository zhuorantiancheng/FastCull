use fastcull::{state, xmp};
use std::fs;

fn rated_tiff(rating: u16) -> Vec<u8> {
    let mut bytes = b"II\x2a\0\x08\0\0\0\x01\0\x46\x47\x03\0\x01\0\0\0".to_vec();
    bytes.extend(rating.to_le_bytes());
    bytes.extend([0; 6]);
    bytes
}

#[test]
fn camera_stars_are_loaded_and_explicit_zero_wins() {
    let dir = tempfile::tempdir().unwrap();
    for ext in ["ARW", "CR2", "NEF", "DNG"] {
        let path = dir.path().join(format!("rated-{ext}.{ext}"));
        let original = rated_tiff(4);
        fs::write(&path, &original).unwrap();
        assert_eq!(xmp::read(&path).unwrap().rating, Some(4));
        xmp::write(
            &path,
            &xmp::Updates {
                exposure: Some(333333),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(xmp::read(&path).unwrap().rating, Some(4));
        xmp::write(
            &path,
            &xmp::Updates {
                rating: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(xmp::read(&path).unwrap().rating, Some(0));
        assert_eq!(fs::read(path).unwrap(), original);
    }
}

#[test]
fn lightroom_zero_overrides_camera_stars_without_existing_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rated.ARW");
    fs::write(&path, rated_tiff(5)).unwrap();
    assert!(
        state::apply_one(&state::Item {
            path: path.clone(),
            updates: xmp::Updates {
                rating: Some(0),
                ..Default::default()
            }
        })
        .unwrap()
    );
    assert_eq!(xmp::read(&path).unwrap().rating, Some(0));
}

#[test]
fn embedded_xmp_overrides_exif_and_invalid_offsets_fall_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("embedded.NEF");
    let packet = br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:a="http://ns.adobe.com/xap/1.0/"><a:Rating>3</a:Rating></rdf:Description></rdf:RDF>"#;
    let mut bytes = rated_tiff(5);
    bytes[8] = 2;
    bytes.truncate(22);
    bytes.extend([0xbc, 2, 1, 0]);
    bytes.extend((packet.len() as u32).to_le_bytes());
    bytes.extend(38u32.to_le_bytes());
    bytes.extend([0; 4]);
    bytes.extend(packet);
    fs::write(&path, &bytes).unwrap();
    assert_eq!(xmp::read(&path).unwrap().rating, Some(3));
    bytes[30..34].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    assert_eq!(xmp::read(&path).unwrap().rating, Some(5));
    bytes[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    fs::write(&path, bytes).unwrap();
    assert_eq!(xmp::read(&path).unwrap().rating, None);
}

#[test]
fn jpeg_big_endian_exif_rating_and_explicit_clear() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("camera.jpg");
    let tiff = b"MM\0\x2a\0\0\0\x08\0\x01\x47\x46\0\x03\0\0\0\x01\0\x02\0\0\0\0\0\0";
    let mut jpeg = vec![255, 216, 255, 225];
    jpeg.extend(((tiff.len() + 8) as u16).to_be_bytes());
    jpeg.extend(b"Exif\0\0");
    jpeg.extend(tiff);
    jpeg.extend([255, 217]);
    fs::write(&path, jpeg).unwrap();
    assert_eq!(xmp::read(&path).unwrap().rating, Some(2));
    xmp::write(
        &path,
        &xmp::Updates {
            rating: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(xmp::read(&path).unwrap().rating, Some(0));
}
