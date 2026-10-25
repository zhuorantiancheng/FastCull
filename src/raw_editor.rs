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
        .unwrap_or_else(|| platform::data_directory().join("raw-editor/local-cache-v2"))
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
        "libraw-local-v2",
        env!("CARGO_PKG_VERSION"),
        path,
        source,
        packet,
        edge,
        nonce,
    )
        .hash(&mut h);
    cache_directory().join(format!("{:016x}.png", h.finish()))
}
fn decode_png(path: &Path) -> Result<egui::ColorImage> {
    let mut decoder = png::Decoder::new(BufReader::new(fs::File::open(path)?));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let size = reader
        .output_buffer_size()
        .context("PNG dimensions overflow")?;
    ensure!(size <= 640_000_000, "RAW preview exceeds memory limit");
    let mut b = vec![0; size];
    let info = reader.next_frame(&mut b)?;
    let data = &b[..info.buffer_size()];
    let dimensions = [info.width as usize, info.height as usize];
    let image = match info.color_type {
        png::ColorType::Rgba => egui::ColorImage::from_rgba_unmultiplied(dimensions, data),
        png::ColorType::Rgb => egui::ColorImage::from_rgb(dimensions, data),
        _ => anyhow::bail!("RAW preview must be RGB/sRGB"),
    };
    Ok(image)
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
        if path.extension().is_none_or(|s| s != "png")
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
    exposure: f64,
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
    Quick {
        image: egui::ColorImage,
        ms: f64,
    },
    Preview {
        generation: u64,
        image: Result<egui::ColorImage, String>,
        ms: f64,
        cached: bool,
        exposure: f64,
        temperature: Option<f64>,
        tint: Option<f64>,
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
                            let quick = (|| -> Result<egui::ColorImage> {
                                let info = fastcull::arw::ArwInfo::read(&path)?;
                                let pixels = fastcull::imaging::decode_level(
                                    &path,
                                    &info,
                                    fastcull::imaging::Level::Fit(1280),
                                )?;
                                let pixels = fastcull::imaging::orient(pixels, info.orientation);
                                Ok(egui::ColorImage::from_rgba_premultiplied(
                                    [pixels.width, pixels.height],
                                    &pixels.rgba,
                                ))
                            })();
                            match quick {
                                Ok(image) => Event::Quick {
                                    image,
                                    ms: quick_started.elapsed().as_secs_f64() * 1000.,
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
                            let mut cached = output.is_file();
                            let mut effective = (None, None);
                            if let Ok(text) = fs::read_to_string(output.with_extension("wb")) {
                                let mut values = text.split_whitespace();
                                effective = (
                                    values.next().and_then(|v| v.parse::<f64>().ok()),
                                    values.next().and_then(|v| v.parse::<f64>().ok()),
                                );
                            }
                            let result = (|| -> Result<egui::ColorImage> {
                                ensure!(
                                    stamp(&job.source)? == job.source_stamp,
                                    "RAW 已被其他程序修改，请重新加载"
                                );
                                let existing = if cached {
                                    decode_png(&output).ok()
                                } else {
                                    None
                                };
                                let image = if let Some(image) = existing {
                                    image
                                } else {
                                    cached = false;
                                    prepare_preview_cache(output.parent().unwrap())?;
                                    let temporary = tempfile::Builder::new()
                                        .prefix(".render-")
                                        .suffix(".png")
                                        .tempfile_in(output.parent().unwrap())?
                                        .into_temp_path();
                                    let rendered = local_raw::render(&local_raw::Request {
                                        source: job.source.clone(),
                                        xmp: job.packet,
                                        output: temporary.to_path_buf(),
                                        max_edge: job.edge,
                                    })?;
                                    effective = (rendered.temperature, rendered.tint);
                                    let _ = fs::write(
                                        output.with_extension("wb"),
                                        format!(
                                            "{} {}",
                                            effective.0.map(|v| v.to_string()).unwrap_or_default(),
                                            effective.1.map(|v| v.to_string()).unwrap_or_default()
                                        ),
                                    );
                                    let image = decode_png(&temporary)?;
                                    temporary.persist(&output).map_err(|e| e.error)?;
                                    image
                                };
                                ensure!(
                                    g.load(Ordering::Relaxed) == job.generation,
                                    "superseded preview"
                                );
                                trim_previews(output.parent().unwrap(), &output);
                                Ok(image)
                            })()
                            .map_err(|e| format!("{e:#}"));
                            if g.load(Ordering::Relaxed) != job.generation {
                                continue;
                            }
                            Event::Preview {
                                generation: job.generation,
                                image: result,
                                ms: started.elapsed().as_secs_f64() * 1000.,
                                cached,
                                exposure: job.exposure,
                                temperature: effective.0,
                                tint: effective.1,
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
    picking: bool,
    effective_wb: Option<WhiteBalance>,
    balance: Balance,
    temperature: String,
    tint: String,
    worker: Worker,
    generation: u64,
    texture: Option<TextureHandle>,
    changed_at: Option<Instant>,
    error: Option<String>,
    status: String,
    rendering: bool,
    saving: bool,
    full: bool,
    zoom: f32,
    pan: Vec2,
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
            picking: false,
            effective_wb: original.white_balance.as_ref().and_then(|w| w.edit()),
            source_orientation: original.orientation,
            original,
            balance: Balance::Original,
            temperature,
            tint,
            worker: Worker::new(ctx.clone()),
            generation: 0,
            texture: None,
            changed_at: None,
            error: None,
            status: "正在加载本地 RAW 显影…".into(),
            rendered_exposure: initial_exposure,
            rendering: false,
            saving: false,
            full: false,
            zoom: 1.,
            pan: Vec2::ZERO,
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
            exposure,
            basic,
            white_balance,
            // Bring an unsaved LR rotation along with its develop recipe only
            // when the source has no explicit, potentially newer orientation.
            orientation: if self.source_orientation.is_none()
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
    pub fn saving(&self) -> bool {
        self.saving
    }
    fn render(&mut self) -> Result<()> {
        let mut updates = self.updates()?;
        updates.basic = BasicAdjustments::default();
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
            exposure: self.exposure,
        });
        self.rendering = true;
        self.confirmed = false;
        self.changed_at = None;
        self.error = None;
        self.status = "本地 RAW 显影中…".into();
        Ok(())
    }
    pub fn show(&mut self, ctx: &egui::Context) -> Action {
        let mut action = Action::None;
        while let Ok(event) = self.worker.receive.try_recv() {
            match event {
                Event::Quick { image, ms } => {
                    if let Some(dir) = std::env::var_os("FASTCULL_RAW_EDIT_SMOKE_DIR") {
                        let _ = fs::create_dir_all(&dir);
                        let _ =
                            fs::write(PathBuf::from(dir).join("quick-ms.txt"), format!("{ms:.3}"));
                    }
                    if self.texture.is_none() {
                        self.sample = crate::raw_sample::SampleMap::from_image(&image).ok();
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
                Event::Preview {
                    generation,
                    image,
                    ms,
                    cached,
                    exposure,
                    temperature,
                    tint,
                } if generation == self.generation => {
                    self.rendering = false;
                    match image {
                        Ok(image) => {
                            self.sample = crate::raw_sample::SampleMap::from_image(&image).ok();
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
                                        self.full, image.size[0], image.size[1]
                                    );
                                }
                            }
                            self.texture = Some(ctx.load_texture(
                                format!("raw-edit-{generation}"),
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                            if let (Some(t), Some(tint)) = (temperature, tint) {
                                let t = t.round() as u32;
                                let tint = tint.round() as i32;
                                let value = WhiteBalance::Custom {
                                    temperature: t,
                                    tint,
                                };
                                if value.valid() {
                                    self.effective_wb = Some(value);
                                    if self.balance != Balance::Custom {
                                        self.temperature = t.to_string();
                                        self.tint = tint.to_string();
                                    }
                                }
                            }
                            self.rendered_exposure = exposure;
                            self.confirmed = true;
                            self.status = format!(
                                "sRGB · {} · {:.0} ms",
                                if cached {
                                    "缓存"
                                } else {
                                    "LibRaw 本地显影"
                                },
                                ms
                            );
                            self.error = None;
                        }
                        Err(e) => self.error = Some(e),
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
                            self.source_orientation = values.orientation;
                            self.original = values.clone();
                            self.balance = Balance::Original;
                            self.render_packet = self.packet.clone().unwrap_or_else(xmp::template);
                            self.status = "已保存显影调整 · sRGB".into();
                            self.error = None;
                            action = Action::Saved(Box::new((values, updates)));
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                _ => {}
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
        egui::SidePanel::right("raw-edit-controls").exact_width(310.).show(ctx,|ui|{
            egui::ScrollArea::vertical().show(ui,|ui|{
                ui.heading("RAW 显影");ui.weak("本地开源引擎 · sRGB");
                ui.label("显影配置：相机色彩信息 → sRGB");
                if let Some(profile)=&self.original.camera_profile {ui.weak(format!("XMP 配置文件（保留）：{profile}"));}
                if let Some(version)=&self.original.process_version {ui.weak(format!("XMP 处理版本：{version}"));}
                ui.separator();
                let (mut native_changed,mut changed)=(false,false);
                ui.add_enabled_ui(!self.saving && self.ready,|ui|{
                    ui.horizontal(|ui|{ui.strong("亮");ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{ui.checkbox(&mut self.enable_light,"预览").on_hover_text("临时查看亮度调整前的画面，不改保存参数");});});
                    ui.label("曝光");
                    match xmp::exposure_range(&self.render_packet) {
                        Ok(range)=>{native_changed|=ui.add(egui::Slider::new(&mut self.exposure,f64::from(*range.start())/1e6..=f64::from(*range.end())/1e6).step_by(0.01).suffix(" EV")).changed();}
                        Err(error)=>{ui.weak(format!("无法编辑曝光：{error}"));}
                    }
                    for (index,label) in ["对比度","高光","阴影","白色","黑色"].iter().enumerate() {
                        changed|=basic_slider(ui,&mut self.basic,BasicControl::ALL[index],self.basic_ranges[index],label);
                    }
                    ui.separator();
                    ui.horizontal(|ui|{ui.strong("颜色");ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{ui.checkbox(&mut self.enable_color,"预览").on_hover_text("临时查看饱和度调整前的画面；白平衡以 RAW 显影为准，不改保存参数");});});
                    ui.horizontal(|ui|{
                        ui.label("白平衡");
                        if ui.add_enabled(self.confirmed && self.sample.is_some(),egui::Button::new(if self.picking {"取消取样"}else{"灰点吸管"})).on_hover_text("点击图中应为中性灰或白的区域估算白平衡，再用本地 RAW 重新显影；可继续微调").clicked(){self.picking = !self.picking;}
                    });
                    egui::ComboBox::from_id_salt("raw-wb").selected_text(match self.balance {Balance::Original=>"保留原设置",Balance::AsShot=>"原片",Balance::Custom=>"自定义"}).show_ui(ui,|ui|{
                        native_changed|=ui.selectable_value(&mut self.balance,Balance::Original,"保留原设置").changed();
                        native_changed|=ui.selectable_value(&mut self.balance,Balance::AsShot,"原片").changed();
                        native_changed|=ui.selectable_value(&mut self.balance,Balance::Custom,"自定义").changed();
                    });
                    if self.balance==Balance::Custom {
                        if let Ok(mut value)=self.temperature.parse::<u32>() {
                            ui.label("色温");if ui.add(egui::Slider::new(&mut value,2000..=50000).logarithmic(true).suffix(" K")).changed(){self.temperature=value.to_string();native_changed=true;}
                        }else{ui.horizontal(|ui|{ui.label("色温 K");native_changed|=ui.text_edit_singleline(&mut self.temperature).changed();});}
                        if let Ok(mut value)=self.tint.parse::<i32>() {
                            ui.label("色调");if ui.add(egui::Slider::new(&mut value,-150..=150)).changed(){self.tint=value.to_string();native_changed=true;}
                        }else{ui.horizontal(|ui|{ui.label("色调");native_changed|=ui.text_edit_singleline(&mut self.tint).changed();});}
                    }else{
                        if let Some(wb)=&self.original.white_balance {ui.weak(format!("原设置：{}",wb.mode.as_deref().unwrap_or("原片")));}
                        if !self.temperature.is_empty(){ui.weak(format!("色温 {} K · 色调 {}",self.temperature,self.tint));}
                    }
                    for (index,label) in ["自然饱和度","饱和度"].iter().enumerate(){let c=index+5;changed|=basic_slider(ui,&mut self.basic,BasicControl::ALL[c],self.basic_ranges[c],label);}
                    ui.separator();
                    if ui.button(if self.full {"适屏预览"}else{"加载全尺寸 / 100%"}).clicked(){self.full = !self.full;self.zoom=1.;self.pan=Vec2::ZERO;if let Err(e)=self.render(){self.error=Some(format!("{e:#}"));}}
                    let valid=self.updates();if let Err(error)=&valid {ui.colored_label(Color32::LIGHT_RED,error.to_string());}
                    if ui.add_enabled(valid.is_ok() && self.dirty() && self.confirmed && !self.rendering && self.changed_at.is_none(),egui::Button::new("保存到 XMP")).clicked(){
                        self.worker.save(SaveJob{source:self.path.clone(),source_stamp:self.source_stamp.clone(),expected:self.packet.clone(),base:self.render_packet.clone(),prepared_source:self.render_source.clone(),updates:valid.unwrap()});self.saving=true;self.status="正在保存 XMP…".into();
                    }
                    if native_changed {self.changed_at=Some(Instant::now());self.generation=self.worker.generation.fetch_add(1,Ordering::Relaxed)+1;self.confirmed=false;self.status="实时预览 · 后台更新 RAW".into();}
                    if changed {self.error=None;}
                });
                if ui.add_enabled(!self.saving,egui::Button::new("重新读取 RAW / XMP（丢弃草稿）")).clicked(){action=Action::Reload;}
                if ui.add_enabled(!self.saving,egui::Button::new(if self.dirty(){"丢弃未保存调整并关闭"}else{"关闭编辑窗口"})).clicked(){action=Action::Cancel;}
                if let Some(error)=&self.error {ui.separator();ui.colored_label(Color32::LIGHT_RED,error);}
                ui.weak("独立窗口，主选片器可继续操作");
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            let (area, response) =
                ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
            ui.painter().rect_filled(area, 0., Color32::from_gray(18));
            if let Some(texture) = &self.texture {
                let source = texture.size_vec2();
                let fit = (area.width() / source.x).min(area.height() / source.y);
                let scale = if self.full {
                    1. / ctx.pixels_per_point()
                } else {
                    fit
                } * self.zoom;
                if response.dragged() {
                    self.pan += response.drag_delta();
                }
                if response.double_clicked() {
                    self.pan = Vec2::ZERO;
                    self.zoom = 1.;
                }
                if response.hovered() {
                    let factor = ctx.input(|i| i.zoom_delta());
                    if factor != 1. {
                        self.zoom = (self.zoom * factor).clamp(0.1, 8.);
                    }
                }
                let rect = egui::Rect::from_center_size(area.center() + self.pan, source * scale);
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
                let exposure = if self.enable_light {
                    self.exposure
                } else {
                    self.original.exposure.unwrap_or(0) as f64 / 1e6
                };
                let mut basic = [0.; 7];
                for (i, c) in BasicControl::ALL.into_iter().enumerate() {
                    basic[i] = if (i < 5 && self.enable_light) || (i >= 5 && self.enable_color) {
                        self.basic.get(c).unwrap_or(0) as f32 / 100.
                    } else {
                        self.original.basic.get(c).unwrap_or(0) as f32 / 100.
                    };
                }
                if let Some(preview) = &self.preview {
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
                        crate::raw_preview::Settings {
                            exposure: (exposure - self.rendered_exposure) as f32,
                            basic,
                            wb: [1., 1., 1.],
                        },
                    );
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
    let mut value = basic.get(control).unwrap_or(0);
    if let Some((min, max)) = range {
        if ui
            .add(egui::Slider::new(&mut value, min..=max).text(label))
            .changed()
        {
            basic.set(control, Some(value));
            return true;
        }
    } else {
        ui.add_enabled(false, egui::Slider::new(&mut value, -100..=100).text(label))
            .on_hover_text("当前处理版本不支持此项；保留原设置");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
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
