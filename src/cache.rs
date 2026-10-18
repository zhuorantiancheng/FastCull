use eframe::egui;
use fastcull::{
    arw::ArwInfo,
    imaging::{self, Level},
    session::Session,
    xmp::{self, Values},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, RwLock},
    thread,
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Task {
    pub index: usize,
    pub level: Level,
    pub generation: u64,
}
pub struct Decoded {
    pub task: Task,
    pub result: Result<(ArwInfo, egui::ColorImage), String>,
    pub ms: f64,
}
pub struct Metadata {
    pub index: usize,
    version: u64,
    pub result: Result<Values, String>,
}
pub struct Indexed {
    pub index: usize,
    pub result: Result<ArwInfo, String>,
}
#[derive(Clone, Copy)]
struct MetaTask {
    index: usize,
    version: u64,
    retry: bool,
}
struct Queue {
    reference: Option<usize>,
    tasks: Vec<Task>,
    pending: HashSet<Task>,
    idle_full: HashSet<usize>,
    results: Vec<Decoded>,
    full_sizes: BTreeMap<usize, [usize; 2]>,
    current: usize,
    generation: u64,
    stop: bool,
    meta: BTreeMap<usize, MetaTask>,
    metadata: Vec<Metadata>,
    versions: Vec<u64>,
    index_started: bool,
    index_tasks: BTreeSet<usize>,
    indexed: Vec<Indexed>,
}
impl Queue {
    fn new(count: usize) -> Self {
        Self {
            reference: None,
            tasks: Vec::new(),
            pending: HashSet::new(),
            idle_full: HashSet::new(),
            results: Vec::new(),
            full_sizes: BTreeMap::new(),
            current: 0,
            generation: 0,
            stop: false,
            meta: (0..count)
                .map(|index| {
                    (
                        index,
                        MetaTask {
                            index,
                            version: 0,
                            retry: false,
                        },
                    )
                })
                .collect(),
            metadata: Vec::new(),
            versions: vec![0; count],
            index_started: false,
            index_tasks: BTreeSet::new(),
            indexed: Vec::new(),
        }
    }
    fn navigate(&mut self, index: usize) {
        self.current = index;
        self.generation = self.generation.wrapping_add(1);
        self.idle_full.clear();
        // Cancel queued work, but retain in-flight decodes: pixel identity does
        // not change when the cursor moves. Requests deduplicate across epochs.
        for task in self.tasks.drain(..) {
            self.pending.remove(&task);
        }
        self.results.retain(|d| {
            let keep = d.task.index.abs_diff(index) <= 2 || Some(d.task.index) == self.reference;
            if !keep {
                self.pending.remove(&d.task);
            }
            keep
        });
    }
    fn publish(&mut self, decoded: Decoded) {
        if self.stop || !self.pending.contains(&decoded.task) {
            return;
        }
        if decoded.task.generation != self.generation
            && decoded.task.index.abs_diff(self.current) > 2
            && Some(decoded.task.index) != self.reference
        {
            self.pending.remove(&decoded.task);
            return;
        }
        self.results.push(decoded);
        // Nonblocking mailbox: stale/prefetch results can never hold a foreground
        // worker in send(). Retain the best four; the current result is protected.
        if self.results.len() > 4 {
            let worst = self
                .results
                .iter()
                .enumerate()
                .max_by_key(|(_, d)| rank(d.task, self.current, self.reference))
                .unwrap()
                .0;
            let removed = self.results.swap_remove(worst);
            self.pending.remove(&removed.task);
        }
    }
}
pub struct Pool {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    locations: Arc<Vec<RwLock<PathBuf>>>,
    metadata_worker: Option<thread::JoinHandle<()>>,
}
// Serialize only the first probe for one photo, not independent photos. Failed
// probes remain retryable (unlike caching Result in OnceLock).
fn read_info(cache: &Mutex<Option<ArwInfo>>, path: &std::path::Path) -> anyhow::Result<ArwInfo> {
    let mut cached = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(info) = cached.as_ref() {
        return Ok(info.clone());
    }
    let info = ArwInfo::read(path)?;
    *cached = Some(info.clone());
    Ok(info)
}
/// `egui::Color32` is `#[repr(C)] [u8; 4]`, so an opaque RGBA8 buffer can be
/// moved instead of copied pixel by pixel (25 MP costs ~27 ms on the reference
/// machine). Pixel order, alpha and colour interpretation stay unchanged:
/// `Color32::from_rgba_premultiplied` is a plain byte constructor.
fn color_image(width: usize, height: usize, rgba: Vec<u8>) -> egui::ColorImage {
    if width == 0
        || height == 0
        || rgba.len() != width * height * 4
        || !rgba.capacity().is_multiple_of(4)
        || !(rgba.as_ptr() as usize).is_multiple_of(std::mem::align_of::<egui::Color32>())
    {
        return egui::ColorImage::from_rgba_premultiplied([width, height], &rgba);
    }
    let mut rgba = std::mem::ManuallyDrop::new(rgba);
    let len = rgba.len() / 4;
    let cap = rgba.capacity() / 4;
    // The block was allocated for `u8` (align 1). The Windows heap and the macOS
    // allocator return 16-byte aligned blocks and ignore the layout on release;
    // the alignment check above keeps the conversion itself from misaligning.
    let pixels = unsafe { Vec::from_raw_parts(rgba.as_mut_ptr().cast::<egui::Color32>(), len, cap) };
    egui::ColorImage::new([width, height], pixels)
}
fn priority(task: Task, current: usize) -> (usize, usize) {
    let class = match (task.index == current, task.level) {
        (true, Level::FastThumb) => 0,
        (true, Level::Fit(_)) => 1,
        (true, Level::Full) => 2,
        (true, Level::Thumb) => 3,
        (false, Level::FastThumb) => 4,
        (false, Level::Thumb) => 5,
        (false, Level::Fit(_)) => 6,
        (false, Level::Full) => 7,
    };
    (class, task.index.abs_diff(current))
}
fn rank(task: Task, current: usize, reference: Option<usize>) -> (usize, usize) {
    if Some(task.index) == reference
        && !matches!(task.level, Level::Thumb | Level::FastThumb)
    {
        (if task.level == Level::Full { 1 } else { 0 }, 1)
    } else {
        priority(task, current)
    }
}
fn eligible(worker: usize, task: &Task, queue: &Queue) -> bool {
    // Camera thumbnails are sub-millisecond work: any background worker may
    // pick them up so grid and filmstrip cells never wait behind a 100% decode.
    // Worker 0 stays reserved for the current photo's fit/full work.
    if task.level == Level::FastThumb {
        return worker != 0;
    }
    (worker != 0
        || (task.index == queue.current
            && task.level != Level::Thumb
            && !(task.level == Level::Full && queue.idle_full.contains(&task.index))))
        && (worker < 2 || task.level != Level::Full)
}
impl Pool {
    pub fn new(
        paths: Arc<Vec<PathBuf>>,
        ctx: egui::Context,
        session: Option<Arc<Session>>,
    ) -> Self {
        let paths: Arc<Vec<RwLock<PathBuf>>> =
            Arc::new(paths.iter().cloned().map(RwLock::new).collect());
        let queue = Arc::new((Mutex::new(Queue::new(paths.len())), Condvar::new()));
        let infos: Arc<Vec<Mutex<Option<ArwInfo>>>> =
            Arc::new((0..paths.len()).map(|_| Mutex::new(None)).collect());
        let count = thread::available_parallelism()
            .map_or(2, usize::from)
            // 12 workers measured ~4.5x the 1-worker thumbnail throughput and
            // 1.9x the old cap of 4; 16+ workers add nothing. Full-size work
            // stays limited to the first two workers, so the extra threads only
            // shorten grid/filmstrip and preview queues.
            .clamp(2, 12);
        for worker in 0..count {
            let (queue, paths, ctx, infos) =
                (queue.clone(), paths.clone(), ctx.clone(), infos.clone());
            thread::Builder::new()
                .name(format!("decode-{worker}"))
                .spawn(move || {
                    loop {
                        let task = {
                            let (lock, wake) = &*queue;
                            let mut q = lock.lock().unwrap();
                            loop {
                                if q.stop {
                                    return;
                                }
                                // Reserve one worker for CURRENT fit/full, not background prefetch.
                                let candidate = q
                                    .tasks
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, t)| eligible(worker, t, &q))
                                    .min_by_key(|(_, t)| rank(**t, q.current, q.reference))
                                    .map(|(i, _)| i);
                                if let Some(i) = candidate {
                                    break q.tasks.swap_remove(i);
                                }
                                q = wake.wait(q).unwrap();
                            }
                        };
                        let start = Instant::now();
                        // The context is only used to request a repaint; no UI
                        // state is changed inside the decoder's panic boundary.
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                            || -> anyhow::Result<_> {
                                let path =
                                    paths[task.index].read().unwrap_or_else(|e| e.into_inner());
                                let info = read_info(&infos[task.index], &path)?;
                                let p = imaging::decode_level_with_geometry(
                                    &path,
                                    &info,
                                    task.level,
                                    &mut |size| {
                                        if task.level == Level::Full {
                                            let mut q = queue.0.lock().unwrap();
                                            if !q.stop && q.pending.contains(&task) {
                                                q.full_sizes.insert(task.index, size);
                                            }
                                            drop(q);
                                            ctx.request_repaint();
                                        }
                                    },
                                )?;
                                // All supported camera/JPEG decoders return opaque
                                // RGBA. Hand the buffer over without rewriting it:
                                // a 25 MP copy costs ~27 ms per 100% view.
                                let color = color_image(p.width, p.height, p.rgba);
                                Ok((info, color))
                            },
                        ))
                        .map_err(|_| "JPEG worker panicked".to_owned())
                        .and_then(|r| r.map_err(|e| format!("{e:#}")));
                        let mut q = queue.0.lock().unwrap();
                        q.publish(Decoded {
                            task,
                            result,
                            ms: start.elapsed().as_secs_f64() * 1000.,
                        });
                        drop(q);
                        ctx.request_repaint();
                    }
                })
                .expect("spawn decoder");
        }
        let (index_queue, index_paths, index_ctx) = (queue.clone(), paths.clone(), ctx.clone());
        thread::Builder::new()
            .name("capture-index".into())
            .spawn(move || {
                loop {
                    let index = {
                        let (lock, wake) = &*index_queue;
                        let mut q = lock.lock().unwrap();
                        loop {
                            if q.stop {
                                return;
                            }
                            if !q.index_tasks.is_empty() {
                                let current = q.current;
                                break if q.index_tasks.remove(&current) {
                                    current
                                } else {
                                    q.index_tasks.pop_first().unwrap()
                                };
                            }
                            q = wake.wait(q).unwrap();
                        }
                    };
                    let result = std::panic::catch_unwind(|| -> anyhow::Result<ArwInfo> {
                        let path = index_paths[index].read().unwrap_or_else(|e| e.into_inner());
                        read_info(&infos[index], &path)
                    })
                    .map_err(|_| "capture index panicked".to_owned())
                    .and_then(|v| v.map_err(|e| format!("{e:#}")));
                    index_queue
                        .0
                        .lock()
                        .unwrap()
                        .indexed
                        .push(Indexed { index, result });
                    index_ctx.request_repaint();
                }
            })
            .expect("spawn capture index");
        let (meta_queue, meta_ctx, meta_paths) = (queue.clone(), ctx.clone(), paths.clone());
        let metadata_worker = thread::Builder::new()
            .name("photo-metadata".into())
            .spawn(move || {
                let start = Instant::now();
                loop {
                    let task = {
                        let (lock, wake) = &*meta_queue;
                        let mut q = lock.lock().unwrap();
                        loop {
                            if !q.meta.is_empty() {
                                let current = q.current;
                                break q
                                    .meta
                                    .remove(&current)
                                    .unwrap_or_else(|| q.meta.pop_first().unwrap().1);
                            }
                            if q.stop {
                                return;
                            }
                            q = wake.wait(q).unwrap();
                        }
                    };
                    let result = std::panic::catch_unwind(|| -> anyhow::Result<Values> {
                        let path = meta_paths[task.index]
                            .read()
                            .unwrap_or_else(|e| e.into_inner());
                        if let Some(session) = &session {
                            (if task.retry {
                                session.retry(task.index)
                            } else {
                                session.initialize(task.index)
                            })
                            .map_err(anyhow::Error::msg)?;
                        }
                        xmp::read(&path)
                    })
                    .map_err(|_| "metadata worker panicked".into())
                    .and_then(|r| r.map_err(|e| format!("{e:#}")));
                    let mut q = meta_queue.0.lock().unwrap();
                    if q.versions[task.index] == task.version {
                        q.metadata.push(Metadata {
                            index: task.index,
                            version: task.version,
                            result,
                        });
                    }
                    if q.meta.is_empty()
                        && let Some(s) = &session
                    {
                        let stats = s.stats();
                        log::info!(
                            "LR state complete {:.1}ms wrote={} kept={} error={}",
                            start.elapsed().as_secs_f64() * 1000.,
                            stats.wrote,
                            stats.kept,
                            stats.error
                        );
                    }
                    drop(q);
                    meta_ctx.request_repaint();
                }
            })
            .expect("spawn metadata worker");
        Self {
            queue,
            locations: paths,
            metadata_worker: Some(metadata_worker),
        }
    }
    pub fn locations(&self) -> Arc<Vec<RwLock<PathBuf>>> {
        self.locations.clone()
    }
    pub fn write_xmp(&self, index: usize, updates: &xmp::Updates) -> anyhow::Result<()> {
        let path = self.locations[index]
            .read()
            .unwrap_or_else(|e| e.into_inner());
        xmp::write(&path, updates).map(|_| ())
    }
    pub fn navigate(&self, index: usize) {
        self.queue.0.lock().unwrap().navigate(index);
        self.queue.1.notify_all();
    }
    pub fn cancel(&self, index: usize) {
        let mut q = self.queue.0.lock().unwrap();
        q.tasks.retain(|t| t.index != index);
        q.pending.retain(|t| t.index != index);
        q.results.retain(|d| d.task.index != index);
    }
    pub fn reference(&self, index: Option<usize>) {
        self.queue.0.lock().unwrap().reference = index;
    }
    pub fn start_index(&self) {
        let mut q = self.queue.0.lock().unwrap();
        if !q.index_started {
            q.index_started = true;
            q.index_tasks = (0..q.versions.len()).collect();
            self.queue.1.notify_all();
        }
    }
    pub fn take_indexed(&self) -> Vec<Indexed> {
        std::mem::take(&mut self.queue.0.lock().unwrap().indexed)
    }
    pub fn request(&self, index: usize, level: Level) {
        self.enqueue(index, level, false);
    }
    pub fn warm_full(&self, index: usize) {
        self.enqueue(index, Level::Full, true);
    }
    fn enqueue(&self, index: usize, level: Level, idle: bool) {
        let mut q = self.queue.0.lock().unwrap();
        if level == Level::Full {
            if idle {
                q.idle_full.insert(index);
            } else {
                q.idle_full.remove(&index);
            }
        }
        let t = Task {
            index,
            level,
            generation: q.generation,
        };
        if !q
            .pending
            .iter()
            .any(|p| p.index == index && p.level == level)
        {
            q.pending.insert(t);
            q.tasks.push(t);
            self.queue.1.notify_all();
        }
    }
    pub fn take_decoded(&self) -> Option<Decoded> {
        let mut q = self.queue.0.lock().unwrap();
        let i = q
            .results
            .iter()
            .enumerate()
            .min_by_key(|(_, d)| rank(d.task, q.current, q.reference))?
            .0;
        let d = q.results.swap_remove(i);
        q.pending.remove(&d.task);
        Some(d)
    }
    pub fn take_full_sizes(&self) -> BTreeMap<usize, [usize; 2]> {
        std::mem::take(&mut self.queue.0.lock().unwrap().full_sizes)
    }
    pub fn take_metadata(&self) -> Vec<Metadata> {
        let mut q = self.queue.0.lock().unwrap();
        let results = std::mem::take(&mut q.metadata);
        results
            .into_iter()
            .filter(|m| m.version == q.versions[m.index])
            .collect()
    }
    pub fn retry_metadata(&self, index: usize) {
        let mut q = self.queue.0.lock().unwrap();
        q.versions[index] += 1;
        let version = q.versions[index];
        q.meta.insert(
            index,
            MetaTask {
                index,
                version,
                retry: true,
            },
        );
        self.queue.1.notify_all();
    }
    #[cfg(test)]
    pub fn requested(&self, index: usize, level: Level) -> bool {
        self.queue
            .0
            .lock()
            .unwrap()
            .pending
            .iter()
            .any(|t| t.index == index && t.level == level)
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.queue.0.lock().unwrap().stop = true;
        self.queue.1.notify_all();
        // Preserve all LR initialization even if the user closes immediately.
        if let Some(worker) = self.metadata_worker.take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn color_image_reuses_the_rgba_buffer_without_changing_pixels() {
        let rgba: Vec<u8> = (0..3 * 2 * 4).map(|v| (v * 37) as u8).collect();
        let expected = egui::ColorImage::from_rgba_premultiplied([3, 2], &rgba);
        let actual = color_image(3, 2, rgba);
        assert_eq!(actual.size, [3, 2]);
        assert_eq!(actual.source_size, expected.source_size);
        assert_eq!(actual.pixels, expected.pixels);
    }
    #[test]
    fn color_image_falls_back_for_empty_images() {
        let actual = color_image(0, 0, Vec::new());
        assert!(actual.pixels.is_empty());
    }
    #[test]
    fn camera_thumb_precedes_preview_and_ignores_the_foreground_worker() {
        let fast = Task {
            index: 8,
            level: Level::FastThumb,
            generation: 0,
        };
        let thumb = Task {
            index: 8,
            level: Level::Thumb,
            generation: 0,
        };
        assert!(priority(fast, 8) < priority(thumb, 8));
        // Worker 0 stays reserved for the current photo's fit/full decode.
        assert!(!eligible(0, &fast, &Queue::new(10)));
        assert!(eligible(1, &fast, &Queue::new(10)));
    }
    #[test]
    fn fit_worker_delivers_source_dimensions_with_rounded_preview() {
        let paths = Arc::new(vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg"),
        ]);
        let pool = Pool::new(paths, egui::Context::default(), None);
        pool.request(0, Level::Fit(7));
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        let decoded = loop {
            if let Some(value) = pool.take_decoded() {
                break value;
            }
            assert!(Instant::now() < deadline, "fit decoder did not finish");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let (info, pixels) = decoded.result.unwrap();
        assert_eq!(info.image_size, Some((16, 12)));
        assert_eq!(pixels.size, [7, 5]);
        assert!(
            pool.take_full_sizes().is_empty(),
            "fit must not claim it decoded full resolution"
        );
    }
    #[test]
    fn worker_publishes_actual_full_geometry_before_pixels_are_consumed() {
        let paths = Arc::new(vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg"),
        ]);
        let pool = Pool::new(paths, egui::Context::default(), None);
        pool.request(0, Level::Full);
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        let decoded = loop {
            if let Some(value) = pool.take_decoded() {
                break value;
            }
            assert!(Instant::now() < deadline, "decoder did not finish");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let (_, pixels) = decoded.result.unwrap();
        assert_eq!(pool.take_full_sizes().get(&0), Some(&pixels.size));
    }
    fn tiny_arw(path: &std::path::Path) {
        let jpeg = include_bytes!("../tests/fixtures/tiny.jpg");
        let mut raw = vec![0u8; 64];
        raw[..8].copy_from_slice(&[b'I', b'I', 42, 0, 8, 0, 0, 0]);
        raw[8..10].copy_from_slice(&2u16.to_le_bytes());
        for (n, tag, value) in [(0, 0x0201u16, 64u32), (1, 0x0202u16, jpeg.len() as u32)] {
            let o = 10 + n * 12;
            raw[o..o + 2].copy_from_slice(&tag.to_le_bytes());
            raw[o + 2..o + 4].copy_from_slice(&4u16.to_le_bytes());
            raw[o + 4..o + 8].copy_from_slice(&1u32.to_le_bytes());
            raw[o + 8..o + 12].copy_from_slice(&value.to_le_bytes());
        }
        raw.extend_from_slice(jpeg);
        std::fs::write(path, raw).unwrap();
    }
    #[test]
    fn move_queue_serializes_file_access_and_decoder_and_rating_use_relocated_path() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("one.ARW");
        tiny_arw(&raw);
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let paths = Arc::new(vec![raw.clone()]);
        let ctx = egui::Context::default();
        let pool = Pool::new(paths.clone(), ctx.clone(), None);
        let mut mover =
            crate::move_jobs::Mover::new(pool.locations(), ctx, dir.path().join("journal"));
        mover
            .enqueue(fastcull::relocating::prepare(&paths, &[0], &target).unwrap())
            .unwrap();
        pool.start_index();
        pool.request(0, Level::Full);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let mut decoded = false;
        while !decoded || mover.jobs > 0 {
            if let Some(d) = pool.take_decoded() {
                assert!(d.result.is_ok(), "{:?}", d.result.err());
                decoded = true;
            }
            for event in mover.take() {
                if let crate::move_jobs::Event::Finished { error, .. } = event {
                    assert!(error.is_none(), "{error:?}");
                }
            }
            assert!(Instant::now() < deadline);
            thread::sleep(std::time::Duration::from_millis(2));
        }
        pool.write_xmp(
            0,
            &xmp::Updates {
                rating: Some(5),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(xmp::read(&target.join("one.ARW")).unwrap().rating, Some(5));
        assert!(!raw.exists());
        assert!(!raw.with_extension("xmp").exists());
    }
    #[test]
    fn invalid_sidecar_does_not_prevent_image_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.ARW");
        tiny_arw(&path);
        std::fs::write(path.with_extension("xmp"), "<broken>").unwrap();
        let session = Arc::new(Session::new(vec![fastcull::state::Item {
            path: path.clone(),
            updates: fastcull::xmp::Updates {
                rating: Some(3),
                ..Default::default()
            },
        }]));
        let pool = Pool::new(
            Arc::new(vec![path]),
            egui::Context::default(),
            Some(session),
        );
        pool.request(0, Level::Full);
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(d) = pool.take_decoded() {
                assert!(d.result.is_ok(), "{:?}", d.result.err());
                break;
            }
            assert!(Instant::now() < deadline, "image decode timed out");
            thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    fn result(index: usize, level: Level, generation: u64) -> Decoded {
        Decoded {
            task: Task {
                index,
                level,
                generation,
            },
            result: Err("fixture".into()),
            ms: 0.,
        }
    }
    #[test]
    fn stale_results_cannot_block_latest_current_photo() {
        let mut q = Queue::new(10);
        let stale = result(0, Level::Full, 0);
        q.pending.insert(stale.task);
        q.navigate(5);
        q.publish(stale);
        assert!(q.results.is_empty());
        let current = result(5, Level::Full, 1);
        q.pending.insert(current.task);
        q.publish(current);
        for i in 0..10 {
            let thumbnail = result(i, Level::Thumb, 1);
            q.pending.insert(thumbnail.task);
            q.publish(thumbnail);
        }
        assert!(q.results.len() <= 4);
        assert!(
            q.results
                .iter()
                .any(|d| d.task.index == 5 && d.task.level == Level::Full)
        );
    }
    #[test]
    fn navigation_reuses_inflight_next_photo_and_ready_neighbours() {
        let mut q = Queue::new(20);
        let next = result(1, Level::Full, 0);
        q.pending.insert(next.task);
        let ready = result(2, Level::Fit(2560), 0);
        q.pending.insert(ready.task);
        q.publish(ready);
        q.navigate(1);
        q.publish(next);
        assert_eq!(q.results.len(), 2);
        assert!(
            q.pending
                .iter()
                .any(|t| t.index == 1 && t.level == Level::Full)
        );
        q.navigate(19);
        assert!(q.results.is_empty());
        assert!(q.pending.is_empty());
    }
    #[test]
    fn cancelled_inflight_result_cannot_overwrite_retry() {
        let mut q = Queue::new(1);
        let old = result(0, Level::Full, 0);
        q.pending.insert(old.task);
        q.pending.clear();
        q.navigate(0);
        q.publish(old);
        assert!(q.results.is_empty());
    }
    #[test]
    fn idle_full_never_occupies_the_reserved_foreground_worker() {
        let mut q = Queue::new(2);
        q.idle_full.insert(0);
        let full = result(0, Level::Full, 0).task;
        assert!(!eligible(0, &full, &q));
        assert!(eligible(1, &full, &q));
        assert!(!eligible(2, &full, &q));
        assert!(eligible(0, &result(0, Level::Fit(1024), 0).task, &q));
        q.idle_full.remove(&0);
        assert!(eligible(0, &full, &q));
    }
    #[test]
    fn main_preview_precedes_background_full_and_visible_thumbs() {
        assert!(
            priority(
                Task {
                    index: 8,
                    level: Level::Fit(2560),
                    generation: 0
                },
                8
            ) < priority(
                Task {
                    index: 9,
                    level: Level::Full,
                    generation: 0
                },
                8
            )
        );
        assert!(
            priority(
                Task {
                    index: 9,
                    level: Level::Thumb,
                    generation: 0
                },
                8
            ) < priority(
                Task {
                    index: 9,
                    level: Level::Full,
                    generation: 0
                },
                8
            )
        );
    }
}
