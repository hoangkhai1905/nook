//! "Is a fullscreen app or game in front?"

use crate::monitor::{self, Rect};
use crate::window::Hwnd;
use windows::Win32::Foundation::RECT;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{
    QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetClassNameW, GetForegroundWindow, GetShellWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, IsZoomed, WS_CAPTION,
};

/// Whether a window that covers the notch's monitor is fullscreen. A
/// maximized window with a title bar is not (on a monitor without a taskbar
/// it covers everything too), but browser video fullscreen is a maximized
/// window *without* one, so that still counts.
fn covering_window_is_fullscreen(zoomed: bool, has_caption: bool) -> bool {
    !(zoomed && has_caption)
}

/// True when the foreground window covers the whole monitor the notch is on
/// (borderless fullscreen, including a maximized window without a title bar)
/// or the shell reports exclusive D3D fullscreen / presentation mode.
/// Ordinary maximized windows, windows on other monitors, the desktop and our
/// own windows (e.g. the draw-on-screen overlay) never count.
pub fn foreground_is_fullscreen(own: Hwnd) -> bool {
    // SAFETY: read-only shell query.
    if let Ok(state) = unsafe { SHQueryUserNotificationState() }
        && (state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE)
    {
        return true;
    }
    // SAFETY: read-only window queries; handles may be stale, which only
    // makes the calls fail.
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg == own.get() || fg == GetShellWindow() {
            return false;
        }
        if !IsWindowVisible(fg).as_bool() {
            return false;
        }
        let has_caption = GetWindowLongPtrW(fg, GWL_STYLE) as u32 & WS_CAPTION.0 == WS_CAPTION.0;
        if !covering_window_is_fullscreen(IsZoomed(fg).as_bool(), has_caption) {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return false;
        }
        let mut class = [0u16; 64];
        let n = GetClassNameW(fg, &mut class).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        if matches!(class.as_str(), "WorkerW" | "Progman" | "Shell_TrayWnd") {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let (Ok(mon), Ok(ours)) = (
            monitor::of_window(Hwnd::from_raw(fg.0 as isize)),
            monitor::of_window(own),
        ) else {
            return false;
        };
        mon.bounds == ours.bounds && Rect::from(r).covers(&mon.bounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximized_counts_only_without_title_bar() {
        // Borderless game (TFT): not maximized, no title bar.
        assert!(covering_window_is_fullscreen(false, false));
        // Edge/YouTube video fullscreen: maximized, no title bar.
        assert!(covering_window_is_fullscreen(true, false));
        // Ordinary maximized window (e.g. on a monitor without a taskbar).
        assert!(!covering_window_is_fullscreen(true, true));
        // A captioned window sized over the monitor but not maximized.
        assert!(covering_window_is_fullscreen(false, true));
    }
}
