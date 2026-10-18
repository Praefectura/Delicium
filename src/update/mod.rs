//! Self-update for the launcher and the engine.
//!
//! The release workflow publishes a small manifest plus one asset per file at
//! a fixed GitHub Releases URL. This module understands that manifest,
//! downloads and verifies its payloads, and installs them into the versioned
//! application directory. It has no opinion about *when* to update: the
//! launcher decides that and calls in here.
//!
//! Layering, from the bottom up:
//!
//! - [`sha256`] is a self-contained FIPS 180-4 implementation, so payload
//!   verification does not depend on a platform crypto API or a new crate.
//! - [`version`] parses and orders `MAJOR.MINOR.PATCH[-pre]` and decides
//!   whether a candidate may replace the running build.
//! - [`manifest`] is the untrusted release document: strict parsing, path
//!   validation, size and digest checks.
//! - [`state`] is the on-disk record of which versions are installed.
//! - [`fetch`] is the network boundary and [`install`] the filesystem
//!   transaction on top of it.
//!
//! `install` is generic over the fetcher so the whole transaction can be
//! exercised against local fixtures with no network and no real release.

pub mod error;
pub mod fetch;
pub mod install;
pub mod manifest;
pub mod sha256;
pub mod state;
pub mod url;
pub mod version;
pub mod winfile;
pub mod winhttp;

pub use error::UpdateError;
pub use fetch::{Fetcher, MemFetcher, Response};
pub use install::{InstallMarker, Installer, StagedFile};
pub use manifest::{FileEntry, Manifest};
pub use state::{InstallState, InstalledVersion, OpenedVersion, Watermark};
pub use url::HttpsUrl;
pub use version::{Channel, Version};
pub use winhttp::WinHttpFetcher;

/// The version this build reports, parsed once per call.
pub fn current_version() -> Result<Version, UpdateError> {
    Version::current()
}
