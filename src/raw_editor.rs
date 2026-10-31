//! On-demand Local RAW workspace. Only the newest preview is queued; saves are
//! explicit, asynchronous and never discarded. Normal culling never starts it.
use anyhow::{Context, Result, ensure};
use eframe::egui::{self, Color32, TextureHandle, Vec2};
use fastcull::{
    local_raw, platform,
    xmp::{self, BasicAdjustments, BasicControl, Updates, Values, WhiteBalance},
};
use std::{
    collections::{VecDeque, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Stamp {
    len: u64,
    modified: SystemTime,
}
fn stamp(path: &Path) -> Result<Stamp> {
    let m = fs::metadata(path)?;
    Ok(Stamp {
        len: m.len(),
        modified: m.modified()?,
    })
}
pub fn supported(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        ["arw", "nef", "dng"]
            .iter()
            .any(|v| e.eq_ignore_ascii_case(v))
    })
}
fn cache_directory() -> PathBuf {
    std::env::var_os("FASTCULL_RAW_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| platform::data_directory().join("raw-editor/scene-cache-v1"))
}
const PREVIEW_MARKER: &str = "FastCull RAW preview cache v1\n";
fn prepare_preview_cache(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    if fs::read_dir(directory)?.next().is_none() {
        use std::io::Write;
        let marker = directory.join(".fastcull-previews");
        if let Ok(mut file) = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
        {
            file.write_all(PREVIEW_MARKER.as_bytes())?;
        }
    }
    Ok(())
}
fn cache_file(path: &Path, source: &Stamp, packet: &str, edge: u32, nonce: u64) -> PathBuf {
    let mut h = DefaultHasher::new();
    (
        fastcull::raw_color::PIPELINE_VERSION,
        env!("CARGO_PKG_VERSION"),
        path,
        source,
        packet,
        edge,
        nonce,
    )
        .hash(&mut h);
    cache_directory().join(format!("{:016x}.fcr", h.finish()))
}
/// Only our hashed preview PNGs are eligible. Never follow links or recurse into
/// Photoshop work copies, Lightroom exports or the user's photo directories.
fn trim_previews(directory: &Path, keep: &Path) {
    if fs::read_to_string(directory.join(".fastcull-previews"))
        .ok()
        .as_deref()
        != Some(PREVIEW_MARKER)
    {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().is_none_or(|s| s != "png" && s != "fcr")
            || stem.len() != 16
            || !stem.bytes().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_file() && !is_link(&metadata) {
            files.push((
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                path,
                metadata.len(),
            ));
        }
    }
    files.sort_by_key(|entry| entry.0);
    let mut bytes: u64 = files.iter().map(|entry| entry.2).sum();
    let mut count = files.len();
    for (_, path, len) in files {
        if count <= 64 && bytes <= 1_073_741_824 {
            break;
        }
        if path != keep && fs::remove_file(path).is_ok() {
            bytes = bytes.saturating_sub(len);
            count -= 1;
        }
    }
}
#[derive(Clone)]
struct PreviewJob {
    generation: u64,
    source: PathBuf,
    source_stamp: Stamp,
    identity_source: PathBuf,
    identity_stamp: Stamp,
    packet: String,
    edge: u32,
    nonce: u64,
}
struct SaveJob {
    source: PathBuf,
    source_stamp: Stamp,
    expected: Option<String>,
    base: String,
    prepared_source: PathBuf,
    updates: Updates,
}
struct PrepareJob {
    source: PathBuf,
    mailbox: PathBuf,
    id: String,
}
enum Job {
    Quick(PathBuf),
    Preview(PreviewJob),
    Save(SaveJob),
    Prepare(PrepareJob),
}
enum Event {
    Dimensions {
        size: Vec2,
        orientation: u16,
    },
    Quick {
        image: egui::ColorImage,
        ms: f64,
        size: Vec2,
        orientation: u16,
    },
    Scene {
        generation: u64,
        image: Result<Box<fastcull::raw_color::Frame>, String>,
        ms: f64,
        cached: bool,
    },
    Saved {
        result: Result<(Values, Updates), String>,
        packet: Option<String>,
        source_stamp: Option<Stamp>,
    },
    Prepared(Result<(PathBuf, String, Values), String>),
}
fn prepare_catalog(job: &PrepareJob, stop: &AtomicBool) -> Result<(PathBuf, String, Values)> {
    let _mailbox_guard = crate::raw_gate::acquire(&job.mailbox, stop)?;
    use std::io::Write;
    let parent = job.mailbox.parent().context("LR 会话目录缺失")?;
    let cache = parent
        .parent()
        .context("LR 数据目录缺失")?
        .join("raw-cache");
    fs::create_dir_all(&cache)?;
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let target = cache.join(format!("lr-{}-{nonce}", job.id));
    let response = job.mailbox.with_extension("raw-result");
    match fs::remove_file(&response) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut request = tempfile::NamedTempFile::new_in(parent)?;
    writeln!(
        request,
        "{}\n{}\n{}",
        job.id,
        fastcull::handoff::encode(job.source.to_str().context("LR 原片路径必须为 Unicode")?),
        fastcull::handoff::encode(target.to_str().context("LR 缓存路径必须为 Unicode")?)
    )?;
    request.as_file().sync_all()?;
    request.persist(&job.mailbox).map_err(|e| e.error)?;
    let started = Instant::now();
    loop {
        ensure!(!stop.load(Ordering::Relaxed), "LR 显影加载已取消");
        if let Ok(text) = fs::read_to_string(&response) {
            let mut lines = text.lines();
            if lines.next() == Some(job.id.as_str())
                && lines.next()
                    == Some(fastcull::handoff::encode(job.source.to_str().unwrap()).as_str())
                && let Some(line) = lines.next()
            {
                if let Some(message) = line.strip_prefix("ERROR\t") {
                    anyhow::bail!("LR 显影读取：{}", fastcull::handoff::decode(message)?);
                }
                if let Some(path) = line.strip_prefix("OK\t") {
                    let path = PathBuf::from(fastcull::handoff::decode(path)?);
                    let mut expected_name =
                        PathBuf::from(job.source.file_name().context("LR 原片文件名缺失")?);
                    expected_name.set_extension(
                        job.source
                            .extension()
                            .unwrap()
                            .to_string_lossy()
                            .to_ascii_lowercase(),
                    );
                    ensure!(
                        path.parent() == Some(target.as_path())
                            && path.file_name() == expected_name.file_name(),
                        "LR 返回了本次请求以外的文件"
                    );
                    let packet = xmp::packet(&path)?.context("LR 导出没有完整 XMP 显影设置")?;
                    let values = xmp::read(&path)?;
                    return Ok((path, packet, values));
                }
            }
        }
        ensure!(
            started.elapsed() < Duration::from_secs(90),
            "等待读取 LR 显影设置超时；请重新载入最新版插件并保持 Lightroom 运行"
        );
        thread::sleep(Duration::from_millis(100));
    }
}
fn write_packet(
    source: &Path,
    expected: Option<&str>,
    base: &str,
    updates: &Updates,
) -> Result<String> {
    use std::io::Write;
    ensure!(
        xmp::packet(source)?.as_deref() == expected,
        "XMP 已被其他程序修改，请重新加载后再保存"
    );
    let sidecar = source.with_extension("xmp");
    let before = match fs::read_to_string(&sidecar) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let merged = if expected == Some(base) {
        base.to_owned()
    } else {
        xmp::merge_develop(expected.unwrap_or(&xmp::template()), base)?
    };
    let packet = xmp::patch(&merged, updates)?;
    let parent = source
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(packet.as_bytes())?;
    temp.as_file().sync_all()?;
    ensure!(
        fs::read_to_string(&sidecar).ok() == before,
        "XMP 保存期间已改变，请重试"
    );
    temp.persist(sidecar).map_err(|e| e.error)?;
    Ok(packet)
}
fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn same_file_bytes(a: &Path, b: &Path) -> Result<bool> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut a, mut b) = (
        BufReader::new(fs::File::open(a)?),
        BufReader::new(fs::File::open(b)?),
    );
    let (mut left, mut right) = ([0; 65536], [0; 65536]);
    loop {
        let n = a.read(&mut left)?;
        if n == 0 {
            return Ok(b.read(&mut right)? == 0);
        }
        b.read_exact(&mut right[..n])?;
        if left[..n] != right[..n] {
            return Ok(false);
        }
    }
}
fn resource_parent(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut parent = root.to_path_buf();
    for component in relative.parent().into_iter().flat_map(Path::components) {
        ensure!(
            matches!(component, std::path::Component::Normal(_)),
            "LR 资源路径无效"
        );
        parent.push(component);
        match fs::symlink_metadata(&parent) {
            Ok(m) => ensure!(
                m.is_dir() && !is_link(&m),
                "LR 资源目标包含链接或非目录：{}",
                parent.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&parent)?,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(parent)
}
fn preserve_resources(prepared: &Path, source: &Path) -> Result<()> {
    let Some(root) = prepared
        .parent()
        .filter(|p| p.join(".fastcull-adobe-export").is_file())
    else {
        return Ok(());
    };
    let destination = source.parent().context("RAW 资源目录不存在")?;
    let sidecar = prepared.with_extension("xmp");
    let mut directories = vec![root.to_path_buf()];
    let mut count = 0;
    while let Some(directory) = directories.pop() {
        ensure!(
            !is_link(&fs::symlink_metadata(&directory)?),
            "LR 资源目录包含链接，未保存"
        );
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            ensure!(!is_link(&entry.metadata()?), "LR 资源包含符号链接，未保存");
            if path == prepared || path == sidecar || path == root.join(".fastcull-adobe-export") {
                continue;
            }
            count += 1;
            ensure!(count <= 4096, "LR 资源数量超出保存限制");
            let relative = path.strip_prefix(root)?;
            ensure!(
                relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "LR 资源路径无效"
            );
            let target = destination.join(relative);
            if kind.is_dir() {
                directories.push(path);
                continue;
            }
            ensure!(kind.is_file(), "LR 资源不是普通文件");
            let parent = resource_parent(destination, relative)?;
            if let Ok(metadata) = fs::symlink_metadata(&target) {
                ensure!(
                    !is_link(&metadata) && metadata.is_file(),
                    "LR 资源目标不是普通文件"
                );
            }
            if target.exists() {
                // Existing resources can be shared by other photos: never replace
                // a different mask/profile merely because this photo references it.
                ensure!(
                    same_file_bytes(&path, &target)?,
                    "已有 LR 资源内容冲突：{}；请先在 LR 保存元数据后重新加载",
                    target.display()
                );
            } else {
                let mut temp = tempfile::NamedTempFile::new_in(parent)?;
                std::io::copy(&mut fs::File::open(&path)?, &mut temp)?;
                temp.as_file().sync_all()?;
                temp.persist_noclobber(target).map_err(|e| e.error)?;
            }
        }
    }
    Ok(())
}
struct Worker {
    queue: Arc<(Mutex<VecDeque<Job>>, Condvar)>,
    stop: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    receive: mpsc::Receiver<Event>,
}
impl Worker {
    fn new(ctx: egui::Context) -> Self {
        let queue = Arc::new((Mutex::new(VecDeque::<Job>::new()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let (send, receive) = mpsc::channel();
        let (q, s, g) = (queue.clone(), stop.clone(), generation.clone());
        thread::Builder::new()
            .name("local-raw-editor".into())
            .spawn(move || {
                loop {
                    let job = {
                        let (lock, wake) = &*q;
                        let mut jobs = lock.lock().unwrap();
                        while jobs.is_empty() && !s.load(Ordering::Relaxed) {
                            jobs = wake.wait(jobs).unwrap();
                        }
                        if s.load(Ordering::Relaxed) {
                            return;
                        }
                        jobs.pop_front().unwrap()
                    };
                    let event = match job {
                        Job::Quick(path) => {
                            let quick_started = Instant::now();
                            let quick = (|| -> Result<_> {
                                let info = fastcull::arw::ArwInfo::read(&path)?;
                                if let Some((w, h)) = info.image_size.or(info.raw_size) {
                                    let _ = send.send(Event::Dimensions {
                                        size: Vec2::new(w as f32, h as f32),
                                        orientation: info.orientation,
                                    });
                                }
                                let pixels = fastcull::imaging::decode_level(
                                    &path,
                                    &info,
                                    fastcull::imaging::Level::Fit(1280),
                                )?;
                                let pixels = fastcull::imaging::orient(pixels, info.orientation);
                                let size = info
                                    .image_size
                                    .or(info.raw_size)
                                    .map(|(w, h)| Vec2::new(w as f32, h as f32))
                                    .unwrap_or(Vec2::new(
                                        pixels.width as f32,
                                        pixels.height as f32,
                                    ));
                                Ok((
                                    egui::ColorImage::from_rgba_premultiplied(
                                        [pixels.width, pixels.height],
                                        &pixels.rgba,
                                    ),
                                    size,
                                    info.orientation,
                                ))
                            })();
                            match quick {
                                Ok((image, size, orientation)) => Event::Quick {
                                    image,
                                    ms: quick_started.elapsed().as_secs_f64() * 1000.,
                                    size,
                                    orientation,
                                },
                                Err(_) => continue,
                            }
                        }
                        Job::Prepare(job) => {
                            Event::Prepared(prepare_catalog(&job, &s).map_err(|e| format!("{e:#}")))
                        }
                        Job::Preview(job) => {
                            let started = Instant::now();
                            let output = cache_file(
                                &job.identity_source,
                                &job.identity_stamp,
                                &job.packet,
                                job.edge,
                                job.nonce,
                            );
                            let mut cached = false;
                            let result = (|| -> Result<Box<fastcull::raw_color::Frame>> {
                                ensure!(
                                    stamp(&job.source)? == job.source_stamp,
                                    "RAW 已被其他程序修改，请重新加载"
                                );
                                let existing = fastcull::raw_float_cache::read(&output)
                                    .ok()
                                    .filter(|frame| {
                                        fastcull::raw_color::find_profile(
                                            &frame.make,
                                            &frame.model,
                                            &frame.requested_profile,
                                        )
                                        .is_ok_and(|p| {
                                            p.map_or(0, |p| p.fingerprint)
                                                == frame.profile_fingerprint
                                        })
                                    });
                                let image = if let Some(image) = existing {
                                    cached = true;
                                    image
                                } else {
                                    prepare_preview_cache(output.parent().unwrap())?;
                                    let rendered =
                                        local_raw::render_scene(&local_raw::SceneRequest {
                                            source: job.source.clone(),
                                            xmp: job.packet,
                                            max_edge: job.edge,
                                        })?;
                                    // Cache is optional; a full disk must not hide an otherwise valid RAW frame.
                                    if let Err(error) =
                                        fastcull::raw_float_cache::write(&output, &rendered.frame)
                                    {
                                        log::warn!("linear preview cache: {error:#}");
                                    }
                                    rendered.frame
                                };
                                ensure!(
                                    g.load(Ordering::Relaxed) == job.generation,
                                    "superseded preview"
                                );
                                trim_previews(output.parent().unwrap(), &output);
                                Ok(Box::new(image))
                            })()
                            .map_err(|e| format!("{e:#}"));
                            if g.load(Ordering::Relaxed) != job.generation {
                                continue;
                            }
                            Event::Scene {
                                generation: job.generation,
                                image: result,
                                ms: started.elapsed().as_secs_f64() * 1000.,
                                cached,
                            }
                        }
                        Job::Save(job) => {
                            let mut committed_packet = None;
                            let result = (|| -> Result<(Values, Updates)> {
                                ensure!(
                                    stamp(&job.source)? == job.source_stamp,
                                    "RAW 已被其他程序修改，请重新加载后再保存"
                                );
                                ensure!(
                                    xmp::packet(&job.source)? == job.expected,
                                    "XMP 已被其他程序修改，请重新加载后再保存"
                                );
                                preserve_resources(&job.prepared_source, &job.source)?;
                                committed_packet = Some(write_packet(
                                    &job.source,
                                    job.expected.as_deref(),
                                    &job.base,
                                    &job.updates,
                                )?);
                                if fastcull::formats::is_dng(&job.source) {
                                    let packet = xmp::packet(&job.source)?
                                        .context("saved DNG XMP missing")?;
                                    fastcull::dng::write_xmp(&job.source, &packet).context(
                                        "旁车已保存，但 DNG 内嵌 XMP 同步失败；可重试保存",
                                    )?;
                                }
                                Ok((xmp::read(&job.source)?, job.updates))
                            })()
                            .map_err(|e| format!("{e:#}"));
                            let source_stamp = committed_packet
                                .as_ref()
                                .and_then(|_| stamp(&job.source).ok());
                            Event::Saved {
                                result,
                                packet: committed_packet,
                                source_stamp,
                            }
                        }
                    };
                    if send.send(event).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn local RAW editor");
        Self {
            queue,
            stop,
            generation,
            receive,
        }
    }
    fn preview(&self, mut job: PreviewJob) -> u64 {
        local_raw::cancel_pending();
        job.generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let generation = job.generation;
        let mut q = self.queue.0.lock().unwrap();
        q.retain(|j| !matches!(j, Job::Preview(_)));
        q.push_back(Job::Preview(job));
        self.queue.1.notify_one();
        generation
    }
    fn save(&self, job: SaveJob) {
        let mut q = self.queue.0.lock().unwrap();
        q.retain(|j| !matches!(j, Job::Preview(_)));
        q.push_front(Job::Save(job));
        self.queue.1.notify_one();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        local_raw::cancel_pending();
        self.queue.1.notify_one();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Balance {
    Original,
    AsShot,
    Custom,
}
pub enum Action {
    None,
    Cancel,
    Reload,
    Saved(Box<(Values, Updates)>),
}
pub struct Editor {
    pub path: PathBuf,
    original: Values,
    draft_crop: fastcull::crop::Crop,
    draft_orientation: u16,
    base_orientation: u16,
    rating: u8,
    source_size: Option<Vec2>,
    crop_editor: Option<crate::crop_ui::Editor>,
    crop_ready: bool,
    packet: Option<String>,
    source_stamp: Stamp,
    source_orientation: Option<u16>,
    render_source: PathBuf,
    render_packet: String,
    preparing: bool,
    ready: bool,
    exposure: f64,
    basic: BasicAdjustments,
    enable_light: bool,
    enable_color: bool,
    basic_ranges: [Option<(i32, i32)>; 7],
    sample: Option<crate::raw_sample::SampleMap>,
    histogram: Option<crate::raw_histogram::Histogram>,
    picking: bool,
    effective_wb: Option<WhiteBalance>,
    balance: Balance,
    temperature: String,
    tint: String,
    worker: Worker,
    generation: u64,
    texture: Option<TextureHandle>,
    linear_texture: Option<Arc<crate::raw_preview::LinearTexture>>,
    grey_sampler: Option<fastcull::raw_color::GreySampler>,
    profile_status: String,
    color_warning: Option<String>,
    changed_at: Option<Instant>,
    error: Option<String>,
    status: String,
    rendering: bool,
    saving: bool,
    save_error: Option<String>,
    full: bool,
    view: crate::raw_view::View,
    nonce: u64,
    preview: Option<Arc<crate::raw_preview::Preview>>,
    rendered_exposure: f64,
    smoke_step: u8,
    smoke_at: Instant,
    confirmed: bool,
}
impl Editor {
    pub fn new(
        path: PathBuf,
        ctx: &egui::Context,
        preview: Option<Arc<crate::raw_preview::Preview>>,
        catalog: Option<(PathBuf, String)>,
    ) -> Result<Self> {
        Self::with_nonce(path, ctx, preview, catalog, 0)
    }
    fn with_nonce(
        path: PathBuf,
        ctx: &egui::Context,
        preview: Option<Arc<crate::raw_preview::Preview>>,
        catalog: Option<(PathBuf, String)>,
        nonce: u64,
    ) -> Result<Self> {
        ensure!(supported(&path), "RAW 编辑第一版支持 ARW、NEF 和 DNG");
        let path = std::path::absolute(path)?;
        let original = xmp::read(&path)?;
        let temperature = original
            .white_balance
            .as_ref()
            .and_then(|v| v.temperature)
            .map(|v| v.to_string())
            .unwrap_or_default();
        let tint = original
            .white_balance
            .as_ref()
            .and_then(|v| v.tint)
            .map(|v| v.to_string())
            .unwrap_or_default();
        let initial_exposure = f64::from(original.exposure.unwrap_or(0)) / 1_000_000.;
        let packet = xmp::packet(&path)?;
        let mut editor = Self {
            packet: packet.clone(),
            source_stamp: stamp(&path)?,
            path,
            render_source: PathBuf::new(),
            render_packet: packet.clone().unwrap_or_else(xmp::template),
            preparing: catalog.is_some(),
            ready: catalog.is_none(),
            exposure: initial_exposure,
            basic: original.basic,
            enable_light: true,
            enable_color: true,
            basic_ranges: BasicControl::ALL.map(|c| {
                xmp::basic_range(packet.as_deref().unwrap_or(&xmp::template()), c)
                    .ok()
                    .map(|r| (*r.start(), *r.end()))
            }),
            sample: None,
            histogram: None,
            picking: false,
            effective_wb: original.white_balance.as_ref().and_then(|w| w.edit()),
            source_orientation: original.orientation,
            draft_crop: original.crop.unwrap_or_default(),
            draft_orientation: original.orientation.unwrap_or(1),
            base_orientation: original.orientation.unwrap_or(1),
            rating: original.rating.unwrap_or(0),
            source_size: None,
            crop_editor: None,
            crop_ready: false,
            original,
            balance: Balance::Original,
            temperature,
            tint,
            worker: Worker::new(ctx.clone()),
            generation: 0,
            texture: None,
            linear_texture: None,
            grey_sampler: None,
            profile_status: "正在读取配置文件…".into(),
            color_warning: None,
            changed_at: None,
            error: None,
            status: "正在加载本地 RAW 显影…".into(),
            rendered_exposure: initial_exposure,
            rendering: false,
            saving: false,
            save_error: None,
            full: false,
            view: Default::default(),
            nonce,
            preview,
            smoke_step: 0,
            smoke_at: Instant::now(),
            confirmed: false,
        };
        editor.render_source = editor.path.clone();
        editor
            .worker
            .queue
            .0
            .lock()
            .unwrap()
            .push_back(Job::Quick(editor.path.clone()));
        editor.worker.queue.1.notify_one();
        if let Some((mailbox, id)) = catalog {
            editor
                .worker
                .queue
                .0
                .lock()
                .unwrap()
                .push_back(Job::Prepare(PrepareJob {
                    source: editor.path.clone(),
                    mailbox,
                    id,
                }));
            editor.worker.queue.1.notify_one();
            editor.status = "正在读取 Lightroom 目录显影设置…".into();
        } else {
            editor.render()?;
        }
        Ok(editor)
    }
    fn updates(&self) -> Result<Updates> {
        ensure!(
            self.exposure.is_finite() && (-5.0..=5.0).contains(&self.exposure),
            "曝光须在 -5 到 +5 EV"
        );
        let exposure = (self.exposure * 1_000_000.).round() as i32;
        let white_balance = match self.balance {
            Balance::Original => None,
            Balance::AsShot => Some(WhiteBalance::AsShot),
            Balance::Custom => {
                let temperature = self
                    .temperature
                    .trim()
                    .parse()
                    .context("请输入有效色温（K）")?;
                let tint = self.tint.trim().parse().context("请输入有效色调")?;
                let wb = WhiteBalance::Custom { temperature, tint };
                ensure!(wb.valid(), "色温范围为 2000–50000 K，色调为 -150–150");
                Some(wb)
            }
        };
        let exposure = (exposure != self.original.exposure.unwrap_or(0)).then_some(exposure);
        let mut basic = BasicAdjustments::default();
        for c in BasicControl::ALL {
            let value = self.basic.get(c).unwrap_or(0);
            if value != self.original.basic.get(c).unwrap_or(0) {
                basic.set(c, Some(value));
            }
        }
        Ok(Updates {
            rating: (self.rating != self.original.rating.unwrap_or(0)).then_some(self.rating),
            crop: (self.draft_crop != self.original.crop.unwrap_or_default())
                .then_some(self.draft_crop),
            exposure,
            basic,
            white_balance,
            // Bring an unsaved LR rotation along with its develop recipe only
            // when the source has no explicit, potentially newer orientation.
            orientation: if self.draft_orientation != self.base_orientation
                || self.draft_crop != self.original.crop.unwrap_or_default()
            {
                Some(self.draft_orientation)
            } else if self.source_orientation.is_none()
                && (exposure.is_some() || white_balance.is_some())
            {
                self.original.orientation
            } else {
                None
            },
            ..Default::default()
        })
    }
    pub fn dirty(&self) -> bool {
        self.updates().map_or(true, |u| !u.empty())
    }
    /// Capture an unfinished crop when closing the editor; saving waits for this
    /// final state and keeps the window open if the atomic write fails.
    pub fn auto_save(&mut self) -> Result<bool> {
        if self.saving || self.preparing {
            return Ok(false);
        }
        if !self.ready {
            if !self.dirty() {
                return Ok(true);
            }
            anyhow::bail!("RAW 设置尚未读完，请先重新读取后再保存");
        }
        if let Some(error) = &self.save_error {
            anyhow::bail!("{error}");
        }
        self.finish_crop(true)?;
        let updates = self.updates()?;
        if updates.empty() {
            return Ok(true);
        }
        self.worker.save(SaveJob {
            source: self.path.clone(),
            source_stamp: self.source_stamp.clone(),
            expected: self.packet.clone(),
            base: self.render_packet.clone(),
            prepared_source: self.render_source.clone(),
            updates,
        });
        self.saving = true;
        self.error = None;
        self.status = "正在自动保存 XMP…".into();
        Ok(false)
    }
    pub fn retry_save(&mut self) {
        self.save_error = None;
    }
    fn begin_crop(&mut self) -> Result<()> {
        let size = self.source_size.context("正在读取照片尺寸，请稍候")?;
        self.crop_editor = Some(crate::crop_ui::Editor::new(
            self.draft_crop,
            size,
            self.draft_orientation,
        ));
        self.picking = false;
        self.render()
    }
    fn finish_crop(&mut self, apply: bool) -> Result<()> {
        if let Some(editor) = self.crop_editor.take() {
            if apply {
                let geometry = fastcull::crop::Geometry::full(editor.display());
                self.draft_crop =
                    if editor.geometry.angle == 0. && editor.geometry.rect == geometry.rect {
                        Default::default()
                    } else {
                        editor.value()
                    };
                self.draft_orientation = editor.orientation;
            }
            self.texture = None;
            self.linear_texture = None;
            self.grey_sampler = None;
            self.view.fit();
            self.render()?;
        }
        Ok(())
    }
    fn crop_canvas(&mut self, ui: &mut egui::Ui) {
        let (area, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        ui.painter().rect_filled(area, 0., Color32::from_gray(18));
        if !self.crop_ready {
            ui.painter().text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "正在加载完整裁切范围…",
                egui::FontId::proportional(16.),
                Color32::GRAY,
            );
            return;
        }
        let settings = self.preview_settings();
        if let (Some(editor), Some(texture)) = (&mut self.crop_editor, &self.texture) {
            let display = editor.display();
            let (s, c) = editor.geometry.angle.to_radians().sin_cos();
            let bounds = Vec2::new(
                c.abs() * display.x + s.abs() * display.y,
                s.abs() * display.x + c.abs() * display.y,
            );
            let scale = (area.width() / bounds.x).min(area.height() / bounds.y) * 0.94;
            let rect = egui::Rect::from_center_size(area.center(), display * scale);
            let geometry = fastcull::crop::Geometry {
                rect: egui::Rect::from_center_size((display * 0.5).to_pos2(), bounds),
                angle: editor.geometry.angle,
            };
            let backdrop = egui::Rect::from_center_size(area.center(), bounds * scale);
            let uv = geometry.uvs(editor.size, editor.orientation);
            let painter = ui.painter_at(area);
            if let Some(preview) = &self.preview {
                if let Some(linear) = &self.linear_texture {
                    preview.paint_linear(&painter, area, backdrop, linear.clone(), uv, settings);
                } else {
                    preview.paint(&painter, area, backdrop, texture.id(), uv, settings);
                }
            } else {
                crate::ui::paint_uv(&painter, texture, backdrop, uv);
            }
            editor.overlay(ui, &response, rect);
        }
    }
    fn render(&mut self) -> Result<()> {
        let mut updates = self.updates()?;
        updates.basic = BasicAdjustments::default();
        updates.rating = None;
        if self.crop_editor.is_some() {
            updates.crop = Some(Default::default());
            updates.orientation = Some(1);
            self.crop_ready = false;
        }
        let packet = if updates.empty() {
            self.render_packet.clone()
        } else {
            xmp::patch(&self.render_packet, &updates)?
        };
        self.generation = self.worker.preview(PreviewJob {
            generation: 0,
            source: self.render_source.clone(),
            source_stamp: stamp(&self.render_source)?,
            identity_source: self.path.clone(),
            identity_stamp: self.source_stamp.clone(),
            packet,
            edge: if self.full { 0 } else { 1600 },
            nonce: self.nonce,
        });
        self.rendering = true;
        self.confirmed = false;
        self.changed_at = None;
        self.error = None;
        self.status = "本地 RAW 显影中…".into();
        Ok(())
    }
    fn preview_settings(&self) -> crate::raw_preview::Settings {
        let exposure = if self.enable_light {
            self.exposure
        } else {
            self.original.exposure.unwrap_or(0) as f64 / 1e6
        };
        let basic = BasicControl::ALL.map(|c| {
            let enabled = match c {
                BasicControl::Vibrance | BasicControl::Saturation => self.enable_color,
                _ => self.enable_light,
            };
            (if enabled {
                self.basic.get(c)
            } else {
                self.original.basic.get(c)
            })
            .unwrap_or(0) as f32
                / 100.
        });
        crate::raw_preview::Settings {
            exposure: if self.linear_texture.is_some() {
                exposure as f32
            } else {
                (exposure - self.rendered_exposure) as f32
            },
            basic,
            wb: [1.; 3],
        }
    }
    pub fn show(&mut self, ctx: &egui::Context) -> Action {
        let mut action = Action::None;
        let mut crop_action = crate::crop_ui::Action::None;
        while let Ok(event) = self.worker.receive.try_recv() {
            match event {
                Event::Dimensions { size, orientation } => {
                    self.source_size = Some(size);
                    if self.original.orientation.is_none() {
                        self.draft_orientation = orientation;
                        self.base_orientation = orientation;
                    }
                }
                Event::Quick {
                    image,
                    ms,
                    size,
                    orientation,
                } => {
                    self.source_size = Some(size);
                    if self.original.orientation.is_none() {
                        self.draft_orientation = orientation;
                        self.base_orientation = orientation;
                    }
                    if let Some(dir) = std::env::var_os("FASTCULL_RAW_EDIT_SMOKE_DIR") {
                        let _ = fs::create_dir_all(&dir);
                        let _ =
                            fs::write(PathBuf::from(dir).join("quick-ms.txt"), format!("{ms:.3}"));
                    }
                    if self.texture.is_none() {
                        self.sample = crate::raw_sample::SampleMap::from_image(&image).ok();
                        self.histogram = Some(crate::raw_histogram::Histogram::from_image(&image));
                        self.texture = Some(ctx.load_texture(
                            "raw-quick",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                        self.rendered_exposure = 0.;
                        self.status = "快速原片预览 · 正在后台显影".into();
                    }
                }
                Event::Prepared(result) => {
                    self.preparing = false;
                    match result {
                        Ok((source, packet, mut values)) => {
                            let merged = (|| -> Result<String> {
                                let mut merged = xmp::merge_develop(
                                    self.packet.as_deref().unwrap_or(&xmp::template()),
                                    &packet,
                                )?;
                                if self.source_orientation.is_none()
                                    && let Some(orientation) = values.orientation
                                {
                                    merged = xmp::patch(
                                        &merged,
                                        &Updates {
                                            orientation: Some(orientation),
                                            ..Default::default()
                                        },
                                    )?;
                                }
                                Ok(merged)
                            })();
                            let packet = match merged {
                                Ok(packet) => packet,
                                Err(error) => {
                                    self.ready = false;
                                    self.error = Some(format!("LR 配方合并失败：{error:#}"));
                                    continue;
                                }
                            };
                            values.orientation = self.source_orientation.or(values.orientation);
                            self.ready = true;
                            // A fresh LR export can contain changed mask payloads
                            // even if its XMP references have stayed identical.
                            let mut identity = DefaultHasher::new();
                            source.hash(&mut identity);
                            self.nonce.hash(&mut identity);
                            self.nonce = identity.finish();
                            self.render_source = source;
                            self.render_packet = packet.clone();
                            self.basic = values.basic;
                            self.basic_ranges = BasicControl::ALL.map(|c| {
                                xmp::basic_range(&packet, c)
                                    .ok()
                                    .map(|r| (*r.start(), *r.end()))
                            });
                            self.original = values;
                            self.draft_crop = self.original.crop.unwrap_or_default();
                            self.draft_orientation =
                                self.original.orientation.unwrap_or(self.draft_orientation);
                            self.base_orientation = self.draft_orientation;
                            self.rating = self.original.rating.unwrap_or(0);
                            self.exposure =
                                f64::from(self.original.exposure.unwrap_or(0)) / 1_000_000.;
                            self.temperature = self
                                .original
                                .white_balance
                                .as_ref()
                                .and_then(|v| v.temperature)
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            self.tint = self
                                .original
                                .white_balance
                                .as_ref()
                                .and_then(|v| v.tint)
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            if let Err(e) = self.render() {
                                self.error = Some(format!("{e:#}"));
                            }
                        }
                        Err(e) => {
                            self.error = Some(e);
                            self.status = "未能读取 LR 显影设置".into();
                        }
                    }
                }
                Event::Scene {
                    generation,
                    image,
                    ms,
                    cached,
                } if generation == self.generation => {
                    self.rendering = false;
                    match image {
                        Ok(frame) => {
                            let frame = *frame;
                            self.crop_ready = self.crop_editor.is_some();
                            self.profile_status = frame.profile;
                            self.color_warning = frame.warning;
                            self.grey_sampler = Some(frame.grey);
                            if let (Some(t), Some(tint)) = (frame.temperature, frame.tint) {
                                let wb = WhiteBalance::Custom {
                                    temperature: t.round() as u32,
                                    tint: tint.round() as i32,
                                };
                                if wb.valid() {
                                    self.effective_wb = Some(wb);
                                    if self.balance != Balance::Custom {
                                        self.temperature = format!("{t:.0}");
                                        self.tint = format!("{tint:.0}");
                                    }
                                }
                            }
                            let image = crate::raw_preview::LinearImage {
                                size: frame.size,
                                pixels: frame.pixels,
                                recipe: crate::raw_preview::Recipe {
                                    look: frame.recipe.look.map(|map| {
                                        crate::raw_preview::HsvLookup {
                                            dims: map.dims,
                                            data: map.data,
                                            encoding: if map.encoding
                                                == fastcull::dcp::Encoding::Srgb
                                            {
                                                1
                                            } else {
                                                0
                                            },
                                        }
                                    }),
                                    curve: frame.recipe.curve,
                                    baseline_exposure: frame.recipe.baseline_exposure,
                                },
                            };
                            self.histogram =
                                Some(crate::raw_histogram::Histogram::from_linear(&image));
                            self.sample = None;
                            let dimensions = image.size;
                            if let Some(preview) = &self.preview {
                                self.linear_texture = Some(preview.upload(image));
                                self.texture = Some(ctx.load_texture(
                                    "raw-linear-anchor",
                                    egui::ColorImage::filled([1, 1], Color32::TRANSPARENT),
                                    Default::default(),
                                ));
                            } else {
                                let mut settings = self.preview_settings();
                                settings.exposure = self.exposure as f32;
                                let encoded = egui::ColorImage {
                                    size: image.size,
                                    source_size: Vec2::new(
                                        image.size[0] as f32,
                                        image.size[1] as f32,
                                    ),
                                    pixels: image
                                        .pixels
                                        .iter()
                                        .map(|p| {
                                            let rgb = crate::raw_preview::evaluate_scene(
                                                [p[0], p[1], p[2]],
                                                settings,
                                                &image.recipe,
                                            );
                                            Color32::from_rgb(
                                                (rgb[0] * 255.).round() as u8,
                                                (rgb[1] * 255.).round() as u8,
                                                (rgb[2] * 255.).round() as u8,
                                            )
                                        })
                                        .collect(),
                                };
                                self.linear_texture = None;
                                self.texture = Some(ctx.load_texture(
                                    "raw-scene-software",
                                    encoded,
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                            if self.source_size.is_none() {
                                self.source_size =
                                    Some(Vec2::new(dimensions[0] as f32, dimensions[1] as f32));
                            }
                            self.rendered_exposure = 0.;
                            self.confirmed = true;
                            self.error = None;
                            self.status = format!(
                                "浮点显影 · {} · {:.0} ms",
                                if cached { "缓存" } else { "配置文件" },
                                ms
                            );
                            if let Some(dir) = std::env::var_os("FASTCULL_RAW_EDIT_SMOKE_DIR") {
                                use std::io::Write;
                                let dir = PathBuf::from(dir);
                                let _ = fs::create_dir_all(&dir);
                                if let Ok(mut file) = fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open(dir.join("timings.tsv"))
                                {
                                    let _ = writeln!(
                                        file,
                                        "{generation}\t{cached}\t{ms:.3}\t{}\t{}\t{}",
                                        self.full, dimensions[0], dimensions[1]
                                    );
                                }
                            }
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Event::Saved {
                    result,
                    packet,
                    source_stamp,
                } => {
                    self.saving = false;
                    if let Some(packet) = packet {
                        self.packet = Some(packet);
                    }
                    if let Some(stamp) = source_stamp {
                        self.source_stamp = stamp;
                    }
                    match result {
                        Ok((values, updates)) => {
                            self.save_error = None;
                            self.source_orientation = values.orientation;
                            self.original = values.clone();
                            self.base_orientation = self.draft_orientation;
                            self.balance = Balance::Original;
                            self.render_packet = self.packet.clone().unwrap_or_else(xmp::template);
                            self.status = "已保存显影调整 · sRGB".into();
                            self.error = None;
                            // Applying a crop invalidates the previous texture. A
                            // manual save must still leave a fresh image visible.
                            if self.texture.is_none()
                                && let Err(error) = self.render()
                            {
                                self.error = Some(format!("{error:#}"));
                            }
                            action = Action::Saved(Box::new((values, updates)));
                        }
                        Err(e) => {
                            self.save_error = Some(e.clone());
                            self.error = Some(e);
                        }
                    }
                }
                _ => {}
            }
        }
        if self.ready && !self.saving && !ctx.wants_keyboard_input() {
            for (key, value) in [
                (egui::Key::Num0, 0),
                (egui::Key::Num1, 1),
                (egui::Key::Num2, 2),
                (egui::Key::Num3, 3),
                (egui::Key::Num4, 4),
                (egui::Key::Num5, 5),
            ] {
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                    self.rating = value;
                }
            }
            if let Some(editor) = &mut self.crop_editor {
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                    crop_action = crate::crop_ui::Action::Cancel;
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                    crop_action = crate::crop_ui::Action::Apply;
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Z)) {
                    editor.undo();
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Y)) {
                    editor.redo();
                }
            } else {
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Z)) {
                    if self.view.is_fit() {
                        self.view.set_scale(1.);
                    } else {
                        self.view.fit();
                    }
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::T))
                    && let Err(error) = self.begin_crop()
                {
                    self.error = Some(format!("{error:#}"));
                }
            }
        }
        egui::TopBottomPanel::top("raw-edit-title").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("RAW 编辑");
                ui.label(self.path.file_name().unwrap_or_default().to_string_lossy());
                if self.rendering || self.saving || self.preparing {
                    ui.spinner();
                }
                ui.weak(&self.status);
            });
        });
        egui::SidePanel::right("raw-edit-controls")
            .exact_width(346.)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_gray(35))
                    .inner_margin(14.),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(8., 8.);
                ui.spacing_mut().slider_rail_height = 3.;
                ui.visuals_mut().widgets.inactive.bg_fill = Color32::from_gray(165);
                ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::from_gray(48);
                ui.visuals_mut().widgets.hovered.bg_fill = Color32::from_gray(220);
                egui::TopBottomPanel::bottom("raw-actions")
                    .frame(egui::Frame::new().inner_margin(egui::Margin {
                        top: 10,
                        ..Default::default()
                    }))
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.weak("星级");
                            crate::ui::rating_control(
                                ui,
                                &mut self.rating,
                                self.ready && !self.saving,
                            );
                            if ui
                                .add_enabled(
                                    self.ready && !self.saving && self.source_size.is_some(),
                                    egui::Button::new("裁切 T")
                                        .selected(self.crop_editor.is_some()),
                                )
                                .clicked()
                            {
                                let result = if self.crop_editor.is_some() {
                                    self.finish_crop(true)
                                } else {
                                    self.begin_crop()
                                };
                                if let Err(error) = result {
                                    self.error = Some(format!("{error:#}"));
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.weak("查看");
                            for (fit, label) in [(true, "适屏"), (false, "100%")] {
                                if ui
                                    .add_enabled(
                                        !self.saving && self.ready && self.crop_editor.is_none(),
                                        egui::Button::new(label).selected(if fit {
                                            self.view.is_fit()
                                        } else {
                                            !self.view.is_fit()
                                                && (self.view.scale() - 1.).abs() < 0.001
                                        }),
                                    )
                                    .clicked()
                                {
                                    if fit {
                                        self.view.fit();
                                    } else {
                                        self.view.set_scale(1.);
                                    }
                                }
                            }
                            let mut percent = self.view.scale() * 100.;
                            if ui
                                .add_enabled(
                                    !self.saving && self.ready && self.crop_editor.is_none(),
                                    egui::DragValue::new(&mut percent)
                                        .range(1.0..=800.0)
                                        .speed(1.)
                                        .fixed_decimals(0)
                                        .suffix("%"),
                                )
                                .on_hover_text("滚轮缩放 · 拖动平移 · 双击或 Z 切换适屏 / 100%")
                                .changed()
                            {
                                self.view.set_scale(percent / 100.);
                            }
                        });
                        let valid = self.updates();
                        if let Err(error) = &valid {
                            ui.colored_label(Color32::LIGHT_RED, error.to_string());
                        }
                        if ui
                            .add_enabled(
                                !self.saving
                                    && self.ready
                                    && valid.is_ok()
                                    && (self.dirty() || self.crop_editor.is_some())
                                    && (self.confirmed || self.crop_ready)
                                    && !self.rendering
                                    && self.changed_at.is_none(),
                                egui::Button::new("保存调整到 XMP")
                                    .fill(Color32::from_rgb(57, 100, 145))
                                    .min_size(Vec2::new(ui.available_width(), 32.)),
                            )
                            .clicked()
                        {
                            self.retry_save();
                            if let Err(error) = self.auto_save() {
                                self.error = Some(format!("{error:#}"));
                            }
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!self.saving, egui::Button::new("重新读取"))
                                .on_hover_text("重新读取 RAW / XMP，丢弃本窗口草稿")
                                .clicked()
                            {
                                action = Action::Reload;
                            }
                            if ui
                                .add_enabled(!self.saving, egui::Button::new("关闭窗口"))
                                .on_hover_text(if self.dirty() {
                                    "关闭前自动保存 XMP"
                                } else {
                                    "关闭 RAW 编辑窗口，自动保存修改"
                                })
                                .clicked()
                            {
                                action = Action::Cancel;
                            }
                        });
                    });
                if let Some(editor) = &mut self.crop_editor {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add_enabled_ui(!self.saving, |ui| {
                            crop_action = editor.controls(ui);
                        });
                    });
                    return;
                }
                let settings = self.preview_settings();
                if let Some(histogram) = &mut self.histogram {
                    histogram.show(ui, settings);
                } else {
                    ui.strong("直方图");
                    ui.add_space(124.);
                    ui.weak("正在加载预览…");
                }
                ui.weak(format!("配置文件：{}", self.profile_status))
                    .on_hover_text(
                        self.color_warning
                            .as_deref()
                            .unwrap_or("本地高精度显影；默认色调与滑块模型仍可能与 LR 不同。"),
                    );
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let (mut native_changed, mut changed) = (false, false);
                        ui.add_enabled_ui(!self.saving && self.ready, |ui| {
                            section_header(
                                ui,
                                "色调",
                                &mut self.enable_light,
                                "预览亮度调整；不改保存参数",
                            );
                            match xmp::exposure_range(&self.render_packet) {
                                Ok(range) => {
                                    changed |= adjustment_slider(
                                        ui,
                                        "曝光",
                                        &mut self.exposure,
                                        f64::from(*range.start()) / 1e6
                                            ..=f64::from(*range.end()) / 1e6,
                                        0.01,
                                        false,
                                        "",
                                    )
                                }
                                Err(error) => {
                                    ui.weak(format!("无法编辑曝光：{error}"));
                                }
                            }
                            for (index, label) in ["对比度", "高光", "阴影", "白色", "黑色"]
                                .iter()
                                .enumerate()
                            {
                                changed |= basic_slider(
                                    ui,
                                    &mut self.basic,
                                    BasicControl::ALL[index],
                                    self.basic_ranges[index],
                                    label,
                                );
                            }
                            ui.add_space(5.);
                            ui.separator();
                            section_header(
                                ui,
                                "颜色",
                                &mut self.enable_color,
                                "预览饱和度调整；白平衡以 RAW 显影为准，不改保存参数",
                            );
                            ui.horizontal(|ui| {
                                ui.label("白平衡");
                                let before = self.balance;
                                egui::ComboBox::from_id_salt("raw-wb")
                                    .width(120.)
                                    .selected_text(match self.balance {
                                        Balance::Original => "保留原设置",
                                        Balance::AsShot => "原片",
                                        Balance::Custom => "自定义",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.balance,
                                            Balance::Original,
                                            "保留原设置",
                                        );
                                        ui.selectable_value(
                                            &mut self.balance,
                                            Balance::AsShot,
                                            "原片",
                                        );
                                        ui.selectable_value(
                                            &mut self.balance,
                                            Balance::Custom,
                                            "自定义",
                                        );
                                    });
                                if before != self.balance {
                                    if self.balance == Balance::Custom {
                                        if self.temperature.parse::<u32>().is_err() {
                                            self.temperature = "6500".into();
                                        }
                                        if self.tint.parse::<i32>().is_err() {
                                            self.tint = "0".into();
                                        }
                                    }
                                    native_changed = true;
                                }
                                if ui
                                    .add_enabled(
                                        self.confirmed
                                            && (self.grey_sampler.is_some()
                                                || self.sample.is_some()),
                                        egui::Button::new(if self.picking {
                                            "取消"
                                        } else {
                                            "吸管"
                                        }),
                                    )
                                    .on_hover_text("点击图中未剪切的中性灰或白区域估算白平衡")
                                    .clicked()
                                {
                                    self.picking = !self.picking;
                                }
                            });
                            let mut temperature = self.temperature.parse::<f64>().unwrap_or(6500.);
                            let mut tint = self.tint.parse::<f64>().unwrap_or(0.);
                            let wb_changed = adjustment_slider(
                                ui,
                                "色温",
                                &mut temperature,
                                2000.0..=50000.0,
                                1.,
                                true,
                                "",
                            ) | adjustment_slider(
                                ui,
                                "色调",
                                &mut tint,
                                -150.0..=150.0,
                                1.,
                                false,
                                "",
                            );
                            if wb_changed {
                                self.balance = Balance::Custom;
                                self.temperature = format!("{temperature:.0}");
                                self.tint = format!("{tint:.0}");
                                native_changed = true;
                            }
                            for (index, label) in ["自然饱和度", "饱和度"].iter().enumerate()
                            {
                                let c = index + 5;
                                changed |= basic_slider(
                                    ui,
                                    &mut self.basic,
                                    BasicControl::ALL[c],
                                    self.basic_ranges[c],
                                    label,
                                );
                            }
                            if native_changed {
                                self.changed_at = Some(Instant::now());
                                self.generation =
                                    self.worker.generation.fetch_add(1, Ordering::Relaxed) + 1;
                                self.confirmed = false;
                                self.status = "实时预览 · 后台更新 RAW".into();
                            }
                            if changed {
                                self.error = None;
                            }
                        });
                        if let Some(error) = &self.error {
                            ui.separator();
                            ui.colored_label(Color32::LIGHT_RED, error);
                        }
                        ui.add_space(8.);
                        egui::CollapsingHeader::new("显影信息").show(ui, |ui| {
                            ui.weak("线性浮点 · ProPhoto → sRGB");
                            ui.weak(format!("已应用：{}", self.profile_status));
                            if let Some(profile) = &self.original.camera_profile {
                                ui.weak(format!("XMP 配置文件：{profile}"));
                            }
                            if let Some(warning) = &self.color_warning {
                                ui.colored_label(Color32::from_rgb(220, 185, 110), warning);
                            }
                            if let Some(version) = &self.original.process_version {
                                ui.weak(format!("处理版本：{version}"));
                            }
                        });
                    });
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.crop_editor.is_some() {
                self.crop_canvas(ui);
                return;
            }
            let (area, response) =
                ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
            ui.painter().rect_filled(area, 0., Color32::from_gray(18));
            if let Some(texture) = &self.texture {
                let pixels = self
                    .source_size
                    .map(|size| {
                        self.draft_crop
                            .geometry(size, self.draft_orientation)
                            .rect
                            .size()
                    })
                    .unwrap_or_else(|| {
                        self.linear_texture
                            .as_ref()
                            .map_or_else(|| texture.size_vec2(), |t| t.size_vec2())
                    });
                let rect = self
                    .view
                    .interact(ui, area, &response, pixels, self.picking);
                if self.picking {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if self.picking
                    && response.clicked()
                    && let Some(position) = response.interact_pointer_pos()
                {
                    let uv = [
                        (position.x - rect.min.x) / rect.width(),
                        (position.y - rect.min.y) / rect.height(),
                    ];
                    let estimate = (|| -> Result<_> {
                        if let Some(sampler) = &self.grey_sampler {
                            return Ok(crate::raw_sample::WhiteBalanceEstimate {
                                white_balance: sampler.estimate(uv)?,
                                used_initial_guess: false,
                            });
                        }
                        let rgb = self
                            .sample
                            .as_ref()
                            .context("请先加载 RAW 预览")?
                            .sample(uv)?;
                        crate::raw_sample::estimate(rgb, self.effective_wb)
                    })();
                    match estimate {
                        Ok(result) => {
                            if let WhiteBalance::Custom { temperature, tint } = result.white_balance
                            {
                                self.balance = Balance::Custom;
                                self.temperature = temperature.to_string();
                                self.tint = tint.to_string();
                                self.changed_at = Some(Instant::now());
                                self.confirmed = false;
                                self.picking = false;
                                self.status = if result.used_initial_guess {
                                    "灰点估算 · 正在重新显影，可继续微调".into()
                                } else {
                                    "灰点取样 · 正在重新显影".into()
                                };
                            }
                        }
                        Err(error) => self.error = Some(format!("取样失败：{error:#}")),
                    }
                }
                if let Some(preview) = &self.preview {
                    let uv = [
                        egui::pos2(0., 0.),
                        egui::pos2(1., 0.),
                        egui::pos2(1., 1.),
                        egui::pos2(0., 1.),
                    ];
                    if let Some(linear) = &self.linear_texture {
                        preview.paint_linear(
                            &ui.painter().with_clip_rect(area),
                            area,
                            rect,
                            linear.clone(),
                            uv,
                            self.preview_settings(),
                        );
                    } else {
                        preview.paint(
                            &ui.painter().with_clip_rect(area),
                            area,
                            rect,
                            texture.id(),
                            [
                                egui::pos2(0., 0.),
                                egui::pos2(1., 0.),
                                egui::pos2(1., 1.),
                                egui::pos2(0., 1.),
                            ],
                            self.preview_settings(),
                        );
                    }
                } else {
                    ui.painter().with_clip_rect(area).image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
                        Color32::WHITE,
                    );
                }
            } else {
                ui.painter().text(
                    area.center(),
                    egui::Align2::CENTER_CENTER,
                    "正在后台加载 RAW / XMP…",
                    egui::FontId::proportional(16.),
                    Color32::GRAY,
                );
            }
        });
        // Quality and view scale are independent: replacing the 1600px preview
        // with full detail must not move the image or change the displayed zoom.
        if self.view.needs_full()
            && !self.full
            && self.ready
            && !self.saving
            && self.crop_editor.is_none()
        {
            self.full = true;
            if let Err(error) = self.render() {
                self.error = Some(format!("{error:#}"));
            }
        }
        let crop_result = match crop_action {
            crate::crop_ui::Action::Apply => self.finish_crop(true),
            crate::crop_ui::Action::Cancel => self.finish_crop(false),
            crate::crop_ui::Action::None => Ok(()),
        };
        if let Err(error) = crop_result {
            self.error = Some(format!("{error:#}"));
        }
        if self
            .changed_at
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(250))
            && !ctx.input(|i| i.pointer.primary_down())
            && !self.saving
            && let Err(e) = self.render()
        {
            self.error = Some(format!("{e:#}"));
            self.changed_at = None;
        }
        if self.rendering || self.saving || self.preparing || self.changed_at.is_some() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        if let Some(error) = self.preview.as_ref().and_then(|p| p.error()) {
            self.error = Some(format!("显影预览失败：{error}"));
        }
        self.smoke(ctx);
        action
    }
    fn smoke(&mut self, ctx: &egui::Context) {
        let Some(directory) = std::env::var_os("FASTCULL_RAW_EDIT_SMOKE_DIR").map(PathBuf::from)
        else {
            return;
        };
        fs::create_dir_all(&directory).unwrap();
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot {
                image, user_data, ..
            } = event
                && let Some(name) = user_data
                    .data
                    .and_then(|v| v.downcast_ref::<String>().cloned())
            {
                let file = fs::File::create(directory.join(name)).unwrap();
                let mut png = png::Encoder::new(file, image.size[0] as u32, image.size[1] as u32);
                png.set_color(png::ColorType::Rgba);
                png.set_depth(png::BitDepth::Eight);
                let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                png.write_header().unwrap().write_image_data(&rgba).unwrap();
            }
        }
        if let Some(error) = &self.error {
            fs::write(directory.join("error.txt"), error).unwrap();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.smoke_at.elapsed() > Duration::from_secs(180) {
            fs::write(directory.join("error.txt"), "RAW editor smoke timeout").unwrap();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.texture.is_none()
            || self.rendering
            || self.saving
            || self.preparing
            || self.changed_at.is_some()
        {
            return;
        }
        let request = |name: &str| {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                name.to_owned(),
            )))
        };
        match self.smoke_step {
            0 => {
                request("00-loaded.png");
                self.smoke_step = 1;
                self.smoke_at = Instant::now();
            }
            1 if directory.join("00-loaded.png").is_file() => {
                // Re-request the exact same recipe to measure a real cache hit.
                self.render().unwrap();
                self.smoke_step = 8;
            }
            8 => {
                self.exposure = (self.exposure + 0.5).min(5.);
                self.balance = Balance::Custom;
                self.basic = BasicAdjustments {
                    contrast: Some(20),
                    highlights: Some(-35),
                    shadows: Some(25),
                    whites: Some(15),
                    blacks: Some(-10),
                    vibrance: Some(20),
                    saturation: Some(10),
                };
                self.temperature = "4850".into();
                self.tint = "17".into();
                self.render().unwrap();
                self.smoke_step = 2;
            }
            2 => {
                request("01-edited.png");
                self.smoke_step = 3;
            }
            3 if directory.join("01-edited.png").is_file() => {
                self.full = true;
                self.view.set_scale(1.);
                self.render().unwrap();
                self.smoke_step = 4;
            }
            4 => {
                request("02-full.png");
                self.smoke_step = 5;
            }
            5 if directory.join("02-full.png").is_file() => {
                self.worker.save(SaveJob {
                    source: self.path.clone(),
                    source_stamp: self.source_stamp.clone(),
                    expected: self.packet.clone(),
                    base: self.render_packet.clone(),
                    prepared_source: self.render_source.clone(),
                    updates: self.updates().unwrap(),
                });
                self.saving = true;
                self.smoke_step = 6;
            }
            6 if !self.dirty() => {
                request("03-saved.png");
                self.smoke_step = 7;
            }
            7 if directory.join("03-saved.png").is_file() => {
                fs::write(
                    directory.join("result.txt"),
                    format!(
                        "saved=true exposure={} white_balance={:?} profile={:?} full={} status={} basic={:?}",
                        self.original.exposure.unwrap_or(0),
                        self.original.white_balance,
                        self.original.camera_profile,
                        self.full,
                        self.status,
                        self.original.basic
                    ),
                )
                .unwrap();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
        ctx.request_repaint_after(Duration::from_millis(80));
    }
}

fn basic_slider(
    ui: &mut egui::Ui,
    basic: &mut BasicAdjustments,
    control: BasicControl,
    range: Option<(i32, i32)>,
    label: &str,
) -> bool {
    let mut value = f64::from(basic.get(control).unwrap_or(0));
    let (min, max) = range.unwrap_or((-100, 100));
    let changed = ui
        .add_enabled_ui(range.is_some(), |ui| {
            adjustment_slider(
                ui,
                label,
                &mut value,
                f64::from(min)..=f64::from(max),
                1.,
                false,
                "",
            )
        })
        .inner;
    if changed {
        basic.set(control, Some(value.round() as i32));
    }
    changed
}

fn section_header(ui: &mut egui::Ui, label: &str, enabled: &mut bool, hint: &str) {
    ui.horizontal(|ui| {
        ui.strong(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.checkbox(enabled, "预览").on_hover_text(hint);
        });
    });
}

fn adjustment_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    step: f64,
    logarithmic: bool,
    suffix: &str,
) -> bool {
    ui.push_id(label, |ui| {
        ui.horizontal(|ui| {
            ui.add_sized([64., 22.], egui::Label::new(label));
            ui.spacing_mut().slider_width = (ui.available_width() - 66.).max(60.);
            let slider = ui
                .scope(|ui| {
                    ui.spacing_mut().interact_size.y = 22.;
                    let widgets = &mut ui.visuals_mut().widgets;
                    for widget in [
                        &mut widgets.inactive,
                        &mut widgets.hovered,
                        &mut widgets.active,
                    ] {
                        widget.bg_fill = Color32::TRANSPARENT;
                        widget.weak_bg_fill = Color32::TRANSPARENT;
                        widget.fg_stroke = egui::Stroke::NONE;
                        widget.bg_stroke = egui::Stroke::NONE;
                    }
                    ui.add(
                        egui::Slider::new(value, range.clone())
                            .step_by(step)
                            .logarithmic(logarithmic)
                            .show_value(false)
                            .handle_shape(egui::style::HandleShape::Circle)
                            .trailing_fill(false),
                    )
                })
                .inner;
            let mut changed = slider.changed();
            let radius = slider.rect.height() / 2.5;
            let rail = slider.rect.shrink2(Vec2::new(radius, 0.));
            let y = rail.center().y;
            let colors = match label {
                "色温" => [
                    Color32::from_rgb(80, 130, 205),
                    Color32::from_rgb(225, 181, 100),
                ],
                "色调" => [
                    Color32::from_rgb(92, 173, 117),
                    Color32::from_rgb(188, 112, 180),
                ],
                _ => [Color32::from_gray(85), Color32::from_gray(175)],
            };
            let alpha = if ui.is_enabled() { 1. } else { 0.4 };
            let mut mesh = egui::Mesh::default();
            for (x, color) in [(rail.left(), colors[0]), (rail.right(), colors[1])] {
                mesh.colored_vertex(egui::pos2(x, y - 1.5), color.gamma_multiply(alpha));
                mesh.colored_vertex(egui::pos2(x, y + 1.5), color.gamma_multiply(alpha));
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(1, 3, 2);
            ui.painter().add(egui::Shape::mesh(mesh));
            let (min, max) = (*range.start(), *range.end());
            let normalized = if logarithmic {
                (*value / min).ln() / (max / min).ln()
            } else {
                (*value - min) / (max - min)
            };
            let x = egui::lerp(rail.x_range(), normalized.clamp(0., 1.) as f32);
            let knob = egui::Rect::from_center_size(egui::pos2(x, y), Vec2::new(6., 12.));
            ui.painter().rect(
                knob,
                2.,
                Color32::from_gray(if slider.hovered() || slider.dragged() {
                    245
                } else {
                    205
                })
                .gamma_multiply(alpha),
                egui::Stroke::new(1., Color32::from_gray(25)),
                egui::StrokeKind::Inside,
            );
            changed |= ui
                .add_sized(
                    [54., 22.],
                    egui::DragValue::new(value)
                        .range(range)
                        .speed(step)
                        .fixed_decimals(if step < 1. { 2 } else { 0 })
                        .suffix(suffix),
                )
                .changed();
            changed
        })
        .inner
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scene_fixture(size: [usize; 2]) -> Box<fastcull::raw_color::Frame> {
        use fastcull::{
            raw_color::{Frame, GreySampler, Recipe},
            white_balance as wb,
        };
        Box::new(Frame {
            size,
            pixels: vec![[0.18, 0.18, 0.18, 1.]; size[0] * size[1]],
            recipe: Recipe {
                look: None,
                curve: vec![0., 1.],
                baseline_exposure: 0.,
            },
            grey: GreySampler {
                size: [3, 3],
                samples: vec![wb::xyz(5500., 0.).unwrap().map(|v| (v * 0.1) as f32); 9],
                uvs: [[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
                applied_mul: [1.; 3],
                native_matrix: wb::IDENTITY,
                profile_color: None,
                profile_illuminants: [2850., 6500.],
                profile_dual: false,
                calibration: [wb::IDENTITY; 2],
            },
            temperature: Some(5500.),
            tint: Some(0.),
            profile: "fixture applied profile".into(),
            warning: None,
            profile_fingerprint: 1,
            make: "fixture".into(),
            model: "fixture".into(),
            requested_profile: "fixture".into(),
        })
    }
    #[test]
    fn ordinary_wheel_zooms_the_raw_canvas() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wheel.ARW");
        fs::write(&path, b"private UI fixture").unwrap();
        let ctx = egui::Context::default();
        let mut editor = Editor::new(path, &ctx, None, None).unwrap();
        editor.source_size = Some(Vec2::new(6000., 4000.));
        editor.texture = Some(ctx.load_texture(
            "fixture",
            egui::ColorImage::filled([60, 40], Color32::GRAY),
            Default::default(),
        ));
        for frame in 0..3 {
            let mut events = vec![egui::Event::PointerMoved(egui::pos2(300., 300.))];
            if frame == 2 {
                events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0., 120.),
                    modifiers: Default::default(),
                });
            }
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1100., 800.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    editor.show(ctx);
                },
            );
            if frame < 2 {
                assert!(
                    editor.view.is_fit(),
                    "percentage control must preserve initial fit mode"
                );
            }
        }
        assert!(
            !editor.view.is_fit(),
            "ordinary wheel must enlarge RAW without holding Ctrl"
        );
        assert!(
            editor.full,
            "zooming past fit must request native detail automatically"
        );
    }
    #[test]
    fn full_preview_arrival_keeps_native_zoom_for_crop_rotation_and_dpi() {
        for ppp in [1., 1.5, 2.] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("detail.ARW");
            fs::write(&path, b"private UI fixture").unwrap();
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let mut editor = Editor::new(path, &ctx, None, None).unwrap();
            let (send, receive) = mpsc::channel();
            editor.worker.receive = receive;
            editor.full = true;
            editor.source_size = Some(Vec2::new(2400., 1600.));
            editor.draft_orientation = 6;
            editor.draft_crop = fastcull::crop::Crop {
                enabled: true,
                right: 500_000,
                ..Default::default()
            };
            editor.view.set_scale(1.);
            let mut bounds = Vec::new();
            for size in [[160, 120], [1600, 1200]] {
                send.send(Event::Scene {
                    generation: editor.generation,
                    image: Ok(scene_fixture(size)),
                    ms: 0.,
                    cached: false,
                })
                .unwrap();
                let mut result = None;
                for _ in 0..2 {
                    let output = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                Vec2::new(1100., 800.),
                            )),
                            ..Default::default()
                        },
                        |ctx| {
                            editor.show(ctx);
                        },
                    );
                    let id = editor.texture.as_ref().unwrap().id();
                    result = output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Mesh(mesh) if mesh.texture_id == id => {
                            Some(mesh.calc_bounds())
                        }
                        _ => None,
                    });
                }
                bounds.push(result.expect("actual RAW image mesh"));
            }
            assert_eq!(bounds[0], bounds[1], "texture replacement moved the image");
            assert!((bounds[1].width() * ppp - 1600.).abs() < 1.);
            assert!((bounds[1].height() * ppp - 1200.).abs() < 1.);
        }
    }
    #[test]
    fn scene_arrival_enables_sensor_picker_and_keeps_stale_results_out() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scene.ARW");
        fs::write(&path, b"private UI fixture").unwrap();
        let ctx = egui::Context::default();
        let mut editor = Editor::new(path, &ctx, None, None).unwrap();
        let (send, receive) = mpsc::channel();
        editor.worker.receive = receive;
        editor.generation = 4;
        send.send(Event::Dimensions {
            size: Vec2::new(6000., 4000.),
            orientation: 6,
        })
        .unwrap();
        send.send(Event::Scene {
            generation: 3,
            image: Ok(scene_fixture([6, 4])),
            ms: 0.,
            cached: true,
        })
        .unwrap();
        let _ = ctx.run(Default::default(), |ctx| {
            editor.show(ctx);
        });
        assert!(!editor.confirmed);
        assert_eq!(editor.source_size, Some(Vec2::new(6000., 4000.)));
        send.send(Event::Scene {
            generation: 4,
            image: Ok(scene_fixture([6, 4])),
            ms: 0.,
            cached: true,
        })
        .unwrap();
        let _ = ctx.run(Default::default(), |ctx| {
            editor.show(ctx);
        });
        assert!(editor.confirmed && editor.sample.is_none());
        assert!(
            editor
                .grey_sampler
                .as_ref()
                .unwrap()
                .estimate([0.5, 0.5])
                .is_ok()
        );
        assert_eq!(editor.profile_status, "fixture applied profile");
        assert!(editor.histogram.is_some());
        assert_eq!(editor.draft_orientation, 6);
        assert_eq!(editor.source_size, Some(Vec2::new(6000., 4000.)));
        let generation = editor.generation;
        editor.exposure = 1.25;
        assert_eq!(editor.preview_settings().exposure, 1.25);
        assert_eq!(editor.generation, generation);
        assert_eq!(editor.updates().unwrap().exposure, Some(1_250_000));
    }
    #[test]
    fn auto_save_commits_unapplied_crop_rating_and_exposure_before_becoming_clean() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.ARW");
        fs::write(&path, b"private metadata fixture").unwrap();
        let ctx = egui::Context::default();
        let mut editor = Editor::new(path.clone(), &ctx, None, None).unwrap();
        editor.source_size = Some(Vec2::new(600., 400.));
        editor.rating = 4;
        editor.exposure = 0.65;
        editor.crop_editor = Some(crate::crop_ui::Editor::new(
            Default::default(),
            Vec2::new(600., 400.),
            1,
        ));
        editor
            .crop_editor
            .as_mut()
            .unwrap()
            .select_ratio("竖向".into(), Some(2. / 3.));
        let crop = editor.crop_editor.as_ref().unwrap().value();
        assert!(!editor.auto_save().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saved = None;
        while saved.is_none() {
            let _ = ctx.run(Default::default(), |ctx| {
                if let Action::Saved(value) = editor.show(ctx) {
                    saved = Some(value);
                }
            });
            assert!(
                Instant::now() < deadline,
                "auto-save did not finish: {:?}",
                editor.error
            );
            thread::sleep(Duration::from_millis(5));
        }
        let (_, updates) = *saved.unwrap();
        assert_eq!(updates.rating, Some(4));
        assert_eq!(updates.crop, Some(crop));
        assert_eq!(updates.exposure, Some(650_000));
        let values = xmp::read(&path).unwrap();
        assert_eq!(values.crop, Some(crop));
        assert_eq!(values.rating, Some(4));
        assert_eq!(values.exposure, Some(650_000));
        assert!(
            editor.auto_save().unwrap(),
            "only a saved clean editor may close"
        );
    }
    #[test]
    fn auto_save_conflict_keeps_the_draft_and_external_sidecar() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.ARW");
        fs::write(&path, b"private metadata fixture").unwrap();
        let ctx = egui::Context::default();
        let mut editor = Editor::new(path.clone(), &ctx, None, None).unwrap();
        editor.rating = 5;
        xmp::write(
            &path,
            &Updates {
                rating: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!editor.auto_save().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while editor.save_error.is_none() {
            let _ = ctx.run(Default::default(), |ctx| {
                editor.show(ctx);
            });
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert!(editor.auto_save().is_err());
        assert_eq!(editor.rating, 5);
        assert!(editor.dirty());
        assert_eq!(xmp::read(&path).unwrap().rating, Some(2));
    }
    #[test]
    fn adjustment_sliders_settle_after_pointer_release_without_rerendering() {
        let ctx = egui::Context::default();
        let (mut exposure, mut temperature, mut tint) = (1.2000000000000002, 5325., 5.);
        for frame in 0..4 {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let changed =
                        adjustment_slider(ui, "曝光", &mut exposure, -5.0..=5.0, 0.01, false, "")
                            | adjustment_slider(
                                ui,
                                "色温",
                                &mut temperature,
                                2000.0..=50000.0,
                                1.,
                                true,
                                "",
                            )
                            | adjustment_slider(
                                ui,
                                "色调",
                                &mut tint,
                                -150.0..=150.0,
                                1.,
                                false,
                                "",
                            );
                    assert!(
                        !changed,
                        "idle frame {frame} must not restart RAW rendering"
                    );
                });
            });
        }
    }
    #[test]
    fn source_identity_and_xmp_changes_do_not_share_preview_cache() {
        let s = Stamp {
            len: 10,
            modified: SystemTime::UNIX_EPOCH,
        };
        let a = cache_file(Path::new("a.ARW"), &s, "packet", 2560, 0);
        assert_ne!(a, cache_file(Path::new("a.ARW"), &s, "edited", 2560, 0));
        assert_ne!(a, cache_file(Path::new("a.ARW"), &s, "packet", 0, 0));
        assert_ne!(a, cache_file(Path::new("b.ARW"), &s, "packet", 2560, 0));
    }
    #[test]
    fn rejected_external_changes_remain_rejected_on_retry() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("a.arw");
        fs::write(&raw, b"raw").unwrap();
        let original = xmp::patch(
            &xmp::template(),
            &Updates {
                rating: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let external = xmp::patch(
            &original,
            &Updates {
                rating: Some(5),
                ..Default::default()
            },
        )
        .unwrap();
        fs::write(raw.with_extension("xmp"), &external).unwrap();
        for _ in 0..2 {
            assert!(
                write_packet(
                    &raw,
                    Some(&original),
                    &original,
                    &Updates {
                        exposure: Some(500_000),
                        ..Default::default()
                    }
                )
                .is_err()
            );
            assert_eq!(
                fs::read_to_string(raw.with_extension("xmp")).unwrap(),
                external
            );
        }
    }
    #[test]
    fn lr_recipe_save_preserves_source_selection_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("a.nef");
        fs::write(&raw, b"raw").unwrap();
        let source = xmp::patch(
            &xmp::template(),
            &Updates {
                rating: Some(5),
                flag: Some("pick".into()),
                ..Default::default()
            },
        )
        .unwrap();
        fs::write(raw.with_extension("xmp"), &source).unwrap();
        let catalog = xmp::patch(
            &xmp::template(),
            &Updates {
                rating: Some(1),
                exposure: Some(1_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        write_packet(
            &raw,
            Some(&source),
            &catalog,
            &Updates {
                exposure: Some(1_500_000),
                ..Default::default()
            },
        )
        .unwrap();
        let values = xmp::read(&raw).unwrap();
        assert_eq!(values.rating, Some(5));
        assert_eq!(values.flag.as_deref(), Some("pick"));
        assert_eq!(values.exposure, Some(1_500_000));
        assert_eq!(fs::read(raw).unwrap(), b"raw");
    }
    #[test]
    fn relative_lr_resources_are_preserved_without_overwriting_existing_assets() {
        let directory = tempfile::tempdir().unwrap();
        let prepared = directory.path().join("export");
        let source = directory.path().join("photos");
        fs::create_dir_all(prepared.join("masks")).unwrap();
        fs::create_dir(&source).unwrap();
        fs::write(prepared.join(".fastcull-adobe-export"), b"").unwrap();
        let raw = prepared.join("a.dng");
        fs::write(&raw, b"raw").unwrap();
        fs::write(prepared.join("a.xmp"), b"xmp").unwrap();
        let asset = vec![27_u8; 131_073];
        fs::write(prepared.join("masks/a.acr"), &asset).unwrap();
        let original = source.join("a.dng");
        fs::write(&original, b"original").unwrap();
        preserve_resources(&raw, &original).unwrap();
        preserve_resources(&raw, &original).unwrap();
        assert_eq!(fs::read(source.join("masks/a.acr")).unwrap(), asset);
        assert_eq!(fs::read(&original).unwrap(), b"original");
        assert!(!source.join("a.xmp").exists());
        fs::write(prepared.join("masks/a.acr"), b"different").unwrap();
        assert!(preserve_resources(&raw, &original).is_err());
        assert_eq!(fs::read(source.join("masks/a.acr")).unwrap(), asset);
    }
    #[test]
    fn preview_budget_leaves_unrelated_files_and_current_image() {
        let directory = tempfile::tempdir().unwrap();
        prepare_preview_cache(directory.path()).unwrap();
        let keep = directory.path().join("0000000000000000.png");
        for i in 0..66 {
            fs::write(directory.path().join(format!("{i:016x}.png")), b"png").unwrap();
        }
        fs::write(directory.path().join("user-photo.png"), b"photo").unwrap();
        trim_previews(directory.path(), &keep);
        assert!(keep.is_file());
        assert_eq!(
            fs::read(directory.path().join("user-photo.png")).unwrap(),
            b"photo"
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 66);
    }
}
