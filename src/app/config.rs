//! Persistent application configuration, stored as pretty-printed JSON in
//! `%APPDATA%\Delicium\config.json`.
//!
//! The file records the scene (instance sources and positions) and the
//! hit-test preference; it is written on a clean exit and read on the next
//! launch. Loading and saving are gated by [`enabled`]: the smoke test and
//! benchmarks never touch the user's file, and `DELICIUM_NO_CONFIG` disables
//! both sides as an escape hatch.
//!
//! Writes go through a temporary file plus an atomic rename, so a crash or a
//! full disk cannot leave a half-written file behind. A file that exists but
//! cannot be parsed is left in place and the defaults take over.

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
    File { path: PathBuf },
}

impl EntrySource {
    /// The persistent form of a load source, when one exists: the validation
    /// probe cannot round-trip, and network paths are rejected by the loader
    /// itself, so neither is configuration material.
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
        }
    }
}

/// Whether configuration may be read and written for these options. The
/// smoke test and benchmarks must stay deterministic and operate on their
/// own scenes; `DELICIUM_NO_CONFIG` set to anything but the empty string or
/// `0` disables persistence explicitly.
pub(super) fn enabled(options: &Options) -> bool {
    if options.smoke_test || options.bench.is_some() {
        return false;
    }
    let disabled = std::env::var_os("DELICIUM_NO_CONFIG")
        .is_some_and(|value| !value.is_empty() && value != "0");
    !disabled
}

/// `%APPDATA%\Delicium\config.json`; `None` without an `APPDATA` variable.
pub(super) fn config_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("Delicium").join("config.json"))
}

/// Loads the saved configuration, if any.
///
/// A missing file is the normal first launch and is silent; a file that
/// cannot be read, parsed or understood is reported and ignored, left on
/// disk untouched so its content is not destroyed by the defaults. Entries
/// beyond [`MAX_INSTANCES`] are truncated and entries with invalid positions
/// or non-local paths are dropped.
pub(super) fn load() -> Option<Config> {
    load_from(&config_path()?)
}

/// Saves the configuration through a temporary file and an atomic rename.
pub(super) fn save(config: &Config) -> Result<(), String> {
    let path = config_path()
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
    let text = serde_json::to_string_pretty(config)
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
