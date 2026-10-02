/// audio.rs — Microphone capture with Voice Activity Detection.
///
/// Uses `cpal` for cross-platform audio input and `webrtc-vad` to
/// automatically stop recording when the user goes silent.
///
/// Flow:
///   1. Open default input device at 16kHz / mono / f32
///   2. Stream samples into a ring buffer
///   3. Run VAD on each 30ms frame
///   4. Once `vad_silence_ms` of consecutive silence is detected → stop
///   5. Return the captured Vec<f32> to the pipeline

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, StreamConfig,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use webrtc_vad::{Vad, VadMode};

use crate::config::AppConfig;
use crate::pipeline::PipelineError;

use std::sync::atomic::{AtomicBool, Ordering};

const TARGET_RATE: u32 = 16_000;

pub enum AudioEvent {
    #[allow(dead_code)]
    Partial(Vec<f32>, u32),
    Final(Vec<f32>, u32),
    Error(PipelineError),
}

/// Capture microphone audio until silence is detected or `stop_rx` fires.
/// Streams events back via an unbounded channel.
pub fn capture_until_silence(
    cfg: &AppConfig,
    stop_rx: oneshot::Receiver<()>,
) -> mpsc::UnboundedReceiver<AudioEvent> {
    let silence_threshold = cfg.vad_silence_ms;
    let max_secs = cfg.max_record_secs;
    
    let (tx, rx) = mpsc::unbounded_channel::<AudioEvent>();
    let tx_clone = tx.clone();
    
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let cancel_clone = Arc::clone(&cancel_flag);

    std::thread::spawn(move || {
        if let Err(e) = capture_blocking(silence_threshold, max_secs, tx_clone, cancel_clone) {
            let _ = tx.send(AudioEvent::Error(e));
        }
    });
    
    let cancel_async = Arc::clone(&cancel_flag);
    tokio::spawn(async move {
        let _ = stop_rx.await;
        cancel_async.store(true, Ordering::SeqCst);
    });

    rx
}

// ---------------------------------------------------------------------------
// Blocking capture implementation (runs on a dedicated OS thread)
// ---------------------------------------------------------------------------

fn capture_blocking(
    silence_ms: u64,
    max_secs: u64,
    tx: mpsc::UnboundedSender<AudioEvent>,
    cancel_flag: Arc<AtomicBool>,
) -> Result<(), PipelineError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| PipelineError::AudioCapture(
            "No input device found -- check microphone permissions".into(),
        ))?;

    log::info!("Audio device: {}", device.name().unwrap_or_default());

    let captured: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(
        Vec::with_capacity(TARGET_RATE as usize * 120),
    ));
    let captured_clone = Arc::clone(&captured);

    let (stream, _) = open_stream(&device, captured_clone)
        .map_err(|e| PipelineError::AudioCapture(format!("Cannot open audio stream: {e}")))?;

    stream
        .play()
        .map_err(|e| PipelineError::AudioCapture(format!("Cannot start stream: {e}")))?;

    let frame_samples: usize = (TARGET_RATE as usize * 30) / 1000;
    let max_frames = (max_secs * 1000 / 30) as usize;
    let silence_frames = (silence_ms / 30).max(1) as usize;

    let mut vad = Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate16kHz, VadMode::Quality);
    let mut consecutive_silent = 0usize;
    let mut processed_samples = 0usize;

    for frame_idx in 0..max_frames {
        // Audio chunk accumulation loop
        if cancel_flag.load(Ordering::Relaxed) {
            log::info!("[audio] PTT key release signaled — stopping audio capture at frame {frame_idx}.");
            break;
        }

        std::thread::sleep(Duration::from_millis(30));

        if cancel_flag.load(Ordering::Relaxed) {
            log::info!("[audio] PTT key release signaled — stopping audio capture at frame {frame_idx}.");
            break;
        }

        let available = captured.lock().map(|b| b.len()).unwrap_or(0);
        let end = processed_samples + frame_samples;
        if end > available {
            continue;
        }

        let frame_i16: Vec<i16> = captured
            .lock()
            .map(|buf| {
                buf[processed_samples..end]
                    .iter()
                    .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                    .collect()
            })
            .unwrap_or_default();

        processed_samples = end;

        match vad.is_voice_segment(&frame_i16) {
            Ok(true)  => {
                consecutive_silent = 0;
            },
            Ok(false) => {
                consecutive_silent += 1;
            },
            Err(e)    => log::warn!("VAD error on frame {frame_idx}: {e:?}"),
        }

        if consecutive_silent >= silence_frames && frame_idx > 10 {
            log::info!("VAD: silence detected after {} frames, stopping.", frame_idx);
            break;
        }
    }

    drop(stream);

    let samples = match Arc::try_unwrap(captured) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc)  => arc.lock().map(|g| g.clone()).unwrap_or_default(),
    };

    let _ = tx.send(AudioEvent::Final(samples, TARGET_RATE));
    Ok(())
}

// ---------------------------------------------------------------------------
// Multi-strategy stream opener
// ---------------------------------------------------------------------------
// Windows WASAPI drivers often only expose I16 at 44100/48000 Hz stereo.
// We try three strategies in order, converting all output to TARGET_RATE mono f32.

fn open_stream(
    device: &cpal::Device,
    captured: Arc<Mutex<Vec<f32>>>,
) -> Result<(cpal::Stream, u32), Box<dyn std::error::Error + Send + Sync>> {
    let supported: Vec<_> = device.supported_input_configs()?.collect();

    // Strategy 1: F32, mono, exactly TARGET_RATE
    for range in &supported {
        if range.sample_format() == SampleFormat::F32
            && range.channels() == 1
            && range.min_sample_rate().0 <= TARGET_RATE
            && range.max_sample_rate().0 >= TARGET_RATE
        {
            let config = StreamConfig {
                channels: 1,
                sample_rate: cpal::SampleRate(TARGET_RATE),
                buffer_size: cpal::BufferSize::Default,
            };
            let cap = Arc::clone(&captured);
            let stream = device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    if let Ok(mut buf) = cap.lock() {
                        buf.extend_from_slice(data);
                    }
                },
                |e| log::error!("Stream error: {e}"),
                None,
            )?;
            log::info!("Audio: F32 mono 16kHz (ideal path)");
            return Ok((stream, TARGET_RATE));
        }
    }

    // Strategy 2: F32, any rate/channels -> mono mix + resample
    for range in &supported {
        if range.sample_format() == SampleFormat::F32 {
            let native_rate = range.max_sample_rate().0;
            let channels = range.channels() as usize;
            let config = StreamConfig {
                channels: range.channels(),
                sample_rate: cpal::SampleRate(native_rate),
                buffer_size: cpal::BufferSize::Default,
            };
            let cap = Arc::clone(&captured);
            let ratio = native_rate as f64 / TARGET_RATE as f64;
            let stream = device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|ch| ch.iter().sum::<f32>() / channels as f32)
                        .collect();
                    let resampled = downsample(&mono, ratio);
                    if let Ok(mut buf) = cap.lock() {
                        buf.extend_from_slice(&resampled);
                    }
                },
                |e| log::error!("Stream error: {e}"),
                None,
            )?;
            log::info!("Audio: F32 {native_rate}Hz {channels}ch resampled to {TARGET_RATE}Hz mono");
            return Ok((stream, native_rate));
        }
    }

    // Strategy 3: I16 (most common on Windows WASAPI) -> convert + resample
    let default_cfg = device.default_input_config()?;
    let native_rate = default_cfg.sample_rate().0;
    let channels = default_cfg.channels() as usize;
    let config: StreamConfig = default_cfg.into();
    let cap = Arc::clone(&captured);
    let ratio = native_rate as f64 / TARGET_RATE as f64;

    let stream = device.build_input_stream(
        &config,
        move |data: &[i16], _| {
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|ch| {
                    let sum: f32 = ch.iter().map(|&s| s as f32 / i16::MAX as f32).sum();
                    sum / channels as f32
                })
                .collect();
            let resampled = downsample(&mono, ratio);
            if let Ok(mut buf) = cap.lock() {
                buf.extend_from_slice(&resampled);
            }
        },
        |e| log::error!("Stream error: {e}"),
        None,
    )?;

    log::info!("Audio: I16 {native_rate}Hz {channels}ch converted+resampled to {TARGET_RATE}Hz mono");
    Ok((stream, native_rate))
}

// ---------------------------------------------------------------------------
// Linear-interpolation downsampler
// ---------------------------------------------------------------------------

/// Downsample `input` by `ratio` (ratio = native_rate / target_rate).
fn downsample(input: &[f32], ratio: f64) -> Vec<f32> {
    if (ratio - 1.0).abs() < 1e-6 {
        return input.to_vec();
    }
    let out_len = (input.len() as f64 / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 * ratio;
        let idx = src as usize;
        let frac = (src - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}
