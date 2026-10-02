#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Pipeline Optimization: Suppress backlog accumulation by filtering intermediate partial frames.
// main.rs — Tauri app entry point.
// Registers tray icon, global hotkey, and orchestrates the full pipeline.

mod audio;
mod config;
mod context_detector;
mod context_router;
mod injector;
mod llm_cleaner;
mod pipeline;
mod transcriber;

use pipeline::{ContextProfile, DictationMode};
use reqwest::Client;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};
use tauri_plugin_global_shortcut::ShortcutState;
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;
use tokio::net::TcpStream as TokioTcpStream;
use tokio::sync::{mpsc, oneshot, Mutex};

// ---------------------------------------------------------------------------
// Shared app state
// ---------------------------------------------------------------------------

struct AppState {
    config: config::AppConfig,
    profiles: Vec<ContextProfile>,
    http_client: Client,
    /// Current dictation mode — toggled by the UI
    mode: DictationMode,
    /// Sender to cancel an in-progress recording
    stop_tx: Option<mpsc::Sender<()>>,
    /// Whether a pipeline cycle is currently running
    recording: bool,
    /// Last successfully injected transcript (for UI)
    last_transcript: String,
    /// Channel to send text to the Enigo background worker thread
    injector_tx: mpsc::UnboundedSender<injector::InjectRequest>,
    /// Handle keeping the whisper sidecar alive
    #[allow(dead_code)]
    sidecar_child: Option<CommandChild>,
    /// HWND of the window that was active before hotkey was pressed
    previous_hwnd: Option<isize>,
}

// ---------------------------------------------------------------------------
// Tauri commands (called from SvelteKit UI)
// ---------------------------------------------------------------------------

#[tauri::command]
async fn get_status(state: tauri::State<'_, Arc<Mutex<AppState>>>) -> Result<String, String> {
    let s = state.lock().await;
    Ok(if s.recording { "recording" } else { "idle" }.into())
}

#[tauri::command]
async fn get_last_transcript(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<String, String> {
    Ok(state.lock().await.last_transcript.clone())
}

#[tauri::command]
async fn set_mode(
    mode: String,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<(), String> {
    let new_mode = match mode.as_str() {
        "voice_only" => DictationMode::VoiceOnly,
        "context"    => DictationMode::Context,
        other        => return Err(format!("Unknown mode: {other}")),
    };
    state.lock().await.mode = new_mode;
    log::info!("Dictation mode set to: {mode}");
    Ok(())
}

#[tauri::command]
async fn get_mode(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<String, String> {
    let mode = match state.lock().await.mode {
        DictationMode::VoiceOnly => "voice_only",
        DictationMode::Context   => "context",
    };
    Ok(mode.into())
}

#[tauri::command]
async fn start_recording(
    app: AppHandle,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<(), String> {
    let state_clone = state.inner().clone();
    let window_title = context_detector::active_window_title().unwrap_or_default();

    // Set up the stop channel atomically before spawning
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    {
        let mut s = state_clone.lock().await;
        if s.recording {
            return Ok(()); // Already recording
        }
        s.recording = true;
        s.stop_tx = Some(stop_tx);
    }

    tauri::async_runtime::spawn(async move {
        run_pipeline(app, state_clone, window_title, stop_rx).await;
    });
    Ok(())
}


// ---------------------------------------------------------------------------
// Pipeline runner
// ---------------------------------------------------------------------------

/// run_pipeline assumes recording=true and stop_tx are already set in AppState.
/// The Pressed hotkey handler is responsible for the setup so that the
/// Released handler always sees valid state regardless of scheduling order.
async fn run_pipeline(
    app: AppHandle,
    state: Arc<Mutex<AppState>>,
    window_title: String,
    mut stop_rx: mpsc::Receiver<()>,  // passed in — set up by caller before w.show()
) {
    let mode = {
        let s = state.lock().await;
        s.mode.clone()
    };

    log::info!("Pipeline started | mode={:?} | window='{window_title}'", mode);
    app.emit("pipeline-event", "RecordingStarted").ok();

    let (cfg, profiles, client) = {
        let s = state.lock().await;
        (s.config.clone(), s.profiles.clone(), s.http_client.clone())
    };

    let profile = context_router::route(&window_title, &profiles);
    log::info!("Context profile: {} (mode={:?})", profile.name, mode);

    loop {
        let (chunk_stop_tx, chunk_stop_rx) = oneshot::channel::<()>();
        
        let mut audio_rx = audio::capture_until_silence(&cfg, chunk_stop_rx);
        
        tokio::spawn(async move {
            let _ = stop_rx.recv().await;
            let _ = chunk_stop_tx.send(());
        });

        while let Some(audio_event) = audio_rx.recv().await {
            let (samples, sample_rate, is_final) = match audio_event {
                audio::AudioEvent::Error(e) => {
                    log::error!("Audio capture error: {e}");
                    app.emit("pipeline-event", format!("Error:{e}")).ok();
                    break;
                }
                audio::AudioEvent::Partial(s, r) => (s, r, false),
                audio::AudioEvent::Final(s, r) => (s, r, true),
            };

            if samples.len() < (sample_rate as f32 * 0.4) as usize {
                if is_final { break; } else { continue; }
            }
            
            // In Push-to-Talk, never run heavy sequential Whisper passes on partial chunks.
            // Transcribe exactly once on final to guarantee sub-400ms injection.
            if !is_final {
                continue;
            }

            app.emit("pipeline-event", "AudioCaptured").ok();

            match transcriber::transcribe(&cfg, &samples, &profile, sample_rate).await {
                Err(e) => {
                    log::error!("Transcription error: {e}");
                    app.emit("pipeline-event", format!("Error:{e}")).ok();
                }
                Ok(raw_transcript) => {
                    if raw_transcript.trim().is_empty() {
                        if is_final { break; } else { continue; }
                    }
                    
                    if !is_final {
                        app.emit("pipeline-event", format!("PartialTranscriptReady:{raw_transcript}")).ok();
                        continue;
                    }
                    
                    app.emit("pipeline-event", format!("FinalTranscriptReady:{raw_transcript}")).ok();

                    let injector_tx = state.lock().await.injector_tx.clone();

                    let text_to_inject = if mode == DictationMode::VoiceOnly {
                        match llm_cleaner::clean_voice_only(&client, &raw_transcript).await {
                            Ok(cleaned) => cleaned,
                            Err(e) => { log::error!("Voice-only clean error: {e}"); raw_transcript.clone() }
                        }
                    } else if mode == DictationMode::Context {
                        if let Err(e) = llm_cleaner::clean_stream(&client, &raw_transcript, &profile, &window_title, injector_tx.clone()).await {
                            log::error!("LLM stream error: {e}");
                        }
                        break;
                    } else {
                        raw_transcript.clone()
                    };

                    if !text_to_inject.trim().is_empty() {
                        log::info!("[pipeline] Sending injection request: '{}'",
                            &text_to_inject[..text_to_inject.len().min(60)]);
                        let _ = injector_tx.send(injector::InjectRequest {
                            text: text_to_inject.clone(),
                            app_handle: Some(app.clone()),
                            previous_hwnd: state.lock().await.previous_hwnd,
                        });
                        state.lock().await.last_transcript = text_to_inject;
                    }
                    
                    app.emit("pipeline-event", "InjectionComplete").ok();
                    break;
                }
            }
        }
        break; // break outer loop once audio stream finishes
    }

    {
        let mut s = state.lock().await;
        s.recording = false;
        s.stop_tx = None;
    }
    app.emit("pipeline-event", "Idle").ok();
}

// ---------------------------------------------------------------------------
// App setup
// ---------------------------------------------------------------------------

fn main() {
    env_logger::init();

    // Load .env from the project root (dev) or app bundle directory (production)
    let env_candidates = [
        // Dev: project root, two levels above src-tauri/src/
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(".env"),
        // Production: next to the executable
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(".env")))
            .unwrap_or_default(),
    ];
    for path in &env_candidates {
        if path.exists() {
            // Simple .env parser — read KEY=VALUE lines and set them as env vars
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.starts_with('#') || line.is_empty() { continue; }
                    if let Some((key, val)) = line.split_once('=') {
                        let key = key.trim();
                        let val = val.trim().trim_matches('"');
                        // Don't override values already set in the real environment
                        if std::env::var(key).is_err() {
                            std::env::set_var(key, val);
                        }
                    }
                }
                log::info!("Loaded .env from: {}", path.display());
            }
            break;
        }
    }

    log::info!("Voice Dictation starting...");

    let cfg = config::load();
    let profiles = ContextProfile::built_ins();
    let http_client = Client::new();

    let state = Arc::new(Mutex::new(AppState {
        config: cfg.clone(),
        profiles,
        http_client: http_client.clone(),
        mode: DictationMode::VoiceOnly,
        stop_tx: None,
        recording: false,
        last_transcript: String::new(),
        injector_tx: injector::init().expect("Failed to initialize injector"),
        sidecar_child: None,
        previous_hwnd: None,
    }));

    let state_clone = Arc::clone(&state);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcuts(["alt+shift+v"])
                .unwrap()
                .with_handler(move |app, shortcut, event| {
                    let matches_hotkey = shortcut.matches(
                        tauri_plugin_global_shortcut::Modifiers::ALT
                            | tauri_plugin_global_shortcut::Modifiers::SHIFT,
                        tauri_plugin_global_shortcut::Code::KeyV,
                    );

                    if !matches_hotkey {
                        return;
                    }

                    let app_clone = app.clone();
                    let state = app.state::<Arc<tokio::sync::Mutex<AppState>>>().inner().clone();

                    match event.state() {
                        // ── KEY DOWN: start recording (Push-to-Talk mode) ──────────────────
                        ShortcutState::Pressed => {
                            tauri::async_runtime::spawn(async move {
                                // ── ATOMIC SETUP: claim the recording slot immediately ──────
                                // Must happen BEFORE showing the window so that any Released
                                // event that fires concurrently always sees recording=true
                                // and a valid stop_tx to send on.
                                let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
                                let already_recording = {
                                    let mut s = state.lock().await;
                                    if s.recording {
                                        true
                                    } else {
                                        s.recording = true;
                                        s.stop_tx = Some(stop_tx);
                                        false
                                    }
                                };
                                if already_recording {
                                    return; // Drop the channel — stop_tx goes with it
                                }

                                // Capture window title while user's app still has focus
                                log::info!("Hotkey DOWN: starting recording (push-to-talk)");
                                let window_title = context_detector::active_window_title()
                                    .unwrap_or_default();
                                log::info!("Captured window title: '{window_title}'");

                                #[cfg(target_os = "windows")]
                                {
                                    let active_hwnd = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as isize };
                                    state.lock().await.previous_hwnd = Some(active_hwnd);
                                }

                                if let Some(w) = app_clone.get_webview_window("main") {
                                    w.set_always_on_top(true).ok();
                                    w.show().ok();
                                    w.unminimize().ok();
                                }

                                run_pipeline(app_clone, state, window_title, stop_rx).await;
                            });
                        }

                        // ── KEY UP: stop recording immediately (the PTT signal) ────────────
                        // This eliminates the ~800ms VAD silence-timeout dead time.
                        ShortcutState::Released => {
                            tauri::async_runtime::spawn(async move {
                                // Gate on stop_tx.take() — not on is_recording.
                                // This handles multiple spurious UP events correctly:
                                // only the FIRST UP that finds a Some(stop_tx) acts;
                                // subsequent UPs from modifier-key releases get None and exit.
                                let stop_tx = {
                                    let mut s = state.lock().await;
                                    s.stop_tx.take()
                                };
                                if let Some(tx) = stop_tx {
                                    log::info!("Hotkey UP: stopping recording (push-to-talk release)");
                                    let _ = tx.try_send(());
                                    app_clone.emit("pipeline-event", "RecordingStopped").ok();
                                    if let Some(w) = app_clone.get_webview_window("main") {
                                        w.hide().ok();
                                    }
                                }
                                // If stop_tx was None, this is a duplicate UP event — ignore
                            });
                        }

                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .manage(state_clone)
        .setup(move |app| {
            let app_state = app.state::<Arc<tokio::sync::Mutex<AppState>>>().inner().clone();
            
            // ---- Sidecar auto-launch ----
            {
                let sidecar_port = cfg.sidecar_port;
                let state_for_sidecar = app.state::<Arc<tokio::sync::Mutex<AppState>>>().inner().clone();
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    // Try launching as a Tauri-managed sidecar first
                    let child = app_handle
                        .shell()
                        .sidecar("whisper_server")
                        .and_then(|cmd| cmd.spawn());

                    let child = match child {
                        Ok((mut rx, child)) => {
                            // Forward sidecar stdout/stderr to Rust log
                            tauri::async_runtime::spawn(async move {
                                use tauri_plugin_shell::process::CommandEvent;
                                while let Some(event) = rx.recv().await {
                                    match event {
                                        CommandEvent::Stdout(line) => {
                                            log::info!("[sidecar] {}", String::from_utf8_lossy(&line));
                                        }
                                        CommandEvent::Stderr(line) => {
                                            log::warn!("[sidecar] {}", String::from_utf8_lossy(&line));
                                        }
                                        CommandEvent::Error(e) => {
                                            log::error!("[sidecar error] {e}");
                                        }
                                        _ => {}
                                    }
                                }
                            });
                            log::info!("Whisper sidecar spawned (exe)");
                            Some(child)
                        }
                        Err(e) => {
                            log::warn!("Sidecar exe not found ({e}), trying python fallback...");
                            // Dev fallback: run the .py directly with absolute path
                            let py_path = concat!(
                                env!("CARGO_MANIFEST_DIR"),
                                "/../sidecar/whisper_server.py"
                            );
                            match app_handle
                                .shell()
                                .command("python")
                                .args([py_path])
                                .spawn()
                            {
                                Ok((mut rx, child)) => {
                                    tauri::async_runtime::spawn(async move {
                                        use tauri_plugin_shell::process::CommandEvent;
                                        while let Some(event) = rx.recv().await {
                                            match event {
                                                CommandEvent::Stdout(line) => {
                                                    log::info!("[sidecar] {}", String::from_utf8_lossy(&line));
                                                }
                                                CommandEvent::Stderr(line) => {
                                                    log::warn!("[sidecar] {}", String::from_utf8_lossy(&line));
                                                }
                                                _ => {}
                                            }
                                        }
                                    });
                                    log::info!("Whisper sidecar spawned (python fallback)");
                                    Some(child)
                                }
                                Err(e2) => {
                                    log::error!("Failed to launch whisper sidecar: {e2}");
                                    None
                                }
                            }
                        }
                    };

                    // Store the child handle so it stays alive with the app
                    state_for_sidecar.lock().await.sidecar_child = child;

                    // Wait for the sidecar TCP port to become reachable (up to 30s)
                    wait_for_sidecar(sidecar_port, 30).await;
                });
            }

            // Pre-warm Gemini in the background
            tauri::async_runtime::spawn(async move {
                let state_guard = app_state.lock().await;
                let prewarm = state_guard.config.prewarm_on_start;
                let client_clone = state_guard.http_client.clone();
                drop(state_guard); // Release lock before async work
                if prewarm {
                    llm_cleaner::prewarm(&client_clone).await;
                }
            });

            // ---- Tray Icon ----
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "Open", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            let icon = app.default_window_icon()
                .ok_or("No default icon found")?
                .clone();
            TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            w.show().ok();
                            w.set_focus().ok();
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                        if let Some(app) = tray.app_handle().get_webview_window("main") {
                            app.show().ok();
                            app.set_focus().ok();
                        }
                    }
                })
                .build(app)?;

            log::info!("App ready and running. Waiting for hotkey...");


            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_last_transcript,
            set_mode,
            get_mode,
            start_recording
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ---------------------------------------------------------------------------
// Sidecar readiness helper
// ---------------------------------------------------------------------------

/// Poll `port` on 127.0.0.1 every 500ms until it accepts a connection (ready)
/// or `timeout_secs` elapses. Logs status either way.
async fn wait_for_sidecar(port: u16, timeout_secs: u64) {
    let addr = format!("127.0.0.1:{port}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    log::info!("Waiting for whisper sidecar on {addr}...");
    loop {
        if std::time::Instant::now() >= deadline {
            log::error!("Sidecar did not become ready within {timeout_secs}s — transcription will fail");
            return;
        }
        match TokioTcpStream::connect(&addr).await {
            Ok(_) => {
                log::info!("Whisper sidecar is ready on {addr}");
                return;
            }
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}
