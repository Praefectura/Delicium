//! WinHTTP as a [`Fetcher`], through hand-written FFI.
//!
//! The crate already talks to Win32 by declaring the few functions it needs
//! (`src/platform/`), and the updater follows the same pattern: no HTTP crate,
//! no TLS crate, no `Cargo.lock` change. WinHTTP supplies TLS verification,
//! the system proxy configuration and redirects.
//!
//! Security properties, all enforced here rather than by the caller:
//!
//! - every request is opened with `WINHTTP_FLAG_SECURE`, so the connection is
//!   TLS from the first byte;
//! - the redirect policy refuses an HTTPS to HTTP downgrade, caps the chain,
//!   and the final URL is queried and re-validated against [`HttpsUrl`];
//! - a certificate failure is reported as an error, never retried in the
//!   clear;
//! - every read is bounded by the caller's limit and by a total time budget.

use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use super::error::UpdateError;
use super::fetch::{Fetcher, Response};
use super::url::HttpsUrl;

#[link(name = "winhttp")]
unsafe extern "system" {
    fn WinHttpOpen(
        agent: *const u16,
        access_type: u32,
        proxy: *const u16,
        proxy_bypass: *const u16,
        flags: u32,
    ) -> *mut core::ffi::c_void;
    fn WinHttpConnect(
        session: *mut core::ffi::c_void,
        server: *const u16,
        port: u16,
        reserved: u32,
    ) -> *mut core::ffi::c_void;
    fn WinHttpOpenRequest(
        connect: *mut core::ffi::c_void,
        verb: *const u16,
        object: *const u16,
        version: *const u16,
        referrer: *const u16,
        accept_types: *const *const u16,
        flags: u32,
    ) -> *mut core::ffi::c_void;
    fn WinHttpSetTimeouts(
        handle: *mut core::ffi::c_void,
        resolve: i32,
        connect: i32,
        send: i32,
        receive: i32,
    ) -> i32;
    fn WinHttpSetOption(
        handle: *mut core::ffi::c_void,
        option: u32,
        buffer: *const core::ffi::c_void,
        length: u32,
    ) -> i32;
    fn WinHttpSendRequest(
        request: *mut core::ffi::c_void,
        headers: *const u16,
        headers_length: u32,
        optional: *const core::ffi::c_void,
        optional_length: u32,
        total_length: u32,
        context: usize,
    ) -> i32;
    fn WinHttpReceiveResponse(
        request: *mut core::ffi::c_void,
        reserved: *const core::ffi::c_void,
    ) -> i32;
    fn WinHttpQueryHeaders(
        request: *mut core::ffi::c_void,
        info_level: u32,
        name: *const u16,
        buffer: *mut core::ffi::c_void,
        length: *mut u32,
        index: *mut u32,
    ) -> i32;
    fn WinHttpQueryDataAvailable(request: *mut core::ffi::c_void, available: *mut u32) -> i32;
    fn WinHttpReadData(
        request: *mut core::ffi::c_void,
        buffer: *mut core::ffi::c_void,
        to_read: u32,
        read: *mut u32,
    ) -> i32;
    fn WinHttpQueryOption(
        handle: *mut core::ffi::c_void,
        option: u32,
        buffer: *mut core::ffi::c_void,
        length: *mut u32,
    ) -> i32;
    fn WinHttpCloseHandle(handle: *mut core::ffi::c_void) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}

/// Ask WinHTTP to use the machine's proxy configuration (Windows 8.1+).
const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
/// The request must be TLS.
const FLAG_SECURE: u32 = 0x0080_0000;
/// Redirect behaviour: never downgrade HTTPS to HTTP.
const OPTION_REDIRECT_POLICY: u32 = 88;
const REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP: u32 = 1;
/// Longest redirect chain this updater follows.
const OPTION_MAX_REDIRECTS: u32 = 89;
const MAX_REDIRECTS: u32 = 5;
/// The URL the request ended at.
const OPTION_URL: u32 = 34;
const QUERY_STATUS_CODE: u32 = 19;
const QUERY_CONTENT_LENGTH: u32 = 5;
const QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
/// `WinHttpQueryHeaders` reports a missing header with this error.
const ERROR_WINHTTP_HEADER_NOT_FOUND: u32 = 12150;

/// Total time one request may take, across redirects and reads.
const TOTAL_BUDGET: Duration = Duration::from_secs(60);
/// Bytes read from the socket at a time.
const READ_CHUNK: u32 = 64 * 1024;

/// A live WinHTTP session, closed on drop.
struct Session(*mut core::ffi::c_void);

impl Session {
    fn open() -> Result<Self, UpdateError> {
        let agent = wide("Delicium updater");
        // SAFETY: the agent string is NUL-terminated and only read; a null
        // proxy with automatic detection is the documented default.
        let handle = unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                ACCESS_TYPE_AUTOMATIC_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            )
        };
        if handle.is_null() {
            return Err(last_error("WinHttpOpen"));
        }
        let session = Self(handle);
        // SAFETY: `handle` is a session this call just opened and still owns.
        let ok = unsafe { WinHttpSetTimeouts(handle, 5_000, 5_000, 10_000, 15_000) };
        if ok == 0 {
            return Err(last_error("WinHttpSetTimeouts"));
        }
        Ok(session)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle belongs to this session and is closed once.
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// A WinHTTP connection or request, closed on drop.
struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle is owned by this value and closed once.
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// The real [`Fetcher`].
pub struct WinHttpFetcher {
    /// The final URL of the last request, useful for diagnostics.
    pub last_url: std::cell::RefCell<Option<String>>,
}

impl Default for WinHttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl WinHttpFetcher {
    pub fn new() -> Self {
        Self {
            last_url: std::cell::RefCell::new(None),
        }
    }

    /// The final URL of the last request, when redirects were followed.
    pub fn last_url(&self) -> Option<String> {
        self.last_url.borrow().clone()
    }
}

impl Fetcher for WinHttpFetcher {
    fn fetch(&self, url: &str, limit: u64) -> Result<Response, UpdateError> {
        let mut body = Vec::new();
        let outcome = request(url, limit, |chunk| {
            body.extend_from_slice(chunk);
            Ok(())
        })?;
        *self.last_url.borrow_mut() = outcome.final_url.clone();
        Ok(Response {
            status: outcome.status,
            body,
            final_url: outcome.final_url,
        })
    }

    fn download(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, UpdateError> {
        let mut written = 0u64;
        let outcome = request(url, limit, |chunk| {
            sink.write_all(chunk).map_err(|error| {
                UpdateError::network(format!("cannot write the payload: {error}"))
            })?;
            written += chunk.len() as u64;
            Ok(())
        })?;
        *self.last_url.borrow_mut() = outcome.final_url;
        Ok(written)
    }
}

/// What one request produced.
struct Outcome {
    status: u32,
    final_url: Option<String>,
}

/// Performs one GET and hands each chunk to `sink`, returning the status.
fn request(
    url: &str,
    limit: u64,
    mut sink: impl FnMut(&[u8]) -> Result<(), UpdateError>,
) -> Result<Outcome, UpdateError> {
    let parsed = HttpsUrl::parse(url)?;
    let deadline = Instant::now() + TOTAL_BUDGET;
    let session = Session::open()?;
    let host = wide(&parsed.host);
    // SAFETY: `session` is live; the host string is NUL-terminated.
    let connect = unsafe { WinHttpConnect(session.0, host.as_ptr(), 443, 0) };
    if connect.is_null() {
        return Err(last_error("WinHttpConnect"));
    }
    let connect = Handle(connect);
    let verb = wide("GET");
    let target = wide(&parsed.target);
    // SAFETY: the connection is live; both strings are NUL-terminated; no
    // accept types are needed for a file download.
    let request = unsafe {
        WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            FLAG_SECURE,
        )
    };
    if request.is_null() {
        return Err(last_error("WinHttpOpenRequest"));
    }
    let request = Handle(request);
    // SAFETY: `request` is live; the policy word and the redirect cap are
    // passed by pointer as WinHTTP expects.
    unsafe {
        let policy = REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP;
        if WinHttpSetOption(
            request.0,
            OPTION_REDIRECT_POLICY,
            (&policy as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        ) == 0
        {
            return Err(last_error("WinHttpSetOption(REDIRECT_POLICY)"));
        }
        let redirects = MAX_REDIRECTS;
        if WinHttpSetOption(
            request.0,
            OPTION_MAX_REDIRECTS,
            (&redirects as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        ) == 0
        {
            return Err(last_error("WinHttpSetOption(MAX_REDIRECTS)"));
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let header_text = wide(&format!(
        "User-Agent: Delicium-updater/{}\r\nAccept: application/json, application/octet-stream\r\nX-Delicium-Nonce: {nonce}\r\n",
        env!("CARGO_PKG_VERSION")
    ));
    // SAFETY: `request` is live. The header block is NUL-terminated and its
    // explicit length excludes the terminator, as WinHTTP documents.
    let sent = unsafe {
        WinHttpSendRequest(
            request.0,
            header_text.as_ptr(),
            (header_text.len() - 1) as u32,
            std::ptr::null(),
            0,
            0,
            0,
        )
    };
    if sent == 0 {
        return Err(last_error("WinHttpSendRequest"));
    }
    // SAFETY: `request` is live and the previous send completed.
    if unsafe { WinHttpReceiveResponse(request.0, std::ptr::null()) } == 0 {
        return Err(last_error("WinHttpReceiveResponse"));
    }
    let status =
        query_u32(request.0, QUERY_STATUS_CODE | QUERY_FLAG_NUMBER).map_err(
            |error| match error {
                QueryError::Missing => {
                    UpdateError::network("the response carries no status code".to_owned())
                }
                QueryError::Failed(error) => error,
            },
        )?;
    if status != 200 {
        return Err(UpdateError::Http(status));
    }
    // Confirm the request really ended at an HTTPS URL, whatever the
    // redirect policy allowed.
    if let Some(final_url) = query_string(request.0, OPTION_URL) {
        HttpsUrl::parse(&final_url).map_err(|error| {
            UpdateError::network(format!("the request ended at an unusable URL: {error}"))
        })?;
    }
    // A declared length above the limit is refused before reading a byte.
    match query_u32(request.0, QUERY_CONTENT_LENGTH | QUERY_FLAG_NUMBER) {
        Ok(declared) if u64::from(declared) > limit => {
            return Err(UpdateError::TooLarge {
                what: format!("response from `{url}`"),
                actual: u64::from(declared),
                limit,
            });
        }
        // No length is normal for a chunked response; the running total below
        // still bounds it.
        Ok(_) | Err(QueryError::Missing) => {}
        Err(QueryError::Failed(error)) => return Err(error),
    }
    let final_url = query_string(request.0, OPTION_URL);
    let mut total: u64 = 0;
    let mut buffer = vec![0u8; READ_CHUNK as usize];
    loop {
        if Instant::now() > deadline {
            return Err(UpdateError::network(format!(
                "the transfer from `{url}` exceeded its {} second budget",
                TOTAL_BUDGET.as_secs()
            )));
        }
        let mut available = 0u32;
        // SAFETY: `request` is live; `available` is a valid out-pointer.
        if unsafe { WinHttpQueryDataAvailable(request.0, &mut available) } == 0 {
            return Err(last_error("WinHttpQueryDataAvailable"));
        }
        if available == 0 {
            break;
        }
        let to_read = available.min(READ_CHUNK);
        let mut read = 0u32;
        // SAFETY: the buffer is at least `to_read` bytes; `read` is a valid
        // out-pointer.
        if unsafe { WinHttpReadData(request.0, buffer.as_mut_ptr().cast(), to_read, &mut read) }
            == 0
        {
            return Err(last_error("WinHttpReadData"));
        }
        if read == 0 {
            break;
        }
        total += u64::from(read);
        if total > limit {
            return Err(UpdateError::TooLarge {
                what: format!("response from `{url}`"),
                actual: total,
                limit,
            });
        }
        sink(&buffer[..read as usize])?;
    }
    Ok(Outcome { status, final_url })
}

enum QueryError {
    Missing,
    Failed(UpdateError),
}

/// Reads a numeric header or option.
fn query_u32(handle: *mut core::ffi::c_void, info_level: u32) -> Result<u32, QueryError> {
    let mut value = 0u32;
    let mut length = std::mem::size_of::<u32>() as u32;
    // SAFETY: `handle` is live; the out-buffer and its length are consistent.
    let ok = unsafe {
        WinHttpQueryHeaders(
            handle,
            info_level,
            std::ptr::null(),
            (&mut value as *mut u32).cast(),
            &mut length,
            std::ptr::null_mut(),
        )
    };
    if ok != 0 {
        return Ok(value);
    }
    // SAFETY: `GetLastError` reads the calling thread's error slot.
    let error = unsafe { GetLastError() };
    if error == ERROR_WINHTTP_HEADER_NOT_FOUND {
        Err(QueryError::Missing)
    } else {
        Err(QueryError::Failed(winhttp_error(
            "WinHttpQueryHeaders",
            error,
        )))
    }
}

/// Reads a wide-string option (the final URL).
fn query_string(handle: *mut core::ffi::c_void, option: u32) -> Option<String> {
    let mut length = 0u32;
    // SAFETY: a null buffer with a zero length asks for the required size.
    unsafe {
        WinHttpQueryOption(handle, option, std::ptr::null_mut(), &mut length);
    }
    if length == 0 {
        return None;
    }
    let mut buffer = vec![0u16; (length as usize).div_ceil(2)];
    // SAFETY: the buffer is `length` bytes; the call fills it and updates the
    // length it actually wrote.
    let ok = unsafe { WinHttpQueryOption(handle, option, buffer.as_mut_ptr().cast(), &mut length) };
    if ok == 0 {
        return None;
    }
    let characters = (length as usize) / 2;
    let text = String::from_utf16_lossy(&buffer[..characters.min(buffer.len())]);
    Some(text.trim_end_matches('\0').to_owned())
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn last_error(call: &str) -> UpdateError {
    // SAFETY: `GetLastError` reads the calling thread's error slot.
    let error = unsafe { GetLastError() };
    winhttp_error(call, error)
}

/// Maps the WinHTTP error space to messages a log reader can act on.
fn winhttp_error(call: &str, error: u32) -> UpdateError {
    let detail = match error {
        12002 => "the request timed out".to_owned(),
        12007 => "the host name could not be resolved".to_owned(),
        12029 => "the server could not be reached".to_owned(),
        12030 | 12152 => "the connection closed unexpectedly".to_owned(),
        12157 => "the secure connection failed".to_owned(),
        // Every certificate failure (expired, wrong host, untrusted chain)
        // lands here and is reported, never retried in the clear.
        12175 => "TLS certificate validation failed".to_owned(),
        12017 => "the request was cancelled".to_owned(),
        other => format!("WinHTTP error {other}"),
    };
    UpdateError::network(format!("{call}: {detail}"))
}

/// Reads a whole file into memory, refusing files above `limit`. The updater
/// uses it to hash an asset that is already on disk.
pub fn read_capped(path: &Path, limit: u64) -> Result<Vec<u8>, UpdateError> {
    let metadata = std::fs::metadata(path).map_err(|error| UpdateError::io("stat", path, error))?;
    if metadata.len() > limit {
        return Err(UpdateError::TooLarge {
            what: path.display().to_string(),
            actual: metadata.len(),
            limit,
        });
    }
    std::fs::read(path).map_err(|error| UpdateError::io("read", path, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_actionable_messages() {
        let tls = winhttp_error("WinHttpReceiveResponse", 12175).to_string();
        assert!(tls.contains("certificate"), "{tls}");
        let timeout = winhttp_error("WinHttpReceiveResponse", 12002).to_string();
        assert!(timeout.contains("timed out"), "{timeout}");
        let other = winhttp_error("WinHttpOpen", 12345).to_string();
        assert!(other.contains("12345"), "{other}");
        // Every mapped error is a network error, so callers can treat the
        // whole class as "try again later" without inspecting strings.
        for code in [12002, 12007, 12029, 12030, 12152, 12157, 12175, 12017] {
            assert!(matches!(
                winhttp_error("call", code),
                UpdateError::Network(_)
            ));
        }
    }

    #[test]
    fn wide_strings_are_nul_terminated() {
        let text = wide("abc");
        assert_eq!(text, vec![b'a' as u16, b'b' as u16, b'c' as u16, 0]);
    }

    #[test]
    fn read_capped_refuses_oversized_files() {
        let directory =
            std::env::temp_dir().join(format!("delicium-winhttp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("payload.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read_capped(&path, 10).unwrap().len(), 10);
        assert!(matches!(
            read_capped(&path, 9),
            Err(UpdateError::TooLarge { .. })
        ));
        assert!(read_capped(&directory.join("missing"), 10).is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
