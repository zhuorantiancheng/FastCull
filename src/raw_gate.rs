//! Serialize the existing single LR metadata-export mailbox across editor processes.
#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::{Result, ensure};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[cfg(windows)]
pub struct Guard(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(windows)]
pub fn acquire(path: &Path, stop: &AtomicBool) -> Result<Guard> {
    use std::hash::{Hash, Hasher};
    use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hash);
    let name: Vec<u16> = format!("Local\\FastCull-LR-raw-{:016x}", hash.finish())
        .encode_utf16()
        .chain([0])
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    ensure!(!handle.is_null(), "无法锁定 LR 显影邮箱");
    let started = Instant::now();
    loop {
        let state = unsafe { WaitForSingleObject(handle, 40) };
        if state == 0 || state == 0x80 {
            return Ok(Guard(handle));
        }
        if state != 258
            || stop.load(Ordering::Relaxed)
            || started.elapsed() > Duration::from_secs(40)
        {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
            anyhow::bail!("LR 正在处理另一个 RAW 请求或加载已取消，请稍后重试");
        }
    }
}
#[cfg(target_os = "macos")]
pub struct Guard {
    // Closing the descriptor releases flock. Keep the inode on disk: deleting
    // a lock pathname would let another editor lock a different inode.
    _file: std::fs::File,
}
#[cfg(target_os = "macos")]
pub fn acquire(path: &Path, stop: &AtomicBool) -> Result<Guard> {
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn flock(fd: std::ffi::c_int, operation: std::ffi::c_int) -> std::ffi::c_int;
    }
    const LOCK_EX: std::ffi::c_int = 2;
    const LOCK_NB: std::ffi::c_int = 4;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("raw-lock"))
        .context("无法打开 LR 显影邮箱锁")?;
    let started = Instant::now();
    loop {
        ensure!(
            !stop.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(40),
            "LR 正在处理另一个 RAW 请求或加载已取消，请稍后重试"
        );
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
            return Ok(Guard { _file: file });
        }
        let error = std::io::Error::last_os_error();
        ensure!(
            matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ),
            "无法锁定 LR 显影邮箱：{error}"
        );
        std::thread::sleep(Duration::from_millis(40));
    }
}
#[cfg(not(any(windows, target_os = "macos")))]
pub struct Guard {
    _guard: std::sync::MutexGuard<'static, ()>,
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn acquire(_path: &Path, stop: &AtomicBool) -> Result<Guard> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let started = Instant::now();
    loop {
        if let Ok(guard) = LOCK.try_lock() {
            return Ok(Guard { _guard: guard });
        }
        ensure!(
            !stop.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(40),
            "LR raw mailbox busy"
        );
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::MetadataExt,
        process::{Child, Command},
        sync::Arc,
        thread,
    };

    fn child(path: &Path, mode: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "raw_gate::tests::flock_child_process",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("FASTCULL_RAW_GATE_TEST_MODE", mode)
            .env("FASTCULL_RAW_GATE_TEST_PATH", path)
            .spawn()
            .unwrap()
    }
    fn wait(child: &mut Child) {
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "RAW lock child failed: {status}");
                return;
            }
            if started.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("RAW lock child timed out");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    fn flock_child_process() {
        let Ok(mode) = std::env::var("FASTCULL_RAW_GATE_TEST_MODE") else {
            return;
        };
        let path =
            std::path::PathBuf::from(std::env::var_os("FASTCULL_RAW_GATE_TEST_PATH").unwrap());
        fs::write(path.with_extension("trying"), "ready").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        if mode == "cancel" {
            let cancellation = stop.clone();
            let cancel = thread::spawn(move || {
                thread::sleep(Duration::from_millis(120));
                cancellation.store(true, Ordering::Relaxed);
            });
            let started = Instant::now();
            assert!(acquire(&path, &stop).is_err());
            assert!(started.elapsed() < Duration::from_secs(3));
            cancel.join().unwrap();
        } else {
            assert_eq!(mode, "acquire");
            let _guard = acquire(&path, &stop).unwrap();
            fs::write(path.with_extension("acquired"), "locked").unwrap();
        }
    }
    #[test]
    fn macos_mailbox_lock_excludes_an_actual_second_process_and_keeps_the_inode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.raw-request");
        let guard = acquire(&path, &AtomicBool::new(false)).unwrap();
        let inode = fs::metadata(path.with_extension("raw-lock")).unwrap().ino();
        let mut process = child(&path, "acquire");
        let started = Instant::now();
        while !path.with_extension("trying").is_file() {
            if started.elapsed() > Duration::from_secs(10) {
                let _ = process.kill();
                let _ = process.wait();
                panic!("RAW lock child did not start");
            }
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(160));
        assert!(
            !path.with_extension("acquired").exists(),
            "second process bypassed the mailbox lock"
        );
        drop(guard);
        wait(&mut process);
        assert!(path.with_extension("acquired").is_file());
        assert_eq!(
            fs::metadata(path.with_extension("raw-lock")).unwrap().ino(),
            inode
        );
        let _guard = acquire(&path, &AtomicBool::new(false)).unwrap();
    }
    #[test]
    fn macos_mailbox_lock_can_cancel_while_another_process_owns_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.raw-request");
        let _guard = acquire(&path, &AtomicBool::new(false)).unwrap();
        let mut process = child(&path, "cancel");
        wait(&mut process);
    }
}
