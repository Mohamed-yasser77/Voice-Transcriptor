/// transcriber.rs — TCP socket client for the Python faster-whisper sidecar.
///
/// Sends base64-encoded raw PCM to whisper_server.py and returns the transcript.
///
/// Note: Binary framing (removing base64) is a planned optimization but requires
/// rebuilding the compiled whisper_server.exe sidecar. Until then, base64 is used
/// to remain compatible with the existing exe.

use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::config::AppConfig;
use crate::pipeline::{ContextProfile, PipelineError};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct TranscribeRequest<'a> {
    audio_b64: String,
    sample_rate: u32,
    profile: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_words: Option<String>,
}

#[derive(Deserialize)]
struct TranscribeResponse {
    transcript: Option<String>,
    duration_ms: Option<u64>,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Send PCM audio to the Whisper sidecar and receive the raw transcript.
pub async fn transcribe(
    cfg: &AppConfig,
    audio: &[f32],
    profile: &ContextProfile,
    sample_rate: u32,
) -> Result<String, PipelineError> {
    // Encode f32 samples as raw little-endian bytes then base64
    let raw_bytes: Vec<u8> = audio
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&raw_bytes);

    let lower_profile = profile.name.to_lowercase();
    let custom_words = if profile.vocabulary_hints.is_empty() {
        None
    } else {
        Some(profile.vocabulary_hints.join(", "))
    };

    let request = TranscribeRequest {
        audio_b64,
        sample_rate,
        profile: lower_profile.as_str(),
        custom_words,
    };

    let addr = format!("127.0.0.1:{}", cfg.sidecar_port);
    let stream = TcpStream::connect(&addr).await.map_err(|e| {
        PipelineError::Transcription(format!("Cannot connect to sidecar at {addr}: {e}"))
    })?;

    let (reader, mut writer) = stream.into_split();

    // Send request as newline-delimited JSON
    let line = serde_json::to_string(&request)
        .map_err(|e| PipelineError::Transcription(e.to_string()))?
        + "\n";
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| PipelineError::Transcription(e.to_string()))?;

    // Read response line
    let mut buf_reader = BufReader::new(reader);
    let mut response_line = String::new();
    buf_reader
        .read_line(&mut response_line)
        .await
        .map_err(|e| PipelineError::Transcription(e.to_string()))?;

    let response: TranscribeResponse = serde_json::from_str(response_line.trim())
        .map_err(|e| PipelineError::Transcription(format!("Bad sidecar response: {e}")))?;

    if let Some(err) = response.error {
        return Err(PipelineError::Transcription(err));
    }

    let transcript = response.transcript.unwrap_or_default();
    if let Some(ms) = response.duration_ms {
        log::info!("Transcribed in {ms}ms: '{}'", &transcript[..transcript.len().min(80)]);
    }

    Ok(transcript)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transcribe_request_serialization_with_custom_words() {
        let req = TranscribeRequest {
            audio_b64: "dGVzdA==".into(),
            sample_rate: 16000,
            profile: "vscode",
            custom_words: Some("Mohamed Yasser, Kuz, Rust".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"profile\":\"vscode\""));
        assert!(json.contains("\"custom_words\":\"Mohamed Yasser, Kuz, Rust\""));
    }

    #[test]
    fn test_transcribe_request_serialization_without_custom_words() {
        let req = TranscribeRequest {
            audio_b64: "dGVzdA==".into(),
            sample_rate: 16000,
            profile: "default",
            custom_words: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"profile\":\"default\""));
        assert!(!json.contains("custom_words"));
    }
}
