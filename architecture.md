## Architecture: Local-First Contextual Voice Transcriptor

### Design Decisions

- **Tauri 2.0 over Electron**: Tauri uses a native OS webview and a Rust core — no bundled Chromium. Binary size drops from ~150MB to ~5MB. System-level APIs (hotkeys, accessibility, audio) are written in Rust where latency matters most.
- **Python sidecar for Whisper**: `faster-whisper` runs faster in Python/CTranslate2 than `whisper.cpp` on CPU. Tauri spawns it as a child process and communicates via a local Unix/TCP socket. This keeps the Rust core free of heavy Python GIL dependencies.
- **Hardware Acceleration**: GPU acceleration via CUDA 12 and cuDNN 9 using `base.en` with FP16 compute type.
- **Native Win32 SendInput**: Batched `KEYEVENTF_UNICODE` input simulation with zero scancode bugs, active modifier key untangling, and instant HWND focus restoration.
- **Event-driven pipeline**: All stages (capture → transcribe → clean → inject) communicate through Rust `tokio` async channels. No stage blocks another.
- **SvelteKit for UI**: Minimal bundle, reactive state without a virtual DOM, ideal for a tray-based status indicator.

---

### Key Interfaces

```rust
// Rust: pipeline message types
enum PipelineEvent {
    RecordingStarted,
    AudioCaptured(Vec<f32>),       // raw PCM samples
    TranscriptReady(String),       // raw Whisper output
    CleanedTextReady(String),      // LLM-cleaned output
    InjectionComplete,
    Error(PipelineError),
}

// Rust: context profile
struct ContextProfile {
    name: String,                  // "Gmail", "Slack", "VS Code"
    window_title_patterns: Vec<String>,
    system_prompt: String,
    vocabulary_hints: Vec<String>, // injected into Whisper context
}
```

---

### Data Flow

```mermaid
sequenceDiagram
    autonumber
    actor User
    participant OS as Windows OS
    participant Hotkey as Tauri Hotkey Listener
    participant Audio as audio.rs (cpal/VAD)
    participant Pipeline as main.rs (run_pipeline)
    participant Whisper as whisper_server.py (CUDA)
    participant Injector as injector.rs (SendInput)

    User->>Hotkey: Key Down (Hold Alt+Shift+V)
    Hotkey->>OS: Capture active HWND (previous_hwnd)
    Hotkey->>Audio: Start capture_blocking()

    User->>Hotkey: Key Up (Release Alt+Shift+V)
    Hotkey->>Audio: stop_rx fires -> cancel_flag = true (<20ms)
    Audio-->>Pipeline: AudioEvent::Final(samples)
    Pipeline->>Whisper: transcribe(final_samples) [Single call, zero queue backlog]
    Whisper-->>Pipeline: Raw Transcript (~540-600ms)
    Pipeline->>Pipeline: clean_voice_only() [Local regex <1ms]
    Pipeline->>Injector: InjectRequest { text, previous_hwnd }
    Injector->>OS: SetForegroundWindow(previous_hwnd) (Fast-path 10ms)
    Injector->>OS: SendInput(KEYEVENTF_UNICODE)
    OS-->>User: Text appears at cursor position!
```
