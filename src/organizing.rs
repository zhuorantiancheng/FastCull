//! Folder organization is a durable plan. Source files and catalog identities stay intact.
use crate::handoff::{decode, encode};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Plan {
    pub id: String,
    pub revision: u64,
    pub sources: Vec<PathBuf>,
    pub moves: BTreeMap<PathBuf, PathBuf>,
    pub folders: BTreeSet<PathBuf>,
}
#[derive(Clone, Debug)]
pub struct Node {
    pub path: PathBuf,
    pub count: usize,
    pub direct: usize,
    pub children: Vec<Node>,
}
pub fn path_key(p: &Path) -> String {
    #[cfg(windows)]
    {
        p.to_string_lossy().replace('\\', "/").to_lowercase()
    }
    // APFS can be case-sensitive; backslash is a legal Unix filename character.
    #[cfg(not(windows))]
    {
        p.to_string_lossy().into_owned()
    }
}
pub fn folder_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 240,
        "请输入有效的文件夹名称"
    );
    ensure!(
        name != "." && name != ".." && !name.ends_with([' ', '.']),
        "文件夹名称不能是 . 或 ..，也不能以空格或点结尾"
    );
    ensure!(
        !name
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c)),
        "文件夹名称不能包含路径分隔符或特殊字符"
    );
    let base = name.split('.').next().unwrap().to_uppercase();
    ensure!(
        !["CON", "PRN", "AUX", "NUL", "CLOCK$"].contains(&base.as_str())
            && !(base.len() == 4
                && (base.starts_with("COM") || base.starts_with("LPT"))
                && matches!(base.as_bytes()[3], b'1'..=b'9')),
        "此名称为 Windows 保留名称"
    );
    Ok(())
}
impl Plan {
    pub fn new(id: String, sources: Vec<PathBuf>) -> Self {
        Self {
            id,
            revision: 0,
            sources,
            moves: BTreeMap::new(),
            folders: BTreeSet::new(),
        }
    }
    pub fn read(file: &Path, id: &str, sources: Vec<PathBuf>) -> Result<Self> {
        let text = fs::read_to_string(file)?;
        let mut lines = text.lines();
        let h: Vec<_> = lines
            .next()
            .context("empty organization plan")?
            .split('\t')
            .collect();
        ensure!(
            h.len() == 3 && h[0] == "FASTCULL_FOLDERS_V1" && h[1] == id,
            "整理计划会话标识不匹配"
        );
        let mut p = Self::new(id.to_owned(), sources);
        p.revision = h[2].parse()?;
        for line in lines {
            let f: Vec<_> = line.split('\t').collect();
            match f.as_slice() {
                ["FOLDER", path] => {
                    p.folders.insert(PathBuf::from(decode(path)?));
                }
                ["MOVE", from, to] => {
                    let from = PathBuf::from(decode(from)?);
                    let to = PathBuf::from(decode(to)?);
                    ensure!(p.sources.contains(&from), "整理记录超出原始会话");
                    ensure!(
                        from.file_name() == to.file_name() && to.is_absolute(),
                        "整理计划只能改变文件夹"
                    );
                    ensure!(!p.moves.contains_key(&from), "重复的整理记录");
                    p.moves.insert(from, to);
                }
                _ => anyhow::bail!("无效的整理记录"),
            }
        }
        let mut targets = BTreeSet::new();
        ensure!(
            p.moves.values().all(|v| targets.insert(path_key(v))),
            "整理计划包含重名目标"
        );
        Ok(p)
    }
    pub fn write(&self, file: &Path) -> Result<()> {
        let mut text = format!("FASTCULL_FOLDERS_V1\t{}\t{}\n", self.id, self.revision);
        for p in &self.folders {
            text.push_str(&format!(
                "FOLDER\t{}\n",
                encode(p.to_str().context("文件夹路径不是 UTF-8")?)
            ));
        }
        for (from, to) in &self.moves {
            text.push_str(&format!(
                "MOVE\t{}\t{}\n",
                encode(from.to_str().context("照片路径不是 UTF-8")?),
                encode(to.to_str().context("目标路径不是 UTF-8")?)
            ));
        }
        let mut tmp =
            tempfile::NamedTempFile::new_in(file.parent().context("整理计划缺少父目录")?)?;
        tmp.write_all(text.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(file).map_err(|e| e.error)?;
        Ok(())
    }
    pub fn effective_path(&self, i: usize) -> &Path {
        self.moves
            .get(&self.sources[i])
            .map_or(self.sources[i].as_path(), PathBuf::as_path)
    }
    pub fn in_folder(&self, i: usize, folder: &Path, recursive: bool) -> bool {
        self.effective_path(i).parent().is_some_and(|parent| {
            if recursive {
                parent.starts_with(folder)
            } else {
                parent == folder
            }
        })
    }
    pub fn stage(&mut self, indices: &[usize], destination: &Path, file: &Path) -> Result<()> {
        ensure!(
            destination.is_absolute() && destination.is_dir(),
            "目标文件夹不存在或不能访问"
        );
        ensure!(!indices.is_empty(), "请先选择照片");
        let mut next = self.clone();
        for &i in indices {
            let from = self.sources.get(i).context("照片索引超出会话")?;
            ensure!(from.is_file(), "原照片无法访问：{}", from.display());
            let target = destination.join(from.file_name().context("照片缺少文件名")?);
            if path_key(from) == path_key(&target) {
                next.moves.remove(from);
                continue;
            }
            ensure!(!target.exists(), "目标已有同名照片：{}", target.display());
            for sidecar in crate::formats::companions(&target) {
                ensure!(!sidecar.exists(), "目标已有同名旁车：{}", sidecar.display());
            }
            next.moves.insert(from.clone(), target);
        }
        let mut occupied = BTreeMap::new();
        for source in &next.sources {
            let target = next.moves.get(source).unwrap_or(source);
            if let Some(previous) = occupied.insert(path_key(target), source) {
                ensure!(
                    previous == source,
                    "计划中的照片在目标文件夹重名：{}",
                    target.display()
                );
            }
        }
        next.revision += 1;
        next.write(file)?;
        *self = next;
        Ok(())
    }
    pub fn cancel(&mut self, source: &Path, file: &Path) -> Result<()> {
        let mut next = self.clone();
        if next.moves.remove(source).is_some() {
            next.revision += 1;
            next.write(file)?;
            *self = next;
        }
        Ok(())
    }
    pub fn create_folder(
        &mut self,
        parent: &Path,
        name: &str,
        file: Option<&Path>,
    ) -> Result<PathBuf> {
        folder_name(name)?;
        ensure!(
            parent.is_absolute() && parent.is_dir(),
            "父文件夹不存在或不能访问"
        );
        let child = parent.join(name);
        fs::create_dir(&child).with_context(|| format!("无法创建文件夹 {}", child.display()))?;
        // Creation is already real. Keep the new directory visible even if the plan disk is unavailable.
        self.folders.insert(child.clone());
        self.revision += 1;
        if let Some(file) = file {
            self.write(file)
                .context("文件夹已创建，但整理记录保存失败；请重试保存")?;
        }
        Ok(child)
    }
    pub fn tree(&self) -> Vec<Node> {
        let mut rows: BTreeMap<PathBuf, (usize, usize)> = BTreeMap::new();
        for original in &self.sources {
            if let Some(parent) = original.parent() {
                for p in parent.ancestors().filter(|p| !p.as_os_str().is_empty()) {
                    rows.entry(p.to_owned()).or_default();
                }
            }
        }
        for i in 0..self.sources.len() {
            if let Some(parent) = self.effective_path(i).parent() {
                rows.entry(parent.to_owned()).or_default().1 += 1;
                for p in parent.ancestors().filter(|p| !p.as_os_str().is_empty()) {
                    rows.entry(p.to_owned()).or_default().0 += 1;
                }
            }
        }
        for folder in &self.folders {
            for p in folder.ancestors().filter(|p| !p.as_os_str().is_empty()) {
                rows.entry(p.to_owned()).or_default();
            }
        }
        let mut children: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        for p in rows.keys() {
            if let Some(parent) = p.parent()
                && rows.contains_key(parent)
            {
                children
                    .entry(parent.to_owned())
                    .or_default()
                    .push(p.clone());
            }
        }
        fn node(
            path: &Path,
            rows: &BTreeMap<PathBuf, (usize, usize)>,
            children: &BTreeMap<PathBuf, Vec<PathBuf>>,
        ) -> Node {
            let descendants = children
                .get(path)
                .into_iter()
                .flatten()
                .map(|p| node(p, rows, children))
                .collect();
            Node {
                path: path.to_owned(),
                count: rows[path].0,
                direct: rows[path].1,
                children: descendants,
            }
        }
        rows.keys()
            .filter(|p| p.parent().is_none_or(|parent| !rows.contains_key(parent)))
            .map(|p| node(p, &rows, &children))
            .collect()
    }
}
