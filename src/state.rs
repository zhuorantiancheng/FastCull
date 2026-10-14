use crate::xmp::{self, Updates};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone)]
pub struct Item {
    pub path: PathBuf,
    pub updates: Updates,
}
pub fn parse(text: &str) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for (n, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let v: Vec<_> = line.split('|').collect();
        ensure!(v.len() <= 4, "state line {}: too many fields", n + 1);
        let rating = match v.get(1) {
            None => None,
            Some(&"") => Some(0),
            Some(v) => Some(
                v.parse::<u8>()
                    .with_context(|| format!("state line {}: invalid rating", n + 1))?,
            ),
        };
        ensure!(
            rating.is_none_or(|v| v <= 5),
            "state line {}: rating outside 0..5",
            n + 1
        );
        let flag = match v.get(3) {
            None => None,
            Some(&"1") => Some("pick".to_owned()),
            Some(&"-1") => Some("reject".to_owned()),
            Some(&"0" | &"") => Some(String::new()),
            _ => anyhow::bail!("state line {}: invalid flag", n + 1),
        };
        items.push(Item {
            path: PathBuf::from(v[0]),
            updates: Updates {
                rating,
                label: v.get(2).map(|v| v.to_string()),
                flag,
                ..Default::default()
            },
        });
    }
    Ok(items)
}
pub fn read(path: &Path) -> Result<Vec<Item>> {
    parse(&fs::read_to_string(path).with_context(|| format!("read state {}", path.display()))?)
}
#[derive(Default, Debug, Clone)]
pub struct Stats {
    pub wrote: usize,
    pub kept: usize,
    pub error: usize,
}
impl Stats {
    pub fn print(&self) {
        println!(
            "RESULT wrote={} kept={} error={}",
            self.wrote, self.kept, self.error
        );
    }
}
pub fn apply(items: &[Item]) -> Stats {
    let mut stats = Stats::default();
    for item in items {
        match apply_one(item) {
            Ok(true) => stats.wrote += 1,
            Ok(false) => stats.kept += 1,
            Err(e) => {
                stats.error += 1;
                println!("ERROR {}: {e:#}", item.path.display());
                log::error!("apply {}: {e:#}", item.path.display());
            }
        }
    }
    stats
}
pub fn apply_one(item: &Item) -> Result<bool> {
    if item.updates.empty() {
        return Ok(false);
    }
    let has_metadata = if crate::formats::is_jpeg(&item.path) {
        crate::jpeg::read_packet(&item.path)?.is_some()
    } else {
        item.path.with_extension("xmp").exists()
    };
    if !has_metadata
        && item.updates.zero()
        && !(item.updates.rating == Some(0)
            && crate::camera_rating::read(&item.path).is_some_and(|rating| rating > 0))
    {
        return Ok(false);
    }
    xmp::write(&item.path, &item.updates)
}
