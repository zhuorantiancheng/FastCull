//! Versioned C ABI; LibRaw state and allocations remain inside the native DLL.
use crate::{
    arw::{ArwInfo, CaptureMetadata, JpegRef},
    imaging::{Level, Pixels},
};
use anyhow::{Result, ensure};
use std::path::Path;

#[cfg(any(windows, target_os = "macos"))]
mod desktop {
    use super::*;
    #[cfg(windows)]
    use std::os::windows::ffi::OsStrExt;
    use std::{ffi::c_void, sync::OnceLock};
    #[cfg(windows)]
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        LoadLibraryExW,
    };
    #[repr(C)]
    struct Info {
        abi: u32,
        width: u32,
        height: u32,
        orientation: u32,
        pw: u32,
        ph: u32,
        iso: f32,
        aperture: f32,
        shutter: f32,
        focal: f32,
        make: [u8; 64],
        model: [u8; 64],
        lens: [u8; 128],
        captured: [u8; 32],
    }
    impl Default for Info {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }
    #[repr(C)]
    #[derive(Default)]
    struct Frame {
        kind: u32,
        width: u32,
        height: u32,
        channels: u32,
        bits: u32,
        rendered: u32,
        len: u64,
        data: *const u8,
        owner: *mut c_void,
    }
    #[cfg(windows)]
    type PathChar = u16;
    #[cfg(target_os = "macos")]
    type PathChar = std::ffi::c_char;
    type Probe = unsafe extern "C" fn(*const PathChar, *mut Info, *mut u8) -> i32;
    type Decode = unsafe extern "C" fn(*const PathChar, i32, *mut Frame, *mut u8) -> i32;
    type Release = unsafe extern "C" fn(*mut c_void);
    type ScaledJpeg = unsafe extern "C" fn(*const u8, u32, u32, u32, *mut Frame) -> i32;
    struct OwnedFrame {
        frame: Frame,
        release: Release,
    }
    impl Drop for OwnedFrame {
        fn drop(&mut self) {
            unsafe {
                (self.release)(self.frame.owner);
            }
        }
    }
    struct Library {
        probe: Probe,
        decode: Decode,
        release: Release,
        scaled_jpeg: Option<(ScaledJpeg, Release)>,
        #[cfg(target_os = "macos")]
        _module: libloading::Library,
    }
    static LIBRARY: OnceLock<std::result::Result<Library, String>> = OnceLock::new();
    #[cfg(windows)]
    fn library() -> Result<&'static Library> {
        LIBRARY.get_or_init(|| {
            let exe=std::env::current_exe().map_err(|e|e.to_string())?;
            let mut path=exe.with_file_name("fastcull_raw.dll");
            if cfg!(debug_assertions) && !path.exists() {path=Path::new(env!("CARGO_MANIFEST_DIR")).join("lrplugins/FastCull.lrplugin/fastcull_raw.dll");}
            let path:Vec<u16>=path.as_os_str().encode_wide().chain([0]).collect();
            // The module lives for the process lifetime. Symbols are the fixed,
            // versioned ABI of our bundled adapter, never Rust/C++ struct layouts.
            unsafe {
                let module=LoadLibraryExW(path.as_ptr(),std::ptr::null_mut(),LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR|LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
                if module.is_null(){return Err(format!("无法加载 fastcull_raw.dll：{}",std::io::Error::last_os_error()));}
                let probe=GetProcAddress(module,c"fc_raw_info".as_ptr().cast()).ok_or("原生 RAW 元数据接口缺失")?;
                let decode=GetProcAddress(module,c"fc_raw_decode".as_ptr().cast()).ok_or("原生 RAW 解码接口缺失")?;
                let release=GetProcAddress(module,c"fc_raw_release".as_ptr().cast()).ok_or("原生 RAW 释放接口缺失")?;
                let scaled_jpeg = GetProcAddress(module,c"fc_jpeg_scaled".as_ptr().cast())
                    .zip(GetProcAddress(module,c"fc_jpeg_release".as_ptr().cast()))
                    .map(|(decode,release)| (std::mem::transmute::<unsafe extern "system" fn()->isize,ScaledJpeg>(decode), std::mem::transmute::<unsafe extern "system" fn()->isize,Release>(release)));
                Ok(Library {probe:std::mem::transmute::<unsafe extern "system" fn()->isize,Probe>(probe),decode:std::mem::transmute::<unsafe extern "system" fn()->isize,Decode>(decode),release:std::mem::transmute::<unsafe extern "system" fn()->isize,Release>(release),scaled_jpeg})
            }
        }).as_ref().map_err(|e|anyhow::anyhow!(e.clone()))
    }
    #[cfg(target_os = "macos")]
    fn library() -> Result<&'static Library> {
        LIBRARY
            .get_or_init(|| {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let adjacent = exe.with_file_name("libfastcull_raw.dylib");
                let bundled = exe
                    .parent()
                    .ok_or("missing executable directory")?
                    .join("../Frameworks/libfastcull_raw.dylib");
                let path = if bundled.is_file() { bundled } else { adjacent };
                // The library is retained alongside its copied C function pointers.
                // Only load from our app bundle or executable directory, never PATH.
                unsafe {
                    let module = libloading::Library::new(&path)
                        .map_err(|e| format!("无法加载 {}：{e}", path.display()))?;
                    let probe = *module
                        .get::<Probe>(b"fc_raw_info\0")
                        .map_err(|e| e.to_string())?;
                    let decode = *module
                        .get::<Decode>(b"fc_raw_decode\0")
                        .map_err(|e| e.to_string())?;
                    let release = *module
                        .get::<Release>(b"fc_raw_release\0")
                        .map_err(|e| e.to_string())?;
                    Ok(Library {
                        probe,
                        decode,
                        release,
                        scaled_jpeg: None,
                        _module: module,
                    })
                }
            })
            .as_ref()
            .map_err(|e| anyhow::anyhow!(e.clone()))
    }
    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes.split(|v| *v == 0).next().unwrap_or_default())
            .trim()
            .to_owned()
    }
    pub fn scaled_jpeg(bytes: &[u8], width: u32, height: u32) -> Option<Pixels> {
        let (decode, release) = library().ok()?.scaled_jpeg?;
        let mut frame = Frame::default();
        let status = unsafe {
            decode(
                bytes.as_ptr(),
                bytes.len().try_into().ok()?,
                width,
                height,
                &mut frame,
            )
        };
        if status != 0 {
            return None;
        }
        let owned = OwnedFrame { frame, release };
        let f = &owned.frame;
        let expected = u64::from(f.width) * u64::from(f.height) * 4;
        if f.data.is_null()
            || f.width < width
            || f.height < height
            || expected > 640_000_000
            || f.len != expected
        {
            return None;
        }
        Some(Pixels {
            width: f.width as usize,
            height: f.height as usize,
            rgba: unsafe { std::slice::from_raw_parts(f.data, f.len as usize) }.to_vec(),
        })
    }
    fn optional(bytes: &[u8]) -> Option<String> {
        let s = text(bytes);
        (!s.is_empty()).then_some(s)
    }
    fn positive(value: f32) -> Option<f64> {
        (value.is_finite() && value > 0.).then_some(value as f64)
    }
    #[cfg(windows)]
    fn native_path(path: &Path) -> Result<Vec<u16>> {
        let path: Vec<_> = path.as_os_str().encode_wide().collect();
        ensure!(!path.contains(&0), "RAW 路径含 NUL");
        Ok(path.into_iter().chain([0]).collect())
    }
    #[cfg(target_os = "macos")]
    fn native_path(path: &Path) -> Result<std::ffi::CString> {
        use std::os::unix::ffi::OsStrExt;
        Ok(std::ffi::CString::new(path.as_os_str().as_bytes())?)
    }
    pub fn read(path: &Path) -> Result<ArwInfo> {
        let lib = library()?;
        let mut info = Info::default();
        let mut error = [0u8; 256];
        let path = native_path(path)?;
        let code = unsafe { (lib.probe)(path.as_ptr(), &mut info, error.as_mut_ptr()) };
        ensure!(code == 0, "RAW 元数据：{}", text(&error));
        ensure!(info.abi == 1, "RAW 适配层版本不匹配");
        let capture = CaptureMetadata {
            make: optional(&info.make),
            camera: optional(&info.model),
            lens: optional(&info.lens),
            captured_at: optional(&info.captured),
            iso: positive(info.iso).map(|v| v.round() as u32),
            aperture: positive(info.aperture),
            shutter: positive(info.shutter),
            focal: positive(info.focal),
        };
        let summary = format!(
            "{}  RAW {}×{}{}",
            capture.summary(),
            info.width,
            info.height,
            if info.pw > 0 && info.ph > 0 {
                format!(" · 预览 {}×{}", info.pw, info.ph)
            } else {
                String::new()
            }
        );
        let jpeg = JpegRef { offset: 0, len: 0 };
        Ok(ArwInfo {
            jpeg: jpeg.clone(),
            preview: jpeg.clone(),
            thumbnail: jpeg,
            orientation: info.orientation as u16,
            summary,
            capture,
            raw_size: Some((info.width, info.height)),
            image_size: Some((info.width, info.height)),
        })
    }
    fn frame(path: &Path, full: bool) -> Result<OwnedFrame> {
        let lib = library()?;
        let mut frame = Frame::default();
        let mut error = [0u8; 256];
        let wide = native_path(path)?;
        let code = unsafe {
            (lib.decode)(
                wide.as_ptr(),
                i32::from(full),
                &mut frame,
                error.as_mut_ptr(),
            )
        };
        ensure!(code == 0, "RAW 解码：{}", text(&error));
        let owned = OwnedFrame {
            frame,
            release: lib.release,
        };
        let frame = &owned.frame;
        ensure!(
            !frame.data.is_null() && frame.len > 0 && frame.len <= 640_000_000,
            "invalid native RAW buffer"
        );
        Ok(owned)
    }
    pub fn preview_jpeg(path: &Path) -> Result<Vec<u8>> {
        let owned = frame(path, false)?;
        let frame = &owned.frame;
        ensure!(frame.kind == 1, "该 RAW 没有 JPEG 格式的内嵌预览");
        Ok(unsafe { std::slice::from_raw_parts(frame.data, frame.len as usize) }.to_vec())
    }
    pub fn decode(
        path: &Path,
        level: Level,
        geometry: &mut dyn FnMut([usize; 2]),
    ) -> Result<Pixels> {
        let owned = frame(path, level == Level::Full)?;
        let frame = &owned.frame;
        let bytes = unsafe { std::slice::from_raw_parts(frame.data, frame.len as usize) };
        let pixels = if frame.kind == 1 {
            crate::imaging::decode_jpeg_bytes_with_geometry(bytes, level, geometry)?
        } else {
            ensure!(
                frame.kind == 2 && frame.bits == 8 && (1..=4).contains(&frame.channels),
                "unsupported native bitmap"
            );
            let n = frame.width as usize * frame.height as usize;
            let channels = frame.channels as usize;
            ensure!(
                n <= 160_000_000 && n.checked_mul(channels) == Some(bytes.len()),
                "invalid native bitmap dimensions"
            );
            geometry([frame.width as usize, frame.height as usize]);
            let mut rgba = Vec::with_capacity(n * 4);
            for p in bytes.chunks_exact(channels) {
                if channels < 3 {
                    rgba.extend([p[0], p[0], p[0], 255]);
                } else {
                    rgba.extend([p[0], p[1], p[2], 255]);
                }
            }
            crate::imaging::resize_level(
                Pixels {
                    width: frame.width as usize,
                    height: frame.height as usize,
                    rgba,
                },
                level,
            )?
        };
        if frame.rendered != 0 {
            log::info!(
                "RAW sensor render full={} {}x{} {}",
                level == Level::Full,
                pixels.width,
                pixels.height,
                path.display()
            );
        }
        Ok(pixels)
    }
}
pub fn read(path: &Path) -> Result<ArwInfo> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        desktop::read(path)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = path;
        anyhow::bail!("此构建没有原生 RAW 解码器")
    }
}
pub(crate) fn scaled_jpeg(bytes: &[u8], width: u32, height: u32) -> Option<Pixels> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        desktop::scaled_jpeg(bytes, width, height)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (bytes, width, height);
        None
    }
}
pub fn decode(path: &Path, level: Level) -> Result<Pixels> {
    decode_with_geometry(path, level, &mut |_| {})
}
pub fn decode_with_geometry(
    path: &Path,
    level: Level,
    geometry: &mut dyn FnMut([usize; 2]),
) -> Result<Pixels> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        desktop::decode(path, level, geometry)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (path, level, geometry);
        anyhow::bail!("此构建没有原生 RAW 解码器")
    }
}
pub fn preview_jpeg(path: &Path) -> Result<Vec<u8>> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        desktop::preview_jpeg(path)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = path;
        anyhow::bail!("此构建没有原生 RAW 解码器")
    }
}
