/// pipeline.rs — Async tokio channel orchestrator.
/// Defines all inter-stage message types and ties audio → transcribe → clean → inject.

use thiserror::Error;

// ---------------------------------------------------------------------------
// Dictation mode — controlled by the UI toggle
// ---------------------------------------------------------------------------

/// Controls how the LLM post-processes the raw Whisper transcript.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationMode {
    /// Remove filler words and fix punctuation only — no context reasoning.
    VoiceOnly,
    /// Full pipeline: detect active window → match profile → domain-aware Gemini prompt.
    Context,
}

impl Default for DictationMode {
    fn default() -> Self {
        Self::Context
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Error)]
pub enum PipelineError {
    #[error("Audio capture failed: {0}")]
    AudioCapture(String),

    #[error("Transcription failed: {0}")]
    Transcription(String),

    #[error("LLM cleaning failed: {0}")]
    LlmClean(String),

    #[error("Text injection failed: {0}")]
    Injection(String),

    #[error("Context detection failed: {0}")]
    ContextDetection(String),
}

// ---------------------------------------------------------------------------
// Pipeline events — the single message type flowing through all tokio channels
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)] // Event types document the pipeline contract; emitted as strings to the Svelte frontend
pub enum PipelineEvent {
    /// User pressed the hotkey — recording should start
    RecordingStarted,

    /// User released the hotkey (or VAD detected silence)
    RecordingStopped,

    /// Partial raw PCM samples captured from microphone (f32, 16kHz mono)
    AudioCapturedPartial(Vec<f32>),

    /// Final raw PCM samples captured from microphone (f32, 16kHz mono)
    AudioCapturedFinal(Vec<f32>),

    /// Raw partial transcript from Whisper sidecar
    PartialTranscriptReady(String),

    /// Raw final transcript from Whisper sidecar
    FinalTranscriptReady(String),

    /// LLM-cleaned text ready to inject into the active window
    CleanedTextReady(String),

    /// Text successfully injected into the focused application
    InjectionComplete,

    /// Non-fatal warning — logged but pipeline continues
    Warning(String),

    /// Fatal error — pipeline aborts current cycle
    Error(PipelineError),
}

// ---------------------------------------------------------------------------
// Context profile — maps an active app to a LLM prompt + vocabulary hints
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextProfile {
    /// Human-readable name, e.g. "Gmail", "Slack", "VS Code"
    pub name: String,

    /// Substrings matched against the OS window title (case-insensitive)
    pub window_title_patterns: Vec<String>,

    /// System prompt injected into the Ollama request
    pub system_prompt: String,

    /// Vocabulary hints forwarded to the Whisper sidecar as `initial_prompt`
    pub vocabulary_hints: Vec<String>,
}

impl ContextProfile {
    /// Returns true if `window_title` matches any of this profile's patterns.
    pub fn matches(&self, window_title: &str) -> bool {
        let lower = window_title.to_lowercase();
        self.window_title_patterns
            .iter()
            .any(|p| lower.contains(p.as_str()))
    }

    /// Built-in default profiles shipped with the app.
    pub fn built_ins() -> Vec<Self> {
        vec![
            Self {
                name: "Gmail".into(),
                window_title_patterns: vec!["gmail".into(), "inbox".into()],
                system_prompt: concat!(
                    "You are an expert email writing assistant embedded in Gmail.\n",
                    "The user has dictated their message by voice.\n\n",
                    "Your task:\n",
                    "1. Remove ALL filler words (um, uh, like, you know, basically, actually, right, so, well, I mean).\n",
                    "2. Fix grammar, punctuation, and capitalisation.\n",
                    "3. Keep a professional yet warm workplace email tone.\n",
                    "4. If the user dictated a full email body (not just a one-liner), ",
                    "   ensure it flows naturally as an email — with a proper greeting ",
                    "   (e.g. 'Hi [name],' or 'Dear [name],') and a polite closing ",
                    "   (e.g. 'Best regards,' or 'Thanks,') IF the user mentioned them. ",
                    "   Do NOT add a greeting or closing the user did not dictate.\n",
                    "5. If the input is a short quick reply (e.g. 'sounds good', 'see you then', 'confirmed'), ",
                    "   clean it lightly and keep it brief — do not expand it.\n",
                    "6. Return ONLY the cleaned email text. No meta-commentary, no labels, no quotes."
                ).into(),
                vocabulary_hints: vec![],
            },
            Self {
                name: "Slack".into(),
                window_title_patterns: vec!["slack".into()],
                system_prompt: "Clean the following dictated message for Slack: remove filler \
                    words, keep the tone casual and concise. Return only the cleaned text."
                    .into(),
                vocabulary_hints: vec![],
            },
            Self {
                name: "VS Code".into(),
                window_title_patterns: vec![
                    "visual studio code".into(),
                    "vscode".into(),
                    "code -".into(),
                    "intellij".into(),
                    "pycharm".into(),
                    "cursor".into(),
                    "vim".into(),
                    "neovim".into(),
                    "sublime text".into(),
                    "antigravity".into(),
                ],
                system_prompt: concat!(
                    "You are an expert software engineering AI assistant embedded in a code editor.\n",
                    "The user speaks their coding intent and you produce the result, ",
                    "ready to be typed directly into the editor.\n\n",
                    "Handle these cases intelligently based on what the user says:\n\n",
                    "CASE 1 — CODE GENERATION REQUEST\n",
                    "If the user asks you to write, create, generate, or implement something ",
                    "(e.g. 'write a function to sort an array', 'implement binary search', ",
                    "'create a REST API endpoint for user login'), ",
                    "generate complete, working, production-quality code.\n\n",
                    "CASE 2 — LEETCODE / COMPETITIVE PROGRAMMING\n",
                    "If the user references a LeetCode problem by number or name ",
                    "(e.g. 'leetcode 271', 'two sum', 'valid parentheses', 'encode and decode strings'), ",
                    "generate a clean, optimal solution. ",
                    "Start with a comment: problem title, number, and a one-line explanation of the approach.\n\n",
                    "CASE 3 — SPOKEN PSEUDOCODE\n",
                    "If the user speaks algorithmic logic aloud ",
                    "(e.g. 'for each item if greater than next swap them'), ",
                    "convert it to proper, syntactically correct code.\n\n",
                    "CASE 4 — CODE COMMENT OR DOCUMENTATION\n",
                    "If the user dictates a description meant to be a comment or docstring, ",
                    "format it as a clean, well-written code comment.\n\n",
                    "CRITICAL RULES:\n",
                    "- The programming language will be specified in the user prompt. Always use it.\n",
                    "- Return ONLY the code or comment — no prose explanations outside code comments.\n",
                    "- Remove all filler words from the spoken input before processing.\n",
                    "- For multi-function or multi-class solutions, include all necessary parts.\n",
                    "- Do NOT wrap output in markdown code fences (no triple backticks). Output raw code ONLY.\n",
                    "- If the user asks to write a program or generate code, you MUST return code, not conversational text."
                ).into(),
                vocabulary_hints: vec![
                    "async await mutex semaphore tokio serde reqwest PyTorch CUDA \
                     Kubernetes GraphQL OAuth2 JWT REST API microservices LeetCode \
                     binary search quicksort mergesort dynamic programming memoization".into(),
                ],
            },
            Self {
                name: "WhatsApp".into(),
                window_title_patterns: vec!["whatsapp".into()],
                system_prompt: "Clean the following dictated WhatsApp message: remove filler \
                    words, keep it conversational. Return only the cleaned text."
                    .into(),
                vocabulary_hints: vec![],
            },
            Self {
                name: "Medical".into(),
                window_title_patterns: vec!["epic".into(), "emr".into(), "meditech".into()],
                system_prompt: "Clean the following dictated medical note: remove filler words, \
                    preserve all medical terminology exactly. Return only the cleaned text."
                    .into(),
                vocabulary_hints: vec![
                    "tachycardia bradycardia hypertension myocardial infarction \
                     electrocardiogram auscultation bronchodilator corticosteroid \
                     anaphylaxis hematocrit".into(),
                ],
            },
            // Default — used when no other profile matches
            Self {
                name: "Default".into(),
                window_title_patterns: vec![],
                system_prompt: "Remove filler words (um, uh, like, you know, actually, basically) \
                    from the following dictated text and fix punctuation. \
                    Return only the cleaned text, nothing else."
                    .into(),
                vocabulary_hints: vec![],
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- PipelineError ----

    #[test]
    fn error_display_includes_message() {
        let e = PipelineError::AudioCapture("no mic".into());
        assert!(e.to_string().contains("no mic"));

        let e = PipelineError::Transcription("timeout".into());
        assert!(e.to_string().contains("timeout"));

        let e = PipelineError::LlmClean("model not found".into());
        assert!(e.to_string().contains("model not found"));

        let e = PipelineError::Injection("enigo error".into());
        assert!(e.to_string().contains("enigo error"));

        let e = PipelineError::ContextDetection("win32 fail".into());
        assert!(e.to_string().contains("win32 fail"));
    }

    // ---- ContextProfile::matches ----

    fn make_profile(patterns: &[&str]) -> ContextProfile {
        ContextProfile {
            name: "Test".into(),
            window_title_patterns: patterns.iter().map(|s| s.to_string()).collect(),
            system_prompt: "clean it".into(),
            vocabulary_hints: vec![],
        }
    }

    #[test]
    fn matches_returns_true_on_exact_pattern() {
        let p = make_profile(&["gmail"]);
        assert!(p.matches("Inbox - Gmail"));
    }

    #[test]
    fn matches_is_case_insensitive() {
        let p = make_profile(&["gmail"]);
        assert!(p.matches("GMAIL — Mohamed's Inbox"));
        assert!(p.matches("Gmail"));
    }

    #[test]
    fn matches_returns_false_when_no_pattern_hit() {
        let p = make_profile(&["gmail"]);
        assert!(!p.matches("Slack — #general"));
    }

    #[test]
    fn matches_any_pattern_in_list_is_sufficient() {
        let p = make_profile(&["gmail", "inbox"]);
        assert!(p.matches("Inbox — Work")); // matches "inbox"
        assert!(p.matches("Gmail — Personal")); // matches "gmail"
    }

    #[test]
    fn matches_empty_patterns_always_false() {
        let p = make_profile(&[]);
        assert!(!p.matches("anything at all"));
    }

    // ---- ContextProfile::built_ins ----

    #[test]
    fn built_ins_contains_default_profile() {
        let profiles = ContextProfile::built_ins();
        assert!(
            profiles.iter().any(|p| p.name == "Default"),
            "Default profile must always be present"
        );
    }

    #[test]
    fn built_ins_default_has_empty_patterns() {
        let profiles = ContextProfile::built_ins();
        let default = profiles.iter().find(|p| p.name == "Default").unwrap();
        assert!(
            default.window_title_patterns.is_empty(),
            "Default profile must not match any pattern — it is the fallback"
        );
    }

    #[test]
    fn built_ins_no_duplicate_names() {
        let profiles = ContextProfile::built_ins();
        let mut names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        let original_len = names.len();
        names.dedup();
        assert_eq!(names.len(), original_len, "built_ins must not have duplicate profile names");
    }

    #[test]
    fn built_ins_gmail_matches_expected_titles() {
        let profiles = ContextProfile::built_ins();
        let gmail = profiles.iter().find(|p| p.name == "Gmail").unwrap();
        assert!(gmail.matches("Inbox (3) - Gmail"));
        assert!(gmail.matches("Inbox — Mohamed"));
        assert!(!gmail.matches("Outlook"));
    }

    #[test]
    fn built_ins_vscode_matches_expected_titles() {
        let profiles = ContextProfile::built_ins();
        let vscode = profiles.iter().find(|p| p.name == "VS Code").unwrap();
        assert!(vscode.matches("main.rs - Visual Studio Code"));
        assert!(vscode.matches("code - voice-dictation"));
        assert!(vscode.matches("IntelliJ IDEA"));
        assert!(vscode.matches("main.py — Cursor"));
        assert!(vscode.matches("skill.py - Antigravity IDE"));
        assert!(!vscode.matches("Gmail - Google Chrome"));
    }

    // ---- PipelineEvent Debug (smoke test) ----

    #[test]
    fn pipeline_event_debug_does_not_panic() {
        let events = vec![
            PipelineEvent::RecordingStarted,
            PipelineEvent::RecordingStopped,
            PipelineEvent::AudioCapturedPartial(vec![0.0, 0.5, -0.5]),
            PipelineEvent::AudioCapturedFinal(vec![0.0, 0.5, -0.5]),
            PipelineEvent::PartialTranscriptReady("hello".into()),
            PipelineEvent::FinalTranscriptReady("hello".into()),
            PipelineEvent::CleanedTextReady("hello world".into()),
            PipelineEvent::InjectionComplete,
            PipelineEvent::Warning("mic lag".into()),
            PipelineEvent::Error(PipelineError::AudioCapture("test".into())),
        ];
        for e in events {
            let _ = format!("{:?}", e); // must not panic
        }
    }
}

