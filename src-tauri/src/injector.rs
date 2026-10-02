/// injector.rs — Injects cleaned text into the currently focused application.
///
/// Strategy (latency-optimised):
///   1. Check for held modifiers (Ctrl, Shift, Alt, Win) and wait or release them.
///   2. Ensure the OS focus returns to the user's app (hide overlay + poll foreground).
///   3. Primary injection: batched SendInput with KEYEVENTF_UNICODE for <= 300 chars.
///   4. Fallback injection: set clipboard via Windows API, Ctrl+V, restore async.
///

use std::time::{Duration, Instant};
use tauri::Manager;
use tokio::sync::mpsc;

use crate::pipeline::PipelineError;

const MAX_KEYBOARD_CHARS: usize = 300;
const FOCUS_TIMEOUT_MS: u64 = 40;
const MODIFIER_WAIT_TIMEOUT_MS: u64 = 40;
const CLIPBOARD_READY_DELAY_MS: u64 = 20;
const CLIPBOARD_RESTORE_DELAY_MS: u64 = 400;

pub struct InjectRequest {
    pub text: String,
    pub app_handle: Option<tauri::AppHandle>,
    pub previous_hwnd: Option<isize>,
}

pub fn init() -> Result<mpsc::UnboundedSender<InjectRequest>, PipelineError> {
    let (tx, mut rx) = mpsc::unbounded_channel::<InjectRequest>();

    std::thread::spawn(move || {
        log::info!("[injector] Worker thread started.");

        while let Some(req) = rx.blocking_recv() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if req.text.is_empty() {
                    return;
                }

                log::info!("[injector] Processing text injection ({} chars)", req.text.len());

                #[cfg(target_os = "windows")]
                inject_windows(req);

                #[cfg(not(target_os = "windows"))]
                log::warn!("[injector] macOS/Linux injection is unimplemented.");
            }));

            if result.is_err() {
                log::error!("[injector] Worker thread caught a panic during injection.");
            }
        }
        log::info!("[injector] Worker thread exiting (channel closed).");
    });

    Ok(tx)
}

#[cfg(target_os = "windows")]
fn inject_windows(req: InjectRequest) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

    let text = req.text;

    // ── 1. Modifiers ──
    let t_mod = Instant::now();
    let mut mods_cleared = false;
    while t_mod.elapsed() < Duration::from_millis(MODIFIER_WAIT_TIMEOUT_MS) {
        unsafe {
            let ctrl = GetAsyncKeyState(VK_CONTROL.0 as i32) as u16;
            let shift = GetAsyncKeyState(VK_SHIFT.0 as i32) as u16;
            let alt = GetAsyncKeyState(VK_MENU.0 as i32) as u16;
            let lwin = GetAsyncKeyState(VK_LWIN.0 as i32) as u16;
            let rwin = GetAsyncKeyState(VK_RWIN.0 as i32) as u16;
            if (ctrl & 0x8000) == 0
                && (shift & 0x8000) == 0
                && (alt & 0x8000) == 0
                && (lwin & 0x8000) == 0
                && (rwin & 0x8000) == 0
            {
                mods_cleared = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    if !mods_cleared {
        log::warn!("[injector] Modifiers held too long, sending explicit key-ups.");
        unsafe {
            let mut inputs = vec![];
            for vk in [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN] {
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: vk,
                            wScan: 0,
                            dwFlags: KEYEVENTF_KEYUP,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                });
            }
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }
    log::debug!("[injector] Modifier check took {:?}", t_mod.elapsed());

    // ── 2. Focus ──
    let t_focus = Instant::now();
    let mut target_hwnd = HWND::default();

    if let Some(app) = req.app_handle {
        if let Some(w) = app.get_webview_window("main") {
            if let Ok(h) = w.hwnd() {
                let tauri_hwnd = HWND(h.0 as _);
                let current_fg = unsafe { GetForegroundWindow() };

                if current_fg == tauri_hwnd {
                    log::debug!("[injector] Tauri window has focus. Hiding it.");
                    w.hide().ok();

                    if let Some(prev) = req.previous_hwnd {
                        let prev_hwnd = HWND(prev as _);
                        log::debug!("[injector] Fast-path focus to previous HWND {:?}", prev_hwnd);
                        unsafe { let _ = SetForegroundWindow(prev_hwnd); };
                        target_hwnd = prev_hwnd;
                    } else {
                        let f_start = Instant::now();
                        while f_start.elapsed() < Duration::from_millis(FOCUS_TIMEOUT_MS) {
                            let fg = unsafe { GetForegroundWindow() };
                            if fg != tauri_hwnd && !fg.is_invalid() {
                                target_hwnd = fg;
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(4));
                        }
                    }
                } else {
                    target_hwnd = current_fg;
                }
            }
        }
    } else {
        target_hwnd = unsafe { GetForegroundWindow() };
    }
    log::debug!("[injector] Focus check took {:?}", t_focus.elapsed());

    // ── 8. Primary Path: KEYEVENTF_UNICODE ──
    if text.chars().count() <= MAX_KEYBOARD_CHARS {
        let t_send = Instant::now();
        log::info!("[injector] Using KEYEVENTF_UNICODE path.");
        if send_unicode_string(&text) {
            log::debug!("[injector] Unicode injection took {:?}", t_send.elapsed());
            return;
        } else {
            log::warn!("[injector] KEYEVENTF_UNICODE failed or partial. Falling back to clipboard.");
        }
    }

    // ── Clipboard Fallback ──
    let t_clip = Instant::now();
    inject_via_clipboard_windows(&text, target_hwnd);
    log::debug!("[injector] Clipboard injection took {:?}", t_clip.elapsed());
}

#[cfg(target_os = "windows")]
fn send_unicode_string(text: &str) -> bool {
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
        VK_CONTROL,
    };

    let mut inputs = Vec::with_capacity(text.encode_utf16().count() * 2);
    for cu in text.encode_utf16() {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                    wScan: cu,
                    dwFlags: KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                    wScan: cu,
                    dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }

    if inputs.is_empty() {
        return true;
    }

    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent as usize != inputs.len() {
            let err = GetLastError();
            log::error!(
                "[injector] SendInput failed. Expected {}, sent {}. UIPI/elevation issue? Error: {:?}",
                inputs.len(),
                sent,
                err
            );

            // Release stuck Ctrl just in case
            let ki = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_CONTROL,
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            SendInput(&[ki], std::mem::size_of::<INPUT>() as i32);
            return false;
        }
    }
    true
}

#[cfg(target_os = "windows")]
fn inject_via_clipboard_windows(text: &str, target_hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_V,
    };

    // Save current clipboard contents
    let (saved_text, had_non_text) = get_clipboard_text_only(target_hwnd);
    if had_non_text {
        log::warn!("[injector] Clipboard contains non-text formats! Overwriting destroys them. \
            However, text > {} chars or Unicode path failed. We must overwrite anyway.", MAX_KEYBOARD_CHARS);
    }

    // Set our text into the clipboard
    if let Err(e) = set_clipboard(text, target_hwnd) {
        log::error!("[injector] Clipboard set failed: {}", e);
        return;
    }

    let seq_num = unsafe { GetClipboardSequenceNumber() };

    std::thread::sleep(Duration::from_millis(CLIPBOARD_READY_DELAY_MS));

    // Send Ctrl+V
    log::info!("[injector] Sending Ctrl+V via native SendInput.");
    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_CONTROL,
                    wScan: 0,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_V,
                    wScan: 0,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_V,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_CONTROL,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ];

    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent != 4 {
            log::error!(
                "[injector] SendInput failed. Expected 4, sent {}. UIPI issue? Error: {:?}",
                sent,
                GetLastError()
            );
            // Try to release Ctrl
            let _ = SendInput(&inputs[3..4], std::mem::size_of::<INPUT>() as i32);
        }
    }

    // Spawn task to restore clipboard asynchronously
    if let Some(prev) = saved_text {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(CLIPBOARD_RESTORE_DELAY_MS));
            let current_seq = unsafe { GetClipboardSequenceNumber() };
            if current_seq != seq_num {
                log::debug!("[injector] User copied something new. Not restoring clipboard.");
                return;
            }

            if let Err(e) = set_clipboard(&prev, windows::Win32::Foundation::HWND::default()) {
                log::warn!("[injector] Failed to restore clipboard: {}", e);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Native Clipboard
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
struct ClipboardGuard;

#[cfg(target_os = "windows")]
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::DataExchange::CloseClipboard();
        }
    }
}

#[cfg(target_os = "windows")]
fn get_clipboard_text_only(owner: windows::Win32::Foundation::HWND) -> (Option<String>, bool) {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{
        EnumClipboardFormats, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;

    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(owner) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    if !opened {
        log::warn!("[injector] get_clipboard: OpenClipboard failed");
        return (None, false);
    }
    let _guard = ClipboardGuard;

    let mut had_non_text = false;
    unsafe {
        let mut fmt = 0;
        loop {
            fmt = EnumClipboardFormats(fmt);
            if fmt == 0 {
                break;
            }
            if fmt != CF_UNICODETEXT {
                had_non_text = true;
            }
        }
    }

    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return (None, had_non_text);
        }

        let h = match GetClipboardData(CF_UNICODETEXT) {
            Ok(h) if !h.is_invalid() => h,
            _ => return (None, had_non_text),
        };

        let ptr = GlobalLock(HGLOBAL(h.0 as _));
        if ptr.is_null() {
            return (None, had_non_text);
        }

        let size_bytes = GlobalSize(HGLOBAL(h.0 as _));
        let len_wchars = size_bytes / 2;
        let slice = std::slice::from_raw_parts(ptr as *const u16, len_wchars);
        let null_pos = slice.iter().position(|&c| c == 0).unwrap_or(len_wchars);
        let text = String::from_utf16_lossy(&slice[..null_pos]);

        let _ = GlobalUnlock(HGLOBAL(h.0 as _));

        if text.is_empty() {
            (None, had_non_text)
        } else {
            (Some(text), had_non_text)
        }
    }
}

#[cfg(target_os = "windows")]
fn set_clipboard(
    text: &str,
    owner: windows::Win32::Foundation::HWND,
) -> Result<(), PipelineError> {
    use windows::Win32::Foundation::{HGLOBAL, GlobalFree};
    use windows::Win32::System::DataExchange::{
        EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    const CF_UNICODETEXT: u32 = 13;

    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(owner) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    if !opened {
        return Err(PipelineError::Injection("OpenClipboard failed".into()));
    }
    let _guard = ClipboardGuard;

    unsafe {
        if EmptyClipboard().is_err() {
            return Err(PipelineError::Injection("EmptyClipboard failed".into()));
        }

        // Exclude formats
        let exclude_fmt =
            RegisterClipboardFormatW(windows::core::w!("ExcludeClipboardContentFromMonitorProcessing"));
        if exclude_fmt != 0 {
            SetClipboardData(exclude_fmt, windows::Win32::Foundation::HANDLE::default()).ok();
        }
        let history_fmt = RegisterClipboardFormatW(windows::core::w!("CanIncludeInClipboardHistory"));
        if history_fmt != 0 {
            let hmem_hist = GlobalAlloc(GMEM_MOVEABLE, 4);
            if let Ok(h) = hmem_hist {
                let ptr = GlobalLock(HGLOBAL(h.0 as _));
                if !ptr.is_null() {
                    *(ptr as *mut u32) = 0;
                    let _ = GlobalUnlock(HGLOBAL(h.0 as _));
                    SetClipboardData(history_fmt, windows::Win32::Foundation::HANDLE(h.0 as _)).ok();
                } else {
                    let _ = GlobalFree(HGLOBAL(h.0 as _));
                }
            }
        }

        // Text
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let size_bytes = wide.len() * 2;

        let hmem = GlobalAlloc(GMEM_MOVEABLE, size_bytes)
            .map_err(|e| PipelineError::Injection(format!("GlobalAlloc failed: {e}")))?;

        let ptr = GlobalLock(HGLOBAL(hmem.0 as _));
        if ptr.is_null() {
            let _ = GlobalFree(HGLOBAL(hmem.0 as _));
            return Err(PipelineError::Injection("GlobalLock returned null".into()));
        }

        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr as *mut u16, wide.len());
        let _ = GlobalUnlock(HGLOBAL(hmem.0 as _));

        if let Err(e) = SetClipboardData(
            CF_UNICODETEXT,
            windows::Win32::Foundation::HANDLE(hmem.0 as _),
        ) {
            let _ = GlobalFree(HGLOBAL(hmem.0 as _));
            return Err(PipelineError::Injection(format!("SetClipboardData failed: {e}")));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_threshold() {
        let short = "a".repeat(300);
        assert!(short.chars().count() <= MAX_KEYBOARD_CHARS);
        let long = "a".repeat(301);
        assert!(long.chars().count() > MAX_KEYBOARD_CHARS);
    }
}
