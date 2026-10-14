//! Serial background move queue shared with decoder/metadata path locks.
use eframe::egui;
use fastcull::relocating::{self, Item, Journal};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, RwLock, mpsc},
    thread,
};

pub enum Event {
    Photo {
        index: usize,
        path: PathBuf,
    },
    Finished {
        indices: Vec<usize>,
        moved: usize,
        error: Option<String>,
    },
}
pub struct Mover {
    sender: Option<mpsc::Sender<Vec<Item>>>,
    receiver: mpsc::Receiver<Event>,
    worker: Option<thread::JoinHandle<()>>,
    pub pending: BTreeSet<usize>,
    pub status: String,
    pub jobs: usize,
}
impl Mover {
    pub fn new(
        locations: Arc<Vec<RwLock<PathBuf>>>,
        ctx: egui::Context,
        directory: PathBuf,
    ) -> Self {
        let (sender, jobs) = mpsc::channel::<Vec<Item>>();
        let (events, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("bridge-file-moves".into())
            .spawn(move || {
                while let Ok(items) = jobs.recv() {
                    let indices = items.iter().map(|i| i.index).collect();
                    let mut moved = 0;
                    let result = (|| -> anyhow::Result<()> {
                        let mut journal = Journal::create(&directory, &items)?;
                        for item in &items {
                            let mut location = locations[item.index]
                                .write()
                                .unwrap_or_else(|e| e.into_inner());
                            // Recheck filesystem state when this queued job starts.
                            let refreshed = relocating::refresh(item, &location)?;
                            if let Err(e) = relocating::execute(&refreshed, &mut journal) {
                                // Publish actual RAW location if a rollback was only partial.
                                if !location.exists() && refreshed.to.exists() {
                                    *location = refreshed.to;
                                    let _ = events.send(Event::Photo {
                                        index: item.index,
                                        path: location.clone(),
                                    });
                                }
                                return Err(
                                    e.context(format!("移动记录：{}", journal.path.display()))
                                );
                            }
                            *location = refreshed.to;
                            moved += 1;
                            let path = location.clone();
                            drop(location);
                            let _ = events.send(Event::Photo {
                                index: item.index,
                                path,
                            });
                            ctx.request_repaint();
                        }
                        journal.finish()?;
                        Ok(())
                    })();
                    let error = result.err().map(|e| format!("{e:#}"));
                    let _ = events.send(Event::Finished {
                        indices,
                        moved,
                        error,
                    });
                    ctx.request_repaint();
                }
            })
            .expect("spawn Bridge move queue");
        Self {
            sender: Some(sender),
            receiver,
            worker: Some(worker),
            pending: BTreeSet::new(),
            status: String::new(),
            jobs: 0,
        }
    }
    pub fn enqueue(&mut self, items: Vec<Item>) -> anyhow::Result<()> {
        anyhow::ensure!(!items.is_empty(), "所选照片已在目标文件夹中");
        anyhow::ensure!(
            items.iter().all(|i| !self.pending.contains(&i.index)),
            "照片已在移动队列中"
        );
        let indices: Vec<_> = items.iter().map(|i| i.index).collect();
        self.sender.as_ref().unwrap().send(items)?;
        self.pending.extend(indices);
        self.jobs += 1;
        Ok(())
    }
    pub fn take(&mut self) -> Vec<Event> {
        let events: Vec<_> = self.receiver.try_iter().collect();
        for event in &events {
            match event {
                Event::Photo { index, .. } => {
                    self.pending.remove(index);
                }
                Event::Finished {
                    indices,
                    moved,
                    error,
                } => {
                    self.jobs = self.jobs.saturating_sub(1);
                    for i in indices {
                        self.pending.remove(i);
                    }
                    self.status = if error.is_some() {
                        format!("已移动 {moved} 张，其余未完成")
                    } else {
                        format!("已移动 {moved} 张")
                    };
                }
            }
        }
        events
    }
}
impl Drop for Mover {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
