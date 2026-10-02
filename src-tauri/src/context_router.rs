/// context_router.rs — Maps the active window title to the best ContextProfile.

use crate::pipeline::ContextProfile;

/// Find the best matching profile for `window_title`.
/// Falls back to the "Default" profile if nothing matches.
pub fn route(window_title: &str, profiles: &[ContextProfile]) -> ContextProfile {
    // First pass: exact-match profiles
    for profile in profiles {
        if profile.name != "Default" && profile.matches(window_title) {
            log::info!("Context: '{}' → profile '{}'", window_title, profile.name);
            return profile.clone();
        }
    }
    // Fallback: return Default profile
    profiles
        .iter()
        .find(|p| p.name == "Default")
        .cloned()
        .unwrap_or_else(|| ContextProfile {
            name: "Default".into(),
            window_title_patterns: vec![],
            system_prompt: "Remove filler words and fix punctuation. Return only the cleaned text."
                .into(),
            vocabulary_hints: vec![],
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::ContextProfile;

    fn profiles() -> Vec<ContextProfile> {
        ContextProfile::built_ins()
    }

    fn custom(name: &str, patterns: &[&str]) -> ContextProfile {
        ContextProfile {
            name: name.into(),
            window_title_patterns: patterns.iter().map(|s| s.to_string()).collect(),
            system_prompt: "clean".into(),
            vocabulary_hints: vec![],
        }
    }

    #[test]
    fn routes_gmail_title_to_gmail_profile() {
        let p = route("Inbox (5) - Gmail", &profiles());
        assert_eq!(p.name, "Gmail");
    }

    #[test]
    fn routes_slack_title_to_slack_profile() {
        let p = route("Mohamed | Slack", &profiles());
        assert_eq!(p.name, "Slack");
    }

    #[test]
    fn routes_vscode_title_to_vscode_profile() {
        let p = route("main.rs - Visual Studio Code", &profiles());
        assert_eq!(p.name, "VS Code");
    }

    #[test]
    fn unknown_title_falls_back_to_default() {
        let p = route("Microsoft Excel - Budget.xlsx", &profiles());
        assert_eq!(p.name, "Default");
    }

    #[test]
    fn empty_title_falls_back_to_default() {
        let p = route("", &profiles());
        assert_eq!(p.name, "Default");
    }

    #[test]
    fn empty_profiles_returns_synthetic_default() {
        // If caller passes an empty slice, route() must still return something safe
        let p = route("anything", &[]);
        assert_eq!(p.name, "Default");
        assert!(!p.system_prompt.is_empty());
    }

    #[test]
    fn first_match_wins_not_default() {
        // Ensure Default is not selected when another profile matches
        let p = vec![
            custom("Default", &[]),
            custom("Work", &["myapp"]),
        ];
        let result = route("myapp - editor", &p);
        assert_eq!(result.name, "Work");
    }

    #[test]
    fn route_is_case_insensitive() {
        let p = profiles();
        let lower = route("inbox (3) - gmail", &p);
        let upper = route("INBOX (3) - GMAIL", &p);
        assert_eq!(lower.name, upper.name);
    }
}
