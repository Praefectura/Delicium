//! The release manifest: the small JSON document a release publishes at a
//! fixed URL, describing which files make up that release and what each of
//! them must hash to.
//!
//! The manifest is written by the release workflow and read over the network,
//! so every field is untrusted. Parsing is strict (unknown fields are
//! rejected, so a newer manifest cannot be silently misread by an older
//! client), every asset path is checked to stay inside the install directory,
//! and every download URL is derived from the fixed repository and the
//! manifest's own tag rather than read from the document, so a compromised
//! manifest cannot redirect a download to another host.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::error::UpdateError;
use super::sha256::{HEX_LEN, parse_hex};
use super::version::{Channel, Version};

/// Manifest schema this build understands.
pub const SCHEMA: u32 = 1;

/// The product name every Delicium manifest must carry.
pub const PRODUCT: &str = "delicium";

/// Operating system this build installs on.
pub const OS: &str = "windows";

/// Architecture this build installs on.
pub const ARCH: &str = "x86_64";

/// Largest manifest this build will read; far above what a release needs.
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// Largest single asset this build will download (the engine is ~24 MB).
pub const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;

/// Most files one manifest may list, across both groups.
pub const MAX_FILES: usize = 32;

/// The fixed release-page URL every asset of a tag is served from.
pub const RELEASE_DOWNLOAD_PREFIX: &str =
    "https://github.com/Praefectura/Delicium/releases/download";

/// One file a release ships.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// Relative path inside the install directory, `/`-separated.
    pub path: String,
    /// File name of the release asset that carries it.
    pub asset: String,
    pub size: u64,
    /// Lowercase hexadecimal SHA-256 of the whole file.
    pub sha256: String,
}

impl FileEntry {
    /// The URL the release serves this asset from, derived from the tag
    /// rather than stored in the manifest.
    pub fn download_url(&self, tag: &str) -> String {
        format!("{RELEASE_DOWNLOAD_PREFIX}/{tag}/{}", self.asset)
    }
}

/// A whole release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub product: String,
    pub channel: String,
    /// `MAJOR.MINOR.PATCH[-pre]`; must equal the tag without its `v`.
    pub version: String,
    /// Release tag, `v` plus the version.
    pub tag: String,
    /// RFC 3339 timestamp, informational only.
    pub published: String,
    /// Release notes, informational only.
    pub notes: Option<String>,
    /// Oldest launcher that can install this release.
    pub min_launcher_version: String,
    pub os: String,
    pub arch: String,
    /// Files placed next to `delicium-launcher.exe` (the launcher itself).
    pub launcher: Vec<FileEntry>,
    /// Files placed in `app\<version>\` (the engine).
    pub app: Vec<FileEntry>,
}

impl Manifest {
    /// Parses and validates a manifest document.
    pub fn parse(bytes: &[u8]) -> Result<Self, UpdateError> {
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(UpdateError::TooLarge {
                what: "update manifest".into(),
                actual: bytes.len() as u64,
                limit: MAX_MANIFEST_BYTES,
            });
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|error| UpdateError::manifest(format!("not UTF-8: {error}")))?;
        let manifest: Self = serde_json::from_str(text)
            .map_err(|error| UpdateError::manifest(format!("not valid JSON: {error}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Checks every field and cross-field invariant.
    pub fn validate(&self) -> Result<(), UpdateError> {
        if self.schema != SCHEMA {
            return Err(UpdateError::manifest(format!(
                "schema {} is not supported (this build reads {SCHEMA})",
                self.schema
            )));
        }
        if self.product != PRODUCT {
            return Err(UpdateError::manifest(format!(
                "product `{}` is not `{PRODUCT}`",
                self.product
            )));
        }
        if self.os != OS || self.arch != ARCH {
            return Err(UpdateError::manifest(format!(
                "release targets {}/{} and this build installs {OS}/{ARCH}",
                self.os, self.arch
            )));
        }
        Channel::parse(&self.channel)?;
        let version = Version::parse(&self.version)?;
        let tag_version = Version::parse_tag(&self.tag)?;
        if version != tag_version {
            return Err(UpdateError::manifest(format!(
                "version `{}` does not match tag `{}`",
                self.version, self.tag
            )));
        }
        Version::parse(&self.min_launcher_version)?;
        if self.published.is_empty() {
            return Err(UpdateError::manifest("published must not be empty"));
        }
        if let Some(notes) = &self.notes
            && notes.len() > 64 * 1024
        {
            return Err(UpdateError::manifest("release notes exceed 64 KiB"));
        }
        if self.app.is_empty() {
            return Err(UpdateError::manifest(
                "the app group must list at least the engine",
            ));
        }
        let total = self.launcher.len() + self.app.len();
        if total > MAX_FILES {
            return Err(UpdateError::manifest(format!(
                "the manifest lists {total} files, more than the {MAX_FILES} it may list"
            )));
        }
        let mut paths = BTreeSet::new();
        for entry in self.launcher.iter().chain(&self.app) {
            entry.validate()?;
            if !paths.insert(entry.path.as_str()) {
                return Err(UpdateError::manifest(format!(
                    "`{}` is listed more than once",
                    entry.path
                )));
            }
        }
        if !self.app.iter().any(|entry| entry.path == "delicium.exe") {
            return Err(UpdateError::manifest(
                "the app group must list `delicium.exe`",
            ));
        }
        Ok(())
    }

    /// The parsed release version; [`Manifest::validate`] has already run.
    pub fn version(&self) -> Version {
        Version::parse(&self.version).expect("a validated manifest has a parsed version")
    }

    /// The parsed launcher requirement; [`Manifest::validate`] has already run.
    pub fn min_launcher_version(&self) -> Version {
        Version::parse(&self.min_launcher_version).expect("a validated manifest has a version")
    }

    /// The engine binary this release installs.
    pub fn engine_entry(&self) -> &FileEntry {
        self.app
            .iter()
            .find(|entry| entry.path == "delicium.exe")
            .expect("a validated manifest lists delicium.exe")
    }
}

impl FileEntry {
    /// Checks one file entry.
    pub fn validate(&self) -> Result<(), UpdateError> {
        if !valid_relative_path(&self.path) {
            return Err(UpdateError::manifest(format!(
                "`{}` is not a safe relative path",
                self.path
            )));
        }
        if !valid_asset_name(&self.asset) {
            return Err(UpdateError::manifest(format!(
                "`{}` is not a safe asset name",
                self.asset
            )));
        }
        if self.size == 0 {
            return Err(UpdateError::manifest(format!(
                "`{}` declares a zero size",
                self.path
            )));
        }
        if self.size > MAX_ASSET_BYTES {
            return Err(UpdateError::TooLarge {
                what: format!("asset `{}`", self.path),
                actual: self.size,
                limit: MAX_ASSET_BYTES,
            });
        }
        if parse_hex(&self.sha256).is_none() {
            return Err(UpdateError::manifest(format!(
                "`{}` does not carry {HEX_LEN} hexadecimal SHA-256 digits",
                self.path
            )));
        }
        if !self.sha256.bytes().all(|byte| !byte.is_ascii_uppercase()) {
            return Err(UpdateError::manifest(format!(
                "`{}` must spell its SHA-256 in lowercase",
                self.path
            )));
        }
        Ok(())
    }
}

/// Whether a manifest path stays inside the directory it installs into:
/// relative, `/`-separated, no empty or `.`/`..` segments, no drive or
/// stream colon, no backslash, no control characters.
pub fn valid_relative_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 200 {
        return false;
    }
    if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return false;
    }
    if path.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return false;
    }
    path.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    })
}

/// Asset names are file names on the release page: one segment, same
/// character set, no traversal.
fn valid_asset_name(asset: &str) -> bool {
    !asset.is_empty() && asset.len() <= 200 && !asset.contains('/') && valid_relative_path(asset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn entry(path: &str, asset: &str, size: u64) -> FileEntry {
        FileEntry {
            path: path.into(),
            asset: asset.into(),
            size,
            sha256: DIGEST.into(),
        }
    }

    fn sample() -> Manifest {
        Manifest {
            schema: SCHEMA,
            product: PRODUCT.into(),
            channel: "stable".into(),
            version: "0.1.1".into(),
            tag: "v0.1.1".into(),
            published: "2026-10-18T00:00:00Z".into(),
            notes: Some("Fixes.\n".into()),
            min_launcher_version: "0.1.0".into(),
            os: OS.into(),
            arch: ARCH.into(),
            launcher: vec![entry(
                "delicium-launcher.exe",
                "delicium-launcher.exe",
                3_000_000,
            )],
            app: vec![entry("delicium.exe", "delicium.exe", 24_464_896)],
        }
    }

    fn error(manifest: &Manifest) -> String {
        manifest.validate().unwrap_err().to_string()
    }

    #[test]
    fn a_release_manifest_round_trips() {
        let text = serde_json::to_string(&sample()).unwrap();
        assert_eq!(Manifest::parse(text.as_bytes()).unwrap(), sample());
        // The written form uses the field names the workflow emits.
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema"], 1);
        assert_eq!(value["min_launcher_version"], "0.1.0");
        assert_eq!(value["app"][0]["path"], "delicium.exe");
    }

    #[test]
    fn download_urls_come_from_the_tag() {
        let manifest = sample();
        assert_eq!(
            manifest.engine_entry().download_url(&manifest.tag),
            "https://github.com/Praefectura/Delicium/releases/download/v0.1.1/delicium.exe"
        );
        assert_eq!(manifest.version().to_string(), "0.1.1");
        assert_eq!(manifest.min_launcher_version().to_string(), "0.1.0");
    }

    #[test]
    fn unknown_and_missing_fields_are_rejected() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(Manifest::parse(value.to_string().as_bytes()).is_err());
        let mut value = serde_json::to_value(sample()).unwrap();
        value["app"][0].as_object_mut().unwrap().remove("sha256");
        assert!(Manifest::parse(value.to_string().as_bytes()).is_err());
        assert!(Manifest::parse(b"not json").is_err());
        assert!(Manifest::parse(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn header_fields_must_match_this_build() {
        type Mutation = fn(&mut Manifest);
        let cases: [(&str, Mutation); 6] = [
            ("schema", |m| m.schema = 2),
            ("product", |m| m.product = "other".into()),
            ("os", |m| m.os = "linux".into()),
            ("arch", |m| m.arch = "aarch64".into()),
            ("channel", |m| m.channel = "beta".into()),
            ("min launcher", |m| m.min_launcher_version = "x".into()),
        ];
        for (label, mutate) in cases {
            let mut manifest = sample();
            mutate(&mut manifest);
            assert!(manifest.validate().is_err(), "accepted {label}");
        }
        // Tag and version must describe the same release.
        let mut manifest = sample();
        manifest.version = "0.1.2".into();
        assert!(error(&manifest).contains("does not match tag"));
        let mut manifest = sample();
        manifest.tag = "v0.1.2".into();
        assert!(error(&manifest).contains("does not match tag"));
        // Informational fields still have to be well formed.
        let mut manifest = sample();
        manifest.published = String::new();
        assert!(manifest.validate().is_err());
        let mut manifest = sample();
        manifest.notes = Some("x".repeat(64 * 1024 + 1));
        assert!(manifest.validate().is_err());
        // A pre-release manifest is structurally valid; whether the channel
        // will install it is decided by `accepts_candidate`.
        let mut manifest = sample();
        manifest.version = "0.1.1-beta.1".into();
        manifest.tag = "v0.1.1-beta.1".into();
        manifest.validate().unwrap();
    }

    #[test]
    fn file_groups_are_checked() {
        let mut manifest = sample();
        manifest.app.clear();
        assert!(error(&manifest).contains("at least the engine"));
        let mut manifest = sample();
        manifest.app = vec![entry("engine.exe", "engine.exe", 10)];
        assert!(error(&manifest).contains("delicium.exe"));
        let mut manifest = sample();
        manifest.app.push(entry("delicium.exe", "other.exe", 10));
        assert!(error(&manifest).contains("more than once"));
        let mut manifest = sample();
        manifest.app = (0..MAX_FILES)
            .map(|index| entry(&format!("f{index}.dll"), &format!("f{index}.dll"), 10))
            .collect();
        assert!(error(&manifest).contains("lists 33 files"));
    }

    #[test]
    fn paths_cannot_escape_the_install_directory() {
        for path in [
            "",
            "/abs.exe",
            "../delicium.exe",
            "app/../../x.exe",
            "app/./x.exe",
            "C:/x.exe",
            "c:x.exe",
            r"app\x.exe",
            "app//x.exe",
            "app/x.exe:stream",
            "app/\u{1}x.exe",
            "app/x y.exe",
        ] {
            let mut manifest = sample();
            manifest.app[0].path = path.into();
            assert!(manifest.validate().is_err(), "accepted app path {path:?}");
        }
        // The asset name is one file name, never a path.
        for asset in ["", "a/b.exe", "../x.exe", r"a\x.exe", "x:y.exe"] {
            let mut manifest = sample();
            manifest.app[0].asset = asset.into();
            assert!(manifest.validate().is_err(), "accepted asset {asset:?}");
        }
        // Nested paths inside app/ and one level of dots are fine.
        assert!(valid_relative_path("app/0.1.1/delicium.exe"));
        assert!(valid_relative_path("delicium-launcher.exe"));
        assert!(valid_relative_path("a.b-c_d/e.f"));
    }

    #[test]
    fn sizes_and_digests_are_checked() {
        let mut manifest = sample();
        manifest.app[0].size = 0;
        assert!(error(&manifest).contains("zero size"));
        let mut manifest = sample();
        manifest.app[0].size = MAX_ASSET_BYTES + 1;
        assert!(error(&manifest).contains("exceeds"));
        for sha in [
            String::new(),
            DIGEST[..HEX_LEN - 1].to_string(),
            format!("{DIGEST}0"),
            "z".repeat(HEX_LEN),
            DIGEST.to_uppercase(),
        ] {
            let mut manifest = sample();
            manifest.app[0].sha256 = sha.clone();
            assert!(manifest.validate().is_err(), "accepted sha256 {sha:?}");
        }
        // A manifest above the byte limit is refused before parsing.
        let mut big = sample();
        big.notes = Some("x".repeat(MAX_MANIFEST_BYTES as usize));
        let text = serde_json::to_vec(&big).unwrap();
        assert!(matches!(
            Manifest::parse(&text),
            Err(UpdateError::TooLarge { .. })
        ));
    }
}
