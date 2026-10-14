#![cfg_attr(windows, windows_subsystem = "windows")]
mod bridge_list;
mod cache;
mod crop_ui;
mod exposure;
mod exposure_preview;
mod filter_ui;
mod focus;
mod folder_ui;
mod launch;
mod logger;
mod move_jobs;
mod ui;
use anyhow::{Context, Result, ensure};
use fastcull::{arw::ArwInfo, handoff, imaging, state, xmp};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

fn launch(state_path: &Path, manifest: Option<&Path>) -> Result<()> {
    launch_mode(state_path, manifest, false)
}
fn launch_mode(state_path: &Path, manifest: Option<&Path>, bridge: bool) -> Result<()> {
    // Private snapshot prevents another LR invocation overwriting the child's input.
    let bytes = std::fs::read(state_path)?;
    let mut snapshot = tempfile::Builder::new()
        .prefix("fastcull-state-")
        .suffix(".txt")
        .tempfile()?;
    snapshot.write_all(&bytes)?;
    let executable = std::env::current_exe()?;
    let (file, path) = snapshot.keep()?;
    drop(file);
    let result = if bridge {
        launch::spawn_bridge(&executable, &path)
    } else {
        launch::spawn(&executable, &path, manifest)
    };
    if let Err(e) = result {
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(())
}
fn files(args: &[std::ffi::OsString]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for arg in args {
        let p = PathBuf::from(arg);
        if p.is_dir() {
            let mut entries = std::fs::read_dir(&p)?
                .map(|e| e.map(|v| v.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            entries.sort();
            out.extend(
                entries
                    .into_iter()
                    .filter(|p| p.is_file() && fastcull::formats::supported(p)),
            );
        } else if fastcull::formats::supported(&p) {
            ensure!(p.is_file(), "照片文件不存在或无法访问：{}", p.display());
            out.push(p);
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    ensure!(
        !out.is_empty(),
        "所选文件中没有支持的 RAW/JPEG 照片，请选择照片或包含照片的文件夹。"
    );
    Ok(out)
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mode = args.first().and_then(|s| s.to_str()).unwrap_or("");
    if args.is_empty() {
        return ui::run_standalone(Vec::new());
    }
    if matches!(mode, "--help" | "-h") {
        println!(
            "FastCull {} (Rust)\nfastcull <folder or ARW files...>\nfastcull --bridge-open <photo-list>\nfastcull --open <statefile> [--session <manifest> --session-id <ID>]\nfastcull --apply-state <statefile>\nfastcull --from-state <statefile>\nfastcull --sync-session <manifest> [after-revision]\nfastcull --read-folders <manifest>\nfastcull --read-sidecars <statefile>\nfastcull --inspect <ARW>\nfastcull --benchmark <ARW> [runs]\nfastcull --export-preview <ARW> <JPEG>",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    match mode {
        "--bridge-open" | "--bridge-list-owned" => {
            ensure!(args.len() == 2, "expected Bridge photo list");
            let file = Path::new(&args[1]);
            if mode == "--bridge-open" {
                let paths = bridge_list::read(file)?;
                launch_mode(file, None, true)?;
                let _ = std::fs::remove_file(file);
                println!("RESULT launched=1 selected_roots={}", paths.len());
            } else {
                ui::run_bridge_request(file.to_path_buf())?;
            }
        }
        "--read-folders" => {
            ensure!(args.len() == 2, "expected session manifest");
            let file = Path::new(&args[1]);
            let manifest = handoff::Manifest::read(file)?;
            let plan_file = file.with_extension("folders");
            let plan = if plan_file.exists() {
                fastcull::organizing::Plan::read(
                    &plan_file,
                    &manifest.id,
                    manifest.entries.iter().map(|e| e.path.clone()).collect(),
                )?
            } else {
                fastcull::organizing::Plan::new(manifest.id.clone(), Vec::new())
            };
            println!(
                "ORGANIZATION\t{}\t{}\t{}",
                plan.id,
                plan.revision,
                if manifest.closed { "closed" } else { "active" }
            );
            for (from, to) in &plan.moves {
                ensure!(
                    to.parent().is_some_and(|p| p.is_dir()),
                    "整理目标文件夹不存在或无法访问：{}",
                    to.display()
                );
                println!(
                    "MOVE\t{}\t{}",
                    handoff::encode(from.to_str().context("source path must be UTF-8")?),
                    handoff::encode(to.to_str().context("target path must be UTF-8")?)
                );
            }
            println!("RESULT moves={}", plan.moves.len());
        }
        "--sync-session" | "--sync-stars" | "--read-sidecars" => {
            ensure!(
                args.len() == 2
                    || (mode == "--sync-session" && (3..=4).contains(&args.len()))
                    || (mode == "--sync-stars" && (3..=4).contains(&args.len())),
                "expected manifest/statefile [after-revision [stars-ack]]"
            );
            let path = Path::new(&args[1]);
            let rows = if mode != "--read-sidecars" {
                let m = handoff::Manifest::read(path)?;
                let after = args
                    .get(2)
                    .and_then(|v| v.to_str())
                    .unwrap_or("0")
                    .parse::<u64>()?;
                ensure!(
                    after <= m.revision,
                    "acknowledged revision is newer than manifest"
                );
                println!(
                    "SESSION\t{}\t{}\t{}",
                    m.id,
                    if m.closed { "closed" } else { "active" },
                    m.revision
                );
                let additional_ack = args
                    .get(3)
                    .and_then(|v| v.to_str())
                    .map(str::parse::<u64>)
                    .transpose()?
                    .unwrap_or(after);
                ensure!(
                    additional_ack <= m.revision,
                    "stars acknowledgement is newer than manifest"
                );
                if mode == "--sync-stars" {
                    println!(
                        "RESULT pending_other_fields={}",
                        m.other_fields_since(additional_ack)
                    );
                    m.stars_since(after)
                } else {
                    m.rows_with_stars_ack(after, additional_ack)
                }
            } else {
                println!("SESSION\t-\tstandalone\t0");
                state::read(path)?
                    .into_iter()
                    .map(|i| {
                        let v = xmp::read(&i.path);
                        (i.path, v)
                    })
                    .collect()
            };
            let (mut count, mut errors) = (0, 0);
            for (path, result) in rows {
                match result {
                    Ok(v) => {
                        handoff::print_row(&path, &v)?;
                        count += 1;
                    }
                    Err(e) => {
                        errors += 1;
                        println!(
                            "ROW_ERROR\t{}\t{}",
                            handoff::encode(path.to_str().context("path must be UTF-8")?),
                            handoff::encode(&format!("{e:#}"))
                        );
                    }
                }
            }
            println!("RESULT rows={count} error={errors}");
        }
        "--open" | "--apply-state" | "--from-state" | "--from-state-owned"
        | "--open-state-owned" => {
            let manifest = if args.len() >= 4 {
                ensure!(args[2] == "--session", "expected --session <manifest>");
                Some(PathBuf::from(&args[3]))
            } else {
                None
            };
            ensure!(
                args.len() == 2
                    || (mode == "--open-state-owned" && args.len() == 4)
                    || (mode == "--open" && args.len() == 6 && args[4] == "--session-id"),
                "expected statefile [--session manifest --session-id ID]"
            );
            let path = Path::new(&args[1]);
            let items = state::read(path);
            if mode == "--from-state-owned" || mode == "--open-state-owned" {
                let _ = std::fs::remove_file(path);
            }
            let items = items?;
            ensure!(!items.is_empty(), "empty photo list");
            if mode == "--open-state-owned" {
                ui::run_state(items, manifest)?;
            } else if mode == "--from-state" || mode == "--from-state-owned" {
                ui::run(items.into_iter().map(|i| i.path).collect())?;
            } else if mode == "--open" {
                let count = items.len();
                if let Some(manifest) = &manifest {
                    ensure!(
                        !manifest.exists(),
                        "session manifest already exists; use a unique session ID"
                    );
                    let id = args[5]
                        .to_str()
                        .context("session ID must be ASCII")?
                        .to_owned();
                    handoff::Manifest::new(
                        id,
                        &items.iter().map(|i| i.path.clone()).collect::<Vec<_>>(),
                    )?
                    .write(manifest)?;
                }
                launch(path, manifest.as_deref())?;
                println!("RESULT wrote=0 kept=0 error=0 pending={count}");
                println!("RESULT launched=1");
            } else {
                let stats = state::apply(&items);
                stats.print();
                ensure!(
                    stats.error == 0,
                    "{} sidecar writes failed; GUI not launched",
                    stats.error
                );
            }
        }
        "--inspect" | "--benchmark" | "--benchmark-sensor" | "--export-preview" => {
            ensure!(args.len() >= 2, "expected ARW file");
            let path = Path::new(&args[1]);
            let info = ArwInfo::read(path)?;
            if let Some((width, height)) = info.raw_size {
                println!(
                    "RAW original={width}x{height} orientation={} {}",
                    info.orientation, info.summary
                );
            } else {
                println!(
                    "JPEG offset={} len={} orientation={} {}",
                    info.jpeg.offset, info.jpeg.len, info.orientation, info.summary
                );
            }
            if mode == "--export-preview" {
                ensure!(args.len() == 3, "expected output JPEG path");
                let dest = Path::new(&args[2]);
                ensure!(!dest.exists(), "output already exists");
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(dest)?;
                let bytes = if info.raw_size.is_some() {
                    fastcull::native_raw::preview_jpeg(path)?
                } else {
                    ArwInfo::read_jpeg(path, &info.jpeg)?
                };
                f.write_all(&bytes)?;
            }
            if mode == "--benchmark" || mode == "--benchmark-sensor" {
                let runs = args
                    .get(2)
                    .and_then(|v| v.to_str())
                    .unwrap_or("5")
                    .parse::<usize>()?;
                ensure!((1..=1000).contains(&runs), "runs must be 1..1000");
                let mut times = Vec::new();
                for _ in 0..runs {
                    let t = std::time::Instant::now();
                    let (_, p) = if mode == "--benchmark-sensor" {
                        imaging::decode_sensor(path, true)?
                    } else {
                        imaging::decode(path, true)?
                    };
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    println!("DECODE {ms:.3}ms {}x{}", p.width, p.height);
                    times.push(ms);
                }
                times.sort_by(f64::total_cmp);
                println!(
                    "BENCH runs={runs} median_ms={:.3} min_ms={:.3}",
                    times[runs / 2],
                    times[0]
                );
            }
        }
        _ => ui::run(files(&args)?)?,
    }
    Ok(())
}
fn main() {
    // Do not call console APIs before entry timing. Rust handles Unicode console
    // output itself; LR's redirected stdout is already a UTF-8 byte stream.
    let entered = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let init_started = std::time::Instant::now();
    logger::init();
    log::info!(
        "startup entry unix_ms={entered} logger_ms={:.1}",
        init_started.elapsed().as_secs_f64() * 1000.
    );
    let interactive = std::env::args_os()
        .nth(1)
        .and_then(|a| a.into_string().ok())
        .is_some_and(|a| {
            a == "--from-state"
                || a == "--from-state-owned"
                || a == "--open-state-owned"
                || a == "--bridge-open"
                || a == "--bridge-list-owned"
                || !a.starts_with('-')
        });
    std::panic::set_hook(Box::new(move |info| {
        log::error!(
            "PANIC {info}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        if interactive && std::thread::current().name() == Some("main") {
            show_error(&format!("选片器发生异常：{info}"));
        }
    }));
    if let Err(e) = run().context("FastCull") {
        println!("ERROR {e:#}");
        log::error!("{e:#}");
        if interactive {
            show_error(&format!("选片器启动失败：{e:#}"));
        }
        std::process::exit(1);
    }
}
fn show_error(message: &str) {
    if std::env::var_os("FASTCULL_SMOKE_DIR").is_some() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        rfd::MessageDialog::new()
            .set_title("FastCull")
            .set_description(format!(
                "{message}\n\n日志目录：{}",
                logger::directory().display()
            ))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    #[cfg(windows)]
    {
        let text: Vec<u16> = format!("{message}\n\n日志目录：{}", logger::directory().display())
            .encode_utf16()
            .chain([0])
            .collect();
        let title: Vec<u16> = "FastCull for ARW".encode_utf16().chain([0]).collect();
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                0x10,
            );
        }
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    #[test]
    fn mixed_cli_selection_ignores_unsupported_files_and_keeps_arw_order() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = [
            "one.ARW",
            "ignore.jpg",
            "ignore.CR3",
            "ignore.xmp",
            "two.arw",
        ]
        .iter()
        .map(|name| dir.path().join(name))
        .collect();
        for path in &paths {
            std::fs::write(path, b"unchanged").unwrap();
        }
        let args: Vec<_> = paths.iter().map(|p| p.as_os_str().to_owned()).collect();
        assert_eq!(
            files(&args).unwrap(),
            [paths[0].clone(), paths[1].clone(), paths[4].clone()]
        );
        assert!(
            files(&args[3..4])
                .unwrap_err()
                .to_string()
                .contains("没有支持的 RAW/JPEG")
        );
        for path in paths {
            assert_eq!(std::fs::read(path).unwrap(), b"unchanged");
        }
        assert!(
            files(&[dir.path().join("missing.ARW").into_os_string()])
                .unwrap_err()
                .to_string()
                .contains("无法访问")
        );
    }
}
