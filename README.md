# Voice Transcriptor 🎙️⚡

An ultra-low-latency, local-first voice dictation system built with **Tauri 2.0 (Rust)**, **SvelteKit**, and a hardware-accelerated **faster-whisper (CUDA FP16)** sidecar.

Dictate text into any active application (VS Code, Chrome, Slack, Discord, Terminal) with sub-600ms latency at the press of a global shortcut.

---

## 🚀 Key Features

* **Sub-600ms Turnaround Latency**: From key-release to text injection on your screen.
* **Hardware-Accelerated STT**: Runs OpenAI Whisper (`base.en`) on local NVIDIA GPUs using **CTranslate2 (CUDA FP16)**.
* **Instant Push-to-Talk (PTT)**: Hardware key-release triggers immediate audio cutoff via an atomic cancellation flag, eliminating silence detection delays.
* **Native Windows Injection**: Batched Win32 `SendInput` (`KEYEVENTF_UNICODE`) with zero scancode bugs, active modifier key untangling, and instant HWND focus restoration.
* **Privacy-First & Offline**: Voice-only mode processes audio locally without sending voice or text data over the internet.
* **Protected Clipboard Fallback**: Uses Windows API flags (`ExcludeClipboardContentFromMonitorProcessing`) to protect long-text paste from third-party clipboard monitors.

---

## 🏗️ Architecture

```mermaid
flowchart LR
    A[Global Hotkey<br/>Alt+Shift+V] --> B[cpal Audio Capture]
    B -->|Atomic PTT Cutoff| C[Raw PCM Stream]
    C -->|Local TCP Socket| D[Python Sidecar<br/>faster-whisper CUDA]
    D -->|Raw Transcript| E[Local Regex Cleaner]
    E -->|Cleaned Text| F[Win32 SendInput<br/>KEYEVENTF_UNICODE]
    F --> G[Target Application Cursor]
```

---

## 🛠️ Tech Stack

* **Core Shell**: [Tauri 2.0](https://tauri.app/) (Rust)
* **Audio Engine**: [`cpal`](https://github.com/RustAudio/cpal) (16kHz / mono / f32) + [`webrtc-vad`](https://crates.io/crates/webrtc-vad)
* **Speech-to-Text**: [`faster-whisper`](https://github.com/SYSTRAN/faster-whisper) (CTranslate2, CUDA 12, cuDNN 9)
* **Desktop UI**: [SvelteKit](https://svelte.dev/) + [Vite](https://vitejs.dev/) + TypeScript
* **OS Automation**: Win32 API via Microsoft's official [`windows`](https://crates.io/crates/windows) crate (v0.58)

---

## ⚙️ Requirements

* **OS**: Windows 10 / 11 (64-bit)
* **GPU (Optional, Recommended)**: NVIDIA GPU with CUDA support for sub-600ms inference (falls back to multi-core CPU automatically)
* **Runtime**:
  * [Rust](https://rustup.rs/) (1.78+)
  * [Node.js](https://nodejs.org/) (18+)
  * Python 3.10+ with `faster-whisper`

---

## 🚀 Getting Started

### 1. Install Dependencies
```bash
# Frontend
npm install

# Python Sidecar
pip install faster-whisper ctranslate2 nvidia-cublas-cu12 nvidia-cudnn-cu12
```

### 2. Configure Environment
Copy `.env.example` to `.env`:
```env
WHISPER_DEVICE=cuda
WHISPER_COMPUTE_TYPE=float16
WHISPER_MODEL=base.en
SIDECAR_PORT=9877
```

### 3. Run Development Server
```bash
npm run tauri dev
```

### 4. Usage
* **Push-to-Talk**: Hold `Alt + Shift + V`, speak your thought, and release. The text appears immediately at your active cursor.

---

## 📄 Documentation

* [Architecture Blueprint](./architecture.md)
* [Final Engineering & Latency Report](./final_report.md)

---

## 📜 License
MIT License. Created by Mohamed Yasser.
