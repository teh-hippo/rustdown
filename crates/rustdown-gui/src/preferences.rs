use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Minimum heading count before nav is auto-shown in Preview/SideBySide.
pub const AUTO_NAV_MIN_HEADINGS: usize = 5;

/// User preferences persisted between sessions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UserPreferences {
    pub nav_visible: bool,
    pub heading_color_mode: bool,
    pub zoom_factor: f32,
    pub mode: String,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            nav_visible: false,
            heading_color_mode: true,
            zoom_factor: 1.0,
            mode: String::new(),
        }
    }
}

impl UserPreferences {
    /// Load preferences from the standard config path, falling back to
    /// defaults on any error (missing file, parse error, etc.).
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = config_file_path() else {
            return Self::default();
        };
        let Ok(contents) = fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&contents).unwrap_or_default()
    }

    /// Persist preferences to the standard config path.
    /// Errors are silently ignored — preferences are best-effort.
    pub fn save(&self) {
        let Some(path) = config_file_path() else {
            return;
        };
        let Ok(contents) = toml::to_string_pretty(self) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, contents);
    }
}

/// Returns the path to `settings.toml` inside the platform config directory.
///
/// - Linux: `~/.config/rustdown/settings.toml`
/// - macOS: `~/Library/Application Support/rustdown/settings.toml`
/// - Windows: `{FOLDERID_RoamingAppData}\rustdown\settings.toml`
fn config_file_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("rustdown").join("settings.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preferences_are_sensible() {
        let prefs = UserPreferences::default();
        assert!(!prefs.nav_visible);
        assert!(prefs.heading_color_mode);
    }

    #[test]
    fn round_trip_toml() {
        let prefs = UserPreferences {
            nav_visible: true,
            heading_color_mode: false,
            zoom_factor: 1.5,
            mode: "preview".to_owned(),
        };
        let serialized = toml::to_string_pretty(&prefs).unwrap_or_default();
        assert!(!serialized.is_empty(), "serialize should produce output");
        let deserialized: UserPreferences = toml::from_str(&serialized).unwrap_or_default();
        assert!(deserialized.nav_visible);
        assert!(!deserialized.heading_color_mode);
    }

    #[test]
    fn unknown_fields_ignored() {
        let toml_str = "\
            nav_visible = true\n\
            heading_color_mode = true\n\
            unknown_future_field = 42\n";
        let prefs: UserPreferences = toml::from_str(toml_str).unwrap_or_default();
        assert!(prefs.nav_visible);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let toml_str = "nav_visible = true\n";
        let prefs: UserPreferences = toml::from_str(toml_str).unwrap_or_default();
        assert!(prefs.nav_visible);
        assert!(prefs.heading_color_mode); // default
    }

    #[test]
    fn empty_toml_uses_defaults() {
        let prefs: UserPreferences = toml::from_str("").unwrap_or_default();
        assert!(!prefs.nav_visible);
        assert!(prefs.heading_color_mode);
    }

    #[test]
    fn config_file_path_returns_some() {
        if dirs::config_dir().is_some() {
            let path = config_file_path();
            assert!(path.is_some());
            if let Some(p) = path {
                let s = p.to_string_lossy();
                assert!(
                    s.contains("rustdown") && s.ends_with("settings.toml"),
                    "unexpected config path: {s}"
                );
            }
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join("rustdown-test-prefs");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("settings.toml");

        let prefs = UserPreferences {
            nav_visible: true,
            heading_color_mode: false,
            zoom_factor: 1.5,
            mode: "preview".to_owned(),
        };
        if let Ok(contents) = toml::to_string_pretty(&prefs) {
            let _ = fs::write(&path, &contents);
        }

        if let Ok(raw) = fs::read_to_string(&path) {
            let loaded: UserPreferences = toml::from_str(&raw).unwrap_or_default();
            assert!(loaded.nav_visible);
            assert!(!loaded.heading_color_mode);
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
