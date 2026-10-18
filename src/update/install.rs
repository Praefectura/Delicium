//! The installation transaction: staging directory in, version directory
//! out, with every byte verified twice.
//!
//! The sequence is deliberately one-way. A release is downloaded into a
//! fresh staging directory beside `app\`, hashed while it is written, read
//! back and hashed again, stamped with `.install.json`, and only then renamed
//! to `app\<version>`. Until that last rename there is no trace of the
//! release anywhere the launcher looks, so a failure at any point leaves the
//! installation exactly as it was.
//!
//! `.install.json` is the only proof a directory is complete. Reuse, crash
//! recovery and rollback all accept a directory only when this marker exists
//! and its digest matches the manifest the release came from.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::UpdateError;
use super::fetch::Fetcher;
use super::manifest::{FileEntry, MAX_ASSET_BYTES, Manifest};
use super::sha256::{Sha256, digest, parse_hex, to_hex};
use super::state::InstalledVersion;
use super::version::Version;
use super::winfile;

/// The marker file inside each installed version directory.
pub const MARKER_NAME: &str = ".install.json";

/// Marker schema this build writes and reads.
pub const MARKER_SCHEMA: u32 = 1;

/// One file recorded in a marker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

impl StagedFile {
    fn from_entry(entry: &FileEntry, written: u64, sha256: String) -> Self {
        Self {
            path: entry.path.clone(),
            size: written,
            sha256,
        }
    }
}

/// `.install.json`: proof that this directory holds exactly the release it
/// claims to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallMarker {
    pub schema: u32,
    pub version: String,
    /// SHA-256 of the manifest this directory was installed from.
    pub manifest_sha256: String,
    pub files: Vec<StagedFile>,
    pub installed_at: u64,
}

impl InstallMarker {
    /// Reads and validates the marker of an installed directory.
    pub fn load(directory: &Path) -> Result<Self, UpdateError> {
        let path = directory.join(MARKER_NAME);
        let bytes = std::fs::read(&path).map_err(|error| UpdateError::io("read", &path, error))?;
        let marker: Self = serde_json::from_slice(&bytes)
            .map_err(|error| UpdateError::state(format!("{}: {error}", path.display())))?;
        marker.validate(&path)?;
        Ok(marker)
    }

    fn validate(&self, path: &Path) -> Result<(), UpdateError> {
        if self.schema != MARKER_SCHEMA {
            return Err(UpdateError::state(format!(
                "{}: marker schema {} is not supported",
                path.display(),
                self.schema
            )));
        }
        Version::parse(&self.version)
            .map_err(|error| UpdateError::state(format!("{}: {error}", path.display())))?;
        if parse_hex(&self.manifest_sha256).is_none() {
            return Err(UpdateError::state(format!(
                "{}: malformed manifest digest",
                path.display()
            )));
        }
        if self.files.is_empty() {
            return Err(UpdateError::state(format!(
                "{}: marker lists no files",
                path.display()
            )));
        }
        for file in &self.files {
            if !super::manifest::valid_relative_path(&file.path) {
                return Err(UpdateError::state(format!(
                    "{}: `{}` is not a safe relative path",
                    path.display(),
                    file.path
                )));
            }
            if parse_hex(&file.sha256).is_none() {
                return Err(UpdateError::state(format!(
                    "{}: `{}` has a malformed digest",
                    path.display(),
                    file.path
                )));
            }
        }
        Ok(())
    }

    /// Whether this directory was installed from exactly this manifest.
    pub fn matches(&self, version: &Version, manifest_sha256: &str) -> bool {
        self.version == version.to_string()
            && self.manifest_sha256.eq_ignore_ascii_case(manifest_sha256)
    }

    /// The files this marker describes, without the marker itself.
    pub fn files(&self) -> &[StagedFile] {
        &self.files
    }
}

/// Writes the installation transaction for one root.
pub struct Installer {
    root: PathBuf,
}

impl Installer {
    /// An installer rooted at the installation directory holding `app\` and
    /// `staging\`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The installation root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where version directories live.
    pub fn app_root(&self) -> PathBuf {
        self.root.join("app")
    }

    /// Where downloads are prepared.
    pub fn staging_root(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// The directory a version installs into.
    pub fn version_dir(&self, version: &Version) -> PathBuf {
        self.app_root().join(version.to_string())
    }

    /// The installed version, when a complete directory for it exists.
    pub fn installed(&self, version: &Version) -> Option<InstallMarker> {
        let directory = self.version_dir(version);
        InstallMarker::load(&directory).ok()
    }

    /// Installs the engine files of `manifest` under `app\<version>`.
    ///
    /// Reuses an already installed, identical directory without touching the
    /// network. Refuses to merge into a directory installed from a different
    /// manifest. On any failure the staging directory is removed and the
    /// installation is unchanged.
    pub fn install_app(
        &self,
        manifest: &Manifest,
        manifest_sha256: &str,
        fetcher: &dyn Fetcher,
        now_unix: u64,
    ) -> Result<InstalledVersion, UpdateError> {
        let version = manifest.version();
        let target = self.version_dir(&version);
        if target.exists() {
            // The directory exists: it is usable only when it was installed
            // from exactly this manifest. Anything else is a conflict, never
            // a merge.
            let marker = InstallMarker::load(&target)?;
            if marker.matches(&version, manifest_sha256) {
                return InstalledVersion::new(&version, manifest_sha256, marker.installed_at);
            }
            return Err(UpdateError::state(format!(
                "{} exists but was installed from a different manifest",
                target.display()
            )));
        }

        std::fs::create_dir_all(self.app_root())
            .map_err(|error| UpdateError::io("create", self.app_root(), error))?;
        std::fs::create_dir_all(self.staging_root())
            .map_err(|error| UpdateError::io("create", self.staging_root(), error))?;
        let staging = self.fresh_staging(&version)?;
        let outcome = self.stage_into(
            &staging,
            manifest,
            &version,
            manifest_sha256,
            fetcher,
            now_unix,
        );
        let staged = match outcome {
            Ok(staged) => staged,
            Err(error) => {
                remove_dir_quietly(&staging);
                return Err(error);
            }
        };
        // Committing is a single rename of a fully verified directory.
        if let Err(error) = winfile::rename_new(&staging, &target) {
            remove_dir_quietly(&staging);
            return Err(UpdateError::io("install into", &target, error));
        }
        Ok(staged)
    }

    /// Downloads the launcher asset of `manifest` into `staging\launcher-<pid>`
    /// and returns the verified file path and digest.
    pub fn stage_launcher(
        &self,
        manifest: &Manifest,
        fetcher: &dyn Fetcher,
    ) -> Result<Option<(PathBuf, FileEntry)>, UpdateError> {
        let Some(entry) = manifest.launcher.first() else {
            return Ok(None);
        };
        let staging =
            self.staging_root()
                .join(format!("launcher-{}-{}", std::process::id(), nanos()));
        std::fs::create_dir_all(&staging)
            .map_err(|error| UpdateError::io("create", &staging, error))?;
        let destination = safe_join(&staging, &entry.path)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| UpdateError::io("create", parent, error))?;
        }
        if let Err(error) = self.download_verified(entry, manifest, fetcher, &destination) {
            remove_dir_quietly(&staging);
            return Err(error);
        }
        Ok(Some((destination, entry.clone())))
    }

    /// Removes every staging directory. Called at startup: nothing here is
    /// referenced by the state and a leftover only wastes space.
    pub fn clear_staging(&self) {
        let staging = self.staging_root();
        let Ok(entries) = std::fs::read_dir(&staging) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Only remove directories; a stray file is not ours to touch.
            if path.is_dir() {
                remove_dir_quietly(&path);
            }
        }
    }

    /// Every complete version directory found under `app\`, newest first.
    pub fn scan_installed(&self) -> Vec<InstalledVersion> {
        let mut installed = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.app_root()) else {
            return installed;
        };
        for entry in entries.flatten() {
            let directory = entry.path();
            if !directory.is_dir() {
                continue;
            }
            let Ok(marker) = InstallMarker::load(&directory) else {
                continue;
            };
            let Ok(version) = Version::parse(&marker.version) else {
                continue;
            };
            if directory.file_name().and_then(|name| name.to_str())
                != Some(version.to_string().as_str())
            {
                continue;
            }
            installed.push(InstalledVersion {
                version: version.to_string(),
                manifest_sha256: marker.manifest_sha256,
                installed_at: marker.installed_at,
            });
        }
        installed.sort_by(|a, b| b.version.cmp(&a.version));
        installed
    }

    /// Deletes a version directory, quietly ignoring a missing one.
    pub fn remove_version(&self, version: &Version) {
        let directory = self.version_dir(version);
        if directory.is_dir() {
            remove_dir_quietly(&directory);
        }
    }

    fn fresh_staging(&self, version: &Version) -> Result<PathBuf, UpdateError> {
        let staging =
            self.staging_root()
                .join(format!("{}-{}-{}", version, std::process::id(), nanos()));
        if staging.exists() {
            remove_dir_quietly(&staging);
        }
        std::fs::create_dir_all(&staging)
            .map_err(|error| UpdateError::io("create", &staging, error))?;
        Ok(staging)
    }

    fn stage_into(
        &self,
        staging: &Path,
        manifest: &Manifest,
        version: &Version,
        manifest_sha256: &str,
        fetcher: &dyn Fetcher,
        now_unix: u64,
    ) -> Result<InstalledVersion, UpdateError> {
        let mut files = Vec::with_capacity(manifest.app.len());
        for entry in &manifest.app {
            let destination = safe_join(staging, &entry.path)?;
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| UpdateError::io("create", parent, error))?;
            }
            let written = self.download_verified(entry, manifest, fetcher, &destination)?;
            files.push(StagedFile::from_entry(entry, written, entry.sha256.clone()));
        }
        // Re-read from disk, not from the download: a short write, a full
        // disk or a concurrent edit all show up here and nowhere else.
        for file in &files {
            let path = safe_join(staging, &file.path)?;
            verify_file(&path, &file.path, file.size, &file.sha256)?;
        }
        let marker = InstallMarker {
            schema: MARKER_SCHEMA,
            version: version.to_string(),
            manifest_sha256: manifest_sha256.to_owned(),
            files,
            installed_at: now_unix,
        };
        write_marker(staging, &marker)?;
        InstalledVersion::new(version, manifest_sha256, now_unix)
    }

    fn download_verified(
        &self,
        entry: &FileEntry,
        manifest: &Manifest,
        fetcher: &dyn Fetcher,
        destination: &Path,
    ) -> Result<u64, UpdateError> {
        let url = entry.download_url(&manifest.tag);
        let file = std::fs::File::create(destination)
            .map_err(|error| UpdateError::io("create", destination, error))?;
        let limit = entry.size.min(MAX_ASSET_BYTES);
        let mut writer = HashingWriter::new(file, entry.size);
        let written = fetcher
            .download(&url, limit, &mut writer)
            .inspect_err(|_| {
                let _ = std::fs::remove_file(destination);
            })?;
        if written != entry.size {
            let _ = std::fs::remove_file(destination);
            return Err(UpdateError::Truncated {
                path: entry.path.clone(),
                actual: written,
                expected: entry.size,
            });
        }
        let actual = to_hex(&writer.digest());
        let file = writer.finish();
        file.sync_all()
            .map_err(|error| UpdateError::io("flush", destination, error))?;
        if !actual.eq_ignore_ascii_case(&entry.sha256) {
            let _ = std::fs::remove_file(destination);
            return Err(UpdateError::HashMismatch {
                path: entry.path.clone(),
                expected: entry.sha256.clone(),
                actual,
            });
        }
        Ok(written)
    }
}

/// A writer that hashes and counts everything that reaches the file.
struct HashingWriter {
    file: std::fs::File,
    hasher: Sha256,
    written: u64,
    limit: u64,
}

impl HashingWriter {
    fn new(file: std::fs::File, limit: u64) -> Self {
        Self {
            file,
            hasher: Sha256::new(),
            written: 0,
            limit,
        }
    }

    fn finish(self) -> std::fs::File {
        self.file
    }

    /// The digest of everything written so far; the writer keeps working.
    fn digest(&self) -> [u8; super::sha256::DIGEST_BYTES] {
        self.hasher.clone().finish()
    }
}

impl Write for HashingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.written);
        if buffer.len() as u64 > remaining {
            return Err(std::io::Error::other(format!(
                "payload exceeded its declared {} bytes",
                self.limit
            )));
        }
        let written = self.file.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Reads a file back and checks its size and digest.
fn verify_file(path: &Path, name: &str, size: u64, sha256: &str) -> Result<(), UpdateError> {
    let bytes = std::fs::read(path).map_err(|error| UpdateError::io("read", path, error))?;
    if bytes.len() as u64 != size {
        return Err(UpdateError::Truncated {
            path: name.to_owned(),
            actual: bytes.len() as u64,
            expected: size,
        });
    }
    let actual = to_hex(&digest(&bytes));
    if !actual.eq_ignore_ascii_case(sha256) {
        return Err(UpdateError::HashMismatch {
            path: name.to_owned(),
            expected: sha256.to_owned(),
            actual,
        });
    }
    Ok(())
}

fn write_marker(directory: &Path, marker: &InstallMarker) -> Result<(), UpdateError> {
    let path = directory.join(MARKER_NAME);
    let text = serde_json::to_vec_pretty(marker)
        .map_err(|error| UpdateError::state(format!("cannot serialize the marker: {error}")))?;
    let mut file =
        std::fs::File::create(&path).map_err(|error| UpdateError::io("create", &path, error))?;
    file.write_all(&text)
        .map_err(|error| UpdateError::io("write", &path, error))?;
    file.sync_all()
        .map_err(|error| UpdateError::io("flush", &path, error))
}

/// Joins a validated relative path onto a root, refusing anything that would
/// still land outside it.
fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, UpdateError> {
    if !super::manifest::valid_relative_path(relative) {
        return Err(UpdateError::state(format!(
            "`{relative}` is not a safe relative path"
        )));
    }
    let joined = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
    if joined.starts_with(root) {
        Ok(joined)
    } else {
        Err(UpdateError::state(format!(
            "`{relative}` escapes {}",
            root.display()
        )))
    }
}

fn remove_dir_quietly(path: &Path) {
    if let Err(error) = std::fs::remove_dir_all(path) {
        log::warn!("Cannot remove {}: {error}", path.display());
    }
}

/// A counter that makes a staging name unique within this process.
fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::fetch::MemFetcher;
    use crate::update::manifest::{ARCH, Manifest, OS, PRODUCT, SCHEMA};

    const ENGINE: &[u8] = b"engine-binary-payload";
    const LAUNCHER: &[u8] = b"launcher-binary";

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "delicium-install-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn entry(path: &str, asset: &str, payload: &[u8]) -> FileEntry {
        FileEntry {
            path: path.into(),
            asset: asset.into(),
            size: payload.len() as u64,
            sha256: to_hex(&digest(payload)),
        }
    }

    fn manifest(version: &str) -> Manifest {
        Manifest {
            schema: SCHEMA,
            product: PRODUCT.into(),
            channel: "stable".into(),
            version: version.into(),
            tag: format!("v{version}"),
            published: "2026-10-18T00:00:00Z".into(),
            notes: None,
            min_launcher_version: "0.1.0".into(),
            os: OS.into(),
            arch: ARCH.into(),
            launcher: vec![entry(
                "delicium-launcher.exe",
                "delicium-launcher.exe",
                LAUNCHER,
            )],
            app: vec![entry("delicium.exe", "delicium.exe", ENGINE)],
        }
    }

    fn fetcher(manifest: &Manifest, payload: &[u8]) -> MemFetcher {
        let mut fetcher = MemFetcher::new();
        for entry in &manifest.app {
            fetcher.insert(entry.download_url(&manifest.tag), payload.to_vec());
        }
        fetcher
    }

    fn manifest_digest(manifest: &Manifest) -> String {
        to_hex(&digest(&serde_json::to_vec(manifest).unwrap()))
    }

    #[test]
    fn installs_a_verified_release_and_reuses_it() {
        let root = root("success");
        let manifest = manifest("0.1.1");
        let digest = manifest_digest(&manifest);
        let installer = Installer::new(&root);
        let installed = installer
            .install_app(&manifest, &digest, &fetcher(&manifest, ENGINE), 100)
            .unwrap();
        assert_eq!(installed.version, "0.1.1");
        let directory = installer.version_dir(&manifest.version());
        assert_eq!(
            std::fs::read(directory.join("delicium.exe")).unwrap(),
            ENGINE
        );
        assert!(directory.join(MARKER_NAME).is_file());
        let marker = InstallMarker::load(&directory).unwrap();
        assert_eq!(marker.files.len(), 1);
        assert_eq!(marker.files[0].sha256, manifest.app[0].sha256);
        // Staging is empty again.
        assert!(
            std::fs::read_dir(installer.staging_root())
                .map(|entries| entries.count() == 0)
                .unwrap_or(true)
        );
        // A second install of the same manifest reuses the directory without
        // a single request.
        let empty = MemFetcher::new();
        let again = installer
            .install_app(&manifest, &digest, &empty, 200)
            .unwrap();
        assert_eq!(
            again.installed_at, 100,
            "the reuse keeps the original stamp"
        );
        assert!(empty.requested().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_different_manifest_for_an_installed_version_is_a_conflict() {
        let root = root("conflict");
        let manifest = manifest("0.1.1");
        let installer = Installer::new(&root);
        installer
            .install_app(
                &manifest,
                &manifest_digest(&manifest),
                &fetcher(&manifest, ENGINE),
                1,
            )
            .unwrap();
        let error = installer
            .install_app(&manifest, &"0".repeat(64), &MemFetcher::new(), 2)
            .unwrap_err();
        assert!(error.to_string().contains("different manifest"), "{error}");
        // The installed directory is untouched.
        assert_eq!(
            std::fs::read(
                installer
                    .version_dir(&manifest.version())
                    .join("delicium.exe")
            )
            .unwrap(),
            ENGINE
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn bad_payloads_never_create_a_version_directory() {
        let root = root("bad");
        let installer = Installer::new(&root);
        let manifest = manifest("0.1.2");
        let digest = manifest_digest(&manifest);
        let version_dir = installer.version_dir(&manifest.version());

        // Wrong content: the digest check refuses it.
        let mut wrong = MemFetcher::new();
        wrong.insert(
            manifest.app[0].download_url(&manifest.tag),
            b"tampered".to_vec(),
        );
        let error = installer
            .install_app(&manifest, &digest, &wrong, 1)
            .unwrap_err();
        assert!(matches!(
            error,
            UpdateError::Truncated { .. } | UpdateError::HashMismatch { .. }
        ));
        assert!(!version_dir.exists());

        // Correct content of the wrong length is refused as truncated.
        let mut short = MemFetcher::new();
        short.insert(
            manifest.app[0].download_url(&manifest.tag),
            ENGINE[..4].to_vec(),
        );
        let error = installer
            .install_app(&manifest, &digest, &short, 1)
            .unwrap_err();
        assert!(matches!(error, UpdateError::Truncated { .. }), "{error}");
        assert!(!version_dir.exists());

        // A 404 is reported as an HTTP failure.
        let mut missing = MemFetcher::new();
        missing.insert_status(manifest.app[0].download_url(&manifest.tag), 404);
        let error = installer
            .install_app(&manifest, &digest, &missing, 1)
            .unwrap_err();
        assert!(matches!(error, UpdateError::Http(404)), "{error}");
        assert!(!version_dir.exists());

        // Same-length tampering reaches the hash comparison.
        let mut same_len = MemFetcher::new();
        same_len.insert(
            manifest.app[0].download_url(&manifest.tag),
            vec![b'x'; ENGINE.len()],
        );
        let error = installer
            .install_app(&manifest, &digest, &same_len, 1)
            .unwrap_err();
        assert!(matches!(error, UpdateError::HashMismatch { .. }), "{error}");
        assert!(!version_dir.exists());

        // Staging is cleaned in every case.
        let leftovers = std::fs::read_dir(installer.staging_root())
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn launcher_assets_are_staged_and_verified() {
        let root = root("launcher");
        let manifest = manifest("0.1.1");
        let installer = Installer::new(&root);
        let mut fetcher = MemFetcher::new();
        fetcher.insert(
            manifest.launcher[0].download_url(&manifest.tag),
            LAUNCHER.to_vec(),
        );
        let (path, entry) = installer
            .stage_launcher(&manifest, &fetcher)
            .unwrap()
            .unwrap();
        assert_eq!(entry.path, "delicium-launcher.exe");
        assert_eq!(std::fs::read(&path).unwrap(), LAUNCHER);
        assert!(path.starts_with(installer.staging_root()));
        // A malformed launcher payload is refused and cleaned up.
        let mut bad = MemFetcher::new();
        bad.insert(
            manifest.launcher[0].download_url(&manifest.tag),
            b"no".to_vec(),
        );
        assert!(installer.stage_launcher(&manifest, &bad).is_err());
        // A manifest without a launcher group stages nothing.
        let mut bare = manifest.clone();
        bare.launcher.clear();
        assert!(installer.stage_launcher(&bare, &fetcher).unwrap().is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scanning_finds_complete_directories_only() {
        let root = root("scan");
        let installer = Installer::new(&root);
        for version in ["0.1.0", "0.1.1"] {
            let manifest = manifest(version);
            installer
                .install_app(
                    &manifest,
                    &manifest_digest(&manifest),
                    &fetcher(&manifest, ENGINE),
                    5,
                )
                .unwrap();
        }
        // An empty directory, and one with a marker naming another version,
        // are not installed versions.
        std::fs::create_dir_all(installer.app_root().join("9.9.9")).unwrap();
        let mismatched = installer.app_root().join("0.2.0");
        std::fs::create_dir_all(&mismatched).unwrap();
        std::fs::write(
            mismatched.join(MARKER_NAME),
            serde_json::to_vec(&InstallMarker {
                schema: MARKER_SCHEMA,
                version: "0.1.0".into(),
                manifest_sha256: "0".repeat(64),
                files: vec![StagedFile {
                    path: "delicium.exe".into(),
                    size: 1,
                    sha256: "0".repeat(64),
                }],
                installed_at: 0,
            })
            .unwrap(),
        )
        .unwrap();
        let found = installer.scan_installed();
        let versions: Vec<_> = found.iter().map(|entry| entry.version.as_str()).collect();
        assert_eq!(versions, ["0.1.1", "0.1.0"]);
        // Staging cleanup removes directories and leaves files alone.
        let staging = installer.staging_root();
        std::fs::create_dir_all(staging.join("leftover")).unwrap();
        std::fs::write(staging.join("note.txt"), b"keep").unwrap();
        installer.clear_staging();
        assert!(!staging.join("leftover").exists());
        assert!(staging.join("note.txt").exists());
        // Removing a version deletes only that directory.
        installer.remove_version(&Version::parse("0.1.0").unwrap());
        assert!(
            !installer
                .version_dir(&Version::parse("0.1.0").unwrap())
                .exists()
        );
        assert!(
            installer
                .version_dir(&Version::parse("0.1.1").unwrap())
                .exists()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn interrupted_staging_is_invisible_and_cleanable() {
        let root = root("interrupted");
        let installer = Installer::new(&root);
        // A staging directory with a half-written file, as a crash leaves it.
        let staging = installer.staging_root().join("0.1.1-1-2");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("delicium.exe"), b"half").unwrap();
        assert!(
            installer
                .installed(&Version::parse("0.1.1").unwrap())
                .is_none()
        );
        assert!(installer.scan_installed().is_empty());
        installer.clear_staging();
        assert!(!staging.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn safe_join_refuses_escapes() {
        let root = Path::new(r"C:\root");
        assert_eq!(
            safe_join(root, "app/delicium.exe").unwrap(),
            root.join("app").join("delicium.exe")
        );
        for relative in ["../x", "/x", r"a\x", "C:/x", "a//b"] {
            assert!(safe_join(root, relative).is_err(), "accepted {relative}");
        }
    }
}
