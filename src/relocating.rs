//! Bridge file moves. Never overwrite a destination; commit each RAW/sidecar
//! group independently and retain a durable record for interrupted operations.
use anyhow::{Context, Result, ensure};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Item {
    pub index: usize,
    pub from: PathBuf,
    pub to: PathBuf,
    pairs: Vec<(PathBuf, PathBuf)>,
}

pub fn prepare(paths: &[PathBuf], indices: &[usize], target: &Path) -> Result<Vec<Item>> {
    ensure!(
        target.is_absolute() && target.is_dir(),
        "目标文件夹不存在或无法访问"
    );
    let canonical_target = fs::canonicalize(target)?;
    let mut occupied: HashSet<_> = fs::read_dir(target)?
        .map(|e| e.map(|v| v.file_name().to_string_lossy().to_lowercase()))
        .collect::<std::io::Result<_>>()?;
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut parents: HashMap<PathBuf, (bool, HashMap<String, Vec<PathBuf>>)> = HashMap::new();
    for &index in indices {
        if !seen.insert(index) {
            continue;
        }
        let from = paths.get(index).context("照片索引超出范围")?;
        ensure!(
            from.is_absolute() && from.is_file() && crate::formats::supported(from),
            "原照片无法访问：{}",
            from.display()
        );
        let parent = from.parent().context("原照片缺少文件夹")?;
        if !parents.contains_key(parent) {
            let same = fs::canonicalize(parent)? == canonical_target;
            let mut sidecars: HashMap<String, Vec<PathBuf>> = HashMap::new();
            if !same {
                for entry in fs::read_dir(parent)? {
                    let entry = entry?;
                    let key = entry.file_name().to_string_lossy().to_lowercase();
                    if key.ends_with(".xmp") || key.ends_with(".acr") {
                        sidecars.entry(key).or_default().push(entry.path());
                    }
                }
            }
            parents.insert(parent.to_owned(), (same, sidecars));
        }
        let (same, sidecars) = &parents[parent];
        if *same {
            continue;
        }
        ensure!(
            ordinary_file(from)?,
            "暂不移动链接或重解析点文件：{}",
            from.display()
        );
        let name = from.file_name().context("原照片缺少文件名")?;
        let sidecar_names: HashSet<_> = crate::formats::companions(from)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_lowercase())
            .collect();
        // Reserve all possible sidecar names, even when not present at source.
        // A destination orphan XMP must not become the moved RAW's metadata.
        for key in std::iter::once(name.to_string_lossy().to_lowercase())
            .chain(sidecar_names.iter().cloned())
        {
            ensure!(
                occupied.insert(key.clone()),
                "目标已有同名照片或旁车，未移动：{key}"
            );
        }
        let mut pairs = Vec::new();
        for key in &sidecar_names {
            if let Some(files) = sidecars.get(key) {
                for file in files {
                    ensure!(ordinary_file(file)?, "旁车不是普通文件：{}", file.display());
                    pairs.push((file.clone(), target.join(file.file_name().unwrap())));
                }
            }
        }
        pairs.sort();
        let to = target.join(name);
        pairs.push((from.clone(), to.clone())); // RAW last: sidecars first.
        items.push(Item {
            index,
            from: from.clone(),
            to,
            pairs,
        });
    }
    Ok(items)
}

fn ordinary_file(path: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Ok(metadata.is_file() && metadata.file_attributes() & 0x400 == 0)
    }
    #[cfg(not(windows))]
    {
        Ok(metadata.file_type().is_file())
    }
}

fn companions(raw: &Path) -> Vec<PathBuf> {
    crate::formats::companions(raw)
}

pub fn refresh(item: &Item, location: &Path) -> Result<Item> {
    ensure!(
        location == item.from && ordinary_file(location)?,
        "照片位置或文件类型已改变：{}",
        location.display()
    );
    ensure!(
        item.to.parent().is_some_and(|p| p.is_dir()),
        "目标文件夹无法访问"
    );
    // Constant-size checks while queued: do not rescan the source directory
    // for every photo. Enumerate again only if a new sidecar appeared.
    for target in companions(&item.to)
        .into_iter()
        .chain(Some(item.to.clone()))
    {
        ensure!(
            !target.try_exists()?,
            "目标已有同名照片或旁车：{}",
            target.display()
        );
    }
    for sidecar in companions(location) {
        if sidecar.try_exists()?
            && !item.pairs.iter().any(|(a, _)| {
                crate::organizing::path_key(a) == crate::organizing::path_key(&sidecar)
            })
        {
            let mut refreshed =
                prepare(&[location.to_owned()], &[0], item.to.parent().unwrap())?.remove(0);
            refreshed.index = item.index;
            return Ok(refreshed);
        }
    }
    Ok(item.clone())
}

pub struct Journal {
    file: File,
    pub path: PathBuf,
}
impl Journal {
    pub fn create(directory: &Path, items: &[Item]) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let temp = tempfile::Builder::new()
            .prefix("move-")
            .suffix(".txt")
            .tempfile_in(directory)?;
        let (file, path) = temp.keep()?;
        let mut journal = Self { file, path };
        let mut plan = String::from("FASTCULL_MOVES_V1");
        for item in items {
            for (a, b) in &item.pairs {
                plan.push_str(&format!(
                    "\nPLAN\t{}\t{}\t{}",
                    item.index,
                    crate::handoff::encode(&a.to_string_lossy()),
                    crate::handoff::encode(&b.to_string_lossy())
                ));
            }
        }
        journal.record(&plan)?;
        Ok(journal)
    }
    fn record(&mut self, text: &str) -> Result<()> {
        writeln!(self.file, "{text}")?;
        self.file.sync_all()?;
        Ok(())
    }
    pub fn finish(&mut self) -> Result<()> {
        self.record("END")
    }
}

pub fn execute(item: &Item, journal: &mut Journal) -> Result<()> {
    execute_with(item, journal, move_file)
}

fn execute_with(
    item: &Item,
    journal: &mut Journal,
    mut operation: impl FnMut(&Path, &Path) -> Result<()>,
) -> Result<()> {
    let mut completed: Vec<(PathBuf, PathBuf)> = Vec::new();
    let result = (|| -> Result<()> {
        for (from, to) in &item.pairs {
            journal.record(&format!(
                "BEGIN\t{}\t{}",
                crate::handoff::encode(&from.to_string_lossy()),
                crate::handoff::encode(&to.to_string_lossy())
            ))?;
            operation(from, to)
                .with_context(|| format!("移动失败：{} → {}", from.display(), to.display()))?;
            completed.push((from.clone(), to.clone()));
            journal.record(&format!(
                "DONE\t{}",
                crate::handoff::encode(&to.to_string_lossy())
            ))?;
        }
        journal.record(&format!("PHOTO\t{}", item.index))?;
        Ok(())
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        for (from, to) in completed.into_iter().rev() {
            if let Err(e) = move_file(&to, &from) {
                failures.push(format!("{}：{e:#}", to.display()));
            } else {
                let _ = journal.record(&format!(
                    "ROLLBACK\t{}",
                    crate::handoff::encode(&from.to_string_lossy())
                ));
            }
        }
        let _ = journal.record(&format!(
            "ERROR\t{}",
            crate::handoff::encode(&format!("{error:#}"))
        ));
        if failures.is_empty() {
            return Err(error.context("本张照片已回退原位置"));
        }
        anyhow::bail!(
            "{error:#}；部分文件无法回退：{}。恢复记录：{}",
            failures.join("；"),
            journal.path.display()
        );
    }
    Ok(())
}

fn move_file(from: &Path, to: &Path) -> Result<()> {
    ensure!(!to.try_exists()?, "目标已经存在：{}", to.display());
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let a: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let b: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        // No REPLACE_EXISTING, no implicit copy/delete success with source left.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(a.as_ptr(), b.as_ptr(), 0)
        } != 0
        {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(17) {
            return Err(error.into());
        } // different volume
    }
    #[cfg(not(windows))]
    {
        match fs::hard_link(from, to) {
            Ok(()) => {
                if let Err(e) = fs::remove_file(from) {
                    let _ = fs::remove_file(to);
                    return Err(e.into());
                }
                return Ok(());
            }
            Err(e) if e.raw_os_error() == Some(18) => {} // EXDEV
            Err(e) => return Err(e.into()),
        }
    }
    copy_move(from, to)
}

fn copy_move(from: &Path, to: &Path) -> Result<()> {
    let mut source = File::open(from)?;
    let info = source.metadata()?;
    let mut temp = tempfile::NamedTempFile::new_in(to.parent().context("目标缺少文件夹")?)?;
    let bytes = std::io::copy(&mut source, temp.as_file_mut())?;
    ensure!(bytes == info.len(), "源文件大小在复制时发生变化");
    let after = source.metadata()?;
    ensure!(
        after.len() == info.len() && after.modified()? == info.modified()?,
        "源文件在复制时发生变化，未移除原文件"
    );
    temp.as_file()
        .set_times(fs::FileTimes::new().set_modified(info.modified()?))?;
    temp.as_file().sync_all()?;
    drop(source);
    let published = temp.persist_noclobber(to).map_err(|e| e.error)?;
    drop(published);
    if let Err(e) = fs::remove_file(from) {
        let cleanup = fs::remove_file(to);
        ensure!(
            cleanup.is_ok(),
            "原文件保留，但无法清理复制副本：{}，{e}",
            to.display()
        );
        return Err(e.into());
    }
    // Preserve permissions after the original has been removed. Failure here
    // doesn't change the committed location or turn a success into a false rollback.
    if let Err(e) = fs::set_permissions(to, info.permissions()) {
        log::warn!("moved file permissions {}: {e}", to.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Vec<PathBuf>, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("原目录");
        let target = dir.path().join("精选");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        let paths: Vec<_> = ["一.ARW", "二.ARW"]
            .into_iter()
            .map(|s| source.join(s))
            .collect();
        for p in &paths {
            fs::write(p, "raw unchanged").unwrap();
            fs::write(p.with_extension("xmp"), "rating and develop").unwrap();
        }
        (dir, paths, target)
    }
    #[test]
    fn moves_batch_then_same_photo_again_with_sidecars_and_no_content_change() {
        let (dir, mut paths, target) = fixture();
        let items = prepare(&paths, &[0, 1, 0], &target).unwrap();
        let mut j = Journal::create(&dir.path().join("journals"), &items).unwrap();
        for item in items {
            execute(&item, &mut j).unwrap();
            assert!(!item.from.exists());
            paths[item.index] = item.to;
        }
        j.finish().unwrap();
        assert!(fs::read_to_string(&j.path).unwrap().ends_with("END\n"));
        let again = dir.path().join("再次移动");
        fs::create_dir(&again).unwrap();
        let item = prepare(&paths, &[0], &again).unwrap().remove(0);
        execute(&item, &mut j).unwrap();
        assert_eq!(fs::read(item.to).unwrap(), b"raw unchanged");
        assert_eq!(
            fs::read(again.join("一.xmp")).unwrap(),
            b"rating and develop"
        );
        assert!(!paths[0].exists());
    }
    #[test]
    fn preflight_rejects_case_collisions_or_orphan_sidecars_before_any_move() {
        let (_dir, paths, target) = fixture();
        fs::write(target.join("二.XMP"), "keep").unwrap();
        assert!(prepare(&paths, &[0, 1], &target).is_err());
        assert!(paths.iter().all(|p| p.exists()));
        assert_eq!(fs::read(target.join("二.XMP")).unwrap(), b"keep");
        assert!(prepare(&paths, &[100], &target).is_err());
        assert!(
            prepare(&paths, &[0], paths[0].parent().unwrap())
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn runtime_collision_rolls_back_sidecar_and_never_overwrites() {
        let (dir, paths, target) = fixture();
        let item = prepare(&paths, &[0], &target).unwrap().remove(0);
        fs::write(&item.to, "external file").unwrap();
        let mut j =
            Journal::create(&dir.path().join("journals"), std::slice::from_ref(&item)).unwrap();
        assert!(execute(&item, &mut j).is_err());
        assert_eq!(fs::read(&item.from).unwrap(), b"raw unchanged");
        assert_eq!(
            fs::read(item.from.with_extension("xmp")).unwrap(),
            b"rating and develop"
        );
        assert!(!item.to.with_extension("xmp").exists());
        assert_eq!(fs::read(&item.to).unwrap(), b"external file");
    }
    #[test]
    fn copy_path_keeps_contents_timestamp_and_refuses_existing_destination() {
        let (_dir, paths, target) = fixture();
        let to = target.join("一.ARW");
        let time = fs::metadata(&paths[0]).unwrap().modified().unwrap();
        copy_move(&paths[0], &to).unwrap();
        assert!(!paths[0].exists());
        assert_eq!(fs::read(&to).unwrap(), b"raw unchanged");
        assert_eq!(fs::metadata(&to).unwrap().modified().unwrap(), time);
        assert!(copy_move(&paths[1], &to).is_err());
        assert!(paths[1].exists());
        assert_eq!(fs::read(&to).unwrap(), b"raw unchanged");
    }
    #[test]
    fn queued_refresh_captures_new_sidecars_and_rejects_new_destination_orphans() {
        let (dir, paths, target) = fixture();
        let item = prepare(&paths, &[0], &target).unwrap().remove(0);
        fs::write(paths[0].with_extension("acr"), "late develop settings").unwrap();
        let refreshed = refresh(&item, &paths[0]).unwrap();
        let mut journal =
            Journal::create(&dir.path().join("journals"), std::slice::from_ref(&item)).unwrap();
        execute(&refreshed, &mut journal).unwrap();
        assert_eq!(
            fs::read(target.join("一.acr")).unwrap(),
            b"late develop settings"
        );
        let second = prepare(&paths, &[1], &target).unwrap().remove(0);
        fs::write(target.join("二.ARW.acr"), "unrelated orphan").unwrap();
        assert!(refresh(&second, &paths[1]).is_err());
        assert!(paths[1].exists());
    }
    #[test]
    fn associated_uppercase_and_appended_sidecars_move_together() {
        let (dir, paths, target) = fixture();
        fs::write(paths[0].with_extension("ACR"), "acr").unwrap();
        let appended = PathBuf::from(format!("{}.XMP", paths[0].display()));
        fs::write(&appended, "appended").unwrap();
        let item = prepare(&paths, &[0], &target).unwrap().remove(0);
        let mut j =
            Journal::create(&dir.path().join("journal"), std::slice::from_ref(&item)).unwrap();
        execute(&item, &mut j).unwrap();
        assert_eq!(fs::read(target.join("一.ACR")).unwrap(), b"acr");
        assert_eq!(fs::read(target.join("一.ARW.XMP")).unwrap(), b"appended");
        assert!(!appended.exists());
    }
}
