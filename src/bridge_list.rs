//! Bridge passes paths only: loading must never apply a host's default ratings.
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub fn read(file: &std::path::Path) -> Result<Vec<PathBuf>> {
    ensure!(
        std::fs::metadata(file)?.len() <= 64 * 1024 * 1024,
        "Bridge photo list is too large"
    );
    parse(&std::fs::read_to_string(file).context("read Bridge photo list")?)
}
fn parse(text: &str) -> Result<Vec<PathBuf>> {
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    ensure!(
        lines.next() == Some("FASTCULL_BRIDGE_V1"),
        "invalid Bridge photo list header"
    );
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let path = PathBuf::from(line);
        // Unknown formats are harmless in a mixed selection. Keep validating
        // supported roots so a missing photo never silently disappears.
        if !path.is_dir() && !is_photo(&path) {
            continue;
        }
        ensure!(
            path.is_absolute() && (path.is_dir() || (path.is_file() && is_photo(&path))),
            "Bridge 选择的照片或文件夹不存在或无法访问：{}",
            path.display()
        );
        if seen.insert(fastcull::organizing::path_key(&path)) {
            paths.push(path);
        }
    }
    ensure!(
        !paths.is_empty(),
        "所选文件中没有支持的 RAW/JPEG 照片或文件夹。"
    );
    Ok(paths)
}

fn is_photo(path: &Path) -> bool {
    fastcull::formats::supported(path)
}

// Called only in the detached child. The Bridge helper validates roots without
// waiting for enumeration, decoding previews, or reading/writing photo metadata.
pub fn expand(roots: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut pending: Vec<_> = roots.into_iter().rev().collect();
    let mut dirs = HashSet::new();
    let mut photos = HashSet::new();
    let mut paths = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = std::fs::metadata(&path)
            .with_context(|| format!("无法访问 Bridge 所选路径：{}", path.display()))?;
        let identity = std::fs::canonicalize(&path)
            .with_context(|| format!("无法解析 Bridge 所选路径：{}", path.display()))?;
        let key = fastcull::organizing::path_key(&identity);
        if metadata.is_dir() {
            if !dirs.insert(key) {
                continue;
            }
            let mut entries = std::fs::read_dir(&path)
                .with_context(|| format!("无法读取文件夹：{}", path.display()))?
                .collect::<std::io::Result<Vec<_>>>()
                .with_context(|| format!("无法列出文件夹：{}", path.display()))?;
            entries.sort_by_key(|e| fastcull::organizing::path_key(&e.path()));
            for entry in entries.into_iter().rev() {
                let info = std::fs::symlink_metadata(entry.path())
                    .with_context(|| format!("无法读取文件属性：{}", entry.path().display()))?;
                // Do not follow descendant symlinks/junctions into other trees or
                // cycles. An explicitly selected root can itself be a junction.
                if is_link(&info) {
                    continue;
                }
                if info.is_dir() || (info.is_file() && is_photo(&entry.path())) {
                    pending.push(entry.path());
                }
            }
        } else if metadata.is_file() && is_photo(&path) && photos.insert(key) {
            paths.push(path);
        }
    }
    ensure!(
        !paths.is_empty(),
        "所选文件夹及子文件夹中没有支持的 RAW/JPEG 照片。"
    );
    Ok(paths)
}

fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_selection_skips_unsupported_files_without_touching_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut selected = Vec::new();
        for name in ["one.ARW", "two.JPG", "three.CR2", "ignore.xmp", "two.arw"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"unchanged").unwrap();
            selected.push(path);
        }
        let text = format!(
            "FASTCULL_BRIDGE_V1\n{}\n",
            selected
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
        let roots = parse(&text).unwrap();
        assert_eq!(
            roots,
            [
                selected[0].clone(),
                selected[1].clone(),
                selected[2].clone(),
                selected[4].clone()
            ]
        );
        assert_eq!(
            expand(roots).unwrap(),
            [
                selected[0].clone(),
                selected[1].clone(),
                selected[2].clone(),
                selected[4].clone()
            ]
        );
        for path in selected {
            assert_eq!(std::fs::read(path).unwrap(), b"unchanged");
        }
    }
    #[test]
    fn preserves_order_unicode_and_xmp_without_applying_ratings() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [
            dir.path().join("照片 100% & (一).ARW"),
            dir.path().join("二.arw"),
        ];
        for path in &paths {
            std::fs::write(path, b"raw").unwrap();
        }
        let sidecar = paths[0].with_extension("xmp");
        std::fs::write(&sidecar, b"existing rating and edits").unwrap();
        let text = format!(
            "\u{feff}FASTCULL_BRIDGE_V1\r\n{}\r\n{}\r\n{}\r\n",
            paths[0].display(),
            paths[1].display(),
            paths[0].display()
        );
        assert_eq!(parse(&text).unwrap(), paths);
        assert_eq!(
            std::fs::read(sidecar).unwrap(),
            b"existing rating and edits"
        );
    }
    #[test]
    fn accepts_selected_folders_without_scanning_their_contents() {
        let dir = tempfile::tempdir().unwrap();
        let folders = [dir.path().join("上午"), dir.path().join("下午")];
        for folder in &folders {
            std::fs::create_dir(folder).unwrap();
        }
        let text = format!(
            "FASTCULL_BRIDGE_V1\n{}\n{}\n",
            folders[0].display(),
            folders[1].display()
        );
        assert_eq!(parse(&text).unwrap(), folders);
    }
    #[test]
    fn recursively_expands_mixed_overlapping_roots_once_in_selected_order() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("上午");
        let b = dir.path().join("下午");
        let nested = a.join("子文件夹");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&b).unwrap();
        let first = b.join("首张.ARW");
        let second = a.join("001.ARW");
        let third = nested.join("照片 100% & (一).arw");
        let fourth = b.join("另一张.ARW");
        for path in [&first, &second, &third, &fourth, &a.join("ignore.png")] {
            std::fs::write(path, b"untouched raw").unwrap();
        }
        let sidecar = third.with_extension("xmp");
        std::fs::write(&sidecar, b"existing rating and edits").unwrap();
        assert_eq!(
            expand(vec![first.clone(), a, nested, b, third.clone()]).unwrap(),
            [first, second, third, fourth]
        );
        assert_eq!(
            std::fs::read(sidecar).unwrap(),
            b"existing rating and edits"
        );
    }
    #[test]
    fn empty_or_non_raw_folders_report_no_arw_and_disappeared_roots_report_access_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            expand(vec![dir.path().to_path_buf()])
                .unwrap_err()
                .to_string()
                .contains("没有支持的 RAW/JPEG")
        );
        std::fs::write(dir.path().join("ignore.png"), b"png").unwrap();
        assert!(expand(vec![dir.path().to_path_buf()]).is_err());
        assert!(
            expand(vec![dir.path().join("missing")])
                .unwrap_err()
                .to_string()
                .contains("无法访问")
        );
    }
    #[test]
    #[cfg(windows)]
    fn skips_descendant_junction_cycles_but_allows_explicit_junction_roots() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("source");
        std::fs::create_dir(&a).unwrap();
        let photo = a.join("one.ARW");
        std::fs::write(&photo, b"raw").unwrap();
        let link = a.join("cycle");
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&a)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert_eq!(expand(vec![a, link.clone()]).unwrap(), [photo]);
        assert_eq!(expand(vec![link.clone()]).unwrap(), [link.join("one.ARW")]);
    }
    #[test]
    fn rejects_wrong_format_relative_missing_and_non_raw_paths() {
        for text in [
            "",
            "FASTCULL_BRIDGE_V1\n",
            "path|5",
            "FASTCULL_BRIDGE_V1\na.ARW",
            "FASTCULL_BRIDGE_V1\nC:/missing.ARW",
        ] {
            assert!(parse(text).is_err());
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.png");
        std::fs::write(&file, b"jpeg").unwrap();
        assert!(parse(&format!("FASTCULL_BRIDGE_V1\n{}", file.display())).is_err());
    }
}
