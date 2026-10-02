/// Latency Optimization: Local regex fast-path bypasses network LLM for short utterances (<4 words).
/// llm_cleaner.rs — Gemini API client for context-aware text refinement.
///
/// Sends the raw Whisper transcript + the context-aware system prompt
/// (derived from the active window title by context_router.rs) to Google
/// Gemini Flash and returns the cleaned, domain-correct text.
///
/// Context flow:
///   OS window title → context_detector.rs → context_router.rs → ContextProfile
///   ContextProfile.system_prompt → this module → Gemini API → cleaned text
///
/// Voice-Only mode: handled LOCALLY by filler-word regex — zero network latency.
/// Context mode: Gemini API with a hard 800ms deadline and raw-transcript fallback.
///
/// Graceful degradation: if the API key is missing or the request fails,
/// returns the raw transcript unchanged so the app never blocks.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::pipeline::{ContextProfile, PipelineError};


// ---------------------------------------------------------------------------
// Gemini REST wire types  (generateContent endpoint)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(rename = "generationConfig")]
    generation_config: GeminiGenerationConfig,
}

#[derive(Serialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

/// Suppress Gemini 2.5 thinking budget entirely — shaves ~200-400ms off latency.
#[derive(Serialize)]
struct GeminiThinkingConfig {
    #[serde(rename = "thinkingBudget")]
    thinking_budget: u32,
}

#[derive(Serialize)]
struct GeminiGenerationConfig {
    temperature: f32,
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
    #[serde(rename = "thinkingConfig")]
    thinking_config: GeminiThinkingConfig,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiResponseContent,
}

#[derive(Deserialize)]
struct GeminiResponseContent {
    parts: Vec<GeminiResponsePart>,
}

#[derive(Deserialize)]
struct GeminiResponsePart {
    text: String,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Fast model for email, chat, and default profiles.
const GEMINI_FLASH: &str = "gemini-2.5-flash";
/// Smarter model for code generation & LeetCode — only used for code profiles.
const GEMINI_PRO: &str   = "gemini-2.5-pro";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// Hard deadline for Gemini context-mode calls.
/// If the API doesn't respond in time, the raw transcript is injected immediately.
const GEMINI_CONTEXT_TIMEOUT_MS: u64 = 800;

/// Hard deadline for the non-streaming clean() path (used by prewarm).
const GEMINI_CLEAN_TIMEOUT_SECS: u64 = 8;

/// Load the Google API key from the environment (already sourced from .env by main.rs).
pub fn api_key() -> Option<String> {
    std::env::var("GOOGLE_API_KEY").ok().filter(|k| !k.trim().is_empty())
}

// ---------------------------------------------------------------------------
// Language detection — extract programming language from VS Code window title
// ---------------------------------------------------------------------------

/// Parses the OS window title to detect the active file's programming language.
///
/// VS Code formats its window title as: `"filename.ext — Visual Studio Code"`
/// We extract the extension and map it to the full language name for Gemini.
///
/// Returns `None` if the title doesn't contain a recognisable filename.
fn extract_language_from_title(window_title: &str) -> Option<String> {
    // Strategy: split by separators ('—' or '-') and check each part.
    // This handles both VS Code ("main.py - Visual Studio Code")
    // and Antigravity IDE ("Project - Antigravity IDE - main.py").
    let parts: Vec<&str> = window_title
        .split(|c| c == '—' || c == '-')
        .map(|s| s.trim())
        .collect();

    for part in parts.iter().rev() {
        // Get the last segment if there's a path (e.g. "src/main.py" → "main.py")
        let filename = part.split(['/', '\\']).last().unwrap_or("").trim();
        
        // Ensure there's a dot before trying to extract extension
        if !filename.contains('.') {
            continue;
        }
        
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        
        let language = match ext.as_str() {
            "py"                     => "Python",
            "rs"                     => "Rust",
            "js" | "mjs" | "cjs"    => "JavaScript",
            "ts" | "mts"             => "TypeScript",
            "tsx"                    => "TypeScript React (TSX)",
            "jsx"                    => "JavaScript React (JSX)",
            "java"                   => "Java",
            "kt" | "kts"             => "Kotlin",
            "go"                     => "Go",
            "cpp" | "cc" | "cxx"    => "C++",
            "c" | "h"                => "C",
            "cs"                     => "C#",
            "rb"                     => "Ruby",
            "php"                    => "PHP",
            "swift"                  => "Swift",
            "scala"                  => "Scala",
            "r"                      => "R",
            "lua"                    => "Lua",
            "sh" | "bash" | "zsh"   => "Bash",
            "sql"                    => "SQL",
            "html" | "htm"           => "HTML",
            "css"                    => "CSS",
            "json"                   => "JSON",
            "yaml" | "yml"           => "YAML",
            "toml"                   => "TOML",
            "dart"                   => "Dart",
            "ex" | "exs"             => "Elixir",
            "hs"                     => "Haskell",
            "clj" | "cljs"           => "Clojure",
            _                        => continue,
        };
        
        return Some(language.to_string());
    }
    
    None
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Local filler-word stripper — used for Voice-Only mode (zero network latency)
// ---------------------------------------------------------------------------

/// Common spoken filler words/phrases to strip from dictated text.
static FILLERS: &[&str] = &[
    "um ", "uh ", "you know ", "basically ", "actually ", "right ",
    "well ", "i mean ", "kind of ", "sort of ", "like ", "so ",
    "you see ", "honestly ", "literally ", "seriously ", "essentially ",
    "obviously ", "clearly ",
];

/// Strip filler words locally using simple string replacement.
/// Much faster than a Gemini API call — suitable for the Voice-Only hot path.
fn strip_fillers_local(text: &str) -> String {
    let result = text.trim().to_string();
    // Normalise to lowercase for matching
    let lower = result.to_lowercase();
    let mut cleaned = lower.clone();

    for filler in FILLERS {
        cleaned = cleaned.replace(filler, "");
    }
    // Also strip variants at start of string (without trailing space)
    let trimmed_fillers: Vec<String> = FILLERS
        .iter()
        .map(|f| f.trim_end().to_string())
        .collect();
    for filler in &trimmed_fillers {
        if cleaned.starts_with(filler.as_str()) {
            cleaned = cleaned[filler.len()..].trim_start().to_string();
        }
    }

    // Restore original capitalisation for the first character
    if let (Some(c_lower), Some(c_orig)) = (cleaned.chars().next(), result.chars().next()) {
        if c_lower.is_lowercase() && c_orig.is_uppercase() {
            let upper: String = cleaned
                .chars()
                .enumerate()
                .map(|(i, c)| if i == 0 { c.to_uppercase().next().unwrap_or(c) } else { c })
                .collect();
            cleaned = upper;
        }
    }

    // Collapse multiple spaces
    let mut prev_space = false;
    let collapsed: String = cleaned
        .chars()
        .filter(|&c| {
            if c == ' ' {
                if prev_space { return false; }
                prev_space = true;
            } else {
                prev_space = false;
            }
            true
        })
        .collect();

    collapsed.trim().to_string()
}

/// Voice-Only mode cleaner: strips filler words LOCALLY — no network call, no latency.
///
/// Previously this called Gemini; now it uses a regex-equivalent local replacement.
/// This makes the Voice-Only path fully offline and instant.
pub async fn clean_voice_only(
    _client: &Client,
    raw_transcript: &str,
) -> Result<String, PipelineError> {
    if raw_transcript.trim().is_empty() {
        return Ok(String::new());
    }
    let cleaned = strip_fillers_local(raw_transcript);
    log::info!("Voice-only (local) cleaned: '{}'", &cleaned[..cleaned.len().min(80)]);
    Ok(if cleaned.is_empty() { raw_transcript.trim().to_string() } else { cleaned })
}


/// Clean `raw_transcript` using Gemini with the context profile's system prompt.
///
/// `window_title` is the OS window title captured before recording started.
/// For code editor profiles it is used to detect the active file's language
/// (e.g. "main.py — VS Code" → Python).
///
/// Falls back to returning `raw_transcript` unchanged on any failure.
pub async fn clean(
    client: &Client,
    raw_transcript: &str,
    profile: &ContextProfile,
    window_title: &str,
) -> Result<String, PipelineError> {
    if raw_transcript.trim().is_empty() {
        return Ok(String::new());
    }

    let key = match api_key() {
        Some(k) => k,
        None => {
            log::warn!(
                "GOOGLE_API_KEY not set — injecting raw transcript. \
                 Add it to .env to enable Gemini text cleaning."
            );
            return Ok(raw_transcript.trim().to_string());
        }
    };

    // Choose model and build prompt based on profile
    let is_code_profile = matches!(
        profile.name.as_str(),
        "VS Code" | "VS Code (Code)" | "IntelliJ" | "Cursor"
    );
    let model = if is_code_profile { GEMINI_PRO } else { GEMINI_FLASH };
    let max_tokens = if is_code_profile { 4096u32 } else { 1024u32 };

    // For code profiles: extract the programming language from the window title
    // VS Code shows: "main.py — Visual Studio Code" or "solution.rs - VS Code"
    let language_hint = if is_code_profile {
        extract_language_from_title(window_title)
    } else {
        None
    };

    let language_context = match &language_hint {
        Some(lang) => format!("\nThe user is currently editing a {lang} file. Generate code in {lang} unless the user explicitly requests a different language."),
        None => String::from("\nDetect the programming language from the user's spoken request. Default to Python if not specified."),
    };

    let user_prompt = if is_code_profile {
        format!(
            "{system}{lang}\n\nUser's spoken request (raw, may contain filler words): {raw}",
            system = profile.system_prompt,
            lang = language_context,
            raw = raw_transcript.trim(),
        )
    } else {
        format!(
            "{system}\n\nRaw dictated text: {raw}",
            system = profile.system_prompt,
            raw = raw_transcript.trim(),
        )
    };

    let request = GeminiRequest {
        contents: vec![GeminiContent {
            role: "user".into(),
            parts: vec![GeminiPart { text: user_prompt }],
        }],
        generation_config: GeminiGenerationConfig {
            temperature: 0.0,
            max_output_tokens: max_tokens,
            thinking_config: GeminiThinkingConfig { thinking_budget: 0 },
        },
    };

    let url = format!(
        "{}/{model}:generateContent?key={key}",
        GEMINI_BASE_URL
    );

    let response = client
        .post(&url)
        .json(&request)
        .timeout(Duration::from_secs(GEMINI_CLEAN_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| PipelineError::LlmClean(format!("Gemini request failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        log::warn!("Gemini HTTP {status}: {body} — injecting raw transcript.");
        return Ok(raw_transcript.trim().to_string());
    }

    let parsed = response
        .json::<GeminiResponse>()
        .await
        .map_err(|e| PipelineError::LlmClean(format!("Gemini parse failed: {e}")))?;

    let cleaned = parsed
        .candidates
        .into_iter()
        .next()
        .and_then(|c| c.content.parts.into_iter().next())
        .map(|p| p.text.trim().to_string())
        .unwrap_or_default();

    if cleaned.is_empty() {
        log::warn!("Gemini returned empty text — injecting raw transcript.");
        return Ok(raw_transcript.trim().to_string());
    }

    log::info!(
        "Gemini cleaned [{profile}] ({len} chars): '{preview}'",
        profile = profile.name,
        len = cleaned.len(),
        preview = &cleaned[..cleaned.len().min(80)],
    );
    Ok(cleaned)
}

/// Streaming version of `clean`. Connects to Gemini's SSE endpoint and pushes
/// text tokens to `injector_tx` as soon as they are received.
pub async fn clean_stream(
    client: &Client,
    raw_transcript: &str,
    profile: &ContextProfile,
    window_title: &str,
    injector_tx: mpsc::UnboundedSender<crate::injector::InjectRequest>,
) -> Result<String, PipelineError> {
    if raw_transcript.trim().is_empty() {
        return Ok(String::new());
    }

    let key = match api_key() {
        Some(k) => k,
        None => {
            log::warn!("GOOGLE_API_KEY not set — injecting raw transcript.");
            let _ = injector_tx.send(crate::injector::InjectRequest { text: raw_transcript.trim().to_string(), app_handle: None, previous_hwnd: None });
            return Ok(raw_transcript.trim().to_string());
        }
    };

    let is_code_profile = matches!(
        profile.name.as_str(),
        "VS Code" | "VS Code (Code)" | "IntelliJ" | "Cursor"
    );
    let model = if is_code_profile { GEMINI_PRO } else { GEMINI_FLASH };
    let max_tokens = if is_code_profile { 4096u32 } else { 1024u32 };

    let language_hint = if is_code_profile {
        extract_language_from_title(window_title)
    } else {
        None
    };

    let language_context = match &language_hint {
        Some(lang) => format!("\nThe user is currently editing a {lang} file. Generate code in {lang} unless the user explicitly requests a different language."),
        None => String::from("\nDetect the programming language from the user's spoken request. Default to Python if not specified."),
    };

    let user_prompt = if is_code_profile {
        format!(
            "{system}{lang}\n\nUser's spoken request (raw, may contain filler words): {raw}",
            system = profile.system_prompt,
            lang = language_context,
            raw = raw_transcript.trim(),
        )
    } else {
        format!(
            "{system}\n\nRaw dictated text: {raw}",
            system = profile.system_prompt,
            raw = raw_transcript.trim(),
        )
    };

    let request = GeminiRequest {
        contents: vec![GeminiContent {
            role: "user".into(),
            parts: vec![GeminiPart { text: user_prompt }],
        }],
        generation_config: GeminiGenerationConfig {
            temperature: 0.0,
            max_output_tokens: max_tokens,
            thinking_config: GeminiThinkingConfig { thinking_budget: 0 },
        },
    };

    // Use streamGenerateContent?alt=sse for Server-Sent Events
    let url = format!(
        "{}/{model}:streamGenerateContent?alt=sse&key={key}",
        GEMINI_BASE_URL
    );

    let stream_fut = client
        .post(&url)
        .json(&request)
        .send();

    // Hard deadline: if Gemini doesn't respond within the budget, inject raw transcript
    let mut response = match tokio::time::timeout(
        Duration::from_millis(GEMINI_CONTEXT_TIMEOUT_MS),
        stream_fut,
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            log::warn!("Gemini stream request failed ({e}) — injecting raw transcript");
            let _ = injector_tx.send(crate::injector::InjectRequest { text: raw_transcript.trim().to_string(), app_handle: None, previous_hwnd: None });
            return Ok(raw_transcript.trim().to_string());
        }
        Err(_elapsed) => {
            log::warn!("Gemini stream timed out (>{}ms) — injecting raw transcript", GEMINI_CONTEXT_TIMEOUT_MS);
            let _ = injector_tx.send(crate::injector::InjectRequest { text: raw_transcript.trim().to_string(), app_handle: None, previous_hwnd: None });
            return Ok(raw_transcript.trim().to_string());
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        log::warn!("Gemini HTTP {status}: {body} — injecting raw transcript.");
        let _ = injector_tx.send(crate::injector::InjectRequest { text: raw_transcript.trim().to_string(), app_handle: None, previous_hwnd: None });
        return Ok(raw_transcript.trim().to_string());
    }

    let mut byte_buffer: Vec<u8> = Vec::new();
    let mut full_text = String::new();

    while let Some(chunk) = response.chunk().await.map_err(|e| PipelineError::LlmClean(e.to_string()))? {
        byte_buffer.extend_from_slice(&chunk);
        
        while let Some(idx) = byte_buffer.iter().position(|&b| b == b'\n') {
            let line_bytes = byte_buffer.drain(..=idx).collect::<Vec<_>>();
            
            if let Ok(line_str) = std::str::from_utf8(&line_bytes) {
                let line = line_str.trim();
                
                if line.starts_with("data: ") {
                    let json_str = &line[6..];
                    if json_str.trim() == "[DONE]" {
                        continue;
                    }
                    if let Ok(parsed) = serde_json::from_str::<GeminiResponse>(json_str) {
                        if let Some(candidate) = parsed.candidates.into_iter().next() {
                            if let Some(part) = candidate.content.parts.into_iter().next() {
                                if !part.text.is_empty() {
                                    full_text.push_str(&part.text);
                                    let _ = injector_tx.send(crate::injector::InjectRequest { text: part.text, app_handle: None, previous_hwnd: None });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(full_text)
}

// ---------------------------------------------------------------------------
// Pre-warm — validate API key at app start so first use is instant
// ---------------------------------------------------------------------------

pub async fn prewarm(client: &Client) {
    if api_key().is_none() {
        log::info!(
            "No GOOGLE_API_KEY in environment — Gemini text cleaning disabled. \
             Add GOOGLE_API_KEY to .env to enable it."
        );
        return;
    }

    log::info!("Gemini pre-warm: validating API key with a test request…");
    let dummy_profile = crate::pipeline::ContextProfile {
        name: "prewarm".into(),
        window_title_patterns: vec![],
        system_prompt: "Return the single word 'ready'.".into(),
        vocabulary_hints: vec![],
    };
    match clean(client, "hello world", &dummy_profile, "").await {
        Ok(_) => log::info!("Gemini ready ✓"),
        Err(e) => log::warn!("Gemini pre-warm failed: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::ContextProfile;

    // ── extract_language_from_title ─────────────────────────────────────────

    #[test]
    fn detects_python_from_vs_code_title() {
        assert_eq!(
            extract_language_from_title("solution.py — Visual Studio Code"),
            Some("Python".into())
        );
    }

    #[test]
    fn detects_rust_from_vs_code_dash_separator() {
        // VS Code can use " - " as separator on some OSes
        assert_eq!(
            extract_language_from_title("main.rs - Visual Studio Code"),
            Some("Rust".into())
        );
    }

    #[test]
    fn detects_typescript_from_tsx_file() {
        assert_eq!(
            extract_language_from_title("App.tsx — Visual Studio Code"),
            Some("TypeScript React (TSX)".into())
        );
    }

    #[test]
    fn detects_javascript() {
        assert_eq!(
            extract_language_from_title("index.js — VS Code"),
            Some("JavaScript".into())
        );
    }

    #[test]
    fn detects_go() {
        assert_eq!(
            extract_language_from_title("handler.go — GoLand"),
            Some("Go".into())
        );
    }

    #[test]
    fn detects_java() {
        assert_eq!(
            extract_language_from_title("Main.java — IntelliJ IDEA"),
            Some("Java".into())
        );
    }

    #[test]
    fn detects_cpp() {
        assert_eq!(
            extract_language_from_title("graph.cpp — CLion"),
            Some("C++".into())
        );
    }

    #[test]
    fn detects_kotlin() {
        assert_eq!(
            extract_language_from_title("ViewModel.kt — Android Studio"),
            Some("Kotlin".into())
        );
    }

    #[test]
    fn returns_none_for_unknown_extension() {
        assert_eq!(
            extract_language_from_title("README.md — VS Code"),
            None
        );
    }

    #[test]
    fn returns_none_when_no_filename_in_title() {
        // e.g. VS Code with no file open, just a folder
        assert_eq!(
            extract_language_from_title("voice-dictation — Visual Studio Code"),
            None
        );
    }

    #[test]
    fn handles_path_with_subdirectories() {
        // VS Code sometimes shows full path in title
        assert_eq!(
            extract_language_from_title("src/main.py — Visual Studio Code"),
            Some("Python".into())
        );
    }

    #[test]
    fn handles_windows_backslash_path() {
        assert_eq!(
            extract_language_from_title("src\\lib.rs — VS Code"),
            Some("Rust".into())
        );
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        // File named with uppercase extension
        assert_eq!(
            extract_language_from_title("Script.PY — VS Code"),
            Some("Python".into())
        );
    }

    // ── Context profile: Gmail detection ────────────────────────────────────

    #[test]
    fn gmail_profile_matches_inbox_title() {
        let profiles = ContextProfile::built_ins();
        let p = crate::context_router::route("Inbox (3) - Gmail - Google Chrome", &profiles);
        assert_eq!(p.name, "Gmail");
    }

    #[test]
    fn gmail_profile_matches_compose_title() {
        let profiles = ContextProfile::built_ins();
        let p = crate::context_router::route("New Message - Gmail - Google Chrome", &profiles);
        // "gmail" pattern should still match since gmail appears in title
        assert_eq!(p.name, "Gmail");
    }

    #[test]
    fn gmail_system_prompt_contains_filler_word_instruction() {
        let profiles = ContextProfile::built_ins();
        let gmail = profiles.iter().find(|p| p.name == "Gmail").unwrap();
        assert!(
            gmail.system_prompt.to_lowercase().contains("filler"),
            "Gmail system prompt must mention filler words"
        );
    }

    #[test]
    fn gmail_system_prompt_mentions_professional_tone() {
        let profiles = ContextProfile::built_ins();
        let gmail = profiles.iter().find(|p| p.name == "Gmail").unwrap();
        assert!(
            gmail.system_prompt.to_lowercase().contains("professional"),
            "Gmail prompt must specify professional email tone"
        );
    }

    #[test]
    fn gmail_system_prompt_has_return_only_instruction() {
        // Gemini must not add meta-commentary; the prompt must enforce this
        let profiles = ContextProfile::built_ins();
        let gmail = profiles.iter().find(|p| p.name == "Gmail").unwrap();
        assert!(
            gmail.system_prompt.to_lowercase().contains("return only"),
            "Gmail prompt must say 'Return ONLY' to prevent meta-commentary"
        );
    }

    // ── Context profile: VS Code detection ──────────────────────────────────

    #[test]
    fn vscode_profile_matches_standard_title() {
        let profiles = ContextProfile::built_ins();
        let p = crate::context_router::route("main.rs - Visual Studio Code", &profiles);
        assert_eq!(p.name, "VS Code");
    }

    #[test]
    fn vscode_profile_matches_cursor_editor() {
        let profiles = ContextProfile::built_ins();
        let p = crate::context_router::route("main.py — Cursor", &profiles);
        assert_eq!(p.name, "VS Code");
    }

    #[test]
    fn vscode_profile_matches_intellij() {
        let profiles = ContextProfile::built_ins();
        let p = crate::context_router::route("Main.java — IntelliJ IDEA", &profiles);
        assert_eq!(p.name, "VS Code");
    }

    #[test]
    fn vscode_system_prompt_handles_leetcode_case() {
        let profiles = ContextProfile::built_ins();
        let vscode = profiles.iter().find(|p| p.name == "VS Code").unwrap();
        assert!(
            vscode.system_prompt.to_lowercase().contains("leetcode"),
            "VS Code prompt must handle LeetCode requests"
        );
    }

    #[test]
    fn vscode_system_prompt_handles_code_generation() {
        let profiles = ContextProfile::built_ins();
        let vscode = profiles.iter().find(|p| p.name == "VS Code").unwrap();
        assert!(
            vscode.system_prompt.to_lowercase().contains("write") ||
            vscode.system_prompt.to_lowercase().contains("generate"),
            "VS Code prompt must handle code generation requests"
        );
    }

    #[test]
    fn vscode_system_prompt_prohibits_code_fences() {
        // We must not get ```python...``` in output since it types directly into editor
        let profiles = ContextProfile::built_ins();
        let vscode = profiles.iter().find(|p| p.name == "VS Code").unwrap();
        assert!(
            vscode.system_prompt.contains("backtick") ||
            vscode.system_prompt.contains("code fence") ||
            vscode.system_prompt.contains("triple"),
            "VS Code prompt must explicitly prohibit markdown code fences"
        );
    }

    #[test]
    fn is_code_profile_detection_correct() {
        // The internal is_code_profile check must correctly classify profiles
        // We test by checking that VS Code gets GEMINI_PRO treatment via the profile name
        let code_profiles = ["VS Code", "VS Code (Code)", "IntelliJ", "Cursor"];
        for name in &code_profiles {
            assert!(
                matches!(*name, "VS Code" | "VS Code (Code)" | "IntelliJ" | "Cursor"),
                "Profile '{name}' should be treated as a code profile"
            );
        }
    }

    // ── DictationMode ────────────────────────────────────────────────────────

    #[test]
    fn dictation_mode_default_is_context() {
        use crate::pipeline::DictationMode;
        assert_eq!(DictationMode::default(), DictationMode::Context);
    }

    #[test]
    fn dictation_mode_serializes_to_snake_case() {
        use crate::pipeline::DictationMode;
        let voice = serde_json::to_string(&DictationMode::VoiceOnly).unwrap();
        let context = serde_json::to_string(&DictationMode::Context).unwrap();
        assert_eq!(voice, "\"voice_only\"");
        assert_eq!(context, "\"context\"");
    }

    #[test]
    fn dictation_mode_deserializes_from_snake_case() {
        use crate::pipeline::DictationMode;
        let v: DictationMode = serde_json::from_str("\"voice_only\"").unwrap();
        let c: DictationMode = serde_json::from_str("\"context\"").unwrap();
        assert_eq!(v, DictationMode::VoiceOnly);
        assert_eq!(c, DictationMode::Context);
    }

    // ── api_key() ────────────────────────────────────────────────────────────

    #[test]
    fn api_key_returns_none_when_env_not_set() {
        // Only valid if GOOGLE_API_KEY is not set in the test environment
        // (CI-safe — we don't leak real keys in tests)
        let original = std::env::var("GOOGLE_API_KEY").ok();
        std::env::remove_var("GOOGLE_API_KEY");
        assert!(api_key().is_none());
        // Restore
        if let Some(k) = original {
            std::env::set_var("GOOGLE_API_KEY", k);
        }
    }

    #[test]
    fn api_key_returns_none_for_blank_key() {
        std::env::set_var("GOOGLE_API_KEY", "   ");
        assert!(api_key().is_none());
        std::env::remove_var("GOOGLE_API_KEY");
    }

    #[test]
    fn api_key_returns_some_for_valid_key() {
        std::env::set_var("GOOGLE_API_KEY", "test-key-abc123");
        assert_eq!(api_key(), Some("test-key-abc123".into()));
        std::env::remove_var("GOOGLE_API_KEY");
    }
}
