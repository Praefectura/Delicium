//! The network boundary.
//!
//! Everything the updater downloads goes through the [`Fetcher`] trait. The
//! real implementation speaks WinHTTP; the tests use [`MemFetcher`], which
//! serves bytes from memory and can fail at a chosen offset, so the whole
//! installation transaction runs with no network at all.
//!
//! Two shapes are needed and they are deliberately separate:
//!
//! - [`Fetcher::fetch`] reads a small document (the manifest) into memory.
//! - [`Fetcher::download`] streams a payload into a writer, so a 24 MB engine
//!   binary is never held in memory and is hashed as it arrives.
//!
//! Both enforce a byte limit: a server that lies about its size, or one that
//! sends no size at all, cannot make the updater read or write unbounded data.

use std::collections::BTreeMap;
use std::io::Write;

use super::error::UpdateError;

/// A small document read fully into memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// HTTP status; anything but 200 is a failure the caller reports.
    pub status: u32,
    pub body: Vec<u8>,
    /// The URL the request ended at, after redirects. Used to confirm that a
    /// redirect did not leave HTTPS.
    pub final_url: Option<String>,
}

impl Response {
    /// A successful response.
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            final_url: None,
        }
    }

    /// The body of a 200 response, or the matching error.
    pub fn into_body(self) -> Result<Vec<u8>, UpdateError> {
        if self.status == 200 {
            Ok(self.body)
        } else {
            Err(UpdateError::Http(self.status))
        }
    }
}

/// Where the updater gets bytes from.
pub trait Fetcher {
    /// Reads at most `limit` bytes of `url`.
    fn fetch(&self, url: &str, limit: u64) -> Result<Response, UpdateError>;

    /// Streams `url` into `sink`, refusing to write more than `limit` bytes.
    ///
    /// Returns the number of bytes written. The caller has a declared size
    /// from the manifest and compares it against this number: a short or long
    /// transfer is a failure even when the HTTP status was 200.
    fn download(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, UpdateError>;
}

/// An in-memory [`Fetcher`] for tests.
///
/// Insert responses by URL, optionally making one of them fail after a given
/// number of bytes to exercise truncation handling.
#[derive(Clone, Debug, Default)]
pub struct MemFetcher {
    entries: BTreeMap<String, MemEntry>,
    log: std::cell::RefCell<Vec<String>>,
}

#[derive(Clone, Debug)]
struct MemEntry {
    status: u32,
    body: Vec<u8>,
    /// Stop the transfer after this many bytes instead of completing it.
    truncate_after: Option<usize>,
    /// Replace the body with a body of `content_length` declared bytes that
    /// never arrive; used to model a chunked or lying server.
    declared_length: Option<u64>,
}

impl MemFetcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `body` at `url` with status 200.
    pub fn insert(&mut self, url: impl Into<String>, body: impl Into<Vec<u8>>) -> &mut Self {
        self.entries.insert(
            url.into(),
            MemEntry {
                status: 200,
                body: body.into(),
                truncate_after: None,
                declared_length: None,
            },
        );
        self
    }

    /// Serves a status with an empty body at `url`.
    pub fn insert_status(&mut self, url: impl Into<String>, status: u32) -> &mut Self {
        self.entries.insert(
            url.into(),
            MemEntry {
                status,
                body: Vec::new(),
                truncate_after: None,
                declared_length: None,
            },
        );
        self
    }

    /// Makes the transfer of `url` stop after `bytes`.
    pub fn truncate(&mut self, url: impl Into<String>, bytes: usize) -> &mut Self {
        if let Some(entry) = self.entries.get_mut(&url.into()) {
            entry.truncate_after = Some(bytes);
        }
        self
    }

    /// The URLs requested so far, in order.
    pub fn requested(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    fn entry(&self, url: &str) -> Result<MemEntry, UpdateError> {
        self.log.borrow_mut().push(url.to_owned());
        let entry =
            self.entries.get(url).cloned().ok_or_else(|| {
                UpdateError::network(format!("no in-memory response for `{url}`"))
            })?;
        if entry.status != 200 {
            return Err(UpdateError::Http(entry.status));
        }
        Ok(entry)
    }
}

impl Fetcher for MemFetcher {
    fn fetch(&self, url: &str, limit: u64) -> Result<Response, UpdateError> {
        let entry = self.entry(url)?;
        if entry.body.len() as u64 > limit {
            return Err(UpdateError::TooLarge {
                what: format!("response from `{url}`"),
                actual: entry.body.len() as u64,
                limit,
            });
        }
        Ok(Response {
            status: entry.status,
            body: entry.body,
            final_url: Some(url.to_owned()),
        })
    }

    fn download(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, UpdateError> {
        let entry = self.entry(url)?;
        if entry.body.len() as u64 > limit {
            return Err(UpdateError::TooLarge {
                what: format!("response from `{url}`"),
                actual: entry.body.len() as u64,
                limit,
            });
        }
        if let Some(declared) = entry.declared_length
            && declared > limit
        {
            return Err(UpdateError::TooLarge {
                what: format!("response from `{url}`"),
                actual: declared,
                limit,
            });
        }
        let usable = entry
            .truncate_after
            .map_or(entry.body.len(), |after| after.min(entry.body.len()));
        sink.write_all(&entry.body[..usable])
            .map_err(|error| UpdateError::network(format!("cannot write the payload: {error}")))?;
        Ok(usable as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_enforces_the_limit_and_reports_status() {
        let mut fetcher = MemFetcher::new();
        fetcher.insert("https://example.invalid/a", b"hello".to_vec());
        fetcher.insert_status("https://example.invalid/missing", 404);
        assert_eq!(
            fetcher.fetch("https://example.invalid/a", 5).unwrap().body,
            b"hello"
        );
        assert!(matches!(
            fetcher.fetch("https://example.invalid/a", 4),
            Err(UpdateError::TooLarge { .. })
        ));
        assert!(matches!(
            fetcher.fetch("https://example.invalid/missing", 1024),
            Err(UpdateError::Http(404))
        ));
        assert!(matches!(
            fetcher.fetch("https://example.invalid/absent", 1024),
            Err(UpdateError::Network(_))
        ));
    }

    #[test]
    fn download_streams_every_byte_and_can_truncate() {
        let mut fetcher = MemFetcher::new();
        fetcher.insert("https://example.invalid/a", vec![7u8; 1000]);
        let mut sink = Vec::new();
        assert_eq!(
            fetcher
                .download("https://example.invalid/a", 1000, &mut sink)
                .unwrap(),
            1000
        );
        assert_eq!(sink, vec![7u8; 1000]);
        fetcher.truncate("https://example.invalid/a", 10);
        let mut short = Vec::new();
        assert_eq!(
            fetcher
                .download("https://example.invalid/a", 1000, &mut short)
                .unwrap(),
            10
        );
        assert_eq!(short.len(), 10);
    }
}
