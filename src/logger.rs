use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
struct Logger(Mutex<File>);
impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record<'_>) {
        if self.enabled(r.metadata())
            && let Ok(mut f) = self.0.lock()
        {
            let _ = writeln!(
                f,
                "{} {} {}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                r.level(),
                r.args()
            );
            let _ = f.flush();
        }
    }
    fn flush(&self) {
        if let Ok(mut f) = self.0.lock() {
            let _ = f.flush();
        }
    }
}
pub fn directory() -> PathBuf {
    std::env::var_os("FASTCULL_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| fastcull::platform::data_directory().join("logs"))
}
pub fn init() {
    static LOGGER: OnceLock<Logger> = OnceLock::new();
    let dir = directory();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = dir.join(format!("fastcull_app_{ts}_{}.log", std::process::id()));
    if let Ok(file) = File::create(&path) {
        let logger = LOGGER.get_or_init(|| Logger(Mutex::new(file)));
        let _ = log::set_logger(logger);
        log::set_max_level(log::LevelFilter::Info);
        log::info!(
            "FastCull {} log={} args={:?}",
            env!("CARGO_PKG_VERSION"),
            path.display(),
            std::env::args_os().collect::<Vec<_>>()
        );
    }
    // User logs are deliberately retained, never deleted during development.
}
