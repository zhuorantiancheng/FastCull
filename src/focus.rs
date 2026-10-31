//! Verify the native foreground window instead of relying on cached egui focus.
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_millis(1500);
const INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
struct Policy {
    done: bool,
    attempts: u8,
    last: Option<Duration>,
}
impl Policy {
    fn attempt(
        &mut self,
        elapsed: Duration,
        visible: bool,
        foreground: bool,
        same_origin: bool,
    ) -> bool {
        if self.done {
            return false;
        }
        if foreground || elapsed >= WINDOW || !same_origin {
            self.done = true;
            return false;
        }
        if !visible || self.attempts >= 3 || self.last.is_some_and(|last| elapsed - last < INTERVAL)
        {
            return false;
        }
        self.last = Some(elapsed);
        self.attempts += 1;
        true
    }
}

pub struct StartupFocus {
    started: Instant,
    policy: Policy,
    #[cfg(windows)]
    origin: (usize, u32),
}

/// Disable the parent native window for the complete child-editor lifetime.
/// Drop also restores it when the parent changes workspace or exits early.
#[derive(Default)]
pub struct ParentLock {
    window: Option<usize>,
}
impl ParentLock {
    pub fn update(&mut self, frame: &eframe::Frame, locked: bool) {
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if locked && self.window.is_none() {
                if let Ok(handle) = frame.window_handle()
                    && let RawWindowHandle::Win32(handle) = handle.as_raw()
                {
                    let window = handle.hwnd.get() as usize;
                    unsafe {
                        windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                            window as _,
                            0,
                        );
                    }
                    self.window = Some(window);
                }
            } else if !locked {
                self.restore();
            }
        }
        #[cfg(not(windows))]
        let _ = (frame, locked);
    }
    fn restore(&mut self) {
        if let Some(window) = self.window.take() {
            #[cfg(windows)]
            unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(window as _, 1);
            }
            #[cfg(not(windows))]
            let _ = window;
        }
    }
}
impl Drop for ParentLock {
    fn drop(&mut self) {
        self.restore();
    }
}
impl StartupFocus {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            policy: Policy::default(),
            #[cfg(windows)]
            origin: native::foreground(),
        }
    }
    pub fn update(&mut self, ctx: &eframe::egui::Context, frame: &eframe::Frame) {
        if self.policy.done {
            return;
        }
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let Ok(handle) = frame.window_handle() else {
                return;
            };
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                return;
            };
            let hwnd = handle.hwnd.get() as usize;
            let visible = native::visible(hwnd);
            let foreground = native::foreground();
            let is_target = foreground.0 == hwnd;
            // Do not keep activating after the user switches to another app.
            let same_origin = foreground.1 == self.origin.1 || is_target || foreground.0 == 0;
            let was_done = self.policy.done;
            if self
                .policy
                .attempt(self.started.elapsed(), visible, is_target, same_origin)
            {
                let (activated, attached) = native::activate(hwnd, self.origin);
                log::info!(
                    "startup native-focus attempt={} visible={} input_attached={} foreground_verified={}",
                    self.policy.attempts,
                    visible,
                    attached,
                    activated
                );
                if activated {
                    self.policy.done = true;
                }
            } else if !was_done && self.policy.done {
                log::info!(
                    "startup native-focus stopped foreground_verified={} same_origin={} attempts={}",
                    is_target,
                    same_origin,
                    self.policy.attempts
                );
            }
        }
        #[cfg(not(windows))]
        {
            let _ = frame;
            let focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));
            if self
                .policy
                .attempt(self.started.elapsed(), true, focused, true)
            {
                ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Focus);
            }
        }
        if !self.policy.done {
            ctx.request_repaint_after(INTERVAL);
        }
    }
}

#[cfg(windows)]
mod native {
    use windows_sys::Win32::{
        Foundation::HWND,
        System::Threading::{AttachThreadInput, GetCurrentThreadId},
        UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId, HWND_TOP, IsWindowVisible,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowPos,
        },
    };
    pub fn foreground() -> (usize, u32) {
        unsafe {
            let window = GetForegroundWindow();
            let mut pid = 0;
            GetWindowThreadProcessId(window, &mut pid);
            (window as usize, pid)
        }
    }
    pub fn visible(window: usize) -> bool {
        unsafe { IsWindowVisible(window as HWND) != 0 }
    }
    struct InputLink(u32, u32);
    impl Drop for InputLink {
        fn drop(&mut self) {
            // Always detach before leaving this single activation call.
            unsafe {
                AttachThreadInput(self.0, self.1, 0);
            }
        }
    }
    pub fn activate(window: usize, origin: (usize, u32)) -> (bool, bool) {
        let hwnd = window as HWND;
        // HWND_TOP changes ordinary z-order, never makes the window topmost.
        unsafe {
            SetWindowPos(
                hwnd,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            SetForegroundWindow(hwnd);
        }
        if foreground().0 == window {
            return (true, false);
        }
        // If direct activation is denied, briefly share the original launcher's
        // input queue. Only use it while that same process is foreground; LR's
        // progress/menu windows may change HWND during an otherwise normal open.
        // This avoids relying on a cached "focused" flag for a background window.
        let mut attached = false;
        let current = foreground();
        if current.0 != 0 && origin.1 != 0 && current.1 == origin.1 {
            unsafe {
                let thread = GetCurrentThreadId();
                let owner = GetWindowThreadProcessId(current.0 as HWND, std::ptr::null_mut());
                if owner != 0 && owner != thread && AttachThreadInput(thread, owner, 1) != 0 {
                    let _link = InputLink(thread, owner);
                    attached = true;
                    SetForegroundWindow(hwnd);
                }
            }
        }
        (foreground().0 == window, attached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
    #[test]
    fn hidden_window_does_not_spend_the_only_activation_chance() {
        let mut p = Policy::default();
        assert!(!p.attempt(ms(0), false, false, true));
        assert!(!p.attempt(ms(100), false, false, true));
        assert!(p.attempt(ms(200), true, false, true));
        assert_eq!(p.attempts, 1);
    }
    #[test]
    fn retries_only_within_startup_and_stops_after_native_confirmation() {
        let mut p = Policy::default();
        assert!(p.attempt(ms(0), true, false, true));
        assert!(!p.attempt(ms(50), true, false, true));
        assert!(p.attempt(ms(100), true, false, true));
        assert!(!p.attempt(ms(200), true, true, true));
        assert!(!p.attempt(ms(300), true, false, true));
    }
    #[test]
    fn user_switch_and_attempt_limits_stop_future_activation() {
        let mut p = Policy::default();
        assert!(!p.attempt(ms(0), true, false, false));
        assert!(!p.attempt(ms(100), true, false, true));
        let mut p = Policy::default();
        for t in [0, 100, 200] {
            assert!(p.attempt(ms(t), true, false, true));
        }
        assert!(!p.attempt(ms(300), true, false, true));
        assert!(!p.attempt(ms(1500), true, false, true));
        assert!(p.done);
    }
}
