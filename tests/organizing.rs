use fastcull::{
    handoff,
    organizing::{Plan, folder_name},
};
use std::{fs, path::Path};

fn count(nodes: &[fastcull::organizing::Node], path: &Path) -> Option<usize> {
    nodes.iter().find_map(|n| {
        if n.path == path {
            Some(n.count)
        } else {
            count(&n.children, path)
        }
    })
}
#[test]
fn planning_updates_tree_and_filters_without_moving_raw_or_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("上午");
    let b = dir.path().join("下午");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    let first = a.join("照片.ARW");
    let second = b.join("other.ARW");
    fs::write(&first, b"raw unchanged").unwrap();
    fs::write(&second, b"second").unwrap();
    fs::write(first.with_extension("xmp"), b"original metadata").unwrap();
    let file = dir.path().join("plan.folders");
    let mut plan = Plan::new("session-test".into(), vec![first.clone(), second.clone()]);
    let target = plan.create_folder(&a, "精选", Some(&file)).unwrap();
    plan.stage(&[0], &target, &file).unwrap();
    assert_eq!(count(&plan.tree(), &target), Some(1));
    assert_eq!(count(&plan.tree(), &a), Some(1));
    assert!(plan.in_folder(0, &a, true));
    assert!(!plan.in_folder(0, &a, false));
    assert!(plan.in_folder(0, &target, false));
    assert_eq!(fs::read(&first).unwrap(), b"raw unchanged");
    assert_eq!(
        fs::read(first.with_extension("xmp")).unwrap(),
        b"original metadata"
    );
    assert!(!target.join("照片.ARW").exists());
    let restored = Plan::read(&file, "session-test", plan.sources.clone()).unwrap();
    assert_eq!(restored.moves, plan.moves);
    plan.cancel(&first, &file).unwrap();
    assert!(plan.moves.is_empty());
    assert_eq!(count(&plan.tree(), &target), Some(0));
}
#[test]
fn batch_collisions_sidecars_and_write_failure_never_partially_change_plan() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    let dest = dir.path().join("dest");
    for p in [&a, &b, &dest] {
        fs::create_dir(p).unwrap();
    }
    let paths = vec![a.join("same.ARW"), b.join("same.ARW")];
    for p in &paths {
        fs::write(p, b"original").unwrap();
    }
    let file = dir.path().join("plan.folders");
    let mut p = Plan::new("session-test".into(), paths.clone());
    assert!(p.stage(&[0, 1], &dest, &file).is_err());
    assert!(p.moves.is_empty());
    assert!(!file.exists());
    fs::write(dest.join("same.xmp"), "keep").unwrap();
    assert!(p.stage(&[0], &dest, &file).is_err());
    assert_eq!(fs::read_to_string(dest.join("same.xmp")).unwrap(), "keep");
    let clean = p.create_folder(&dest, "clean", None).unwrap();
    assert!(
        p.stage(&[0], &clean, &dir.path().join("missing/plan.folders"))
            .is_err()
    );
    assert!(p.moves.is_empty());
    p.stage(&[0], &clean, &file).unwrap();
    p.stage(&[0], &a, &file).unwrap();
    assert!(p.moves.is_empty());
    for source in paths {
        assert_eq!(fs::read(source).unwrap(), b"original");
    }
}
#[test]
fn names_and_manifest_identity_are_checked() {
    for name in [
        "",
        "..",
        ".",
        "../escape",
        "a\\b",
        "a:b",
        "CON",
        "nul.txt",
        "COM1",
        "LPT9",
        "trailing.",
        "trailing ",
    ] {
        assert!(folder_name(name).is_err(), "{name}");
    }
    for name in ["精选", "2026-10-13", "COM10", "normal name"] {
        folder_name(name).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.ARW");
    let file = dir.path().join("plan.folders");
    fs::write(
        &file,
        format!(
            "FASTCULL_FOLDERS_V1\tsession-test\t1\nMOVE\t{}\t{}\n",
            handoff::encode(source.to_str().unwrap()),
            handoff::encode(dir.path().join("dest/a.ARW").to_str().unwrap())
        ),
    )
    .unwrap();
    assert!(Plan::read(&file, "different-session", vec![source.clone()]).is_err());
    assert!(Plan::read(&file, "session-test", vec![]).is_err());
    assert_eq!(
        Plan::read(&file, "session-test", vec![source])
            .unwrap()
            .moves
            .len(),
        1
    );
}

#[test]
fn planned_exit_receipt_explicitly_requests_all_edits_and_plan_handoff() {
    use fastcull::{session::Session, state::Item};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.ARW");
    fs::write(&source, "raw").unwrap();
    let manifest = dir.path().join("session.txt");
    handoff::Manifest::new("session-test".into(), std::slice::from_ref(&source))
        .unwrap()
        .write(&manifest)
        .unwrap();
    let session = Session::new(vec![Item {
        path: source.clone(),
        updates: Default::default(),
    }]);
    session.attach_manifest(manifest.clone()).unwrap();
    let mut plan = Plan::new("session-test".into(), vec![source.clone()]);
    let dest = plan.create_folder(dir.path(), "精选", None).unwrap();
    plan.stage(&[0], &dest, &manifest.with_extension("folders"))
        .unwrap();
    session.close().unwrap();
    session.request_sync().unwrap();
    assert_eq!(
        fs::read_to_string(manifest.with_extension("stars-request")).unwrap(),
        "session-test\nall\n"
    );
    assert!(source.exists());
    assert!(!dest.join("a.ARW").exists());
}

#[test]
fn folder_membership_uses_components_and_tree_keeps_separate_volumes() {
    let p = Plan::new(
        "test".into(),
        vec![
            "D:/RAW/2026/a.ARW".into(),
            "D:/RAW/2026-new/b.ARW".into(),
            "E:/Other/c.ARW".into(),
        ],
    );
    assert!(p.in_folder(0, Path::new("D:/RAW/2026"), true));
    assert!(!p.in_folder(1, Path::new("D:/RAW/2026"), true));
    assert!(!p.in_folder(2, Path::new("D:/RAW"), true));
    assert_eq!(count(&p.tree(), Path::new("D:/RAW")), Some(2));
    assert_eq!(count(&p.tree(), Path::new("E:/Other")), Some(1));
}
#[test]
fn path_identity_obeys_platform_filename_rules() {
    use fastcull::organizing::path_key;
    use std::path::Path;
    let upper = path_key(Path::new("/Photos/A.JPG"));
    let lower = path_key(Path::new("/Photos/a.JPG"));
    if cfg!(windows) {
        assert_eq!(upper, lower);
    } else {
        assert_ne!(upper, lower);
        assert_ne!(
            path_key(Path::new("/Photos/a\\b.JPG")),
            path_key(Path::new("/Photos/a/b.JPG"))
        );
    }
}
