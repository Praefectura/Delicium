//! Persistent application configuration, stored as pretty-printed JSON in
//! `%APPDATA%\Delicium\config.json`.
//!
//! The file records the scene (instance sources and positions) and the
//! hit-test preference; it is written on a clean exit and read on the next
//! launch. Loading and saving are gated by [`enabled`]: the smoke test and
//! benchmarks never touch the user's file, and `DELICIUM_NO_CONFIG` disables
//! both sides as an escape hatch.
//!
//! The launcher's settings page writes the same file while the pet runs, so
//! the shared preference reader (`crate::preferences`) re-reads the
//! preferences when the file's modification time changes; the scene is
//! applied once at startup and is never restarted mid-run.
//!
//! Writes go through a temporary file plus an atomic rename, so a crash or a
//! full disk cannot leave a half-written file behind. A file that exists but
//! cannot be parsed is left in place and the defaults take over. Saving merges
//! into the existing document instead of replacing it: the preference fields
//! owned by the launcher (`wander`, `language`, `default_model`, `chat`) and
//! any field a newer build wrote are preserved.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::cli::Options;
use super::loader::LoadSource;
use crate::simulation::MAX_INSTANCES;

/// Configuration format version written by this build.
pub(super) const VERSION: u32 = 1;

/// World-coordinate bound for persisted positions, mirroring the simulation's
/// own limit; entries outside it are dropped on load.
const POSITION_LIMIT: f32 = 10_000.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Config {
    pub version: u32,
    /// Whether the scene window intercepts input only over character
    /// regions; `false` restores whole-window input.
    pub pixel_hittest: bool,
    pub instances: Vec<InstanceEntry>,
}

/// One persisted instance: where it came from and where it stood.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct InstanceEntry {
    pub source: EntrySource,
    pub position: [f32; 3],
}

/// A load source in its persistent form. The smoke test's validation probe
/// has no persistent form and never round-trips.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum EntrySource {
    Example2d,
    Example3d,
    TexturedExample2d,
    TexturedExample3d,
    File {
        path: PathBuf,
    },
    /// A library id. The file behind a user model is resolved again at load
    /// time, so a model directory that moved does not pin a stale path.
    Library {
        id: String,
    },
}

impl EntrySource {
    /// The persistent form of a load source, when one exists: the validation
    /// probe cannot round-trip, and network paths are rejected by the loader
    /// itself, so none is configuration material.
    pub(super) fn from_load_source(source: &LoadSource) -> Option<Self> {
        match source {
            LoadSource::Example2d => Some(Self::Example2d),
            LoadSource::Example3d => Some(Self::Example3d),
            LoadSource::TexturedExample2d => Some(Self::TexturedExample2d),
            LoadSource::TexturedExample3d => Some(Self::TexturedExample3d),
            LoadSource::File(path) => {
                let text = path.as_os_str().to_string_lossy();
                if text.starts_with("\\\\") || text.starts_with("//") {
                    None
                } else {
                    Some(Self::File { path: path.clone() })
                }
            }
            LoadSource::Library { id, .. } => {
                crate::preferences::valid_id(id).then(|| Self::Library { id: id.clone() })
            }
            LoadSource::ValidationProbe => None,
        }
    }

    /// The loader's source for a persisted entry.
    pub(super) fn into_load_source(self) -> LoadSource {
        match self {
            Self::Example2d => LoadSource::Example2d,
            Self::Example3d => LoadSource::Example3d,
            Self::TexturedExample2d => LoadSource::TexturedExample2d,
            Self::TexturedExample3d => LoadSource::TexturedExample3d,
            Self::File { path } => LoadSource::File(path),
            Self::Library { id } => LoadSource::Library { id, path: None },
        }
    }
}

/// Whether scene configuration may be read and written for these options.
/// The smoke test and benchmarks must stay deterministic and operate on their
/// own scenes; a `--pet` run likewise owns its fixed scene and must not read
/// or overwrite the user's saved scene. `DELICIUM_NO_CONFIG` set to anything
/// but the empty string or `0` disables persistence explicitly.
pub(super) fn enabled(options: &Options) -> bool {
    if options.smoke_test || options.bench.is_some() || options.pet_mode() {
        return false;
    }
    !disabled_by_env()
}

/// Whether the preference fields may be read and followed.
///
/// The pet owns its scene but not its preferences: the launcher's settings
/// page writes them into this same file while the pet runs, and a checkbox
/// that does nothing would be a lie. Bounded runs stay deterministic and
/// still ignore the file entirely.
pub(super) fn preferences_enabled(options: &Options) -> bool {
    if options.smoke_test || options.bench.is_some() {
        return false;
    }
    !disabled_by_env()
}

/// `DELICIUM_NO_CONFIG` set to anything but the empty string or `0`.
fn disabled_by_env() -> bool {
    std::env::var_os("DELICIUM_NO_CONFIG").is_some_and(|value| !value.is_empty() && value != "0")
}

/// `(modified, length)` of the configuration file, or `None` when it does not
/// exist. The pair is enough to notice a rewrite: the launcher writes a new
/// file and renames it into place, so both change on every write.
pub(super) fn stamp() -> Option<(std::time::SystemTime, u64)> {
    let path = crate::preferences::config_path()?;
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    Some((modified, metadata.len()))
}

/// Loads the saved configuration, if any.
///
/// A missing file is the normal first launch and is silent; a file that
/// cannot be read, parsed or understood is reported and ignored, left on
/// disk untouched so its content is not destroyed by the defaults. Entries
/// beyond [`MAX_INSTANCES`] are truncated and entries with invalid positions
/// or non-local paths are dropped.
pub(super) fn load() -> Option<Config> {
    load_from(&crate::preferences::config_path()?)
}

/// Saves the configuration through a temporary file and an atomic rename.
pub(super) fn save(config: &Config) -> Result<(), String> {
    let path = crate::preferences::config_path()
        .ok_or_else(|| "APPDATA is not set; cannot locate the configuration file".to_owned())?;
    save_to(config, &path)
}

fn load_from(path: &Path) -> Option<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::warn!("Cannot read {}: {error}", path.display());
            return None;
        }
    };
    let mut config: Config = match serde_json::from_str(&text) {
        Ok(config) => config,
        Err(error) => {
            log::warn!("Cannot parse {}: {error}", path.display());
            return None;
        }
    };
    if config.version != VERSION {
        log::warn!(
            "Ignoring {}: unsupported configuration version {}",
            path.display(),
            config.version
        );
        return None;
    }
    let stored = config.instances.len();
    config.instances.truncate(MAX_INSTANCES);
    config.instances.retain(valid_entry);
    let dropped = stored - config.instances.len();
    if dropped > 0 {
        log::warn!("Dropped {dropped} invalid configuration entries");
    }
    Some(config)
}

fn save_to(config: &Config, path: &Path) -> Result<(), String> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("Cannot create {}: {error}", directory.display()))?;
    }
    // Merge into the document on disk instead of replacing it: the launcher
    // owns `wander`, `language`, `default_model` and `chat` in the same file,
    // and a newer build may write fields this one cannot represent. Only the
    // three fields this module owns are replaced.
    let mut value = match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value @ serde_json::Value::Object(_))
                if value.get("version").and_then(serde_json::Value::as_u64)
                    == Some(VERSION as u64) =>
            {
                value
            }
            // A file that cannot be parsed or belongs to another version is
            // not configuration material; the defaults take over, which is
            // what the load path already decided for this run.
            _ => serde_json::to_value(config)
                .map_err(|error| format!("Cannot serialize the configuration: {error}"))?,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::to_value(config)
            .map_err(|error| format!("Cannot serialize the configuration: {error}"))?,
        Err(error) => return Err(format!("Cannot read {}: {error}", path.display())),
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| "The configuration is not a JSON object".to_owned())?;
    object.insert("version".into(), serde_json::Value::from(VERSION));
    object.insert(
        "pixel_hittest".into(),
        serde_json::Value::Bool(config.pixel_hittest),
    );
    object.insert(
        "instances".into(),
        serde_json::to_value(&config.instances)
            .map_err(|error| format!("Cannot serialize the configuration: {error}"))?,
    );
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

/// Whether a persisted entry can be used as-is: a finite position inside the
/// simulation's coordinate range and a local (or bundled) source.
fn valid_entry(entry: &InstanceEntry) -> bool {
    let position_ok = entry
        .position
        .iter()
        .all(|value| value.is_finite() && value.abs() <= POSITION_LIMIT);
    let source_ok = match &entry.source {
        EntrySource::File { path } => {
            let text = path.as_os_str().to_string_lossy();
            !text.is_empty() && !text.starts_with("\\\\") && !text.starts_with("//")
        }
        EntrySource::Library { id } => crate::preferences::valid_id(id),
        EntrySource::Example2d
        | EntrySource::Example3d
        | EntrySource::TexturedExample2d
        | EntrySource::TexturedExample3d => true,
    };
    position_ok && source_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory for one test, removed and recreated so reruns start
    /// clean; every name is unique per test.
    fn test_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "delicium-config-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn entry(source: EntrySource, position: [f32; 3]) -> InstanceEntry {
        InstanceEntry { source, position }
    }

    #[test]
    fn wire_format_uses_tagged_sources() {
        let file = entry(
            EntrySource::File {
                path: PathBuf::from("m.dmodel.json"),
            },
            [1.0, 2.0, 0.0],
        );
        assert_eq!(
            serde_json::to_string(&file).unwrap(),
            r#"{"source":{"kind":"file","path":"m.dmodel.json"},"position":[1.0,2.0,0.0]}"#
        );
        let example = entry(EntrySource::TexturedExample2d, [0.0, 0.0, 0.0]);
        assert_eq!(
            serde_json::to_string(&example).unwrap(),
            r#"{"source":{"kind":"textured_example2d"},"position":[0.0,0.0,0.0]}"#
        );
    }

    #[test]
    fn all_sources_round_trip_through_json() {
        let sources = [
            EntrySource::Example2d,
            EntrySource::Example3d,
            EntrySource::TexturedExample2d,
            EntrySource::TexturedExample3d,
            EntrySource::File {
                path: PathBuf::from("models/local.dmodel.json"),
            },
            EntrySource::Library {
                id: "bangboo-eous".into(),
            },
        ];
        let config = Config {
            version: VERSION,
            pixel_hittest: false,
            instances: sources
                .iter()
                .cloned()
                .map(|source| entry(source, [1.5, 0.25, 0.0]))
                .collect(),
        };
        let text = serde_json::to_string_pretty(&config).unwrap();
        let parsed: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn pet_mode_disables_persistence_without_changing_the_format() {
        // The pet scene must not read or overwrite the user's file, whatever
        // DELICIUM_NO_CONFIG says.
        let options = Options {
            pet: Some(crate::app::cli::PetRequest::Library(
                crate::preferences::DEFAULT_PET.into(),
            )),
            ..Options::default()
        };
        assert!(!enabled(&options));
        // The persistent format records the pet as a library entry, so a
        // saved scene that contains it round-trips like any other model.
        let source = LoadSource::Library {
            id: crate::preferences::DEFAULT_PET.into(),
            path: None,
        };
        let entry = EntrySource::from_load_source(&source).expect("library entries round-trip");
        assert_eq!(
            entry,
            EntrySource::Library {
                id: crate::preferences::DEFAULT_PET.into()
            }
        );
        assert_eq!(
            entry.into_load_source().library_id(),
            Some(crate::preferences::DEFAULT_PET)
        );
        assert_eq!(
            EntrySource::from_load_source(&LoadSource::ValidationProbe),
            None
        );
    }

    #[test]
    fn native_pet_mode_needs_no_persistence_either() {
        // Explicit `--pet` runs are standalone scenes, like `--bangboo`.
        let options = Options {
            pet: Some(crate::app::cli::PetRequest::File(PathBuf::from("pet.json"))),
            ..Options::default()
        };
        assert!(!enabled(&options));
        // A file pet still persists as an explicit file when saved from an
        // ordinary run; the mode only decides whether the scene is read.
        assert_eq!(
            EntrySource::from_load_source(&LoadSource::File(PathBuf::from("pet.json"))),
            Some(EntrySource::File {
                path: PathBuf::from("pet.json")
            })
        );
    }

    #[test]
    fn saving_preserves_preferences_owned_by_the_launcher() {
        // The launcher writes `wander`, `language`, `default_model` and
        // `chat` into the same file; a clean engine exit must not drop them.
        let directory = test_dir("preserve");
        let path = directory.join("config.json");
        std::fs::write(
            &path,
            r#"{"version":1,"pixel_hittest":true,"wander":false,"language":"zh_hans",
                "default_model":"puppet2d","chat":{"enabled":true,"base_url":"https://x/v1",
                "model":"m","api_key":""},"instances":[],"future":7}"#,
        )
        .unwrap();
        let config = Config {
            version: VERSION,
            pixel_hittest: false,
            instances: vec![entry(EntrySource::Example3d, [0.0, 0.0, 0.0])],
        };
        save_to(&config, &path).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["pixel_hittest"], false);
        assert_eq!(value["instances"].as_array().unwrap().len(), 1);
        assert_eq!(value["wander"], false);
        assert_eq!(value["language"], "zh_hans");
        assert_eq!(value["default_model"], "puppet2d");
        assert_eq!(value["chat"]["enabled"], true);
        assert_eq!(value["future"], 7);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn save_and_load_round_trip_through_a_file() {
        let directory = test_dir("round-trip");
        let path = directory.join("config.json");
        let config = Config {
            version: VERSION,
            pixel_hittest: true,
            instances: vec![
                entry(EntrySource::Example2d, [-2.4, 0.15, 0.0]),
                entry(
                    EntrySource::File {
                        path: PathBuf::from("local.dmodel.json"),
                    },
                    [0.8, 0.15, 0.0],
                ),
            ],
        };
        save_to(&config, &path).unwrap();
        assert_eq!(load_from(&path).unwrap(), config);
        // The temporary file is gone after the rename.
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn load_truncates_beyond_the_instance_limit() {
        let directory = test_dir("truncate");
        let path = directory.join("config.json");
        let mut config = Config {
            version: VERSION,
            pixel_hittest: true,
            instances: (0..MAX_INSTANCES + 1)
                .map(|index| entry(EntrySource::Example2d, [index as f32, 0.0, 0.0]))
                .collect(),
        };
        save_to(&config, &path).unwrap();
        assert_eq!(load_from(&path).unwrap().instances.len(), MAX_INSTANCES);
        // One invalid entry inside the limit is dropped, the rest survive.
        config.instances.truncate(4);
        config.instances.push(entry(
            EntrySource::Example2d,
            [POSITION_LIMIT + 1.0, 0.0, 0.0],
        ));
        config.instances.push(entry(
            EntrySource::File {
                path: PathBuf::from(r"\\server\share\m.dmodel.json"),
            },
            [0.0, 0.0, 0.0],
        ));
        save_to(&config, &path).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.instances.len(), 4);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn unreadable_files_and_versions_are_ignored_not_overwritten() {
        let directory = test_dir("reject");
        let path = directory.join("config.json");
        // A missing file is a normal first launch.
        assert_eq!(load_from(&path), None);
        // A parse failure is reported and ignored, and the file survives.
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_from(&path), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        // A version this build does not know is ignored the same way.
        std::fs::write(
            &path,
            r#"{"version":2,"pixel_hittest":true,"instances":[]}"#,
        )
        .unwrap();
        assert_eq!(load_from(&path), None);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
