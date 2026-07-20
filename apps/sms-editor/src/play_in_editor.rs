use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

const WINDOW_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EditorHostWindow(isize);

impl EditorHostWindow {
    #[cfg(windows)]
    pub(super) fn from_frame(frame: &eframe::Frame) -> Result<Self, String> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = frame
            .window_handle()
            .map_err(|error| format!("cannot access the editor window handle: {error}"))?;
        match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Ok(Self(handle.hwnd.get())),
            _ => Err("Play in Editor is only available for the Windows editor window".to_string()),
        }
    }

    #[cfg(not(windows))]
    pub(super) fn from_frame(_frame: &eframe::Frame) -> Result<Self, String> {
        Err("Play in Editor is currently available on Windows only".to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmbeddedDolphinEvent {
    Attached,
    Exited,
}

pub(super) struct EmbeddedDolphinSession {
    child: Child,
    started_at: Instant,
    host: EditorHostWindow,
    #[cfg(windows)]
    window: Option<EmbeddedWindow>,
    terminated: bool,
}

impl EmbeddedDolphinSession {
    pub(super) fn new(child: Child, host: EditorHostWindow) -> Self {
        Self {
            child,
            started_at: Instant::now(),
            host,
            #[cfg(windows)]
            window: None,
            terminated: false,
        }
    }

    pub(super) fn poll(&mut self) -> Result<Option<EmbeddedDolphinEvent>, String> {
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| format!("cannot query Dolphin process state: {error}"))?
        {
            return Ok(Some(process_exit_event(status)));
        }

        #[cfg(windows)]
        {
            if let Some(window) = &mut self.window {
                if window.is_valid() {
                    window.poll_focus_handoff();
                    return Ok(None);
                }
                self.window = None;
            }

            if let Some(window) = find_process_window(self.child.id())? {
                self.window = Some(EmbeddedWindow::attach(window, self.host)?);
                return Ok(Some(EmbeddedDolphinEvent::Attached));
            }
        }

        if self.started_at.elapsed() >= WINDOW_DISCOVERY_TIMEOUT {
            return Err(
                "Dolphin started, but its render window did not appear within 30 seconds"
                    .to_string(),
            );
        }

        Ok(None)
    }

    pub(super) fn is_attached(&self) -> bool {
        #[cfg(windows)]
        {
            self.window.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    pub(super) fn set_viewport_bounds(
        &mut self,
        rect: egui::Rect,
        pixels_per_point: f32,
    ) -> Result<(), String> {
        #[cfg(windows)]
        if let Some(window) = &mut self.window {
            let bounds = viewport_bounds_in_pixels(rect, pixels_per_point);
            window.set_bounds(bounds)?;
        }
        Ok(())
    }

    pub(super) fn stop(mut self) -> Result<(), String> {
        self.terminate()
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.detach_window();
        if self.terminated {
            return Ok(());
        }
        if self
            .child
            .try_wait()
            .map_err(|error| format!("cannot query Dolphin process state: {error}"))?
            .is_some()
        {
            self.terminated = true;
            return Ok(());
        }
        self.child
            .kill()
            .map_err(|error| format!("failed to stop Dolphin: {error}"))?;
        self.child
            .wait()
            .map_err(|error| format!("failed to finish stopping Dolphin: {error}"))?;
        self.terminated = true;
        Ok(())
    }

    pub(super) fn detach_window(&mut self) {
        #[cfg(windows)]
        if let Some(mut window) = self.window.take() {
            let _ = window.detach();
        }
    }
}

impl Drop for EmbeddedDolphinSession {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn should_handoff_focus(
    pointer_over_embedded_window: bool,
    mouse_buttons_were_down: bool,
    mouse_buttons_down: bool,
    mouse_button_pressed: bool,
) -> bool {
    pointer_over_embedded_window
        && (mouse_button_pressed || (mouse_buttons_down && !mouse_buttons_were_down))
}

fn process_exit_event(_status: ExitStatus) -> EmbeddedDolphinEvent {
    EmbeddedDolphinEvent::Exited
}

fn viewport_bounds_in_pixels(rect: egui::Rect, pixels_per_point: f32) -> [i32; 4] {
    let scale = pixels_per_point.max(0.01);
    [
        (rect.left() * scale).round() as i32,
        (rect.top() * scale).round() as i32,
        (rect.width() * scale).round().max(1.0) as i32,
        (rect.height() * scale).round().max(1.0) as i32,
    ]
}

#[cfg(windows)]
mod windows_embedding {
    use std::ffi::c_void;
    use std::io;
    use std::ptr;

    use windows_sys::Win32::Foundation::{GetLastError, SetLastError, HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::System::Threading::AttachThreadInput;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SetFocus, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetCursorPos, GetParent, GetWindowLongPtrW, GetWindowRect,
        GetWindowThreadProcessId, IsChild, IsWindow, IsWindowVisible, SetForegroundWindow,
        SetParent, SetWindowLongPtrW, SetWindowPos, ShowWindow, WindowFromPoint, GWL_EXSTYLE,
        GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW, WS_CAPTION,
        WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_APPWINDOW, WS_EX_NOPARENTNOTIFY,
        WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
    };

    use super::{should_handoff_focus, EditorHostWindow};

    #[derive(Debug)]
    pub(super) struct EmbeddedWindow {
        handle: isize,
        host: isize,
        original_parent: isize,
        original_style: isize,
        original_ex_style: isize,
        original_rect: [i32; 4],
        last_bounds: Option<[i32; 4]>,
        mouse_buttons_were_down: bool,
        detached: bool,
    }

    impl EmbeddedWindow {
        pub(super) fn attach(handle: isize, host: EditorHostWindow) -> Result<Self, String> {
            let hwnd = as_hwnd(handle);
            let host_hwnd = as_hwnd(host.0);
            let original_style = window_long(hwnd, GWL_STYLE, "read Dolphin window style")?;
            let original_ex_style =
                window_long(hwnd, GWL_EXSTYLE, "read Dolphin extended window style")?;
            let original_parent = unsafe { GetParent(hwnd) } as isize;
            let original_rect = window_rect(hwnd)?;

            let embedded_style = ((original_style as u32
                & !(WS_POPUP
                    | WS_CAPTION
                    | WS_THICKFRAME
                    | WS_MINIMIZEBOX
                    | WS_MAXIMIZEBOX
                    | WS_SYSMENU))
                | WS_CHILD
                | WS_CLIPCHILDREN
                | WS_CLIPSIBLINGS) as isize;
            let embedded_ex_style =
                ((original_ex_style as u32 & !WS_EX_APPWINDOW) | WS_EX_NOPARENTNOTIFY) as isize;

            if let Err(error) = set_window_long(
                hwnd,
                GWL_STYLE,
                embedded_style,
                "apply embedded Dolphin window style",
            )
            .and_then(|()| {
                set_window_long(
                    hwnd,
                    GWL_EXSTYLE,
                    embedded_ex_style,
                    "apply embedded Dolphin extended window style",
                )
            })
            .and_then(|()| set_parent(hwnd, host_hwnd))
            {
                let _ = set_window_long(
                    hwnd,
                    GWL_STYLE,
                    original_style,
                    "restore Dolphin window style",
                );
                let _ = set_window_long(
                    hwnd,
                    GWL_EXSTYLE,
                    original_ex_style,
                    "restore Dolphin extended window style",
                );
                return Err(error);
            }

            let mut window = Self {
                handle,
                host: host.0,
                original_parent,
                original_style,
                original_ex_style,
                original_rect,
                last_bounds: None,
                mouse_buttons_were_down: false,
                detached: false,
            };
            if let Err(error) = window.refresh_frame() {
                let _ = window.detach();
                return Err(error);
            }
            unsafe {
                ShowWindow(hwnd, SW_SHOW);
            }
            focus_window(hwnd, host_hwnd);
            Ok(window)
        }

        pub(super) fn is_valid(&self) -> bool {
            unsafe { IsWindow(as_hwnd(self.handle)) != 0 }
        }

        pub(super) fn poll_focus_handoff(&mut self) {
            let (buttons_down, button_pressed) = mouse_button_state();
            let mouse_buttons_were_down = self.mouse_buttons_were_down;
            self.mouse_buttons_were_down = buttons_down;

            let mut cursor = POINT::default();
            if unsafe { GetCursorPos(&mut cursor) } == 0 {
                return;
            }
            let target = unsafe { WindowFromPoint(cursor) };
            let embedded = as_hwnd(self.handle);
            let pointer_over_embedded = !target.is_null()
                && (target == embedded || unsafe { IsChild(embedded, target) } != 0);
            if !should_handoff_focus(
                pointer_over_embedded,
                mouse_buttons_were_down,
                buttons_down,
                button_pressed,
            ) {
                return;
            }

            focus_window(target, as_hwnd(self.host));
        }

        pub(super) fn set_bounds(&mut self, bounds: [i32; 4]) -> Result<(), String> {
            if self.last_bounds == Some(bounds) {
                return Ok(());
            }
            let [x, y, width, height] = bounds;
            let result = unsafe {
                SetWindowPos(
                    as_hwnd(self.handle),
                    ptr::null_mut(),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )
            };
            if result == 0 {
                return Err(last_error("position embedded Dolphin window"));
            }
            self.last_bounds = Some(bounds);
            Ok(())
        }

        pub(super) fn detach(&mut self) -> Result<(), String> {
            if self.detached || !self.is_valid() {
                self.detached = true;
                return Ok(());
            }

            let hwnd = as_hwnd(self.handle);
            unsafe {
                ShowWindow(hwnd, SW_HIDE);
            }
            set_parent(hwnd, as_hwnd(self.original_parent))?;
            set_window_long(
                hwnd,
                GWL_STYLE,
                self.original_style,
                "restore Dolphin window style",
            )?;
            set_window_long(
                hwnd,
                GWL_EXSTYLE,
                self.original_ex_style,
                "restore Dolphin extended window style",
            )?;
            let [left, top, right, bottom] = self.original_rect;
            let result = unsafe {
                SetWindowPos(
                    hwnd,
                    ptr::null_mut(),
                    left,
                    top,
                    (right - left).max(1),
                    (bottom - top).max(1),
                    SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOZORDER,
                )
            };
            if result == 0 {
                return Err(last_error("restore Dolphin window position"));
            }
            unsafe {
                ShowWindow(hwnd, SW_SHOW);
            }
            self.detached = true;
            Ok(())
        }

        fn refresh_frame(&mut self) -> Result<(), String> {
            let result = unsafe {
                SetWindowPos(
                    as_hwnd(self.handle),
                    ptr::null_mut(),
                    0,
                    0,
                    1,
                    1,
                    SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOZORDER,
                )
            };
            if result == 0 {
                return Err(last_error("refresh embedded Dolphin window frame"));
            }
            Ok(())
        }
    }

    impl Drop for EmbeddedWindow {
        fn drop(&mut self) {
            let _ = self.detach();
        }
    }

    #[derive(Debug)]
    struct WindowSearch {
        process_id: u32,
        best_handle: isize,
        best_area: i64,
    }

    pub(super) fn find_process_window(process_id: u32) -> Result<Option<isize>, String> {
        let mut search = WindowSearch {
            process_id,
            best_handle: 0,
            best_area: 0,
        };
        let result = unsafe {
            EnumWindows(
                Some(enum_process_window),
                &mut search as *mut WindowSearch as LPARAM,
            )
        };
        if result == 0 {
            return Err(last_error("enumerate Dolphin windows"));
        }
        Ok((search.best_handle != 0).then_some(search.best_handle))
    }

    unsafe extern "system" fn enum_process_window(hwnd: HWND, lparam: LPARAM) -> i32 {
        let search = unsafe { &mut *(lparam as *mut WindowSearch) };
        let mut process_id = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut process_id);
        }
        if process_id != search.process_id
            || unsafe { IsWindowVisible(hwnd) } == 0
            || !unsafe { GetParent(hwnd) }.is_null()
        {
            return 1;
        }

        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            return 1;
        }
        let width = i64::from((rect.right - rect.left).max(0));
        let height = i64::from((rect.bottom - rect.top).max(0));
        let area = width * height;
        if width >= 320 && height >= 240 && area > search.best_area {
            search.best_handle = hwnd as isize;
            search.best_area = area;
        }
        1
    }

    fn window_rect(hwnd: HWND) -> Result<[i32; 4], String> {
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            return Err(last_error("read Dolphin window position"));
        }
        Ok([rect.left, rect.top, rect.right, rect.bottom])
    }

    fn window_long(hwnd: HWND, index: i32, action: &str) -> Result<isize, String> {
        unsafe {
            SetLastError(0);
        }
        let value = unsafe { GetWindowLongPtrW(hwnd, index) };
        if value == 0 && unsafe { GetLastError() } != 0 {
            Err(last_error(action))
        } else {
            Ok(value)
        }
    }

    fn set_window_long(hwnd: HWND, index: i32, value: isize, action: &str) -> Result<(), String> {
        unsafe {
            SetLastError(0);
        }
        let previous = unsafe { SetWindowLongPtrW(hwnd, index, value) };
        if previous == 0 && unsafe { GetLastError() } != 0 {
            Err(last_error(action))
        } else {
            Ok(())
        }
    }

    fn set_parent(hwnd: HWND, parent: HWND) -> Result<(), String> {
        unsafe {
            SetLastError(0);
        }
        let previous = unsafe { SetParent(hwnd, parent) };
        if previous.is_null() && unsafe { GetLastError() } != 0 {
            Err(last_error("attach Dolphin window to the editor"))
        } else {
            Ok(())
        }
    }

    fn mouse_button_state() -> (bool, bool) {
        let states = unsafe {
            [
                GetAsyncKeyState(i32::from(VK_LBUTTON)),
                GetAsyncKeyState(i32::from(VK_RBUTTON)),
                GetAsyncKeyState(i32::from(VK_MBUTTON)),
            ]
        };
        let buttons_down = states.iter().any(|state| (*state as u16) & 0x8000 != 0);
        let button_pressed = states.iter().any(|state| (*state as u16) & 1 != 0);
        (buttons_down, button_pressed)
    }

    fn focus_window(target: HWND, host: HWND) {
        if target.is_null() || host.is_null() {
            return;
        }

        let editor_thread = unsafe { GetWindowThreadProcessId(host, ptr::null_mut()) };
        let dolphin_thread = unsafe { GetWindowThreadProcessId(target, ptr::null_mut()) };
        if editor_thread == 0 || dolphin_thread == 0 {
            return;
        }

        unsafe {
            SetForegroundWindow(host);
        }
        if editor_thread == dolphin_thread {
            unsafe {
                SetFocus(target);
            }
            return;
        }

        if unsafe { AttachThreadInput(editor_thread, dolphin_thread, 1) } == 0 {
            return;
        }
        unsafe {
            SetFocus(target);
            AttachThreadInput(editor_thread, dolphin_thread, 0);
        }
    }

    fn last_error(action: &str) -> String {
        format!("{action}: {}", io::Error::last_os_error())
    }

    fn as_hwnd(value: isize) -> HWND {
        value as *mut c_void
    }
}

#[cfg(windows)]
use windows_embedding::{find_process_window, EmbeddedWindow};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn dropping_session_terminates_the_owned_process() {
        let temp = tempfile::tempdir().expect("create Play in Editor drop-test directory");
        let started = temp.path().join("started");
        let survived = temp.path().join("survived");
        let child = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[IO.File]::WriteAllText($env:SMS_EDITOR_DROP_TEST_STARTED, 'started'); Start-Sleep -Seconds 5; [IO.File]::WriteAllText($env:SMS_EDITOR_DROP_TEST_SURVIVED, 'survived')",
            ])
            .env("SMS_EDITOR_DROP_TEST_STARTED", &started)
            .env("SMS_EDITOR_DROP_TEST_SURVIVED", &survived)
            .spawn()
            .expect("launch disposable child process");
        let session = EmbeddedDolphinSession::new(child, EditorHostWindow(0));

        let deadline = Instant::now() + Duration::from_secs(10);
        while !started.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(started.exists(), "disposable child process did not start");

        drop(session);
        assert!(
            !survived.exists(),
            "dropping the session left its child process running"
        );
    }

    #[test]
    fn focus_handoff_requires_a_new_click_inside_the_embedded_window() {
        assert!(should_handoff_focus(true, false, true, false));
        assert!(should_handoff_focus(true, false, false, true));
        assert!(!should_handoff_focus(true, true, true, false));
        assert!(!should_handoff_focus(false, false, true, true));
    }

    #[test]
    fn viewport_bounds_scale_from_egui_points_to_native_pixels() {
        assert_eq!(
            viewport_bounds_in_pixels(
                egui::Rect::from_min_size(egui::pos2(12.25, 8.5), egui::vec2(640.0, 360.0)),
                1.5,
            ),
            [18, 13, 960, 540]
        );
    }

    #[test]
    fn viewport_bounds_never_collapse_to_zero_pixels() {
        assert_eq!(
            viewport_bounds_in_pixels(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::ZERO),
                2.0,
            ),
            [0, 0, 1, 1]
        );
    }
}
