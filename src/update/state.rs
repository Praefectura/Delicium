//! The on-disk record of which versions are installed.
//!
//! `state.json` lives in the installation root and is the only thing that
//! says which version directory to launch. It is written atomically (temporary
//! file, flush, write-through rename) so a crash leaves either the old or the
//! new file, never a half-written one.
//!
//! The state is recoverable by construction: every field that names a version
//! is re-validated when loaded, and if the file is unusable the launcher can
//! rebuild it from the `.install.json` markers inside `app\` (see
//! [`InstallState::from_installed`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::UpdateError;
use super::sha256::{HEX_LEN, parse_hex};
use super::version::Version;

/// State format written by this build.
pub const SCHEMA: u32 = 1;

/// Largest state file this build will read.
const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// The file name inside the installation root.
pub const FILE_NAME: &str = "state.json";

/// One installed version directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledVersion {
    /// The version string; also the directory name under `app\`.
    pub version: String,
    /// SHA-256 of the manifest this directory was installed from, so a
    /// reinstalled same-version release is distinguishable.
    pub manifest_sha256: String,
    /// Unix seconds when the directory was installed.
    pub installed_at: u64,
}

impl InstalledVersion {
    /// A new record; the version string is validated here.
    pub fn new(
        version: &Version,
        manifest_sha256: impl Into<String>,
        installed_at: u64,
    ) -> Result<Self, UpdateError> {
        let manifest_sha256 = manifest_sha256.into();
        if parse_hex(&manifest_sha256).is_none() {
            return Err(UpdateError::state(format!(
                "manifest digest `{manifest_sha256}` is not {HEX_LEN} hexadecimal digits"
            )));
        }
        Ok(Self {
            version: version.to_string(),
            manifest_sha256,
            installed_at,
        })
    }

    /// The version, re-parsed.
    pub fn parsed(&self) -> Result<Version, UpdateError> {
        Version::parse(&self.version)
            .map_err(|error| UpdateError::state(format!("installed `{}`: {error}", self.version)))
    }

    /// The directory this version installs into.
    pub fn directory(&self, root: &Path) -> PathBuf {
        root.join("app").join(&self.version)
    }

    fn validate(&self) -> Result<(), UpdateError> {
        self.parsed()?;
        if parse_hex(&self.manifest_sha256).is_none() {
            return Err(UpdateError::state(format!(
                "`{}` has a malformed manifest digest",
                self.version
            )));
        }
        Ok(())
    }
}

/// The newest version this installation has ever successfully started.
///
/// It only moves forward, so a stale release published later cannot be
/// installed over a newer one after a local rollback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watermark {
    pub version: String,
    pub tag: String,
}

impl Watermark {
    /// The parsed version, if it is usable.
    pub fn parsed(&self) -> Option<Version> {
        Version::parse(&self.version).ok()
    }

    fn validate(&self) -> Result<(), UpdateError> {
        Version::parse(&self.version)
            .map_err(|error| UpdateError::state(format!("watermark: {error}")))?;
        Version::parse_tag(&self.tag)
            .map_err(|error| UpdateError::state(format!("watermark tag: {error}")))?;
        Ok(())
    }
}

/// What is installed right now and what may replace it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallState {
    pub schema: u32,
    /// The version to launch.
    pub current: Option<InstalledVersion>,
    /// The version a crash loop falls back to.
    pub previous: Option<InstalledVersion>,
    /// An installed version whose first start has not been confirmed yet.
    pub pending: Option<InstalledVersion>,
    pub watermark: Option<Watermark>,
    /// Unix seconds of the last successful write.
    pub updated_at: u64,
}

impl Default for InstallState {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            current: None,
            previous: None,
            pending: None,
            watermark: None,
            updated_at: 0,
        }
    }
}

impl InstallState {
    /// `state.json` inside the installation root.
    pub fn path(root: &Path) -> PathBuf {
        root.join(FILE_NAME)
    }

    /// Loads the state; a missing file is a fresh installation.
    pub fn load(root: &Path) -> Result<Option<Self>, UpdateError> {
        let path = Self::path(root);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(UpdateError::io("read", &path, error)),
        };
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(UpdateError::state(format!(
                "{} is {} bytes, more than the {MAX_STATE_BYTES} byte limit",
                path.display(),
                bytes.len()
            )));
        }
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|error| UpdateError::state(format!("{}: {error}", path.display())))?;
        state.validate()?;
        Ok(Some(state))
    }

    /// Writes the state atomically.
    pub fn save(&mut self, root: &Path, now_unix: u64) -> Result<(), UpdateError> {
        self.schema = SCHEMA;
        self.updated_at = now_unix;
        self.validate()?;
        std::fs::create_dir_all(root).map_err(|error| UpdateError::io("create", root, error))?;
        let path = Self::path(root);
        let temporary = root.join(format!("{FILE_NAME}.{}.tmp", std::process::id()));
        let text = serde_json::to_vec_pretty(self)
            .map_err(|error| UpdateError::state(format!("cannot serialize state: {error}")))?;
        write_synced(&temporary, &text)?;
        if let Err(error) = crate::update::winfile::replace_atomic(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            log::warn!("Cannot move the state file into place: {error}");
            return Err(UpdateError::io("replace", &path, error));
        }
        Ok(())
    }

    /// Everything the state says, checked for internal consistency.
    pub fn validate(&self) -> Result<(), UpdateError> {
        if self.schema != SCHEMA {
            return Err(UpdateError::state(format!(
                "schema {} is not supported (this build reads {SCHEMA})",
                self.schema
            )));
        }
        for entry in [&self.current, &self.previous, &self.pending]
            .into_iter()
            .flatten()
        {
            entry.validate()?;
        }
        if let Some(watermark) = &self.watermark {
            watermark.validate()?;
        }
        // `pending` is the only slot that may repeat a version: a failed
        // start leaves `current` in place and the same version pending.
        if let (Some(current), Some(previous)) = (&self.current, &self.previous)
            && current.version == previous.version
        {
            return Err(UpdateError::state(format!(
                "`{}` is both current and previous",
                current.version
            )));
        }
        Ok(())
    }

    /// The version to launch.
    pub fn current_version(&self) -> Option<Version> {
        self.current.as_ref().and_then(|entry| entry.parsed().ok())
    }

    /// The newest version ever started.
    pub fn watermark_version(&self) -> Option<Version> {
        self.watermark.as_ref().and_then(Watermark::parsed)
    }

    /// Records a version whose first start is about to be attempted.
    pub fn record_pending(&mut self, installed: InstalledVersion) -> Result<(), UpdateError> {
        installed.validate()?;
        self.pending = Some(installed);
        Ok(())
    }

    /// Marks the pending version as the one that launched successfully.
    ///
    /// The previous version stays available for a crash-loop fallback, and
    /// the watermark only moves forward.
    pub fn confirm(&mut self, now_unix: u64) -> Result<OpenedVersion, UpdateError> {
        let Some(installed) = self.pending.take() else {
            return Err(UpdateError::state("there is no pending version to confirm"));
        };
        let version = installed.parsed()?;
        let advanced = match self.watermark_version() {
            Some(existing) => version > existing,
            None => true,
        };
        if advanced {
            self.watermark = Some(Watermark {
                version: version.to_string(),
                tag: version.tag(),
            });
        }
        self.previous = self.current.take();
        self.current = Some(installed);
        self.updated_at = now_unix;
        Ok(if advanced {
            OpenedVersion::Confirmed
        } else {
            OpenedVersion::ConfirmedWithoutWatermark
        })
    }

    /// Drops a pending version whose start failed.
    pub fn discard_pending(&mut self) -> Option<InstalledVersion> {
        self.pending.take()
    }

    /// Rolls back to the previous version, keeping it as current.
    pub fn roll_back(&mut self, now_unix: u64) -> Result<InstalledVersion, UpdateError> {
        let failed = self.current.take();
        let Some(previous) = self.previous.take() else {
            self.current = failed;
            return Err(UpdateError::state(
                "there is no previous version to fall back to",
            ));
        };
        // The failed version must not become `previous`, or a rollback would
        // immediately roll forward again on the next failure.
        if let Some(failed) = failed {
            log::warn!(
                "Rolling back from {} to {}",
                failed.version,
                previous.version
            );
        }
        self.current = Some(previous.clone());
        self.pending = None;
        self.updated_at = now_unix;
        Ok(previous)
    }

    /// Builds a state from directories found on disk, newest first. Used when
    /// `state.json` is missing or unusable.
    pub fn from_installed(mut installed: Vec<InstalledVersion>, now_unix: u64) -> Self {
        installed.sort_by(|a, b| match (a.parsed(), b.parsed()) {
            (Ok(a), Ok(b)) => b.cmp(&a),
            _ => b.version.cmp(&a.version),
        });
        installed.dedup_by(|a, b| a.version == b.version);
        let mut state = Self {
            schema: SCHEMA,
            updated_at: now_unix,
            ..Self::default()
        };
        let mut rest = installed.into_iter();
        state.current = rest.next();
        state.previous = rest.next();
        let newest = state.current.as_ref().and_then(|entry| entry.parsed().ok());
        if let Some(version) = newest {
            state.watermark = Some(Watermark {
                version: version.to_string(),
                tag: version.tag(),
            });
        }
        state
    }
}

/// The outcome of confirming a pending version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenedVersion {
    /// The version is newer than everything installed before.
    Confirmed,
    /// The version is not newer than the watermark: it was reused or rolled
    /// forward locally, and the watermark stayed where it was.
    ConfirmedWithoutWatermark,
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    use std::io::Write;
    let mut file =
        std::fs::File::create(path).map_err(|error| UpdateError::io("create", path, error))?;
    file.write_all(bytes)
        .map_err(|error| UpdateError::io("write", path, error))?;
    file.sync_all()
        .map_err(|error| UpdateError::io("flush", path, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn test_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("delicium-state-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn installed(version: &str, at: u64) -> InstalledVersion {
        InstalledVersion::new(&Version::parse(version).unwrap(), DIGEST, at).unwrap()
    }

    #[test]
    fn save_and_load_round_trip() {
        let root = test_root("round-trip");
        assert_eq!(InstallState::load(&root).unwrap(), None);
        let mut state = InstallState {
            current: Some(installed("0.1.0", 1)),
            ..InstallState::default()
        };
        state.save(&root, 100).unwrap();
        let loaded = InstallState::load(&root).unwrap().unwrap();
        assert_eq!(loaded, state);
        assert_eq!(loaded.updated_at, 100);
        // No temporary file survives.
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn corrupt_or_unknown_states_are_errors_not_silence() {
        let root = test_root("corrupt");
        let path = InstallState::path(&root);
        std::fs::write(&path, "{ not json").unwrap();
        assert!(InstallState::load(&root).is_err());
        std::fs::write(&path, r#"{"schema":9}"#).unwrap();
        assert!(InstallState::load(&root).is_err());
        // A state naming a version that does not parse is refused.
        std::fs::write(
            &path,
            format!(
                r#"{{"schema":1,"current":{{"version":"../x","manifest_sha256":"{DIGEST}","installed_at":0}},"previous":null,"pending":null,"watermark":null,"updated_at":0}}"#
            ),
        )
        .unwrap();
        assert!(InstallState::load(&root).is_err());
        // An oversized file is refused before parsing.
        std::fs::write(&path, vec![b' '; MAX_STATE_BYTES as usize + 1]).unwrap();
        assert!(matches!(
            InstallState::load(&root),
            Err(UpdateError::State(_))
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn duplicate_slots_are_rejected() {
        let mut state = InstallState {
            current: Some(installed("0.1.0", 1)),
            previous: Some(installed("0.1.0", 2)),
            ..InstallState::default()
        };
        assert!(state.validate().is_err());
        state.previous = Some(installed("0.0.9", 2));
        state.validate().unwrap();
    }

    #[test]
    fn confirm_advances_current_previous_and_watermark() {
        let mut state = InstallState {
            current: Some(installed("0.1.0", 1)),
            ..InstallState::default()
        };
        state.record_pending(installed("0.1.1", 2)).unwrap();
        assert_eq!(state.confirm(10).unwrap(), OpenedVersion::Confirmed);
        assert_eq!(state.current.as_ref().unwrap().version, "0.1.1");
        assert_eq!(state.previous.as_ref().unwrap().version, "0.1.0");
        assert_eq!(state.pending, None);
        assert_eq!(
            state.watermark.as_ref().unwrap().version,
            "0.1.1",
            "the watermark follows a confirmed newer version"
        );
        assert_eq!(state.watermark.as_ref().unwrap().tag, "v0.1.1");
        assert_eq!(state.updated_at, 10);

        // Confirming an older pending version keeps the watermark.
        state.record_pending(installed("0.1.0", 3)).unwrap();
        assert_eq!(
            state.confirm(11).unwrap(),
            OpenedVersion::ConfirmedWithoutWatermark
        );
        assert_eq!(state.watermark.as_ref().unwrap().version, "0.1.1");
        // Nothing pending means nothing to confirm.
        assert!(state.confirm(12).is_err());
    }

    #[test]
    fn roll_back_swaps_current_and_previous() {
        let mut state = InstallState {
            current: Some(installed("0.1.1", 2)),
            previous: Some(installed("0.1.0", 1)),
            ..InstallState::default()
        };
        let rolled = state.roll_back(20).unwrap();
        assert_eq!(rolled.version, "0.1.0");
        assert_eq!(state.current.as_ref().unwrap().version, "0.1.0");
        assert_eq!(state.previous, None);
        assert_eq!(state.updated_at, 20);
        // With no previous, the failed version stays current.
        assert!(state.roll_back(21).is_err());
        assert_eq!(state.current.as_ref().unwrap().version, "0.1.0");
    }

    #[test]
    fn rebuilding_from_disk_orders_and_caps_the_slots() {
        let state = InstallState::from_installed(
            vec![
                installed("0.1.0", 1),
                installed("0.2.0", 3),
                installed("0.1.5", 2),
                installed("0.2.0", 4),
            ],
            50,
        );
        assert_eq!(state.current.as_ref().unwrap().version, "0.2.0");
        assert_eq!(state.previous.as_ref().unwrap().version, "0.1.5");
        assert_eq!(state.watermark.as_ref().unwrap().version, "0.2.0");
        assert_eq!(state.updated_at, 50);
        state.validate().unwrap();
        // An empty directory set is a fresh install, not an error.
        let empty = InstallState::from_installed(Vec::new(), 0);
        assert_eq!(empty.current, None);
        assert_eq!(empty.watermark, None);
        empty.validate().unwrap();
    }
}
