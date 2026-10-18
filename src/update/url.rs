//! The only URLs the updater will talk to.
//!
//! Downloads are derived from a fixed repository, so this module is narrow on
//! purpose: `https`, a host, an optional port that must be 443, and a path.
//! No user information, no query-only URLs, no other scheme, and no
//! environment-variable override outside debug builds. A URL that came out of
//! a redirect is checked the same way before its body is used.

use super::error::UpdateError;

/// The fixed address of the newest release manifest.
pub const MANIFEST_URL: &str =
    "https://github.com/Praefectura/Delicium/releases/latest/download/latest.json";

/// Environment variable that overrides the manifest URL in debug builds only,
/// so a developer can point at a local staging release without publishing.
pub const MANIFEST_URL_ENV: &str = "DELICIUM_UPDATE_MANIFEST_URL";

/// A parsed HTTPS URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpsUrl {
    /// Host name without brackets; internationalised names are refused rather
    /// than punycoded, because the updater only ever talks to github.com.
    pub host: String,
    pub port: u16,
    /// Path plus query, beginning with `/`. WinHTTP expects this form.
    pub target: String,
}

impl HttpsUrl {
    /// Parses `https://host[:port]/path`.
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        let rest = text
            .strip_prefix("https://")
            .ok_or_else(|| UpdateError::network(format!("`{text}` is not an https URL")))?;
        if rest.is_empty() {
            return Err(UpdateError::network("the URL has no host".to_owned()));
        }
        if rest.contains('@') {
            return Err(UpdateError::network(
                "URLs with user information are refused".to_owned(),
            ));
        }
        let (authority, target) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(UpdateError::network("the URL has no host".to_owned()));
        }
        if target.contains('#') {
            return Err(UpdateError::network(
                "fragments are not part of a request".to_owned(),
            ));
        }
        if target.bytes().any(|byte| byte < 0x21 || byte == 0x7f) {
            return Err(UpdateError::network(
                "the URL path contains whitespace or control characters".to_owned(),
            ));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => {
                let port: u16 = port
                    .parse()
                    .map_err(|_| UpdateError::network(format!("`{port}` is not a port")))?;
                (host, port)
            }
            None => (authority, 443),
        };
        if host.is_empty() {
            return Err(UpdateError::network("the URL has no host".to_owned()));
        }
        if port != 443 {
            return Err(UpdateError::network(format!(
                "port {port} is refused; only 443 is used"
            )));
        }
        if !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        {
            return Err(UpdateError::network(format!(
                "host `{host}` contains characters this updater does not send"
            )));
        }
        Ok(Self {
            host: host.to_owned(),
            port,
            target: target.to_owned(),
        })
    }
}

/// The manifest URL for this build: the fixed address, or the debug-only
/// override when it points somewhere acceptable.
pub fn manifest_url() -> Result<String, UpdateError> {
    if cfg!(debug_assertions)
        && let Some(override_url) = std::env::var_os(MANIFEST_URL_ENV)
        && let Some(override_url) = override_url.to_str()
    {
        HttpsUrl::parse(override_url)?;
        log::warn!("Using {MANIFEST_URL_ENV} for the update manifest");
        return Ok(override_url.to_owned());
    }
    Ok(MANIFEST_URL.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_release_addresses() {
        let url = HttpsUrl::parse(MANIFEST_URL).unwrap();
        assert_eq!(url.host, "github.com");
        assert_eq!(url.port, 443);
        assert_eq!(
            url.target,
            "/Praefectura/Delicium/releases/latest/download/latest.json"
        );
        let url = HttpsUrl::parse(
            "https://github.com/Praefectura/Delicium/releases/download/v0.1.1/delicium.exe",
        )
        .unwrap();
        assert_eq!(
            url.target,
            "/Praefectura/Delicium/releases/download/v0.1.1/delicium.exe"
        );
        // A bare host is a request for `/`.
        assert_eq!(HttpsUrl::parse("https://github.com").unwrap().target, "/");
        // The explicit default port is accepted.
        assert_eq!(
            HttpsUrl::parse("https://github.com:443/x").unwrap().port,
            443
        );
    }

    #[test]
    fn refuses_everything_else() {
        for text in [
            "",
            "http://github.com/x",
            "ftp://github.com/x",
            "github.com/x",
            "https://",
            "https:///path",
            "https://user:pass@github.com/x",
            "https://github.com:8443/x",
            "https://github.com:notaport/x",
            "https://github.com/x#fragment",
            "https://github.com/a b",
            "https://github.com/a\tb",
            "https://github.com\x01/",
            "https://ex\u{e4}mple.com/x",
            "https://[::1]/x",
        ] {
            assert!(HttpsUrl::parse(text).is_err(), "accepted {text:?}");
        }
    }
}
