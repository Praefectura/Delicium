//! Semantic version numbers as the updater uses them.
//!
//! Only the subset the release channel needs is supported: a strict
//! `MAJOR.MINOR.PATCH` core with an optional dot-separated pre-release. Build
//! metadata is accepted and ignored, because it does not participate in
//! precedence. The accepted character set is deliberately narrow: a version
//! ends up in a directory name and a URL, so anything outside
//! `[0-9A-Za-z.-]` is refused rather than escaped.
//!
//! The comparison below is SemVer 2.0.0 section 11 prefix precedence:
//! numeric identifiers compare numerically, alphanumeric identifiers compare
//! bytewise, a numeric identifier is lower than an alphanumeric one, and a
//! version with a pre-release is lower than the same core without one.

use std::cmp::Ordering;
use std::fmt;

use super::error::UpdateError;

/// One dot-separated pre-release identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Identifier {
    Numeric(u64),
    Text(String),
}

/// A parsed semantic version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    prerelease: Vec<Identifier>,
}

impl Version {
    /// Parses `MAJOR.MINOR.PATCH` with an optional `-pre.release` suffix and
    /// optional `+build` metadata.
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        // Build metadata never participates in precedence, so it is removed
        // before anything else looks at the rest of the string.
        let text = match text.split_once('+') {
            Some((without_build, build)) => {
                if !valid_identifiers(build) {
                    return Err(UpdateError::Version(format!(
                        "build metadata `{build}` is malformed"
                    )));
                }
                without_build
            }
            None => text,
        };
        let (core_text, prerelease_text) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut core = [0u64; 3];
        let mut parts = core_text.split('.');
        for slot in core.iter_mut() {
            let part = parts.next().ok_or_else(|| {
                UpdateError::Version(format!("`{text}` has fewer than three parts"))
            })?;
            *slot = numeric_part(part, text)?;
        }
        if parts.next().is_some() {
            return Err(UpdateError::Version(format!(
                "`{text}` has more than three numeric parts"
            )));
        }
        let prerelease = match prerelease_text {
            None => Vec::new(),
            Some(text) => parse_prerelease(text)?,
        };
        Ok(Self { core, prerelease })
    }

    /// Parses a release tag: `v` followed by a version.
    pub fn parse_tag(tag: &str) -> Result<Self, UpdateError> {
        let text = tag
            .strip_prefix('v')
            .ok_or_else(|| UpdateError::Version(format!("tag `{tag}` must start with `v`")))?;
        Self::parse(text)
    }

    /// The version the running build reports.
    pub fn current() -> Result<Self, UpdateError> {
        Self::parse(env!("CARGO_PKG_VERSION"))
    }

    /// The release tag that carries this version.
    pub fn tag(&self) -> String {
        format!("v{self}")
    }

    /// Whether this is a pre-release (`0.2.0-beta.1`).
    pub fn is_prerelease(&self) -> bool {
        !self.prerelease.is_empty()
    }

    /// The `MAJOR.MINOR.PATCH` part without any pre-release.
    pub fn core_text(&self) -> String {
        format!("{}.{}.{}", self.core[0], self.core[1], self.core[2])
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{}.{}",
            self.core[0], self.core[1], self.core[2]
        )?;
        for (index, identifier) in self.prerelease.iter().enumerate() {
            formatter.write_str(if index == 0 { "-" } else { "." })?;
            match identifier {
                Identifier::Numeric(value) => write!(formatter, "{value}")?,
                Identifier::Text(text) => formatter.write_str(text)?,
            }
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.core.cmp(&other.core) {
            Ordering::Equal => {}
            difference => return difference,
        }
        match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
            // A release outranks any pre-release of the same core.
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => compare_prerelease(&self.prerelease, &other.prerelease),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_prerelease(left: &[Identifier], right: &[Identifier]) -> Ordering {
    for (a, b) in left.iter().zip(right) {
        let ordering = match (a, b) {
            (Identifier::Numeric(a), Identifier::Numeric(b)) => a.cmp(b),
            (Identifier::Numeric(_), Identifier::Text(_)) => Ordering::Less,
            (Identifier::Text(_), Identifier::Numeric(_)) => Ordering::Greater,
            (Identifier::Text(a), Identifier::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn numeric_part(part: &str, whole: &str) -> Result<u64, UpdateError> {
    if part.is_empty() {
        return Err(UpdateError::Version(format!(
            "`{whole}` has an empty numeric part"
        )));
    }
    if !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(UpdateError::Version(format!(
            "`{whole}` has a non-numeric part `{part}`"
        )));
    }
    if part.len() > 1 && part.starts_with('0') {
        return Err(UpdateError::Version(format!(
            "`{whole}` has a leading zero in `{part}`"
        )));
    }
    part.parse()
        .map_err(|_| UpdateError::Version(format!("`{whole}` overflows in `{part}`")))
}

fn parse_prerelease(text: &str) -> Result<Vec<Identifier>, UpdateError> {
    if text.is_empty() {
        return Err(UpdateError::Version(
            "a pre-release must not be empty".into(),
        ));
    }
    let mut identifiers = Vec::new();
    for part in text.split('.') {
        if part.is_empty() {
            return Err(UpdateError::Version(
                "a pre-release identifier must not be empty".into(),
            ));
        }
        if !part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(UpdateError::Version(format!(
                "pre-release identifier `{part}` has a character outside [0-9A-Za-z-]"
            )));
        }
        if part.bytes().all(|byte| byte.is_ascii_digit()) {
            if part.len() > 1 && part.starts_with('0') {
                return Err(UpdateError::Version(format!(
                    "pre-release identifier `{part}` has a leading zero"
                )));
            }
            let value = part.parse().map_err(|_| {
                UpdateError::Version(format!("pre-release identifier `{part}` overflows"))
            })?;
            identifiers.push(Identifier::Numeric(value));
        } else {
            identifiers.push(Identifier::Text(part.to_owned()));
        }
    }
    Ok(identifiers)
}

/// Whether a dot-separated identifier list (build metadata) is well formed:
/// non-empty parts from `[0-9A-Za-z-]` only.
fn valid_identifiers(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

/// The release channel a manifest may name. Only the stable channel is built;
/// listing the others makes the refusal explicit rather than silently
/// accepting a channel this build does not implement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
}

impl Channel {
    /// The manifest's `channel` value for this channel.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
        }
    }

    /// Parses a manifest `channel` value.
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        match text {
            "stable" => Ok(Self::Stable),
            other => Err(UpdateError::Refused(format!(
                "release channel `{other}` is not supported"
            ))),
        }
    }

    /// Whether a version may be installed on this channel.
    pub fn accepts(self, version: &Version) -> bool {
        match self {
            Self::Stable => !version.is_prerelease(),
        }
    }
}

/// Decides whether `candidate` may replace `current`.
///
/// A candidate must be strictly newer, must belong to the channel, and must
/// not be older than the newest version this installation has already run
/// (`watermark`), which is what keeps a stale release from being installed
/// after a rollback.
pub fn accepts_candidate(
    current: &Version,
    watermark: Option<&Version>,
    channel: Channel,
    candidate: &Version,
) -> Result<(), UpdateError> {
    if !channel.accepts(candidate) {
        return Err(UpdateError::Refused(format!(
            "{candidate} is a pre-release and this build follows the stable channel"
        )));
    }
    if candidate <= current {
        return Err(UpdateError::Refused(format!(
            "{candidate} is not newer than the running {current}"
        )));
    }
    if let Some(watermark) = watermark
        && candidate < watermark
    {
        return Err(UpdateError::Refused(format!(
            "{candidate} is older than the newest installed {watermark}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn parses_and_prints_canonically() {
        assert_eq!(version("0.1.0").to_string(), "0.1.0");
        assert_eq!(version("12.34.56").to_string(), "12.34.56");
        assert_eq!(version("0.2.0-beta.1").to_string(), "0.2.0-beta.1");
        assert_eq!(version("0.2.0-rc.1").to_string(), "0.2.0-rc.1");
        assert_eq!(version("0.1.0").tag(), "v0.1.0");
        assert_eq!(version("0.1.0").core_text(), "0.1.0");
        assert_eq!(version("0.1.0-beta.3").core_text(), "0.1.0");
        assert!(!version("0.1.0").is_prerelease());
        assert!(version("0.1.0-beta.3").is_prerelease());
        // Build metadata is accepted and ignored: it has no precedence, so it
        // cannot be part of the canonical form either.
        assert_eq!(version("0.1.0+build.5").to_string(), "0.1.0");
        assert_eq!(Version::parse_tag("v0.1.0").unwrap(), version("0.1.0"));
    }

    #[test]
    fn rejects_malformed_versions() {
        for text in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1..3",
            ".1.2",
            "1.2.",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.x",
            "1.2.3-",
            "1.2.3-a..b",
            "1.2.3-beta_1",
            "1.2.3-beta.01",
            "99999999999999999999.0.0",
            "1.2.3 beta",
            "../1.2.3",
            "1.2.3/../../x",
        ] {
            assert!(Version::parse(text).is_err(), "accepted {text:?}");
        }
        assert!(Version::parse_tag("0.1.0").is_err());
        assert!(Version::parse_tag("V0.1.0").is_err());
    }

    #[test]
    fn semver_precedence() {
        let ordered = [
            "0.1.0-alpha",
            "0.1.0-alpha.1",
            "0.1.0-alpha.beta",
            "0.1.0-beta",
            "0.1.0-beta.2",
            "0.1.0-beta.11",
            "0.1.0-rc.1",
            "0.1.0",
            "0.1.1",
            "0.2.0",
            "1.0.0",
        ];
        for pair in ordered.windows(2) {
            let lower = version(pair[0]);
            let higher = version(pair[1]);
            assert!(lower < higher, "{} < {}", pair[0], pair[1]);
            assert!(higher > lower, "{} > {}", pair[1], pair[0]);
        }
        // Numeric identifiers compare numerically, not lexically.
        assert!(version("1.0.0-2") < version("1.0.0-11"));
        // Numeric identifiers rank below alphanumeric ones.
        assert!(version("1.0.0-9") < version("1.0.0-a"));
        // The current package version parses.
        Version::current().unwrap();
    }

    #[test]
    fn candidate_rules() {
        let stable = Channel::Stable;
        let current = version("0.1.0");
        // Newer stable is accepted.
        accepts_candidate(&current, None, stable, &version("0.1.1")).unwrap();
        accepts_candidate(&current, None, stable, &version("1.0.0")).unwrap();
        // Same, older and pre-release are refused.
        for candidate in ["0.1.0", "0.0.9", "0.1.1-beta.1"] {
            let candidate = version(candidate);
            assert!(
                accepts_candidate(&current, None, stable, &candidate).is_err(),
                "accepted {candidate}"
            );
        }
        // The watermark blocks a version the installation has already passed.
        let watermark = version("0.2.0");
        assert!(accepts_candidate(&current, Some(&watermark), stable, &version("0.1.5")).is_err());
        accepts_candidate(&current, Some(&watermark), stable, &version("0.2.0")).unwrap();
        accepts_candidate(&current, Some(&watermark), stable, &version("0.3.0")).unwrap();
        // Channel parsing names the supported value and refuses the rest.
        assert_eq!(Channel::parse("stable").unwrap(), Channel::Stable);
        assert!(Channel::parse("beta").is_err());
        assert!(Channel::parse("Stable").is_err());
        assert!(Channel::parse("").is_err());
        assert_eq!(Channel::Stable.as_str(), "stable");
    }
}
