//! Small desktop differences shared by the standalone and Adobe entry points.
use std::path::{Path, PathBuf};

pub fn data_directory() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    base.unwrap_or_else(std::env::temp_dir).join("FastCull")
}

pub fn open_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    let program = "explorer.exe";
    #[cfg(target_os = "macos")]
    let program = "/usr/bin/open";
    #[cfg(not(any(windows, target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program).arg(path).spawn()?;
    Ok(())
}

pub fn cjk_font_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    return [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    #[cfg(not(target_os = "macos"))]
    vec![
        std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| "C:/Windows".into())
            .join("Fonts/msyh.ttc"),
    ]
}
