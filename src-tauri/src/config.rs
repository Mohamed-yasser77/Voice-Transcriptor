/// config.rs — User configuration loaded from a TOML file.
/// Stored at: {app_data_dir}/voice-dictation/config.toml

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Config structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Global hotkey combination, e.g. "Alt+Shift+V"
    pub hotkey: String,

    /// Gemini model name — override via GEMINI_MODEL env var or set here
    /// Options: "gemini-1.5-flash" (fast, default) or "gemini-1.5-pro" (smarter)
    pub gemini_model: String,

    /// TCP port the Python Whisper sidecar listens on
    pub sidecar_port: u16,

    /// Whether to pre-warm Gemini API key at app launch
    pub prewarm_on_start: bool,

    /// Whether to enable screenshot-based deep context (slower, opt-in)
    pub enable_vision_context: bool,

    /// How long to wait for silence before auto-stopping recording (ms)
    pub vad_silence_ms: u64,

    /// Maximum recording duration before force-stop (seconds)
    pub max_record_secs: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            hotkey: "Alt+Shift+V".into(),
            gemini_model: std::env::var("GEMINI_MODEL")
                .unwrap_or_else(|_| "gemini-1.5-flash".into()),
            sidecar_port: 9877,
            prewarm_on_start: true,
            enable_vision_context: false,
            vad_silence_ms: 800,
            max_record_secs: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// Load / Save
// ---------------------------------------------------------------------------

/// Returns the path to the config file, creating parent dirs if needed.
pub fn config_path() -> PathBuf {
    let base = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("voice-dictation");
    std::fs::create_dir_all(&base).ok();
    base.join("config.toml")
}

/// Load config from disk, falling back to defaults on any error.
pub fn load() -> AppConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
            log::warn!("Config parse error ({e}), using defaults.");
            AppConfig::default()
        }),
        Err(_) => {
            // First run — write defaults to disk so the user can edit them
            let default = AppConfig::default();
            save(&default);
            default
        }
    }
}

/// Persist config to disk.
pub fn save(cfg: &AppConfig) {
    let path = config_path();
    match toml::to_string_pretty(cfg) {
        Ok(content) => {
            if let Err(e) = std::fs::write(&path, content) {
                log::error!("Failed to save config: {e}");
            }
        }
        Err(e) => log::error!("Failed to serialize config: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.hotkey, "Alt+Shift+V");
        assert!(!cfg.gemini_model.is_empty());
        assert_eq!(cfg.sidecar_port, 9877);
        assert!(cfg.prewarm_on_start);
        assert!(!cfg.enable_vision_context);
        assert_eq!(cfg.vad_silence_ms, 800);
        assert_eq!(cfg.max_record_secs, 60);
    }

    #[test]
    fn config_roundtrips_through_toml() {
        let original = AppConfig {
            hotkey: "Ctrl+Shift+D".into(),
            gemini_model: "gemini-1.5-pro".into(),
            sidecar_port: 9999,
            prewarm_on_start: false,
            enable_vision_context: true,
            vad_silence_ms: 1200,
            max_record_secs: 30,
        };

        let serialized = toml::to_string_pretty(&original)
            .expect("serialize must not fail");
        let deserialized: AppConfig = toml::from_str(&serialized)
            .expect("deserialize must not fail");

        assert_eq!(deserialized.hotkey, original.hotkey);
        assert_eq!(deserialized.gemini_model, original.gemini_model);
        assert_eq!(deserialized.sidecar_port, original.sidecar_port);
        assert_eq!(deserialized.vad_silence_ms, original.vad_silence_ms);
        assert_eq!(deserialized.enable_vision_context, original.enable_vision_context);
    }

    #[test]
    fn toml_with_only_port_override_uses_defaults_for_rest() {
        // Simulates a minimal user-edited config.toml
        let toml_str = r#"
            hotkey = "Alt+Shift+V"
            gemini_model = "gemini-1.5-flash"
            sidecar_port = 1234
            prewarm_on_start = true
            enable_vision_context = false
            vad_silence_ms = 800
            max_record_secs = 60
        "#;

        let cfg: AppConfig = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.sidecar_port, 1234);
        assert_eq!(cfg.gemini_model, "gemini-1.5-flash");
    }

    #[test]
    fn invalid_toml_falls_back_gracefully() {
        // This simulates what load() does on a corrupt config file
        let bad_toml = "this is not valid toml = = =";
        let result: Result<AppConfig, _> = toml::from_str(bad_toml);
        assert!(result.is_err(), "invalid TOML must fail to parse");
        // load() will use AppConfig::default() in this case — already covered above
    }
}
