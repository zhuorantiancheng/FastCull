//! Modeless RAW editor process and typed save receipts. Photo/Adobe I/O never
//! runs on the culler's event thread. Only the parent owns LR session revisions.
use anyhow::{Context, Result, ensure};
use eframe::egui;
use fastcull::{
    platform,
    session::Session,
    xmp::{self, Updates, Values},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

pub struct Request {
    pub source: PathBuf,
    pub receipt: PathBuf,
    pub catalog: Option<(PathBuf, String)>,
}
fn decode_path(text: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(fastcull::handoff::decode(text)?))
}
fn encode_path(path: &Path) -> Result<String> {
    Ok(fastcull::handoff::encode(
        path.to_str().context("RAW 路径必须为 Unicode")?,
    ))
}
impl Request {
    fn write(&self, path: &Path) -> Result<()> {
        let (mailbox, id) = self
            .catalog
            .as_ref()
            .map(|(p, id)| -> Result<_> { Ok((encode_path(p)?, fastcull::handoff::encode(id))) })
            .transpose()?
            .unwrap_or(("-".into(), "-".into()));
        fs::write(
            path,
            format!(
                "FASTCULL_RAW_WINDOW_V1\n{}\n{}\n{mailbox}\n{id}\n",
                encode_path(&self.source)?,
                encode_path(&self.receipt)?
            ),
        )?;
        Ok(())
    }
    fn read(path: &Path) -> Result<Self> {
        ensure!(fs::metadata(path)?.len() < 65536, "RAW 窗口请求过大");
        let text = fs::read_to_string(path)?;
        let lines: Vec<_> = text.lines().collect();
        ensure!(
            lines.len() == 5 && lines[0] == "FASTCULL_RAW_WINDOW_V1",
            "无效 RAW 窗口请求"
        );
        let request = Self {
            source: decode_path(lines[1])?,
            receipt: decode_path(lines[2])?,
            catalog: if lines[3] == "-" && lines[4] == "-" {
                None
            } else {
                Some((decode_path(lines[3])?, fastcull::handoff::decode(lines[4])?))
            },
        };
        ensure!(
            request.receipt.parent() == path.parent(),
            "RAW 回执目录不匹配"
        );
        ensure!(
            crate::raw_editor::supported(&request.source),
            "RAW 编辑支持 ARW/NEF/DNG"
        );
        Ok(request)
    }
    fn saved(&self, updates: &Updates) -> Result<()> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let updates = editable_updates(updates);
        ensure!(!updates.empty(), "RAW 保存回执没有修改字段");
        let packet = xmp::patch(&xmp::template(), &updates)?;
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        ensure!(sequence > 0, "RAW 回执序号已耗尽");
        let mut file =
            tempfile::NamedTempFile::new_in(self.receipt.parent().context("回执目录不存在")?)?;
        write!(
            file,
            "FASTCULL_RAW_SAVED_V1\n{}\n{packet}",
            encode_path(&self.source)?
        )?;
        file.as_file().sync_all()?;
        file.persist_noclobber(
            self.receipt
                .parent()
                .unwrap()
                .join(format!("saved-{sequence:020}.txt")),
        )
        .map_err(|e| e.error)?;
        Ok(())
    }
}
fn editable_updates(updates: &Updates) -> Updates {
    Updates {
        exposure: updates.exposure,
        white_balance: updates.white_balance,
        basic: updates.basic,
        orientation: updates.orientation,
        ..Default::default()
    }
}
fn receipt(text: &str, expected: &Path) -> Result<Updates> {
    let mut parts = text.splitn(3, '\n');
    ensure!(
        parts.next() == Some("FASTCULL_RAW_SAVED_V1"),
        "RAW 保存回执无效"
    );
    ensure!(
        decode_path(parts.next().context("缺少回执照片")?)? == expected,
        "RAW 保存回执照片不匹配"
    );
    let v = xmp::parse_values(parts.next().context("缺少保存字段")?)?;
    let white_balance = v
        .white_balance
        .map(|w| w.edit().context("回执白平衡模式不可编辑"))
        .transpose()?;
    let updates = Updates {
        exposure: v.exposure,
        white_balance,
        basic: v.basic,
        orientation: v.orientation,
        ..Default::default()
    };
    ensure!(!updates.empty(), "RAW 保存回执没有修改字段");
    Ok(updates)
}
pub enum Event {
    Saved(PathBuf, Box<(Values, Updates)>),
    Error(String),
}
struct Open {
    path: PathBuf,
    index: usize,
    catalog: Option<(PathBuf, String)>,
    ctx: egui::Context,
}
enum Message {
    Open(Open),
    Drain(mpsc::Sender<std::result::Result<(), String>>),
}
struct Worker {
    send: mpsc::Sender<Message>,
    receive: mpsc::Receiver<Event>,
}
pub struct Windows {
    worker: Mutex<Option<Worker>>,
    session: Option<Arc<Session>>,
}
impl Windows {
    pub fn new(session: Option<Arc<Session>>) -> Arc<Self> {
        Arc::new(Self {
            worker: Mutex::new(None),
            session,
        })
    }
    pub fn open(
        &self,
        path: PathBuf,
        index: usize,
        catalog: Option<(PathBuf, String)>,
        ctx: &egui::Context,
    ) {
        let mut worker = self.worker.lock().unwrap();
        if worker.is_none() {
            *worker = Some(start(self.session.clone()));
        }
        let _ = worker.as_ref().unwrap().send.send(Message::Open(Open {
            path,
            index,
            catalog,
            ctx: ctx.clone(),
        }));
    }
    pub fn poll(&self) -> Vec<Event> {
        self.worker
            .lock()
            .unwrap()
            .as_ref()
            .map(|w| w.receive.try_iter().collect())
            .unwrap_or_default()
    }
    /// Called after the native culling window has closed. Keep the LR process
    /// and its watcher alive until modeless editors finish their save receipts.
    pub fn finish(&self) -> Result<()> {
        let (send, receive) = mpsc::channel();
        {
            let worker = self.worker.lock().unwrap();
            let Some(worker) = worker.as_ref() else {
                return Ok(());
            };
            worker
                .send
                .send(Message::Drain(send))
                .context("RAW 回执监控已停止")?;
        }
        receive
            .recv()
            .context("RAW 回执监控未确认保存结果")?
            .map_err(anyhow::Error::msg)
    }
}
struct Active {
    child: Child,
    request: Request,
    index: usize,
    directory: Option<tempfile::TempDir>,
    acknowledged: u64,
    done: bool,
    error: Option<String>,
    ctx: egui::Context,
}
impl Drop for Active {
    fn drop(&mut self) {
        let unfinished = self.child.try_wait().ok().flatten().is_none();
        let pending = receipt_files(&self.request).map_or(true, |files| !files.is_empty());
        if (unfinished || pending || self.error.is_some())
            && let Some(directory) = self.directory.take()
        {
            let path = directory.keep();
            log::error!("保留 RAW 窗口未确认回执：{}", path.display());
        }
    }
}
fn receipt_files(request: &Request) -> Result<Vec<(u64, PathBuf)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(request.receipt.parent().context("回执目录不存在")?)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("saved-") || !name.ends_with(".txt") {
            continue;
        }
        let sequence = &name[6..name.len() - 4];
        ensure!(
            sequence.len() == 20 && sequence.bytes().all(|b| b.is_ascii_digit()),
            "RAW 回执文件名无效：{name}"
        );
        let sequence = sequence.parse::<u64>()?;
        ensure!(
            sequence > 0 && entry.file_type()?.is_file(),
            "RAW 回执序号或文件类型无效"
        );
        files.push((sequence, entry.path()));
    }
    files.sort_by_key(|(sequence, _)| *sequence);
    Ok(files)
}
fn consume_receipts(
    request: &Request,
    index: usize,
    acknowledged: &mut u64,
    session: Option<&Session>,
    mut publish: impl FnMut(Values, Updates),
) -> Result<()> {
    for (sequence, path) in receipt_files(request)? {
        if sequence > *acknowledged {
            let updates = receipt(&fs::read_to_string(&path)?, &request.source)?;
            let values = xmp::read(&request.source)?;
            if let Some(session) = session {
                session.record(index, &updates)?;
            }
            *acknowledged = sequence;
            publish(values, updates);
        }
        fs::remove_file(&path).with_context(|| format!("无法确认 RAW 回执：{}", path.display()))?;
    }
    Ok(())
}
fn launch(job: Open) -> Result<Active> {
    ensure!(job.path.is_file(), "RAW 文件不存在：{}", job.path.display());
    let root = platform::data_directory().join("raw-editor/windows");
    fs::create_dir_all(&root)?;
    let directory = tempfile::Builder::new()
        .prefix("editor-")
        .tempdir_in(root)?;
    let request = Request {
        source: std::path::absolute(job.path)?,
        receipt: directory.path().join("saved.txt"),
        catalog: job.catalog,
    };
    let input = directory.path().join("request.txt");
    request.write(&input)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--raw-editor")
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000200 | 0x00000008);
    }
    let child = command.spawn().context("无法打开独立 RAW 编辑窗口")?;
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(child.id());
    }
    Ok(Active {
        child,
        request,
        index: job.index,
        directory: Some(directory),
        acknowledged: 0,
        done: false,
        error: None,
        ctx: job.ctx,
    })
}
fn start(session: Option<Arc<Session>>) -> Worker {
    let (send, jobs) = mpsc::channel();
    let (events, receive) = mpsc::channel();
    thread::Builder::new()
        .name("raw-window-monitor".into())
        .spawn(move || {
            let mut active: Vec<Active> = Vec::new();
            let mut drain = None;
            loop {
                match jobs.recv_timeout(Duration::from_millis(100)) {
                    Ok(Message::Open(job)) => {
                        let ctx = job.ctx.clone();
                        let result = if active.iter().any(|a| a.request.source == job.path) {
                            Err(anyhow::anyhow!("该照片已在 RAW 编辑窗口中打开"))
                        } else {
                            launch(job)
                        };
                        match result {
                            Ok(a) => active.push(a),
                            Err(e) => {
                                let _ = events.send(Event::Error(format!("{e:#}")));
                                ctx.request_repaint();
                            }
                        }
                    }
                    Ok(Message::Drain(sender)) => drain = Some(sender),
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                active.retain_mut(|a| {
                    let status = a.child.try_wait();
                    a.done = status.as_ref().map_or(true, |s| s.is_some());
                    let result = status.map_err(anyhow::Error::from).and_then(|_| {
                        consume_receipts(
                            &a.request,
                            a.index,
                            &mut a.acknowledged,
                            session.as_deref(),
                            |values, updates| {
                                let _ = events.send(Event::Saved(
                                    a.request.source.clone(),
                                    Box::new((values, updates)),
                                ));
                                a.ctx.request_repaint();
                            },
                        )
                    });
                    match result {
                        Ok(()) => a.error = None,
                        Err(e) => {
                            let error = format!(
                                "RAW 已保存，但回执尚未确认：{e:#}。回执保留在 {}",
                                a.request.receipt.parent().unwrap().display()
                            );
                            if a.error.as_ref() != Some(&error) {
                                let _ = events.send(Event::Error(error.clone()));
                                a.ctx.request_repaint();
                            }
                            a.error = Some(error);
                        }
                    }
                    // A failed, closed editor stays retryable while the main window
                    // remains open. Drain reports the failure and preserves its files.
                    !a.done || a.error.is_some()
                });
                if active.iter().all(|a| a.done)
                    && let Some(sender) = drain.take()
                {
                    let errors: Vec<_> = active.iter().filter_map(|a| a.error.as_deref()).collect();
                    let result = if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(errors.join("\n"))
                    };
                    let _ = sender.send(result);
                    return;
                }
            }
        })
        .expect("RAW window monitor");
    Worker { send, receive }
}

struct App {
    request: Request,
    load: mpsc::Receiver<Result<crate::raw_editor::Editor, String>>,
    editor: Option<crate::raw_editor::Editor>,
    error: Option<String>,
    preview: Option<Arc<crate::raw_preview::Preview>>,
    allow_close: bool,
    confirm_close: bool,
}
fn load(
    request: &Request,
    ctx: &egui::Context,
    gpu: Option<Arc<crate::raw_preview::Preview>>,
) -> mpsc::Receiver<Result<crate::raw_editor::Editor, String>> {
    let (send, receive) = mpsc::channel();
    let ctx = ctx.clone();
    let source = request.source.clone();
    let catalog = request.catalog.clone();
    thread::Builder::new()
        .name("raw-window-load".into())
        .spawn(move || {
            let result = crate::raw_editor::Editor::new(source, &ctx, gpu, catalog)
                .map_err(|e| format!("{e:#}"));
            let _ = send.send(result);
            ctx.request_repaint();
        })
        .expect("RAW background loading");
    receive
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Ok(result) = self.load.try_recv() {
            match result {
                Ok(editor) => self.editor = Some(editor),
                Err(error) => self.error = Some(error),
            }
        }
        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && let Some(editor) = &self.editor
            && (editor.saving() || editor.dirty())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !editor.saving() {
                self.confirm_close = true;
            }
        }
        if let Some(editor) = &mut self.editor {
            match editor.show(ctx) {
                crate::raw_editor::Action::Saved(saved) => {
                    if let Err(error) = self.request.saved(&saved.1) {
                        self.error = Some(format!("XMP 已保存，但回执失败：{error:#}"));
                    }
                }
                crate::raw_editor::Action::Cancel => {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                crate::raw_editor::Action::Reload => {
                    self.editor = None;
                    self.error = None;
                    self.load = load(&self.request, ctx, self.preview.clone());
                }
                crate::raw_editor::Action::None => {}
            }
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("RAW 编辑");
                ui.spinner();
                ui.label("正在后台读取 RAW / XMP，主选片窗口可继续操作");
            });
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.confirm_close {
            egui::Window::new("未保存的 RAW 调整")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("关闭将丢弃此窗口中未保存的调整。");
                    if ui.button("丢弃调整并关闭").clicked() {
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("继续编辑").clicked() {
                        self.confirm_close = false;
                    }
                });
        }
        if let Some(error) = &self.error {
            egui::Window::new("RAW 编辑提示").show(ctx, |ui| {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            });
        }
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        fastcull::local_raw::cancel_pending();
        if let Some(preview) = &self.preview {
            preview.destroy();
        }
    }
}
pub fn run(path: &Path) -> Result<()> {
    let request = Request::read(path)?;
    let title = format!(
        "FastCull · RAW 编辑 · {}",
        request
            .source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    );
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280., 850.])
            .with_min_inner_size([850., 650.]),
        ..Default::default()
    };
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            crate::ui::configure(&cc.egui_ctx);
            let preview = cc
                .gl
                .as_ref()
                .map(|gl| crate::raw_preview::Preview::new(gl.clone()));
            let receiver = load(&request, &cc.egui_ctx, preview.clone());
            Ok(Box::new(App {
                request,
                load: receiver,
                editor: None,
                error: None,
                preview,
                allow_close: false,
                confirm_close: false,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("RAW window: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_roundtrip_preserves_only_explicit_edits() {
        let u = Updates {
            exposure: Some(0),
            basic: xmp::BasicAdjustments {
                highlights: Some(-65),
                saturation: Some(0),
                ..Default::default()
            },
            ..Default::default()
        };
        let path = Path::new("照片.dng");
        let text = format!(
            "FASTCULL_RAW_SAVED_V1\n{}\n{}",
            encode_path(path).unwrap(),
            xmp::patch(&xmp::template(), &u).unwrap()
        );
        let parsed = receipt(&text, path).unwrap();
        assert_eq!(parsed.exposure, Some(0));
        assert_eq!(parsed.basic, u.basic);
        assert_eq!(parsed.rating, None);
        assert!(receipt(&text, Path::new("other.dng")).is_err());
    }
    fn fixture() -> (tempfile::TempDir, Request, Arc<Session>, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("photo.ARW");
        fs::write(&source, "test raw").unwrap();
        xmp::write(
            &source,
            &Updates {
                exposure: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        let session_directory = directory.path().join("session");
        fs::create_dir(&session_directory).unwrap();
        let manifest = session_directory.join("session.txt");
        fastcull::handoff::Manifest::new("receipt-test".into(), std::slice::from_ref(&source))
            .unwrap()
            .write(&manifest)
            .unwrap();
        let session = Arc::new(Session::new(vec![fastcull::state::Item {
            path: source.clone(),
            updates: Default::default(),
        }]));
        session.attach_manifest(manifest.clone()).unwrap();
        let request = Request {
            source,
            receipt: directory.path().join("saved.txt"),
            catalog: None,
        };
        (directory, request, session, manifest)
    }
    #[test]
    fn sequential_receipts_preserve_different_fields_before_the_first_poll() {
        let (_directory, request, session, manifest) = fixture();
        let first = Updates {
            exposure: Some(500_000),
            ..Default::default()
        };
        let second = Updates {
            basic: xmp::BasicAdjustments {
                contrast: Some(0),
                ..Default::default()
            },
            ..Default::default()
        };
        xmp::write(&request.source, &first).unwrap();
        request.saved(&first).unwrap();
        xmp::write(&request.source, &second).unwrap();
        request.saved(&second).unwrap();
        assert_eq!(receipt_files(&request).unwrap().len(), 2);
        let mut acknowledged = 0;
        let mut published = Vec::new();
        consume_receipts(
            &request,
            0,
            &mut acknowledged,
            Some(&session),
            |_, updates| published.push(updates),
        )
        .unwrap();
        assert_eq!(published.len(), 2);
        assert_eq!(published[0].exposure, first.exposure);
        assert_eq!(published[1].basic, second.basic);
        let manifest = fastcull::handoff::Manifest::read(&manifest).unwrap();
        assert_eq!(manifest.entries[0].revisions[3], 1);
        assert_eq!(manifest.entries[0].revisions[7], 2);
        assert!(receipt_files(&request).unwrap().is_empty());
    }
    #[test]
    fn a_failed_receipt_stays_unacknowledged_and_does_not_replay_earlier_receipts() {
        let (_directory, request, session, manifest) = fixture();
        request
            .saved(&Updates {
                exposure: Some(0),
                ..Default::default()
            })
            .unwrap();
        let second = Updates {
            basic: xmp::BasicAdjustments {
                saturation: Some(0),
                ..Default::default()
            },
            ..Default::default()
        };
        xmp::write(&request.source, &second).unwrap();
        request.saved(&second).unwrap();
        let files = receipt_files(&request).unwrap();
        let valid = fs::read_to_string(&files[1].1).unwrap();
        fs::write(&files[1].1, "invalid receipt").unwrap();
        let mut acknowledged = 0;
        let mut published = 0;
        assert!(
            consume_receipts(&request, 0, &mut acknowledged, Some(&session), |_, _| {
                published += 1
            })
            .is_err()
        );
        assert_eq!(published, 1);
        assert_eq!(acknowledged, files[0].0);
        assert!(!files[0].1.exists());
        assert!(files[1].1.exists());
        fs::write(&files[1].1, valid).unwrap();
        consume_receipts(&request, 0, &mut acknowledged, Some(&session), |_, _| {
            published += 1
        })
        .unwrap();
        assert_eq!(published, 2);
        let manifest = fastcull::handoff::Manifest::read(&manifest).unwrap();
        assert_eq!(manifest.revision, 2);
        assert_eq!(manifest.entries[0].revisions[3], 1);
    }
    #[test]
    fn failed_session_commit_keeps_the_only_durable_receipt_for_retry() {
        let (directory, request, session, manifest) = fixture();
        request
            .saved(&Updates {
                exposure: Some(0),
                ..Default::default()
            })
            .unwrap();
        let parent = manifest.parent().unwrap();
        let offline = directory.path().join("offline-session");
        fs::rename(parent, &offline).unwrap();
        let mut acknowledged = 0;
        let mut published = 0;
        assert!(
            consume_receipts(&request, 0, &mut acknowledged, Some(&session), |_, _| {
                published += 1
            })
            .is_err()
        );
        assert_eq!(acknowledged, 0);
        assert_eq!(published, 0);
        assert_eq!(receipt_files(&request).unwrap().len(), 1);
        fs::rename(offline, parent).unwrap();
        consume_receipts(&request, 0, &mut acknowledged, Some(&session), |_, _| {
            published += 1
        })
        .unwrap();
        assert_eq!(published, 1);
        assert!(receipt_files(&request).unwrap().is_empty());
        assert!(
            fastcull::handoff::Manifest::read(&manifest)
                .unwrap()
                .entries[0]
                .revisions[3]
                > 0
        );
    }
    #[test]
    fn raw_receipts_cannot_record_culling_fields_or_crop() {
        let (_directory, request, _session, _manifest) = fixture();
        request
            .saved(&Updates {
                rating: Some(5),
                label: Some("Red".into()),
                flag: Some("pick".into()),
                crop: Some(Default::default()),
                orientation: Some(6),
                basic: xmp::BasicAdjustments {
                    vibrance: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();
        let file = &receipt_files(&request).unwrap()[0].1;
        let parsed = receipt(&fs::read_to_string(file).unwrap(), &request.source).unwrap();
        assert_eq!(parsed.orientation, Some(6));
        assert_eq!(parsed.basic.vibrance, Some(0));
        assert!(
            parsed.rating.is_none()
                && parsed.label.is_none()
                && parsed.flag.is_none()
                && parsed.crop.is_none()
        );
    }
    #[test]
    fn finish_propagates_an_unconfirmed_receipt_error() {
        let (send, jobs) = mpsc::channel();
        let (_events, receive) = mpsc::channel();
        let windows = Windows {
            worker: Mutex::new(Some(Worker { send, receive })),
            session: None,
        };
        let thread = thread::spawn(move || {
            if let Message::Drain(reply) = jobs.recv().unwrap() {
                reply.send(Err("unconfirmed receipt".into())).unwrap();
            }
        });
        assert!(
            windows
                .finish()
                .unwrap_err()
                .to_string()
                .contains("unconfirmed receipt")
        );
        thread.join().unwrap();
    }
    #[test]
    fn dropping_a_closed_failed_editor_preserves_its_receipt_directory() {
        let parent = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir_in(parent.path()).unwrap();
        let path = directory.path().to_path_buf();
        let receipt_path = path.join("saved-00000000000000000001.txt");
        fs::write(&receipt_path, "unconfirmed receipt").unwrap();
        #[cfg(windows)]
        let mut command = Command::new("cmd.exe");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.args(["/C", "exit", "0"]).creation_flags(0x08000000);
        }
        #[cfg(not(windows))]
        let mut command = Command::new("sh");
        #[cfg(not(windows))]
        command.args(["-c", "exit 0"]);
        let mut child = command.spawn().unwrap();
        child.wait().unwrap();
        drop(Active {
            child,
            request: Request {
                source: parent.path().join("photo.ARW"),
                receipt: path.join("saved.txt"),
                catalog: None,
            },
            index: 0,
            directory: Some(directory),
            acknowledged: 0,
            done: true,
            error: Some("commit failed".into()),
            ctx: egui::Context::default(),
        });
        assert_eq!(
            fs::read_to_string(receipt_path).unwrap(),
            "unconfirmed receipt"
        );
    }
}
