//! Once-only successful LR initialization, with explicit retries for failures.
//! Background handoff cannot overwrite a later user edit.
use crate::{
    handoff::Manifest,
    state::{self, Item, Stats},
    xmp::{Updates, Values},
};
use std::collections::HashMap;
use std::{path::PathBuf, sync::Mutex};
pub struct Session {
    items: Vec<Item>,
    ready: Vec<Mutex<Option<Result<bool, String>>>>,
    stats: Mutex<Stats>,
    canonical: Vec<usize>,
    manifest: Mutex<Option<(PathBuf, Manifest)>>,
    pub sync_on_exit: std::sync::atomic::AtomicBool,
}
impl Session {
    pub fn new(items: Vec<Item>) -> Self {
        let ready = (0..items.len()).map(|_| Mutex::new(None)).collect();
        let mut latest = HashMap::new();
        for (i, item) in items.iter().enumerate() {
            latest.insert(item.path.clone(), i);
        }
        let canonical = items.iter().map(|i| latest[&i.path]).collect();
        let stats = Stats {
            kept: items.len() - latest.len(),
            ..Default::default()
        };
        Self {
            items,
            ready,
            stats: Mutex::new(stats),
            canonical,
            manifest: Mutex::new(None),
            sync_on_exit: std::sync::atomic::AtomicBool::new(false),
        }
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn stats(&self) -> Stats {
        self.stats.lock().unwrap().clone()
    }
    pub fn initialize(&self, index: usize) -> Result<bool, String> {
        self.initialize_inner(index, false)
    }
    pub fn retry(&self, index: usize) -> Result<bool, String> {
        self.initialize_inner(index, true)
    }
    pub fn initial_values(&self, index: usize) -> Values {
        let u = &self.items[self.canonical[index]].updates;
        Values {
            crop: u.crop,
            exposure: u.exposure,
            rating: u.rating,
            label: u.label.clone(),
            flag: u.flag.clone(),
            orientation: u.orientation,
        }
    }
    pub fn attach_manifest(&self, path: PathBuf) -> anyhow::Result<()> {
        let m = Manifest::read(&path)?;
        anyhow::ensure!(
            m.entries.len() == self.items.len()
                && m.entries
                    .iter()
                    .zip(&self.items)
                    .all(|(e, i)| e.path == i.path),
            "session photo list does not match launch state"
        );
        *self.manifest.lock().unwrap() = Some((path, m));
        Ok(())
    }
    pub fn record(&self, index: usize, u: &Updates) -> anyhow::Result<()> {
        if let Some((path, m)) = self.manifest.lock().unwrap().as_mut() {
            m.record(index, u);
            m.write(path)?;
        }
        Ok(())
    }
    pub fn close(&self) -> anyhow::Result<()> {
        if let Some((path, m)) = self.manifest.lock().unwrap().as_mut() {
            m.closed = true;
            m.write(path)?;
        }
        Ok(())
    }
    pub fn exposure_file(&self) -> Option<PathBuf> {
        self.manifest
            .lock()
            .unwrap()
            .as_ref()
            .map(|(p, _)| p.with_extension("exposure"))
    }
    pub fn organization_file(&self) -> Option<(PathBuf, String)> {
        self.manifest
            .lock()
            .unwrap()
            .as_ref()
            .map(|(p, m)| (p.with_extension("folders"), m.id.clone()))
    }
    pub fn request_sync(&self) -> anyhow::Result<()> {
        use std::io::Write;
        if let Some((path, m)) = self.manifest.lock().unwrap().as_ref() {
            let receipt = path.with_extension("stars-request");
            let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            writeln!(file, "{}", m.id)?;
            // Keep the receipt filename readable by older plugins, but always
            // request all changed fields, even without an organization plan.
            writeln!(file, "all")?;
            file.as_file().sync_all()?;
            file.persist(receipt).map_err(|e| e.error)?;
        }
        Ok(())
    }
    fn initialize_inner(&self, index: usize, retry: bool) -> Result<bool, String> {
        let index = self.canonical[index];
        let mut ready = self.ready[index].lock().unwrap();
        if let Some(result) = &*ready {
            if !retry || result.is_ok() {
                return result.clone();
            }
            self.stats.lock().unwrap().error -= 1;
        }
        let result = {
            let result = state::apply_one(&self.items[index])
                .map_err(|e| format!("{}: {e:#}", self.items[index].path.display()));
            let mut stats = self.stats.lock().unwrap();
            match &result {
                Ok(true) => stats.wrote += 1,
                Ok(false) => stats.kept += 1,
                Err(e) => {
                    stats.error += 1;
                    log::error!("LR state initialization: {e}");
                }
            }
            result
        };
        *ready = Some(result.clone());
        result
    }
}
