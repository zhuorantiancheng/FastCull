//! Spawn the GUI without inheriting LR's pipes or changing caller-owned handles.
use anyhow::Result;
use std::path::Path;

#[cfg(windows)]
pub fn spawn(executable: &Path, snapshot: &Path, manifest: Option<&Path>) -> Result<()> {
    spawn_mode(executable, snapshot, manifest, "--open-state-owned")
}

pub fn spawn_bridge(executable: &Path, snapshot: &Path) -> Result<()> {
    spawn_mode(executable, snapshot, None, "--bridge-list-owned")
}

#[cfg(windows)]
fn spawn_mode(
    executable: &Path,
    snapshot: &Path,
    manifest: Option<&Path>,
    mode: &str,
) -> Result<()> {
    use anyhow::{Context, ensure};
    use std::{
        ffi::OsStr,
        os::windows::{
            ffi::OsStrExt,
            io::{FromRawHandle, OwnedHandle},
        },
        ptr::null,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION,
        STARTF_USESTDHANDLES, STARTUPINFOW,
    };

    let mut application: Vec<u16> = executable.as_os_str().encode_wide().collect();
    ensure!(!application.contains(&0), "executable contains NUL");
    application.push(0);
    let mut args = vec![
        executable.as_os_str(),
        OsStr::new(mode),
        snapshot.as_os_str(),
    ];
    if let Some(manifest) = manifest {
        args.extend([OsStr::new("--session"), manifest.as_os_str()]);
    }
    let mut command = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if i != 0 {
            command.push(b' ' as u16);
        }
        append_argument(&mut command, &arg.encode_wide().collect::<Vec<_>>())?;
    }
    command.push(0);
    ensure!(command.len() <= 32767, "GUI command line is too long");
    // The GUI writes to its own log file and requires no standard streams.
    // Explicit NULL streams also prevent implicit stdio copying by Windows.
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESTDHANDLES,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: all buffers are live and terminated, command is writable, startup
    // has the correct size. No handles are passed or inherited. NULL environment
    // and current directory preserve the caller's environment and working dir.
    let ok = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            null(),
            null(),
            &startup,
            &mut process,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("spawn detached GUI");
    }
    // Transfer this user-triggered launch's foreground permission to the GUI.
    // Windows can deny permission; that must not turn a successful spawn into
    // a failed launch. The GUI makes a single focus request on its first frame.
    let allowed = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(process.dwProcessId)
    };
    log::info!("startup foreground permission granted={}", allowed != 0);
    // SAFETY: CreateProcessW succeeded and returned two uniquely owned handles.
    // Closing them releases resources without waiting for or stopping the GUI.
    unsafe {
        drop(OwnedHandle::from_raw_handle(process.hThread));
        drop(OwnedHandle::from_raw_handle(process.hProcess));
    }
    Ok(())
}

#[cfg(windows)]
fn append_argument(out: &mut Vec<u16>, arg: &[u16]) -> Result<()> {
    anyhow::ensure!(!arg.contains(&0), "GUI argument contains NUL");
    // CRT parsing: double trailing backslashes inside quotes; before a literal
    // quote emit 2*n+1 backslashes. Work in UTF-16 to preserve Windows paths.
    out.push(b'"' as u16);
    let mut slashes = 0;
    for &ch in arg {
        if ch == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        let count = if ch == b'"' as u16 {
            slashes * 2 + 1
        } else {
            slashes
        };
        out.extend(std::iter::repeat_n(b'\\' as u16, count));
        out.push(ch);
        slashes = 0;
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    Ok(())
}

#[cfg(not(windows))]
pub fn spawn(executable: &Path, snapshot: &Path, manifest: Option<&Path>) -> Result<()> {
    spawn_mode(executable, snapshot, manifest, "--open-state-owned")
}

#[cfg(not(windows))]
fn spawn_mode(
    executable: &Path,
    snapshot: &Path,
    manifest: Option<&Path>,
    mode: &str,
) -> Result<()> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(executable);
    command
        .arg(mode)
        .arg(snapshot)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(manifest) = manifest {
        command.arg("--session").arg(manifest);
    }
    command.spawn()?;
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn quotes_preserve_spaces_unicode_quotes_and_backslashes() {
        for (arg, expected) in [
            ("", "\"\""),
            ("测试 图片", "\"测试 图片\""),
            (r"C:\folder with space\", r#""C:\folder with space\\""#),
            ("a\"b", "\"a\\\"b\""),
            ("a\\\"b", "\"a\\\\\\\"b\""),
        ] {
            let mut out = Vec::new();
            append_argument(&mut out, &arg.encode_utf16().collect::<Vec<_>>()).unwrap();
            assert_eq!(String::from_utf16(&out).unwrap(), expected);
        }
        let mut out = Vec::new();
        let unpaired_surrogate = [0xd800];
        append_argument(&mut out, &unpaired_surrogate).unwrap();
        assert_eq!(out, [b'"' as u16, 0xd800, b'"' as u16]);
    }

    #[test]
    fn rejects_embedded_nul_and_reports_creation_failure() {
        assert!(append_argument(&mut Vec::new(), &[0]).is_err());
        let dir = tempfile::tempdir().unwrap();
        let error =
            spawn(&dir.path().join("missing.exe"), Path::new("snapshot"), None).unwrap_err();
        assert!(format!("{error:#}").contains("spawn detached GUI"));
    }
}
