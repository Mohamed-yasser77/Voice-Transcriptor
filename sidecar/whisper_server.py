# Performance Optimization: CUDA 12.4 + cuDNN 9 FP16 acceleration (~500ms latency)
#!/usr/bin/env python3
"""
whisper_server.py — faster-whisper TCP socket server (speech-to-text only).

Tauri sidecar: receives base64-encoded raw PCM audio over a TCP socket,
transcribes it using faster-whisper, and returns the raw transcript as JSON.

LLM post-processing (punctuation, filler-word removal, domain correction)
is handled by the Rust layer (llm_cleaner.rs → Gemini API), NOT here.
This keeps the sidecar lean and focused on its single responsibility: STT.

Architecture:
  Audio → [whisper_server.py: STT] → raw text → [llm_cleaner.rs: Gemini] → clean text

Protocol (newline-delimited JSON):
  Request:  { "audio_b64": "<base64 PCM f32le>", "sample_rate": 16000, "profile": "default" }
  Response: { "transcript": "<raw whisper text>", "duration_ms": 340 }
  Error:    { "error": "reason" }

Latency tunings applied vs original:
  - Model pre-warmed at startup (eliminates cold-start JIT delay on first use)
  - vad_filter=False (VAD is already done by Rust audio.rs — no double processing)
  - beam_size=1, best_of=1, temperature=0.0 (fastest greedy decoding)
  - int8 compute type (CPU-optimised quantisation)

Note: Binary framing (removing base64) is a planned optimization but requires
rebuilding this file into a new exe with PyInstaller to replace the sidecar binary.
"""

import asyncio
import base64
import json
import logging
import os
import struct
import sys
import time
from pathlib import Path

# ---------------------------------------------------------------------------
# Windows: Register NVIDIA CUDA & cuDNN DLL directories from pip packages
# ---------------------------------------------------------------------------
if sys.platform == "win32":
    for pkg in ["nvidia.cublas", "nvidia.cudnn", "nvidia.cuda_nvrtc"]:
        try:
            import importlib.util
            spec = importlib.util.find_spec(pkg)
            if spec and spec.submodule_search_locations:
                for loc in spec.submodule_search_locations:
                    for sub in ["bin", "lib"]:
                        dll_dir = os.path.join(loc, sub)
                        if os.path.isdir(dll_dir):
                            os.add_dll_directory(dll_dir)
                            os.environ["PATH"] = dll_dir + os.pathsep + os.environ.get("PATH", "")
        except Exception:
            pass

import numpy as np
from dotenv import load_dotenv
import ctranslate2
from faster_whisper import WhisperModel

# ---------------------------------------------------------------------------
# Load environment variables — search .env from sidecar/ up to project root
# ---------------------------------------------------------------------------
_here = Path(__file__).parent
for _candidate in [_here / ".env", _here.parent / ".env"]:
    if _candidate.exists():
        load_dotenv(_candidate)
        break

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [whisper_server] %(levelname)s %(message)s",
)
log = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Configuration (override via .env or environment variables)
# ---------------------------------------------------------------------------
HOST = "127.0.0.1"
PORT = int(os.getenv("SIDECAR_PORT", "9877"))
DEFAULT_MODEL = "small.en"
MODEL_SIZE = os.getenv("WHISPER_MODEL", DEFAULT_MODEL)

# Detect CUDA availability
cuda_available = False
try:
    if ctranslate2.get_cuda_device_count() > 0:
        cuda_available = True
except Exception as e:
    log.warning(f"CUDA detection error: {e}")

default_device = "cuda" if cuda_available else "cpu"
default_compute = "float16" if cuda_available else "int8"

DEVICE = os.getenv("WHISPER_DEVICE", default_device)
COMPUTE_TYPE = os.getenv("WHISPER_COMPUTE_TYPE", default_compute)

# ---------------------------------------------------------------------------
# Whisper Model — loaded once at startup, shared across all connections
# ---------------------------------------------------------------------------
log.info(f"Loading Whisper model: {MODEL_SIZE} ({DEVICE}/{COMPUTE_TYPE})")
try:
    model = WhisperModel(MODEL_SIZE, device=DEVICE, compute_type=COMPUTE_TYPE)
except Exception as e:
    log.error(f"Failed to load Whisper on {DEVICE} ({e}). Falling back to 'small.en'...")
    try:
        MODEL_SIZE = "small.en"
        model = WhisperModel(MODEL_SIZE, device=DEVICE, compute_type=COMPUTE_TYPE)
    except Exception as e2:
        log.error(f"Fallback on {DEVICE} failed ({e2}). Falling back to CPU/int8...")
        DEVICE = "cpu"
        COMPUTE_TYPE = "int8"
        model = WhisperModel("small.en", device=DEVICE, compute_type=COMPUTE_TYPE, cpu_threads=4)

log.info("Model loaded. Running warm-up inference...")

# Pre-warm: run one dummy inference so the first real transcription is instant.
# This moves JIT compilation and weight paging to startup, not first use.
_warmup_audio = np.zeros(16000, dtype=np.float32)  # 1 second of silence
list(model.transcribe(
    _warmup_audio,
    language="en",
    beam_size=1,
    best_of=1,
    temperature=0.0,
    vad_filter=False,
)[0])
log.info(f"Warm-up complete. Whisper server ready on {DEVICE}.")


# ---------------------------------------------------------------------------
# Vocabulary hints per profile & user custom vocabulary (Upgrade 1)
# ---------------------------------------------------------------------------
_vocab_cache: str = ""
_vocab_mtime: float = 0.0

def load_custom_vocabulary() -> str:
    """Load user custom words from vocabulary.txt with automatic mtime reloading."""
    global _vocab_cache, _vocab_mtime
    candidates = [
        _here.parent / "vocabulary.txt",
        _here / "vocabulary.txt",
        Path.home() / ".config" / "voice-dictation" / "vocabulary.txt",
    ]
    for p in candidates:
        if p.is_file():
            try:
                mtime = p.stat().st_mtime
                if mtime != _vocab_mtime:
                    terms = []
                    for line in p.read_text(encoding="utf-8").splitlines():
                        line = line.strip()
                        if line and not line.startswith("#"):
                            terms.append(line)
                    _vocab_cache = ", ".join(terms)
                    _vocab_mtime = mtime
                    log.info(f"Loaded {len(terms)} custom vocabulary terms from {p}")
                return _vocab_cache
            except Exception as ex:
                log.warning(f"Could not read vocabulary file {p}: {ex}")
    return _vocab_cache

_PROFILE_VOCAB_HINTS: dict[str, str] = {
    "gmail": "Email correspondence: regards, sincerely, attachments, schedule, follow-up, meeting invite.",
    "slack": "Team chat: standup, PR, merge, deployment, blocker, sprint, slack, channel, thread, ping, sync.",
    "vscode": (
        "Programming terms: async, await, mutex, semaphore, tokio, reqwest, serde, "
        "PyTorch, CUDA, Kubernetes, microservices, REST API, GraphQL, OAuth2, JWT, "
        "Rust, Svelte, Vite, Tauri, TypeScript, Python, backend, frontend."
    ),
    "medical": (
        "Medical terminology: tachycardia, bradycardia, hypertension, myocardial infarction, "
        "electrocardiogram, auscultation, bronchodilator, corticosteroid, anaphylaxis, hematocrit."
    ),
    "legal": (
        "Legal terminology: plaintiff, defendant, deposition, habeas corpus, injunction, tort, "
        "indemnification, affidavit, subpoena, jurisprudence, litigant."
    ),
    "whatsapp": "Chat message: hey, thanks, call me, check this out, see you soon, sounds good.",
    "default": "Proper nouns and common terms: Mohamed Yasser, Kuz, Antigravity.",
}

def normalize_profile(profile: str) -> str:
    """Robust fuzzy matching for profile strings (handles 'VS Code', 'visual-studio-code', etc.)."""
    p = profile.lower().replace(" ", "").replace("-", "").replace("_", "")
    if "vscode" in p or "code" in p or "cursor" in p:
        return "vscode"
    if "gmail" in p or "mail" in p or "outlook" in p:
        return "gmail"
    if "slack" in p or "discord" in p or "teams" in p:
        return "slack"
    if "whatsapp" in p or "telegram" in p:
        return "whatsapp"
    if "medical" in p:
        return "medical"
    if "legal" in p:
        return "legal"
    return "default"

def build_initial_prompt(profile: str, custom_words: str | None = None) -> str | None:
    """Construct a high-context initial_prompt to bias Whisper beam search."""
    norm_profile = normalize_profile(profile)
    profile_hints = _PROFILE_VOCAB_HINTS.get(norm_profile, "")
    user_words = load_custom_vocabulary()

    parts = []
    if user_words:
        parts.append(f"Proper nouns: {user_words}")
    if custom_words and custom_words.strip():
        parts.append(custom_words.strip())
    if profile_hints:
        parts.append(profile_hints)

    if not parts:
        return None
    return ". ".join(parts) + "."


# ---------------------------------------------------------------------------
# Audio helpers
# ---------------------------------------------------------------------------

def decode_pcm(audio_b64: str, sample_rate: int) -> np.ndarray:
    """Decode base64 raw PCM float32-LE bytes into a numpy float32 array."""
    raw_bytes = base64.b64decode(audio_b64)
    num_samples = len(raw_bytes) // 4  # float32 = 4 bytes
    samples = struct.unpack(f"{num_samples}f", raw_bytes)
    audio = np.array(samples, dtype=np.float32)

    # Resample to 16000 Hz if needed (faster-whisper expects 16kHz)
    if sample_rate != 16000:
        import torchaudio
        import torch
        waveform = torch.from_numpy(audio).unsqueeze(0)
        resampler = torchaudio.transforms.Resample(orig_freq=sample_rate, new_freq=16000)
        audio = resampler(waveform).squeeze().numpy()

    return audio


# ---------------------------------------------------------------------------
# Transcription
# ---------------------------------------------------------------------------

def transcribe(audio: np.ndarray, initial_prompt: str | None) -> str:
    """Run faster-whisper inference and return concatenated raw transcript.

    Tuning choices:
      - vad_filter=False: VAD already done by Rust audio.rs — no double-pay.
      - beam_size=1:      Greedy decoding — fastest, minimal quality loss.
      - temperature=0.0:  Deterministic output.
    """
    segments, _ = model.transcribe(
        audio,
        language="en",
        task="transcribe",
        vad_filter=False,              # VAD already done upstream by Rust
        initial_prompt=initial_prompt,
        beam_size=1,                   # fastest greedy decoding
        best_of=1,
        temperature=0.0,
        condition_on_previous_text=False,
    )
    return " ".join(seg.text.strip() for seg in segments).strip()


# ---------------------------------------------------------------------------
# TCP server
# ---------------------------------------------------------------------------

async def handle_client(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    peer = writer.get_extra_info("peername")
    log.info(f"Connection from {peer}")

    try:
        while True:
            line = await reader.readline()
            if not line:
                break

            t0 = time.perf_counter()
            try:
                req = json.loads(line.decode().strip())
                audio_b64 = req["audio_b64"]
                sample_rate = int(req.get("sample_rate", 16000))
                profile = req.get("profile", "default")
                custom_words = req.get("custom_words", None)

                audio = decode_pcm(audio_b64, sample_rate)
                initial_prompt = build_initial_prompt(profile, custom_words)
                transcript = transcribe(audio, initial_prompt)

                duration_ms = int((time.perf_counter() - t0) * 1000)
                response = {"transcript": transcript, "duration_ms": duration_ms}
                log.info(f"Transcribed [{profile}] in {duration_ms}ms: '{transcript[:80]}'")

            except (KeyError, ValueError, json.JSONDecodeError) as e:
                response = {"error": str(e)}
                log.warning(f"Bad request from {peer}: {e}")

            writer.write((json.dumps(response) + "\n").encode())
            await writer.drain()

    except asyncio.IncompleteReadError:
        pass
    finally:
        log.info(f"Disconnected: {peer}")
        writer.close()
        await writer.wait_closed()


async def main() -> None:
    server = await asyncio.start_server(handle_client, HOST, PORT, limit=1048576 * 50)
    addrs = ", ".join(str(s.getsockname()) for s in server.sockets)
    log.info(f"Whisper server listening on {addrs}")
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
