//! Exact Adobe Camera Raw rendering through an installed Photoshop on Windows.
//!
//! Rendering only writes cached copies. `embed_xmp` is a separate, explicit save
//! operation for DNG metadata; it does not decode/re-encode the raw image planes.
#[cfg(any(windows, test))]
use anyhow::Context;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

pub struct Request {
    pub source: PathBuf,
    pub xmp: String,
    pub output: PathBuf,
    /// Maximum output edge; zero retains Adobe's full rendered dimensions.
    pub max_edge: u32,
}

pub struct Rendered {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub profile: String,
    pub timings: Timings,
    /// Effective values actually emitted by Adobe, including As Shot when the
    /// host supplies them. Absence is preserved; no 5500K/zero-tint fallback.
    pub temperature: Option<f64>,
    pub tint: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Timings {
    pub copy_ms: f64,
    pub metadata_ms: f64,
    pub raw_open_ms: f64,
    pub profile_ms: f64,
    pub resize_ms: f64,
    pub write_ms: f64,
    pub wall_ms: f64,
}

static CANCELLATION_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Stops waiting for this process's active preview. Adobe's synchronous RAW open
/// cannot be interrupted safely; its private worker retains the global lock and
/// finishes/cleans up in the background. Explicit metadata saves are unaffected.
pub fn cancel_pending() {
    CANCELLATION_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

/// Uses the installed Adobe renderer and its profiles, masks, lens corrections,
/// process version and crop. Never apply the original crop again to this output.
pub fn render(request: &Request) -> Result<Rendered> {
    #[cfg(windows)]
    {
        windows::render(request)
    }
    #[cfg(not(windows))]
    {
        let _ = request;
        bail!("Adobe RAW 编辑目前需要 Windows、Photoshop 和 Camera Raw。")
    }
}

/// Explicitly saves a complete XMP packet into a DNG with Adobe's safe-update
/// metadata handler. Call only after the user requests saving, never for preview.
pub fn embed_xmp(source: &Path, packet: &str) -> Result<()> {
    if !source
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("dng"))
    {
        bail!("仅 DNG 使用内嵌 XMP 保存；ARW/NEF 应保存到 XMP 旁车。");
    }
    #[cfg(windows)]
    {
        windows::embed_xmp(source, packet)
    }
    #[cfg(not(windows))]
    {
        let _ = packet;
        bail!("DNG Adobe XMP 保存目前需要 Windows 和 Photoshop。")
    }
}

// JSON string syntax is also valid ExtendScript syntax, including surrogate
// escapes. Keep command data out of the shell and pass it over a UTF-8 pipe.
#[cfg(any(windows, test))]
fn quote(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 2);
    result.push('"');
    for ch in value.chars() {
        match ch {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\u{2028}' => result.push_str("\\u2028"),
            '\u{2029}' => result.push_str("\\u2029"),
            c if c < ' ' => {
                use std::fmt::Write;
                let _ = write!(result, "\\u{:04x}", c as u32);
            }
            c => result.push(c),
        }
    }
    result.push('"');
    result
}

/// Photoshop's PNG DOM save omits the profile marker. Pixels have already been
/// converted to the standard sRGB profile, so identify them with PNG's sRGB chunk
/// without another decode, resize, quantization, or compression pass.
#[cfg(any(windows, test))]
fn mark_png_srgb(path: &Path) -> Result<()> {
    let png = std::fs::read(path)?;
    if png.len() < 33 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        bail!("Adobe 输出不是有效 PNG");
    }
    let mut offset = 8;
    while offset + 12 <= png.len() {
        let size = u32::from_be_bytes(png[offset..offset + 4].try_into()?) as usize;
        let kind = &png[offset + 4..offset + 8];
        if kind == b"sRGB" || kind == b"iCCP" {
            return Ok(());
        }
        if kind == b"IDAT" {
            break;
        }
        offset = offset
            .checked_add(size)
            .and_then(|v| v.checked_add(12))
            .context("Invalid PNG chunk length")?;
    }
    // Rendering intent 1 = relative colorimetric, the conversion used above.
    let payload = b"sRGB\x01";
    let mut crc = !0_u32;
    for byte in payload {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    let mut tagged = Vec::with_capacity(png.len() + 13);
    tagged.extend_from_slice(&png[..33]);
    tagged.extend_from_slice(&1_u32.to_be_bytes());
    tagged.extend_from_slice(payload);
    tagged.extend_from_slice(&(!crc).to_be_bytes());
    tagged.extend_from_slice(&png[33..]);
    std::fs::write(path, tagged)?;
    Ok(())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        collections::{HashMap, hash_map::DefaultHasher},
        fs,
        hash::{Hash, Hasher},
        io::{BufRead, BufReader, Write},
        os::windows::process::CommandExt,
        process::{Child, ChildStdin, Command, Stdio},
        sync::{Mutex, OnceLock, mpsc},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    const WORKER: &str = include_str!("../scripts/adobe-raw-render.ps1");
    const JAVASCRIPT: &str = include_str!("../bridge/FastCullRawRender.jsx");
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    // Retain recent work copies without allowing an entire browsing session to
    // accumulate indefinitely. The active image is always kept, even if it alone
    // exceeds the budget; in that case all other eligible entries are removed.
    const CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    const CACHE_ENTRIES: usize = 16;
    const CACHE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
    const CACHE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
    const CACHE_MARKER: &str = ".fastcull-adobe-cache-v1";
    const CACHE_SIGNATURE: &str = "FastCull private Adobe RAW cache v1\n";
    const ENTRY_MARKER: &str = ".fastcull-adobe-work-v1";
    const ENTRY_SIGNATURE: &str = "FastCull private Adobe RAW work copy v1\n";
    const LAST_USED: &str = ".last-used";
    static WORKER_PROCESS: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();

    struct Worker {
        child: Child,
        input: ChildStdin,
        output: mpsc::Receiver<std::io::Result<String>>,
        cache_sweeps: HashMap<PathBuf, (PathBuf, Instant)>,
        directory: PathBuf,
        in_flight: bool,
        cancel_file: Option<PathBuf>,
        pending_lease: Option<fs::File>,
        pending_entry: Option<PathBuf>,
        first_call: bool,
        broken: bool,
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            // An in-flight COM call must retain its cross-process mutex until
            // Photoshop really returns. Closing stdin lets that worker exit
            // afterward; killing it now would falsely release the mutex.
            if !self.in_flight {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    fn powershell() -> PathBuf {
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            let path = PathBuf::from(program_files).join("PowerShell/7/pwsh.exe");
            if path.is_file() {
                return path;
            }
        }
        PathBuf::from("pwsh.exe")
    }

    impl Worker {
        fn start(directory: &Path, cancel: Option<u64>) -> Result<Self> {
            fs::create_dir_all(directory)?;
            let script = directory.join("adobe-raw-render.ps1");
            if fs::read_to_string(&script).ok().as_deref() != Some(WORKER) {
                fs::write(&script, WORKER)?;
            }
            let mut child = Command::new(powershell())
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-STA",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(&script)
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .context("启动 Adobe RAW 后端失败：需要 PowerShell 7、Photoshop 和 Camera Raw。")?;
            let input = child
                .stdin
                .take()
                .context("Adobe worker stdin unavailable")?;
            let stdout = child
                .stdout
                .take()
                .context("Adobe worker stdout unavailable")?;
            let (sender, output) = mpsc::channel();
            std::thread::Builder::new()
                .name("adobe-raw-output".into())
                .spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        let failed = line.is_err();
                        if sender.send(line).is_err() || failed {
                            break;
                        }
                    }
                })?;
            let worker = Self {
                child,
                input,
                output,
                cache_sweeps: HashMap::new(),
                directory: directory.to_path_buf(),
                in_flight: false,
                cancel_file: None,
                pending_lease: None,
                pending_entry: None,
                first_call: true,
                broken: false,
            };
            // Do not write a >4KiB script into an unready anonymous pipe: that
            // can block outside the reply timeout while PowerShell is starting.
            let ready = wait_reply(&worker.output, Duration::from_secs(5), || {
                is_cancelled(cancel)
            })?;
            if ready != "READY" {
                bail!("Adobe worker 启动握手失败");
            }
            Ok(worker)
        }

        fn clear_pending(&mut self) {
            self.in_flight = false;
            self.pending_lease = None;
            self.pending_entry = None;
            if let Some(path) = self.cancel_file.take() {
                let _ = fs::remove_file(path);
            }
        }

        fn ensure_idle(&mut self) -> Result<()> {
            if self.in_flight {
                match self.output.try_recv() {
                    Ok(Ok(_)) => self.clear_pending(), // stale/cancelled result
                    Ok(Err(error)) => {
                        self.broken = true;
                        return Err(error.into());
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        bail!("Photoshop 仍在完成上次 RAW 操作；预览已取消，请稍后重试。")
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.broken = true;
                        bail!("Adobe RAW worker 已退出，请重试。");
                    }
                }
            }
            if self.child.try_wait()?.is_some() {
                self.broken = true;
                bail!("Adobe RAW worker 已退出，请重试。");
            }
            Ok(())
        }

        fn call(
            &mut self,
            javascript: &str,
            cancel: Option<u64>,
            timeout: Duration,
        ) -> Result<String> {
            self.ensure_idle()?;
            if is_cancelled(cancel) {
                bail!("已取消 Adobe RAW 预览");
            }
            static NEXT_REQUEST: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            let serial = NEXT_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let cancel_file = self
                .directory
                .join(format!("cancel-{}-{serial}.flag", std::process::id()));
            let _ = fs::remove_file(&cancel_file);
            // Photoshop's COM DoJavaScript creates a fresh script scope for each
            // call. Keep the host alive, but load the small function body anew.
            let script = format!(
                "{JAVASCRIPT}\nFastCullRaw.setCancel({});\n{javascript}",
                js_path(&cancel_file)?
            );
            writeln!(
                self.input,
                "{{\"script\":{},\"cancel\":{},\"lease\":{}}}",
                quote(&script),
                quote(cancel_file.to_str().context("Invalid Adobe cancel path")?),
                quote(
                    &self
                        .pending_entry
                        .as_ref()
                        .map(|entry| entry
                            .join(".in-use-adobe.lock")
                            .to_string_lossy()
                            .into_owned())
                        .unwrap_or_default()
                )
            )
            .context("Adobe worker pipe closed")?;
            self.input.flush()?;
            self.in_flight = true;
            self.cancel_file = Some(cancel_file.clone());
            let timeout = if self.first_call {
                timeout.max(Duration::from_secs(45))
            } else {
                timeout
            };
            self.first_call = false;
            let line = match wait_reply(&self.output, timeout, || is_cancelled(cancel)) {
                Ok(line) => {
                    self.clear_pending();
                    line
                }
                Err(error) => {
                    // Keep both worker and cross-process lock alive until the
                    // late COM result arrives. Adobe checks this flag between
                    // phases and avoids writing a cancelled preview.
                    let _ = fs::write(cancel_file, b"cancelled\n");
                    return Err(error);
                }
            };
            if let Some(error) = line.strip_prefix("ERR\t") {
                bail!("Adobe Camera Raw：{error}");
            }
            if !line.starts_with("OK") {
                bail!("Adobe RAW 后端返回无效结果：{line}");
            }
            Ok(line)
        }
    }

    fn is_cancelled(generation: Option<u64>) -> bool {
        generation.is_some_and(|g| {
            CANCELLATION_GENERATION.load(std::sync::atomic::Ordering::Acquire) != g
        })
    }

    fn wait_reply(
        receiver: &mpsc::Receiver<std::io::Result<String>>,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancelled() {
                bail!("已取消 Adobe RAW 预览；Adobe 将安全完成当前阶段。");
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!(
                    "Adobe RAW 处理超时；请检查 Photoshop/Camera Raw 的对话框后重试。后台仍在安全清理当前任务。"
                );
            }
            match receiver.recv_timeout(left.min(Duration::from_millis(40))) {
                Ok(result) => return result.map_err(Into::into),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn with_worker<T>(
        directory: &Path,
        cancel: Option<u64>,
        operation: impl FnOnce(&mut Worker) -> Result<T>,
    ) -> Result<T> {
        let lock = WORKER_PROCESS.get_or_init(|| Mutex::new(None));
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut guard = loop {
            if is_cancelled(cancel) {
                bail!("已取消 Adobe RAW 预览");
            }
            match lock.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    bail!("Adobe RAW worker lock poisoned")
                }
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() >= deadline => {
                    bail!("Adobe RAW 编辑器正在忙，请稍后重试。")
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        if guard.is_none() {
            *guard = Some(Worker::start(directory, cancel)?);
        }
        let worker = guard.as_mut().expect("worker initialized");
        let result = worker.ensure_idle().and_then(|()| operation(worker));
        if !worker.in_flight {
            worker.pending_lease = None;
            worker.pending_entry = None;
        }
        if worker.broken {
            *guard = None;
        }
        result
    }

    fn js_path(path: &Path) -> Result<String> {
        // Photoshop accepts ordinary absolute Windows paths; remove the extended
        // canonical-path prefix because ExtendScript does not understand it.
        let absolute = std::path::absolute(path)?;
        let path = absolute.to_str().context("Adobe 路径不是有效 Unicode")?;
        let normalized = path
            .strip_prefix(r"\\?\UNC\")
            .map(|p| format!(r"\\{p}"))
            .unwrap_or_else(|| path.strip_prefix(r"\\?\").unwrap_or(path).to_string());
        Ok(quote(&normalized.replace('\\', "/")))
    }

    fn source_key(path: &Path) -> Result<u64> {
        let metadata =
            fs::metadata(path).with_context(|| format!("无法读取 RAW：{}", path.display()))?;
        let mut hash = DefaultHasher::new();
        std::path::absolute(path)?.hash(&mut hash);
        metadata.len().hash(&mut hash);
        metadata.modified()?.hash(&mut hash);
        Ok(hash.finish())
    }

    fn work_copy_key(source: &Path) -> Result<u64> {
        // Different editor processes must not race while preparing packet/XMP
        // files before the Adobe host lock is acquired. Reuse within one editor
        // session remains unchanged; file copying is cheap beside RAW decoding.
        static SESSION: OnceLock<SystemTime> = OnceLock::new();
        let mut hash = DefaultHasher::new();
        source_key(source)?.hash(&mut hash);
        std::process::id().hash(&mut hash);
        SESSION.get_or_init(SystemTime::now).hash(&mut hash);
        Ok(hash.finish())
    }

    fn lease_entry(entry: &Path) -> Result<fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(entry.join(".in-use.lock"))
            .context("Adobe 工作副本仍被另一个任务占用，请稍后重试。")
    }

    fn entry_in_use(entry: &Path) -> bool {
        use std::os::windows::fs::OpenOptionsExt;
        [".in-use.lock", ".in-use-adobe.lock"].iter().any(|name| {
            let lease = entry.join(name);
            lease.exists()
                && fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .share_mode(0)
                    .open(lease)
                    .is_err()
        })
    }

    pub(super) fn render(request: &Request) -> Result<Rendered> {
        let started = Instant::now();
        let generation = CANCELLATION_GENERATION.load(std::sync::atomic::Ordering::Acquire);
        let extension = request
            .source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "arw" | "nef" | "dng") {
            bail!("Adobe RAW 编辑仅支持 ARW、NEF、DNG。");
        }
        let output_extension = request
            .output
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(output_extension.as_str(), "png" | "jpg" | "jpeg") {
            bail!("Adobe RAW 输出必须是 PNG 或 JPEG。");
        }
        let output = std::path::absolute(&request.output)?;
        let directory = output.parent().context("RAW 输出没有父目录")?;
        fs::create_dir_all(directory)?;
        // Never adopt the previous unmarked `adobe-work` directory: it could be
        // user-created. Cleanup only recognizes newly versioned, marked caches.
        let cache_root = prepare_cache_root(directory)?;
        with_worker(&directory.join("adobe-tools"), Some(generation), |worker| {
            let work_directory = prepare_cache_entry(&cache_root, work_copy_key(&request.source)?)?;
            let lease = lease_entry(&work_directory)?;
            touch_cache_entry(&work_directory, SystemTime::now())?;
            let raw_copy =
                work_directory.join(request.source.file_name().context("RAW 没有文件名")?);
            let new_copy = !raw_copy.is_file();
            let copy_started = Instant::now();
            let previously_applied = fs::read_to_string(applied_path(&work_directory)).ok();
            let reset = request.xmp.trim().is_empty()
                && previously_applied.as_deref().is_some_and(|p| !p.is_empty());
            if new_copy || reset {
                // Reuse this copy across slider updates; large RAW files are only
                // copied again when the source size or modification time changes.
                fs::copy(&request.source, &raw_copy).context("创建 Adobe RAW 工作副本失败")?;
                make_writable(&raw_copy)?;
                if reset && raw_copy.with_extension("xmp").is_file() {
                    fs::remove_file(raw_copy.with_extension("xmp"))?;
                }
            }
            copy_develop_resources(&request.source, &work_directory)?;
            let copy_ms = copy_started.elapsed().as_secs_f64() * 1000.0;
            if is_cancelled(Some(generation)) {
                bail!("已取消 Adobe RAW 预览");
            }
            let sweep_due = new_copy
                || worker
                    .cache_sweeps
                    .get(&cache_root)
                    .is_none_or(|(active, at)| {
                        active != &work_directory || at.elapsed() >= CACHE_SWEEP_INTERVAL
                    });
            if sweep_due {
                if let Err(error) = sweep_cache(
                    &cache_root,
                    &work_directory,
                    SystemTime::now(),
                    CACHE_BYTES,
                    CACHE_ENTRIES,
                    CACHE_AGE,
                ) {
                    log::warn!("Adobe work cache maintenance: {error:#}");
                }
                worker
                    .cache_sweeps
                    .insert(cache_root.clone(), (work_directory.clone(), Instant::now()));
            }
            let packet = work_directory.join("packet.xmp");
            let applied = applied_path(&work_directory);
            let changed = !request.xmp.trim().is_empty()
                && (new_copy || previously_applied.as_deref() != Some(request.xmp.as_str()));
            if changed {
                fs::write(&packet, &request.xmp)?;
                if extension != "dng" {
                    fs::write(raw_copy.with_extension("xmp"), &request.xmp)?;
                }
            }
            let command = format!(
                "FastCullRaw.render({{source:{},packet:{},output:{},maxEdge:{},embed:{}}})",
                js_path(&raw_copy)?,
                js_path(&packet)?,
                js_path(&output)?,
                request.max_edge,
                changed && extension == "dng"
            );
            worker.pending_lease = Some(lease.try_clone()?);
            worker.pending_entry = Some(work_directory.clone());
            let result = worker.call(
                &command,
                Some(generation),
                Duration::from_secs(if request.max_edge == 0 { 90 } else { 30 }),
            )?;
            if changed || reset {
                fs::write(applied, &request.xmp)?;
            }
            let fields: Vec<&str> = result.split('\t').collect();
            if fields.len() < 4 || !output.is_file() {
                bail!("Adobe RAW 没有生成有效图像：{result}");
            }
            if output_extension == "png" {
                mark_png_srgb(&output)?;
            }
            let metric = |index: usize| {
                fields
                    .get(index)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.0)
            };
            Ok(Rendered {
                path: output,
                width: fields[1].parse()?,
                height: fields[2].parse()?,
                profile: fields[3].to_owned(),
                timings: Timings {
                    copy_ms,
                    metadata_ms: metric(4),
                    raw_open_ms: metric(5),
                    profile_ms: metric(6),
                    resize_ms: metric(7),
                    write_ms: metric(8),
                    wall_ms: started.elapsed().as_secs_f64() * 1000.0,
                },
                temperature: fields
                    .get(9)
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite() && *value > 0.0),
                tint: fields
                    .get(10)
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite()),
            })
        })
    }

    fn applied_path(directory: &Path) -> PathBuf {
        directory.join("applied.xmp")
    }

    fn is_reparse(metadata: &fs::Metadata) -> bool {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }

    fn ordinary_directory(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !is_reparse(&m))
    }

    fn has_signature(path: &Path, marker: &str, signature: &str) -> bool {
        let marker = path.join(marker);
        ordinary_directory(path)
            && fs::symlink_metadata(&marker).is_ok_and(|m| m.is_file() && !is_reparse(&m))
            && fs::read_to_string(marker).ok().as_deref() == Some(signature)
    }

    fn prepare_cache_root(parent: &Path) -> Result<PathBuf> {
        let root = parent.join("adobe-work-v1");
        match fs::create_dir(&root) {
            Ok(()) => fs::write(root.join(CACHE_MARKER), CACHE_SIGNATURE)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        if !has_signature(&root, CACHE_MARKER, CACHE_SIGNATURE) {
            bail!(
                "Adobe 工作缓存目录缺少应用标记或含链接，未写入或清理：{}",
                root.display()
            );
        }
        Ok(root)
    }

    fn prepare_cache_entry(root: &Path, key: u64) -> Result<PathBuf> {
        if !has_signature(root, CACHE_MARKER, CACHE_SIGNATURE) {
            bail!("Adobe cache is not owned by FastCull");
        }
        let entry = root.join(format!("{key:016x}"));
        match fs::create_dir(&entry) {
            Ok(()) => fs::write(entry.join(ENTRY_MARKER), ENTRY_SIGNATURE)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        if !has_signature(&entry, ENTRY_MARKER, ENTRY_SIGNATURE) {
            bail!(
                "Adobe 工作副本缺少应用标记或含链接，未修改：{}",
                entry.display()
            );
        }
        Ok(entry)
    }

    fn touch_cache_entry(entry: &Path, now: SystemTime) -> Result<()> {
        fs::write(
            entry.join(LAST_USED),
            now.duration_since(UNIX_EPOCH)?.as_secs().to_string(),
        )?;
        Ok(())
    }

    fn entry_last_used(entry: &Path) -> SystemTime {
        fs::read_to_string(entry.join(LAST_USED))
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
            .or_else(|| fs::metadata(entry.join(ENTRY_MARKER)).ok()?.modified().ok())
            .unwrap_or(UNIX_EPOCH)
    }

    fn tree_size(path: &Path) -> Result<u64> {
        let metadata = fs::symlink_metadata(path)?;
        if is_reparse(&metadata) {
            bail!("Refusing linked Adobe cache item");
        }
        if metadata.is_file() {
            return Ok(metadata.len());
        }
        let mut size = 0_u64;
        for entry in fs::read_dir(path)? {
            size = size.saturating_add(tree_size(&entry?.path())?);
        }
        Ok(size)
    }

    /// Uses only validated, immediate cache children. Reparse points/junctions
    /// are refused both while inspecting and while removing each nested entry.
    /// There is deliberately no shell command or `remove_dir_all` here.
    fn remove_owned_entry(root: &Path, entry: &Path) -> Result<()> {
        if entry.parent() != Some(root)
            || !has_signature(root, CACHE_MARKER, CACHE_SIGNATURE)
            || !has_signature(entry, ENTRY_MARKER, ENTRY_SIGNATURE)
            || entry_in_use(entry)
            || fs::canonicalize(entry)?.parent() != Some(fs::canonicalize(root)?.as_path())
        {
            bail!("Refusing removal outside owned Adobe cache");
        }
        // Preflight refuses the whole entry before deleting anything if one of
        // its resources was replaced with a link.
        tree_size(entry)?;
        remove_cache_tree(entry, true)
    }

    fn remove_cache_tree(path: &Path, top_level: bool) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if is_reparse(&metadata) {
            bail!("Refusing linked Adobe cache removal");
        }
        if metadata.is_file() {
            fs::remove_file(path)?;
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            // Keep the ownership marker until every other file has gone, so a
            // locked file can be retried by a later sweep without orphaning it.
            if top_level && entry.file_name() == ENTRY_MARKER {
                continue;
            }
            remove_cache_tree(&entry.path(), false)?;
        }
        if top_level {
            fs::remove_file(path.join(ENTRY_MARKER))?;
        }
        fs::remove_dir(path)?;
        Ok(())
    }

    fn sweep_cache(
        root: &Path,
        current: &Path,
        now: SystemTime,
        max_bytes: u64,
        max_entries: usize,
        max_age: Duration,
    ) -> Result<()> {
        if !has_signature(root, CACHE_MARKER, CACHE_SIGNATURE) {
            bail!("Refusing unmarked Adobe cache cleanup");
        }
        let mut entries = Vec::new();
        let mut bytes = 0_u64;
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            if !has_signature(&path, ENTRY_MARKER, ENTRY_SIGNATURE) {
                continue;
            }
            let Ok(size) = tree_size(&path) else {
                continue;
            };
            bytes = bytes.saturating_add(size);
            entries.push((path.clone(), entry_last_used(&path), size));
        }
        entries.sort_by_key(|(_, last_used, _)| *last_used);
        let mut count = entries.len();
        for (path, last_used, size) in entries {
            if path == current {
                continue;
            }
            if entry_in_use(&path) {
                continue;
            }
            let aged = now.duration_since(last_used).unwrap_or_default() > max_age;
            if aged || bytes > max_bytes || count > max_entries {
                match remove_owned_entry(root, &path) {
                    Ok(()) => {
                        bytes = bytes.saturating_sub(size);
                        count -= 1;
                    }
                    Err(error) => {
                        log::warn!("Adobe cache entry retained: {}: {error:#}", path.display())
                    }
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::permissions_set_readonly_false)]
    fn make_writable(path: &Path) -> Result<()> {
        // Windows-only: clears the copied file's DOS READONLY attribute. The
        // Unix world-writable behavior mentioned by Clippy cannot occur here.
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
        Ok(())
    }

    fn copy_develop_resources(source: &Path, destination: &Path) -> Result<()> {
        let parent = source.parent().context("RAW 没有父目录")?;
        if parent.join(".fastcull-adobe-export").is_file() {
            // Only Lightroom's explicitly marked, single-image SDK export
            // directory is cloned. Never copy an arbitrary photo collection.
            copy_export_tree(parent, destination, source, &source.with_extension("xmp"))?;
        } else {
            let mut found = false;
            for suffix in ["acr", "ACR"] {
                let resource = source.with_extension(suffix);
                if resource.is_file() {
                    copy_resource(
                        &resource,
                        &destination.join(resource.file_name().context("资源没有文件名")?),
                    )?;
                    found = true;
                    break;
                }
            }
            if !found {
                // Do not retain a previously copied mask file after it has been
                // removed by an external editor.
                let cached = destination.join(
                    source
                        .with_extension("acr")
                        .file_name()
                        .context("资源没有文件名")?,
                );
                if cached.is_file() {
                    fs::remove_file(cached)?;
                }
            }
        }
        Ok(())
    }

    fn copy_export_tree(
        source: &Path,
        destination: &Path,
        raw: &Path,
        sidecar: &Path,
    ) -> Result<()> {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if path == raw
                || path.as_os_str().eq_ignore_ascii_case(sidecar.as_os_str())
                || kind.is_symlink()
                || is_reparse(&fs::symlink_metadata(&path)?)
                || destination.starts_with(std::path::absolute(&path)?)
            {
                continue;
            }
            let target = destination.join(entry.file_name());
            if kind.is_dir() {
                copy_export_tree(&path, &target, raw, sidecar)?;
            } else if kind.is_file() {
                copy_resource(&path, &target)?;
            }
        }
        Ok(())
    }

    fn copy_resource(source: &Path, destination: &Path) -> Result<()> {
        // File-copy timestamps need not be preserved on Windows. Track the
        // original metadata separately to avoid re-copying mask payloads while
        // dragging a slider.
        let mut hash = DefaultHasher::new();
        source.file_name().hash(&mut hash);
        let stamp = destination
            .parent()
            .context("资源没有父目录")?
            .join(format!(".resource-{:016x}", hash.finish()));
        let fingerprint = format!("{:016x}", source_key(source)?);
        if !destination.is_file()
            || fs::read_to_string(&stamp).ok().as_deref() != Some(&fingerprint)
        {
            fs::copy(source, destination).context("复制 Adobe 蒙版/配置文件资源失败")?;
            make_writable(destination)?;
            fs::write(stamp, fingerprint)?;
        }
        Ok(())
    }

    pub(super) fn embed_xmp(source: &Path, packet: &str) -> Result<()> {
        // The explicitly requested destination is the only non-cache file that
        // this module can write. A private packet file carries arbitrary XML.
        fs::metadata(source).context("找不到要保存 XMP 的 DNG")?;
        let cache = std::env::temp_dir().join("FastCull/adobe-save");
        fs::create_dir_all(&cache)?;
        let mut packet_file = tempfile::NamedTempFile::new_in(&cache)?;
        packet_file.write_all(packet.as_bytes())?;
        packet_file.flush()?;
        with_worker(&cache.join("adobe-tools"), None, |worker| {
            worker.call(
                &format!(
                    "FastCullRaw.embed({},{})",
                    js_path(source)?,
                    js_path(packet_file.path())?
                ),
                None,
                Duration::from_secs(45),
            )?;
            Ok(())
        })
    }

    #[cfg(test)]
    mod resource_tests {
        use super::*;
        #[test]
        fn abandoned_reply_wait_returns_promptly_when_cancelled() {
            // Same reply-channel seam as the production worker: Photoshop is
            // still busy, while the user cancels after 25 ms.
            let (_sender, receiver) = mpsc::channel::<std::io::Result<String>>();
            let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let signal = cancelled.clone();
            let setter = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(25));
                signal.store(true, std::sync::atomic::Ordering::Release);
            });
            let start = Instant::now();
            let result = wait_reply(&receiver, Duration::from_millis(500), || {
                cancelled.load(std::sync::atomic::Ordering::Acquire)
            });
            setter.join().unwrap();
            assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
            assert!(result.unwrap_err().to_string().contains("已取消"));
            assert!(
                start.elapsed() < Duration::from_millis(200),
                "cancel waited for a blocked Adobe reply"
            );
        }

        #[test]
        fn reply_wait_has_an_explicit_deadline() {
            let (_sender, receiver) = mpsc::channel::<std::io::Result<String>>();
            let start = Instant::now();
            let result = wait_reply(&receiver, Duration::from_millis(60), || false);
            assert!(result.unwrap_err().to_string().contains("超时"));
            assert!(start.elapsed() < Duration::from_millis(250));
        }

        #[test]
        #[ignore = "requires an installed Photoshop; uses only harmless sleep scripts"]
        fn late_com_reply_retains_lock_and_does_not_poison_the_next_request() {
            let directory = tempfile::tempdir().unwrap();
            let mut first = Worker::start(&directory.path().join("first"), None).unwrap();
            first.call("'OK'", None, Duration::from_secs(10)).unwrap();
            let ready = directory.path().join("ready.txt");
            let active = directory.path().join("active-work-copy");
            fs::create_dir(&active).unwrap();
            first.pending_entry = Some(active.clone());
            let command = format!(
                "(function(){{var f=new File({});f.open('w');f.write('ready');f.close();$.sleep(1600);return 'OK';}})()",
                js_path(&ready).unwrap()
            );
            let running = std::thread::spawn(move || {
                let result = first.call(&command, None, Duration::from_millis(120));
                assert!(result.is_err());
                assert!(first.in_flight);
                assert!(first.ensure_idle().is_err());
                let deadline = Instant::now() + Duration::from_secs(8);
                while first.ensure_idle().is_err() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(40));
                }
                first.call("'OK'", None, Duration::from_secs(5)).unwrap();
            });
            let wait_started = Instant::now();
            while !ready.is_file() {
                assert!(wait_started.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                entry_in_use(&active),
                "Photoshop's worker must independently lease its active cache"
            );
            let mut second = Worker::start(&directory.path().join("second"), None).unwrap();
            second.first_call = false;
            let second_started = Instant::now();
            let result = second.call("'OK'", None, Duration::from_millis(200));
            assert!(
                result.is_err(),
                "second process entered Photoshop while the first was running"
            );
            assert!(second_started.elapsed() < Duration::from_millis(500));
            running.join().unwrap();
            assert!(!entry_in_use(&active));
            // The queued request sees its cancellation flag and returns without
            // entering Photoshop. A later request reuses the healthy worker.
            let deadline = Instant::now() + Duration::from_secs(3);
            while second.ensure_idle().is_err() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(40));
            }
            second.call("'OK'", None, Duration::from_secs(5)).unwrap();
        }
        fn cache_entry(root: &Path, key: u64, timestamp: u64, payload_size: usize) -> PathBuf {
            let entry = prepare_cache_entry(root, key).unwrap();
            touch_cache_entry(&entry, UNIX_EPOCH + Duration::from_secs(timestamp)).unwrap();
            fs::write(entry.join("raw-pixels.bin"), vec![0_u8; payload_size]).unwrap();
            entry
        }

        #[test]
        fn cache_budget_removes_oldest_and_reuses_recent_current_copy() {
            let dir = tempfile::tempdir().unwrap();
            let root = prepare_cache_root(dir.path()).unwrap();
            let oldest = cache_entry(&root, 1, 1, 100);
            let recent = cache_entry(&root, 2, 2, 100);
            let current = cache_entry(&root, 3, 3, 100);
            let budget = tree_size(&recent).unwrap() + tree_size(&current).unwrap();
            sweep_cache(
                &root,
                &current,
                UNIX_EPOCH + Duration::from_secs(4),
                budget,
                16,
                CACHE_AGE,
            )
            .unwrap();
            assert!(!oldest.exists());
            assert!(recent.exists());
            assert_eq!(prepare_cache_entry(&root, 3).unwrap(), current);
            assert_eq!(
                fs::metadata(current.join("raw-pixels.bin")).unwrap().len(),
                100
            );
        }

        #[test]
        fn cache_count_age_and_oversize_current_have_explicit_bounds() {
            let dir = tempfile::tempdir().unwrap();
            let root = prepare_cache_root(dir.path()).unwrap();
            let expired = cache_entry(&root, 1, 1, 1);
            let recent = cache_entry(&root, 2, 90, 1);
            let current = cache_entry(&root, 3, 2, 1000);
            sweep_cache(
                &root,
                &current,
                UNIX_EPOCH + Duration::from_secs(100),
                u64::MAX,
                16,
                Duration::from_secs(20),
            )
            .unwrap();
            assert!(!expired.exists());
            assert!(recent.exists());
            assert!(current.exists()); // Current survives age and size limits.
            sweep_cache(
                &root,
                &current,
                UNIX_EPOCH + Duration::from_secs(100),
                1,
                1,
                CACHE_AGE,
            )
            .unwrap();
            assert!(!recent.exists());
            assert!(current.exists());
        }

        #[test]
        fn cache_sweep_preserves_other_editor_active_lease() {
            let dir = tempfile::tempdir().unwrap();
            let root = prepare_cache_root(dir.path()).unwrap();
            let other = cache_entry(&root, 1, 1, 100);
            let current = cache_entry(&root, 2, 2, 100);
            let lease = lease_entry(&other).unwrap();
            assert!(entry_in_use(&other));
            sweep_cache(&root, &current, SystemTime::now(), 0, 0, Duration::ZERO).unwrap();
            assert!(other.exists());
            assert!(remove_owned_entry(&root, &other).is_err());
            drop(lease);
            assert!(!entry_in_use(&other));
            sweep_cache(&root, &current, SystemTime::now(), 0, 0, Duration::ZERO).unwrap();
            assert!(!other.exists());
            assert!(current.exists());
        }

        #[test]
        fn unmarked_user_directories_and_legacy_cache_are_never_adopted_or_removed() {
            let dir = tempfile::tempdir().unwrap();
            let legacy = dir.path().join("adobe-work");
            fs::create_dir(&legacy).unwrap();
            fs::write(legacy.join("photo.dng"), "user-data").unwrap();
            let root = prepare_cache_root(dir.path()).unwrap();
            let unknown = root.join("user-photos");
            fs::create_dir(&unknown).unwrap();
            fs::write(unknown.join("photo.nef"), "user-data").unwrap();
            let current = cache_entry(&root, 1, 1, 10);
            sweep_cache(&root, &current, SystemTime::now(), 0, 0, Duration::ZERO).unwrap();
            assert_eq!(
                fs::read_to_string(legacy.join("photo.dng")).unwrap(),
                "user-data"
            );
            assert_eq!(
                fs::read_to_string(unknown.join("photo.nef")).unwrap(),
                "user-data"
            );
            assert!(remove_owned_entry(&root, &legacy).is_err());
            assert!(remove_owned_entry(&root, &unknown).is_err());
            let collision = dir.path().join("user-folder");
            fs::create_dir_all(collision.join("adobe-work-v1")).unwrap();
            assert!(prepare_cache_root(&collision).is_err());
            assert!(!collision.join("adobe-work-v1").join(CACHE_MARKER).exists());
        }

        #[test]
        fn unmarked_cache_entry_collision_is_not_overwritten() {
            let dir = tempfile::tempdir().unwrap();
            let root = prepare_cache_root(dir.path()).unwrap();
            let collision = root.join("0000000000000001");
            fs::create_dir(&collision).unwrap();
            fs::write(collision.join("image.arw"), "user-raw").unwrap();
            assert!(prepare_cache_entry(&root, 1).is_err());
            assert_eq!(
                fs::read_to_string(collision.join("image.arw")).unwrap(),
                "user-raw"
            );
        }
        #[test]
        fn ordinary_photo_directory_copies_only_matching_masks() {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("photos");
            let destination = dir.path().join("cache");
            fs::create_dir_all(&source).unwrap();
            fs::create_dir_all(&destination).unwrap();
            let raw = source.join("test.ARW");
            fs::write(&raw, "raw-original").unwrap();
            fs::write(source.join("test.acr"), "mask-data").unwrap();
            fs::write(source.join("unrelated.acr"), "unrelated").unwrap();
            fs::write(source.join("other.ARW"), "other-raw").unwrap();
            copy_develop_resources(&raw, &destination).unwrap();
            assert_eq!(
                fs::read_to_string(destination.join("test.acr")).unwrap(),
                "mask-data"
            );
            assert!(!destination.join("unrelated.acr").exists());
            assert!(!destination.join("other.ARW").exists());
            fs::remove_file(source.join("test.acr")).unwrap();
            copy_develop_resources(&raw, &destination).unwrap();
            assert!(!destination.join("test.acr").exists());
        }

        #[test]
        fn marked_export_preserves_nested_resources_without_overwriting_recipe() {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("sdk-export");
            let destination = source.join("cache/work");
            fs::create_dir_all(source.join("masks")).unwrap();
            fs::create_dir_all(&destination).unwrap();
            let raw = source.join("test.NEF");
            fs::write(&raw, "raw-original").unwrap();
            fs::write(source.join(".fastcull-adobe-export"), "").unwrap();
            fs::write(source.join("masks/model.acr"), "external-mask").unwrap();
            fs::write(source.join("test.xmp"), "older-recipe").unwrap();
            fs::write(destination.join("test.xmp"), "current-edits").unwrap();
            copy_develop_resources(&raw, &destination).unwrap();
            assert_eq!(
                fs::read_to_string(destination.join("masks/model.acr")).unwrap(),
                "external-mask"
            );
            assert_eq!(
                fs::read_to_string(destination.join("test.xmp")).unwrap(),
                "current-edits"
            );
            assert!(!destination.join("test.NEF").exists());
            assert!(!destination.join("cache").exists());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_strings_cannot_escape_into_javascript_or_lines() {
        assert_eq!(
            quote("a\"\\\n\r\t\0\u{2028}\u{2029}中文"),
            "\"a\\\"\\\\\\n\\r\\t\\u0000\\u2028\\u2029中文\""
        );
    }
    #[test]
    fn embedded_save_rejects_other_formats_before_touching_them() {
        assert!(embed_xmp(Path::new("untouched.nef"), "unused").is_err());
        assert!(embed_xmp(Path::new("untouched.arw"), "unused").is_err());
    }
    #[test]
    fn srgb_marker_is_added_once_without_touching_compressed_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("display.png");
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13_u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0_u8; 17]);
        png.extend_from_slice(&0_u32.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        png.extend_from_slice(&[0_u8; 4]);
        std::fs::write(&path, &png).unwrap();
        mark_png_srgb(&path).unwrap();
        let tagged = std::fs::read(&path).unwrap();
        assert_eq!(&tagged[..33], &png[..33]);
        assert_eq!(&tagged[37..42], b"sRGB\x01");
        assert_eq!(&tagged[46..], &png[33..]);
        mark_png_srgb(&path).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), tagged);
    }
}
