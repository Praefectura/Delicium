//! The update check: fetch the release manifest, decide, install.
//!
//! One function does the whole transaction so it can run on a worker thread
//! while the pet keeps running, and the same function serves the first launch
//! (when there is no engine yet) and the settings page's manual check.
//!
//! Decisions, in order:
//!
//! 1. A release whose `min_launcher_version` is newer than this launcher
//!    replaces the launcher image and is **not** installed now: the engine it
//!    ships may speak a protocol this build does not. The next start runs the
//!    new launcher, which installs it. The pet still starts normally, from
//!    the version already installed.
//! 2. A release at or below what is installed is up to date.
//! 3. Otherwise the release must be strictly newer than the running engine,
//!    newer than the watermark (so a stale release cannot be installed after
//!    a rollback), and on the stable channel. Then its files are downloaded,
//!    verified, and committed to `app\<version>`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::update::{
    Channel, Fetcher, InstallMarker, InstallState, InstalledVersion, Manifest, UpdateError,
    Version, install::Installer, manifest::MAX_MANIFEST_BYTES, sha256::to_hex, url,
};

use super::{paths, unix_now};

/// What a check did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing newer is published, or it is already installed.
    UpToDate { version: Version },
    /// A newer engine was installed; it starts on the next launch.
    EngineInstalled {
        version: Version,
        installed: InstalledVersion,
    },
    /// The launcher image was replaced on disk; it takes effect next start.
    LauncherReplaced { version: Version },
}

impl Outcome {
    /// One line for the settings page and the log.
    pub fn describe(&self) -> String {
        match self {
            Self::UpToDate { version } => format!("Up to date ({version})."),
            Self::EngineInstalled { version, .. } => {
                format!("Installed {version}; it starts next time.")
            }
            Self::LauncherReplaced { version } => {
                format!("Launcher updated to {version}; restart to use it.")
            }
        }
    }
}

/// On-disk record of the launcher image that was last put in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherRecord {
    pub schema: u32,
    pub version: String,
    pub sha256: String,
    /// The backup of the image this one replaced, relative to the root.
    pub backup: String,
    pub installed_at: u64,
}

pub const RECORD_SCHEMA: u32 = 1;

impl LauncherRecord {
    pub fn load(root: &Path) -> Option<Self> {
        let path = paths::record_path(root);
        let bytes = std::fs::read(&path).ok()?;
        let record: Self = serde_json::from_slice(&bytes).ok()?;
        (record.schema == RECORD_SCHEMA).then_some(record)
    }

    fn save(&self, root: &Path) -> Result<(), String> {
        let path = paths::record_path(root);
        let text = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("Cannot serialize the launcher record: {error}"))?;
        let temporary = root.join(format!("launcher.{}.tmp", std::process::id()));
        std::fs::write(&temporary, &text)
            .map_err(|error| format!("Cannot write {}: {error}", temporary.display()))?;
        crate::update::winfile::replace_atomic(&temporary, &path)
            .map_err(|error| format!("Cannot replace {}: {error}", path.display()))
    }
}

/// Runs one update check against `root` and applies it.
///
/// `state` is read, never written: the caller owns the installation state and
/// applies [`Outcome::EngineInstalled`] under its own lock.
pub fn check_and_install(
    root: &Path,
    state: &InstallState,
    fetcher: &dyn Fetcher,
) -> Result<Outcome, String> {
    let manifest_url = url::manifest_url().map_err(|error| error.to_string())?;
    let response = fetcher
        .fetch(&manifest_url, MAX_MANIFEST_BYTES)
        .map_err(|error| error.to_string())?;
    let body = response.into_body().map_err(|error| error.to_string())?;
    let manifest_sha256 = to_hex(&crate::update::sha256::digest(&body));
    let manifest = Manifest::parse(&body).map_err(|error| error.to_string())?;
    let target = manifest.version();
    let launcher_version = Version::current().map_err(|error| error.to_string())?;

    // The launcher the release requires is newer than this one: put the new
    // launcher in place and leave the engine for the next start.
    let required = manifest.min_launcher_version();
    if required > launcher_version {
        return install_launcher(root, &manifest, &manifest_sha256, required, fetcher);
    }

    let installer = Installer::new(root);
    if state.current_version().as_ref() == Some(&target) && installer.installed(&target).is_some() {
        return Ok(Outcome::UpToDate { version: target });
    }
    // A directory already installed from exactly this manifest is reused
    // without a new decision; anything else must pass the version rules.
    let already_installed = installer
        .installed(&target)
        .is_some_and(|marker| marker.matches(&target, &manifest_sha256));
    if !already_installed {
        decide(state, &launcher_version, &target, &manifest)?;
    }
    let installed = installer
        .install_app(&manifest, &manifest_sha256, fetcher, unix_now())
        .map_err(|error| error.to_string())?;
    Ok(Outcome::EngineInstalled {
        version: target,
        installed,
    })
}

/// Whether `target` may replace the installed engine.
fn decide(
    state: &InstallState,
    launcher_version: &Version,
    target: &Version,
    manifest: &Manifest,
) -> Result<(), String> {
    let channel = Channel::parse(&manifest.channel).map_err(|error| error.to_string())?;
    match state.current_version() {
        Some(current) => crate::update::version::accepts_candidate(
            &current,
            state.watermark_version().as_ref(),
            channel,
            target,
        )
        .map_err(|error| error.to_string()),
        None => {
            // A fresh installation has no floor other than this build itself:
            // a release older than the launcher that would supervise it is
            // never installed.
            if !channel.accepts(target) {
                return Err(
                    UpdateError::refused(format!("{target} is not on the stable channel"))
                        .to_string(),
                );
            }
            if target < launcher_version {
                return Err(UpdateError::refused(format!(
                    "{target} is older than this launcher ({launcher_version})"
                ))
                .to_string());
            }
            Ok(())
        }
    }
}

/// Replaces the running launcher image with the release's, keeping a backup.
fn install_launcher(
    root: &Path,
    manifest: &Manifest,
    manifest_sha256: &str,
    required: Version,
    fetcher: &dyn Fetcher,
) -> Result<Outcome, String> {
    let installer = Installer::new(root);
    let Some((staged, entry)) = installer
        .stage_launcher(manifest, fetcher)
        .map_err(|error| error.to_string())?
    else {
        return Err(UpdateError::refused(format!(
            "the release requires launcher {required} but ships no launcher"
        ))
        .to_string());
    };
    let current = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the running launcher: {error}"))?;
    let installed = paths::launcher_exe(root);
    if !same_path(&current, &installed) {
        return Err(format!(
            "the running launcher ({}) is not the installed image ({}); self-update is skipped",
            current.display(),
            installed.display()
        ));
    }
    let backup = paths::backup_dir(root).join(format!("delicium-launcher-{}.exe", unix_now()));
    super::win32::replace_file(&installed, &staged, &backup)?;
    log::info!(
        "launcher image replaced with {} ({} bytes, sha256 {}); backup {}",
        manifest.version,
        entry.size,
        entry.sha256,
        backup.display()
    );
    let record = LauncherRecord {
        schema: RECORD_SCHEMA,
        version: manifest.version.clone(),
        sha256: entry.sha256.clone(),
        backup: backup
            .strip_prefix(root)
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| backup.display().to_string()),
        installed_at: unix_now(),
    };
    if let Err(error) = record.save(root) {
        // The image is already replaced; a missing record only costs the
        // settings page its version line.
        log::warn!("Cannot write the launcher record: {error}");
    }
    let _ = manifest_sha256;
    Ok(Outcome::LauncherReplaced { version: required })
}

/// Whether two paths name the same file, compared case-insensitively because
/// Windows file names are.
fn same_path(left: &Path, right: &Path) -> bool {
    fn normalized(path: &Path) -> String {
        path.to_string_lossy().replace('/', "\\").to_lowercase()
    }
    normalized(left) == normalized(right)
}

/// The installed engine this installation should run, preferring a version
/// that was installed but never started.
pub fn target_version(installer: &Installer, state: &InstallState) -> Option<InstalledVersion> {
    state_target(installer.root(), state).or_else(|| installer.scan_installed().into_iter().next())
}

/// The version the state names, without falling back to a directory scan.
///
/// Used after a version was discarded: the directory scan would find the
/// same directory again and start the build that just failed.
pub fn state_target(root: &Path, state: &InstallState) -> Option<InstalledVersion> {
    [&state.pending, &state.current]
        .into_iter()
        .flatten()
        .find(|candidate| candidate.parsed().is_ok() && engine_path(root, candidate).is_file())
        .cloned()
}

/// Removes all but the newest `keep` launcher backups.
///
/// A backup exists so a broken launcher image can be put back by hand;
/// nothing in the launcher reads it, so leaving every replaced image behind
/// costs about 2 MiB per update. Only files named
/// `delicium-launcher-<unix>.exe` are considered.
pub fn prune_launcher_backups(root: &Path, keep: usize) -> Vec<PathBuf> {
    let directory = paths::backup_dir(root);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut backups: Vec<(u64, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter_map(|path| backup_stamp(&path).map(|stamp| (stamp, path)))
        .collect();
    backups.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let mut removed = Vec::new();
    for (_, path) in backups.into_iter().skip(keep) {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                log::info!("removed launcher backup {}", path.display());
                removed.push(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("cannot remove {}: {error}", path.display()),
        }
    }
    removed
}

/// The Unix stamp in `delicium-launcher-<stamp>.exe`, when the name matches.
fn backup_stamp(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("delicium-launcher-")?
        .strip_suffix(".exe")?
        .parse()
        .ok()
}

/// The engine binary of an installed version.
pub fn engine_path(root: &Path, installed: &InstalledVersion) -> std::path::PathBuf {
    installed.directory(root).join(paths::ENGINE_FILE)
}

/// Whether the installed directory of `version` is complete.
pub fn installed_marker(root: &Path, version: &Version) -> Option<InstallMarker> {
    Installer::new(root).installed(version)
}

/// Makes an extracted launcher the installed one.
///
/// A release is downloaded as a zip, so the launcher is not running from
/// `%LOCALAPPDATA%\Delicium\delicium-launcher.exe` on the first launch. Self
/// update replaces that path and only that path, so the image has to be there
/// before the first check runs; otherwise a release that raises
/// `min_launcher_version` could never be installed and every later release
/// would report a failure the user cannot act on.
///
/// Returns whether a copy was made. The copy is skipped when the running
/// image already is the installed one, when the running image cannot replace
/// a file that a different installation owns, and when the copy cannot be
/// read back at the expected size. In every skipped case the launcher keeps
/// running from where it was; the caller only logs the result.
pub fn adopt_self(root: &Path) -> Result<bool, String> {
    let current = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the running launcher: {error}"))?;
    adopt_image(&current, &paths::launcher_exe(root))
}

/// The copy itself, for a caller that knows both paths.
///
/// Split from [`adopt_self`] so the decision — same file, copy, or fail — is
/// testable without a second installation to run from.
fn adopt_image(current: &Path, installed: &Path) -> Result<bool, String> {
    if same_path(current, installed) {
        return Ok(false);
    }
    let source = std::fs::read(current)
        .map_err(|error| format!("Cannot read {}: {error}", current.display()))?;
    if source.is_empty() {
        return Err(format!("{} is empty", current.display()));
    }
    let Some(parent) = installed.parent() else {
        return Err(format!("{} has no parent directory", installed.display()));
    };
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
    // The destination is written through the staging directory and renamed,
    // so a crash never leaves half an executable where the shortcut points.
    let staging = parent
        .join(paths::STAGING_DIR)
        .join(format!("adopt-{}", std::process::id()));
    std::fs::create_dir_all(&staging)
        .map_err(|error| format!("Cannot create {}: {error}", staging.display()))?;
    let staged = staging.join(paths::LAUNCHER_FILE);
    let outcome = std::fs::write(&staged, &source)
        .map_err(|error| format!("Cannot write {}: {error}", staged.display()))
        .and_then(|()| {
            let written = std::fs::metadata(&staged)
                .map_err(|error| format!("Cannot read {}: {error}", staged.display()))?;
            if written.len() != source.len() as u64 {
                return Err(format!(
                    "{} has {} bytes after copying {}",
                    staged.display(),
                    written.len(),
                    source.len()
                ));
            }
            crate::update::winfile::replace_atomic(&staged, installed)
                .map_err(|error| format!("Cannot replace {}: {error}", installed.display()))
        });
    let _ = std::fs::remove_dir_all(&staging);
    outcome.map(|()| {
        log::info!(
            "launcher adopted into the installation: {} -> {}",
            current.display(),
            installed.display()
        );
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::{FileEntry, MemFetcher, sha256::digest};

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "delicium-launcher-update-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn release(version: &str, min_launcher: &str, payload: &[u8]) -> (Manifest, Vec<u8>, String) {
        let digest_hex = to_hex(&digest(payload));
        let manifest = Manifest {
            schema: crate::update::manifest::SCHEMA,
            product: "delicium".into(),
            channel: "stable".into(),
            version: version.into(),
            tag: format!("v{version}"),
            published: "2026-10-18T00:00:00Z".into(),
            notes: None,
            min_launcher_version: min_launcher.into(),
            os: "windows".into(),
            arch: "x86_64".into(),
            launcher: vec![],
            app: vec![FileEntry {
                path: "delicium.exe".into(),
                asset: "delicium.exe".into(),
                size: payload.len() as u64,
                sha256: digest_hex.clone(),
            }],
        };
        (manifest, payload.to_vec(), digest_hex)
    }

    fn fetcher_for(manifest: &Manifest, payload: &[u8]) -> MemFetcher {
        let mut fetcher = MemFetcher::new();
        fetcher.insert(url::MANIFEST_URL, serde_json::to_vec(manifest).unwrap());
        fetcher.insert(
            format!(
                "{}/{}/delicium.exe",
                crate::update::manifest::RELEASE_DOWNLOAD_PREFIX,
                manifest.tag
            ),
            payload.to_vec(),
        );
        fetcher
    }

    #[test]
    fn installs_a_newer_engine_and_reuses_it_the_second_time() {
        let root = temp_root("install");
        let (manifest, payload, _) = release("0.2.0", "0.1.0", b"engine bytes");
        let fetcher = fetcher_for(&manifest, &payload);
        let state = InstallState::default();
        let outcome = check_and_install(&root, &state, &fetcher).unwrap();
        let Outcome::EngineInstalled { version, installed } = outcome else {
            panic!("expected an install");
        };
        assert_eq!(version.to_string(), "0.2.0");
        assert!(engine_path(&root, &installed).is_file());
        // The same manifest installs nothing new: the directory is reused.
        let state = InstallState {
            current: Some(installed),
            ..InstallState::default()
        };
        match check_and_install(&root, &state, &fetcher).unwrap() {
            Outcome::UpToDate { version } => assert_eq!(version.to_string(), "0.2.0"),
            other => panic!("expected up to date, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_releases_that_are_not_newer() {
        let root = temp_root("older");
        let (manifest, payload, _) = release("0.1.0", "0.1.0", b"engine");
        let fetcher = fetcher_for(&manifest, &payload);
        let current =
            InstalledVersion::new(&Version::parse("0.2.0").unwrap(), to_hex(&digest(b"m")), 1)
                .unwrap();
        let state = InstallState {
            current: Some(current),
            ..InstallState::default()
        };
        let error = check_and_install(&root, &state, &fetcher).unwrap_err();
        assert!(error.contains("not newer"), "{error}");
        // A stale release below the watermark is refused even after a
        // rollback left an older version current.
        let (manifest, payload, _) = release("0.3.0", "0.1.0", b"engine3");
        let fetcher = fetcher_for(&manifest, &payload);
        let state = InstallState {
            watermark: Some(crate::update::Watermark {
                version: "0.4.0".into(),
                tag: "v0.4.0".into(),
            }),
            ..state
        };
        let error = check_and_install(&root, &state, &fetcher).unwrap_err();
        assert!(error.contains("newest installed"), "{error}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_required_launcher_is_installed_instead_of_the_engine() {
        let root = temp_root("launcher");
        // Pretend the release requires launcher 99.0.0, which no running
        // build can be; the code decides to self-update before the engine.
        let (mut manifest, payload, _) = release("0.2.0", "99.0.0", b"engine");
        manifest.launcher = vec![FileEntry {
            path: "delicium-launcher.exe".into(),
            asset: "delicium-launcher.exe".into(),
            size: 4,
            sha256: to_hex(&digest(b"new!")),
        }];
        let mut fetcher = fetcher_for(&manifest, &payload);
        fetcher.insert(
            format!(
                "{}/{}/delicium-launcher.exe",
                crate::update::manifest::RELEASE_DOWNLOAD_PREFIX,
                manifest.tag
            ),
            b"new!".to_vec(),
        );
        // The running test binary is not the installed launcher, so the
        // self-update must be refused rather than overwrite the test exe.
        let error = check_and_install(&root, &InstallState::default(), &fetcher).unwrap_err();
        assert!(error.contains("self-update is skipped"), "{error}");
        // The staging copy is not executed and the engine was not installed.
        assert!(!root.join("app").join("0.2.0").join("delicium.exe").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn launcher_backups_are_pruned_to_the_newest() {
        let root = temp_root("backups");
        let directory = paths::backup_dir(&root);
        std::fs::create_dir_all(&directory).unwrap();
        for stamp in [100u64, 300, 200] {
            std::fs::write(
                directory.join(format!("delicium-launcher-{stamp}.exe")),
                b"image",
            )
            .unwrap();
        }
        // Files that do not carry the stamp naming are not ours to remove.
        std::fs::write(directory.join("notes.txt"), b"keep").unwrap();
        std::fs::write(directory.join("delicium-launcher-latest.exe"), b"keep").unwrap();

        let removed = prune_launcher_backups(&root, 1);
        assert_eq!(
            removed,
            [
                directory.join("delicium-launcher-200.exe"),
                directory.join("delicium-launcher-100.exe")
            ]
        );
        assert!(directory.join("delicium-launcher-300.exe").exists());
        assert!(directory.join("notes.txt").exists());
        assert!(directory.join("delicium-launcher-latest.exe").exists());
        // Pruning again leaves the newest alone.
        assert!(prune_launcher_backups(&root, 1).is_empty());
        // Keep zero removes everything named like a backup, nothing else.
        assert_eq!(prune_launcher_backups(&root, 0).len(), 1);
        assert!(directory.join("notes.txt").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_directory_scan_is_only_a_fallback_for_the_state() {
        let root = temp_root("state-target");
        let (manifest, payload, _) = release("0.2.0", "0.1.0", b"engine bytes");
        let fetcher = fetcher_for(&manifest, &payload);
        let Outcome::EngineInstalled { installed, .. } =
            check_and_install(&root, &InstallState::default(), &fetcher).unwrap()
        else {
            panic!("expected an install");
        };
        // A discarded version is not a target again: the scan would find its
        // directory, the state must not.
        let state = InstallState::default();
        assert!(state_target(&root, &state).is_none());
        assert!(target_version(&Installer::new(&root), &state).is_some());
        // The state naming it wins.
        let state = InstallState {
            current: Some(installed.clone()),
            ..InstallState::default()
        };
        assert_eq!(state_target(&root, &state), Some(installed));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn adoption_is_skipped_when_the_image_is_already_installed() {
        let root = temp_root("adopt-skip");
        let installed = paths::launcher_exe(&root);
        std::fs::write(&installed, b"installed").unwrap();
        assert!(!adopt_image(&installed, &installed).unwrap());
        assert_eq!(std::fs::read(&installed).unwrap(), b"installed");
        // Windows paths differ in case and separators but name one file.
        let spelled = root
            .join(paths::LAUNCHER_FILE.to_uppercase())
            .to_string_lossy()
            .replace('\\', "/");
        assert!(!adopt_image(std::path::Path::new(&spelled), &installed).unwrap());
        assert_eq!(std::fs::read(&installed).unwrap(), b"installed");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn adoption_copies_the_running_image_into_the_installation() {
        let root = temp_root("adopt-copy");
        let source = root.join("extracted").join("delicium-launcher.exe");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"launcher bytes").unwrap();
        let installed = paths::launcher_exe(&root);
        assert!(adopt_image(&source, &installed).unwrap());
        assert_eq!(std::fs::read(&installed).unwrap(), b"launcher bytes");
        // The source is left alone and no staging directory survives.
        assert_eq!(std::fs::read(&source).unwrap(), b"launcher bytes");
        assert!(
            std::fs::read_dir(root.join(paths::STAGING_DIR))
                .map(|entries| entries.count() == 0)
                .unwrap_or(true)
        );
        // A second adoption of the same image finds itself installed.
        assert!(!adopt_image(&installed, &installed).unwrap());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
