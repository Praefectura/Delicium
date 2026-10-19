//! User preferences shared by the engine and the launcher.
//!
//! Every preference lives in `%APPDATA%\Delicium\config.json`, the same file
//! that stores the engine's scene. The format is version 1 and every field
//! this module owns is optional on read, so files written by older builds
//! keep working; the launcher rewrites individual fields in place and leaves
//! everything it does not know untouched.
//!
//! The file may contain an OpenAI-compatible API key in plain text. It is
//! never logged, and [`ChatSettings`]'s `Debug` masks it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Configuration format version written by this build.
pub const CONFIG_VERSION: u64 = 1;

/// Library id of the bundled pet character.
pub const DEFAULT_PET: &str = "bangboo-eous";

/// Longest accepted user-library id.
pub const MAX_ID_LENGTH: usize = 64;

/// Upper bounds on the hand-editable string fields. A value above its bound
/// is dropped (back to the default) rather than truncated: a truncated URL or
/// key would fail in a confusing way. The settings page uses the same bounds
/// for its edit fields, so a typed value is never silently dropped later.
pub const MAX_URL_LENGTH: usize = 2048;
pub const MAX_MODEL_LENGTH: usize = 200;
pub const MAX_KEY_LENGTH: usize = 8192;

/// Interface language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Follow the Windows user interface language; English when unknown.
    #[default]
    System,
    /// Simplified Chinese.
    ZhHans,
    /// English.
    En,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::ZhHans => "zh_hans",
            Self::En => "en",
        }
    }

    /// Parses a stored value; anything unknown is `System`, never an error,
    /// so a hand-edited file cannot make the pet unlaunchable.
    pub fn parse(text: &str) -> Self {
        match text {
            "zh_hans" => Self::ZhHans,
            "en" => Self::En,
            _ => Self::System,
        }
    }

    /// The language actually rendered: `System` resolves through the Windows
    /// user interface language.
    pub fn resolved(self) -> Resolved {
        match self {
            Self::ZhHans => Resolved::ZhHans,
            Self::En => Resolved::En,
            Self::System => system_language(),
        }
    }
}

/// A language with `System` already resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolved {
    ZhHans,
    En,
}

impl Resolved {
    pub fn language(self) -> Language {
        match self {
            Self::ZhHans => Language::ZhHans,
            Self::En => Language::En,
        }
    }
}

/// The Windows user interface language, mapped onto the two supported
/// languages. Every Chinese variant reads as Simplified Chinese; anything
/// else is English.
#[cfg(windows)]
fn system_language() -> Resolved {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // SAFETY: the call has no arguments and only reads the user's locale.
    let langid = unsafe { GetUserDefaultUILanguage() };
    // The primary language id is the low 10 bits; 0x04 is Chinese.
    if langid & 0x03ff == 0x0004 {
        Resolved::ZhHans
    } else {
        Resolved::En
    }
}

#[cfg(not(windows))]
fn system_language() -> Resolved {
    Resolved::En
}

/// OpenAI-compatible chat endpoint settings.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSettings {
    /// Whether the pet may call a remote endpoint at all.
    pub enabled: bool,
    /// Base URL, e.g. `https://api.openai.com/v1` or `http://127.0.0.1:8080/v1`.
    pub base_url: String,
    /// Model name sent in the request body.
    pub model: String,
    /// Bearer token. Stored in the user's own configuration file; never
    /// logged, never shown unmasked, never sent anywhere but `base_url`.
    pub api_key: String,
}

impl std::fmt::Debug for ChatSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatSettings")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .finish()
    }
}

impl ChatSettings {
    /// Whether a remote request can be attempted.
    pub fn is_configured(&self) -> bool {
        self.enabled && !self.base_url.trim().is_empty() && !self.model.trim().is_empty()
    }

    fn sanitized(mut self) -> Self {
        if self.base_url.len() > MAX_URL_LENGTH || has_control(&self.base_url) {
            self.base_url.clear();
        }
        if self.model.len() > MAX_MODEL_LENGTH || has_control(&self.model) {
            self.model.clear();
        }
        if self.api_key.len() > MAX_KEY_LENGTH || has_control(&self.api_key) {
            self.api_key.clear();
        }
        self
    }
}

/// Everything the settings page and the running pet both read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// Whether the scene window intercepts input only over character regions.
    pub pixel_hittest: bool,
    /// Whether characters walk around on their own.
    pub wander: bool,
    pub language: Language,
    /// Library id started by the launcher.
    pub default_model: String,
    pub chat: ChatSettings,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            pixel_hittest: true,
            wander: true,
            language: Language::System,
            default_model: DEFAULT_PET.to_owned(),
            chat: ChatSettings::default(),
        }
    }
}

impl Preferences {
    fn sanitized(mut self) -> Self {
        if !valid_id(&self.default_model) {
            self.default_model = DEFAULT_PET.to_owned();
        }
        self.chat = self.chat.sanitized();
        self
    }
}

/// A library id: 1 to 64 ASCII letters, digits, `-` or `_`, with at least
/// one letter or digit. An id made only of separators (`-`, `__`) is refused
/// so it cannot be confused with a flag or a placeholder.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LENGTH
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && id.chars().any(|c| c.is_ascii_alphanumeric())
}

fn has_control(text: &str) -> bool {
    text.chars().any(|c| c.is_control())
}

/// `%APPDATA%\Delicium\config.json`; `None` without an `APPDATA` variable.
pub fn config_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("Delicium").join("config.json"))
}

/// A fresh configuration with defaults and no instances.
pub fn stub() -> Value {
    serde_json::json!({
        "version": CONFIG_VERSION,
        "pixel_hittest": true,
        "wander": true,
        "language": Language::System,
        "default_model": DEFAULT_PET,
        "chat": ChatSettings::default(),
        "instances": [],
    })
}

/// Reads the preferences; `None` when the file is missing, unreadable, not an
/// object, or written by a format version this build does not know.
pub fn read() -> Option<Preferences> {
    let path = config_path()?;
    read_from(&path)
}

/// [`read`] for an explicit path.
pub fn read_from(path: &Path) -> Option<Preferences> {
    let text = std::fs::read_to_string(path).ok()?;
    parse(&text)
}

/// Parses preference fields out of a whole configuration document.
pub fn parse(text: &str) -> Option<Preferences> {
    let value: Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    if object.get("version").and_then(Value::as_u64) != Some(CONFIG_VERSION) {
        return None;
    }
    let preferences: Preferences = serde_json::from_value(value).ok()?;
    Some(preferences.sanitized())
}

/// Reads the configuration, applies `mutate` to its top-level object, and
/// writes it back through a temporary file and an atomic rename.
///
/// A missing file starts from [`stub`]. A file this build cannot parse or a
/// version it does not know is refused instead of rewritten, because writing
/// it would discard fields this build cannot represent.
pub fn update<F>(mutate: F) -> Result<(), String>
where
    F: FnOnce(&mut Map<String, Value>) -> Result<(), String>,
{
    let path = config_path()
        .ok_or_else(|| "APPDATA is not set; cannot locate the configuration file".to_owned())?;
    update_at(&path, mutate)
}

/// [`update`] for an explicit path.
pub fn update_at<F>(path: &Path, mutate: F) -> Result<(), String>
where
    F: FnOnce(&mut Map<String, Value>) -> Result<(), String>,
{
    let mut value: Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("Cannot parse the configuration: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => stub(),
        Err(error) => return Err(format!("Cannot read {}: {error}", path.display())),
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| "The configuration is not a JSON object".to_owned())?;
    match object.get("version").and_then(Value::as_u64) {
        Some(CONFIG_VERSION) => {}
        Some(other) => {
            return Err(format!(
                "Configuration version {other} is not supported (this build writes {CONFIG_VERSION})"
            ));
        }
        None => return Err("The configuration has no usable version field".to_owned()),
    }
    mutate(object)?;
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("Cannot create {}: {error}", directory.display()))?;
    }
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("Cannot serialize the configuration: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)
        .map_err(|error| format!("Cannot write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        format!(
            "Cannot replace {} with {}: {error}",
            path.display(),
            temporary.display()
        )
    })
}

/// Stores one preference field as JSON.
pub fn set(object: &mut Map<String, Value>, key: &str, value: impl Into<Value>) {
    object.insert(key.to_owned(), value.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "delicium-preferences-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // The oldest supported file: only the required scene fields.
        let preferences = parse(r#"{"version":1,"pixel_hittest":false,"instances":[]}"#).unwrap();
        assert!(!preferences.pixel_hittest);
        assert!(preferences.wander);
        assert_eq!(preferences.language, Language::System);
        assert_eq!(preferences.default_model, DEFAULT_PET);
        assert_eq!(preferences.chat, ChatSettings::default());
    }

    #[test]
    fn unknown_versions_and_malformed_documents_are_refused() {
        assert_eq!(parse(r#"{"version":2,"instances":[]}"#), None);
        assert_eq!(parse(r#"{"instances":[]}"#), None);
        assert_eq!(parse("[]"), None);
        assert_eq!(parse("{ not json"), None);
    }

    #[test]
    fn unsafe_values_are_replaced_not_trusted() {
        let preferences = parse(
            r#"{"version":1,"instances":[],"default_model":"../../evil",
                "chat":{"enabled":true,"base_url":"https://x/v1","model":"m","api_key":"a\nb"}}"#,
        )
        .unwrap();
        assert_eq!(preferences.default_model, DEFAULT_PET);
        assert_eq!(preferences.chat.api_key, "");
        assert!(preferences.chat.base_url.starts_with("https://"));
    }

    #[test]
    fn ids_follow_the_documented_rule() {
        for id in ["bangboo-eous", "puppet2d", "my_model-2", "A"] {
            assert!(valid_id(id), "{id}");
        }
        for id in ["", "-", ".", "a b", "a/b", "a.b", &"x".repeat(65), "模型"] {
            assert!(!valid_id(id), "{id}");
        }
    }

    #[test]
    fn the_api_key_never_appears_in_debug_output() {
        let chat = ChatSettings {
            enabled: true,
            base_url: "https://example.test/v1".into(),
            model: "m".into(),
            api_key: "sk-secret-value".into(),
        };
        let text = format!("{chat:?}");
        assert!(!text.contains("sk-secret-value"), "{text}");
        assert!(text.contains("redacted"));
    }

    #[test]
    fn update_preserves_unknown_fields_and_writes_atomically() {
        let directory = test_dir("update");
        let path = directory.join("config.json");
        std::fs::write(
            &path,
            r#"{"version":1,"pixel_hittest":true,"instances":[],"future":7}"#,
        )
        .unwrap();
        update_at(&path, |object| {
            set(object, "wander", false);
            Ok(())
        })
        .unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["wander"], false);
        assert_eq!(value["future"], 7);
        assert!(!path.with_extension("json.tmp").exists());
        // A missing file becomes a complete configuration.
        let fresh = directory.join("fresh.json");
        update_at(&fresh, |object| {
            set(object, "language", "en");
            Ok(())
        })
        .unwrap();
        assert_eq!(read_from(&fresh).unwrap().language, Language::En);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn update_refuses_versions_it_cannot_represent() {
        let directory = test_dir("refuse");
        let path = directory.join("config.json");
        std::fs::write(&path, r#"{"version":9,"instances":[],"future":7}"#).unwrap();
        assert!(update_at(&path, |_| Ok(())).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"version":9,"instances":[],"future":7}"#
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn language_round_trips_and_unknown_values_read_as_system() {
        for language in [Language::System, Language::ZhHans, Language::En] {
            let text = serde_json::to_string(&language).unwrap();
            let parsed: Language = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed, language);
        }
        assert_eq!(Language::parse("zh_hans"), Language::ZhHans);
        assert_eq!(Language::parse("en"), Language::En);
        assert_eq!(Language::parse("klingon"), Language::System);
    }
}
