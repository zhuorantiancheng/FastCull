use std::path::{Path, PathBuf};
pub const EXTENSIONS: &[&str] = &["arw", "cr2", "nef", "jpg", "jpeg", "jpe"];
pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            EXTENSIONS
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
}
pub fn is_jpeg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            ["jpg", "jpeg", "jpe"]
                .iter()
                .any(|v| ext.eq_ignore_ascii_case(v))
        })
}
pub fn companions(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![
        PathBuf::from(format!("{}.xmp", path.display())),
        PathBuf::from(format!("{}.acr", path.display())),
    ];
    if !is_jpeg(path) {
        out.extend([path.with_extension("xmp"), path.with_extension("acr")]);
    }
    out
}
