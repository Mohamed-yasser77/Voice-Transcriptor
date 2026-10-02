/// context_detector.rs — Reads the active OS window title.
///
/// Windows: uses Win32 GetForegroundWindow + GetWindowTextW
/// macOS:   uses NSWorkspace activeApplication (via osascript fallback)
/// Linux:   uses xdotool getactivewindow getwindowname

use crate::pipeline::PipelineError;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the title of the currently focused window, or an empty string.
pub fn active_window_title() -> Result<String, PipelineError> {
    detect_title().map_err(|e| PipelineError::ContextDetection(e.to_string()))
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn detect_title() -> Result<String, String> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == std::ptr::null_mut() {
            return Ok(String::new());
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        Ok(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

// ---------------------------------------------------------------------------
// macOS implementation
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn detect_title() -> Result<String, String> {
    use std::process::Command;
    // AppleScript: get name of front window of (first application process whose frontmost is true)
    let output = Command::new("osascript")
        .args([
            "-e",
            r#"tell application "System Events"
                 set fw to first application process whose frontmost is true
                 get name of first window of fw
               end tell"#,
        ])
        .output()
        .map_err(|e| e.to_string())?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// ---------------------------------------------------------------------------
// Linux implementation
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn detect_title() -> Result<String, String> {
    use std::process::Command;
    let win_id = Command::new("xdotool")
        .args(["getactivewindow"])
        .output()
        .map_err(|e| e.to_string())?;
    let id = String::from_utf8_lossy(&win_id.stdout).trim().to_string();

    let title = Command::new("xdotool")
        .args(["getwindowname", &id])
        .output()
        .map_err(|e| e.to_string())?;

    Ok(String::from_utf8_lossy(&title.stdout).trim().to_string())
}
