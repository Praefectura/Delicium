//! The WinHTTP transport for the chat endpoint: a single HTTPS (or loopback
//! HTTP) POST per request, on its own thread.
//!
//! This mirrors the updater's hand-written WinHTTP FFI
//! (`crate::update::winhttp`) but is a separate implementation on purpose: the
//! updater's client is a GET-only, pinned-GitHub-shaped fetcher whose
//! acceptance tests must keep meaning what they meant, while the chat client
//! sends a body and a bearer token to a user-chosen host and therefore needs
//! its own policy (no redirects at all, a short budget, a small body cap).
//!
//! Security properties enforced here:
//!
//! - the URL has already passed [`super::parse_endpoint`], so remote hosts are
//!   TLS and plain http only exists for loopback;
//! - `WINHTTP_FLAG_SECURE` is set whenever the endpoint is TLS, and a
//!   certificate failure is reported, never retried in the clear;
//! - redirects are disabled: a bearer token must not be forwarded to a host
//!   the user did not configure;
//! - the request and response are both size-capped, and the whole call is
//!   bounded by [`TOTAL_BUDGET`];
//! - the API key appears only in the `Authorization` header, never in a log,
//!   an error string, or the request body.

use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};

use super::{Endpoint, MAX_RESPONSE_BYTES};
use crate::preferences::ChatSettings;

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
    fn WinHttpCloseHandle(handle: *mut core::ffi::c_void) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}

/// Use the machine's proxy configuration (Windows 8.1+).
const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
/// The connection must be TLS.
const FLAG_SECURE: u32 = 0x0080_0000;
/// Redirect behaviour: follow none. Chained value 0 is "never follow".
const OPTION_REDIRECT_POLICY: u32 = 88;
const REDIRECT_POLICY_NEVER: u32 = 0;
const QUERY_STATUS_CODE: u32 = 19;
const QUERY_CONTENT_LENGTH: u32 = 5;
const QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
const ERROR_WINHTTP_HEADER_NOT_FOUND: u32 = 12150;

/// Total time one exchange may take.
const TOTAL_BUDGET: Duration = Duration::from_secs(20);
/// Bytes read from the socket at a time.
const READ_CHUNK: u32 = 32 * 1024;

/// A live WinHTTP session, closed on drop.
struct Session(*mut core::ffi::c_void);

impl Session {
    fn open() -> Result<Self, String> {
        let agent = wide("Delicium chat");
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
        if unsafe { WinHttpSetTimeouts(handle, 5_000, 5_000, 10_000, 15_000) } == 0 {
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

/// Sends one chat completion request and returns the response body.
///
/// `body` is the JSON from [`super::ChatRequest::to_json`]. The key is placed
/// in the `Authorization` header exactly when it is non-empty.
pub fn post_chat(
    settings: &ChatSettings,
    endpoint: &Endpoint,
    body: &str,
) -> Result<String, String> {
    let deadline = Instant::now() + TOTAL_BUDGET;
    let session = Session::open()?;
    let host = wide(&endpoint.host);
    // SAFETY: `session` is live; the host string is NUL-terminated.
    let connect = unsafe { WinHttpConnect(session.0, host.as_ptr(), endpoint.port, 0) };
    if connect.is_null() {
        return Err(last_error("WinHttpConnect"));
    }
    let connect = Handle(connect);
    let verb = wide("POST");
    let target = wide(&endpoint.target);
    // SAFETY: the connection is live; both strings are NUL-terminated.
    let request = unsafe {
        WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            if endpoint.secure { FLAG_SECURE } else { 0 },
        )
    };
    if request.is_null() {
        return Err(last_error("WinHttpOpenRequest"));
    }
    let request = Handle(request);
    // SAFETY: `request` is live; the policy word is passed by pointer as
    // WinHTTP expects. Redirects stay off so the token cannot travel.
    unsafe {
        let policy = REDIRECT_POLICY_NEVER;
        if WinHttpSetOption(
            request.0,
            OPTION_REDIRECT_POLICY,
            (&policy as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        ) == 0
        {
            return Err(last_error("WinHttpSetOption(REDIRECT_POLICY)"));
        }
    }
    let mut headers = format!(
        "Content-Type: application/json\r\nAccept: application/json\r\nUser-Agent: Delicium/{}\r\n",
        env!("CARGO_PKG_VERSION")
    );
    if !settings.api_key.trim().is_empty() {
        headers.push_str("Authorization: Bearer ");
        headers.push_str(settings.api_key.trim());
        headers.push_str("\r\n");
    }
    let header_text = wide(&headers);
    if body.len() > u32::MAX as usize {
        return Err("The chat request body is too large".into());
    }
    // SAFETY: `request` is live. The header block and the body are both
    // readable for their stated lengths; WinHTTP copies them before returning.
    let sent = unsafe {
        WinHttpSendRequest(
            request.0,
            header_text.as_ptr(),
            (header_text.len() - 1) as u32,
            body.as_ptr().cast(),
            body.len() as u32,
            body.len() as u32,
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
    let status = query_u32(request.0, QUERY_STATUS_CODE | QUERY_FLAG_NUMBER)
        .map_err(|error| format!("WinHttpQueryHeaders: {error}"))?;
    if !(200..300).contains(&status) {
        // Read a bounded prefix of the error body: providers put the useful
        // message there, and it is the endpoint's own text, never the key.
        let detail = read_body(&request, deadline, 2048).unwrap_or_default();
        let detail = detail.trim();
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(": {}", detail.chars().take(300).collect::<String>())
        };
        return Err(format!("The chat endpoint answered HTTP {status}{suffix}"));
    }
    match query_u32(request.0, QUERY_CONTENT_LENGTH | QUERY_FLAG_NUMBER) {
        Ok(declared) if declared as usize > MAX_RESPONSE_BYTES => {
            return Err(format!(
                "The chat reply is {declared} bytes, above the {MAX_RESPONSE_BYTES} byte limit"
            ));
        }
        Ok(_) | Err(_) => {}
    }
    read_body(&request, deadline, MAX_RESPONSE_BYTES)
}

/// Reads the response body, capped at `limit` bytes and the deadline.
fn read_body(request: &Handle, deadline: Instant, limit: usize) -> Result<String, String> {
    let mut collected = Vec::new();
    let mut buffer = vec![0u8; READ_CHUNK as usize];
    loop {
        if Instant::now() > deadline {
            return Err(format!(
                "The chat exchange exceeded its {} second budget",
                TOTAL_BUDGET.as_secs()
            ));
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
        if collected.len() + read as usize > limit {
            return Err(format!("The chat reply exceeds the {limit} byte limit"));
        }
        collected.extend_from_slice(&buffer[..read as usize]);
    }
    String::from_utf8(collected).map_err(|_| "The chat reply is not valid UTF-8".to_owned())
}

fn query_u32(handle: *mut core::ffi::c_void, info_level: u32) -> Result<u32, String> {
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
        Err("missing header".to_owned())
    } else {
        Err(winhttp_error("WinHttpQueryHeaders", error))
    }
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn last_error(call: &str) -> String {
    // SAFETY: `GetLastError` reads the calling thread's error slot.
    let error = unsafe { GetLastError() };
    winhttp_error(call, error)
}

/// Maps the WinHTTP error space to messages a user can act on. The messages
/// never include the request headers, so the bearer token cannot leak here.
fn winhttp_error(call: &str, error: u32) -> String {
    let detail = match error {
        12002 => "the request timed out",
        12007 => "the host name could not be resolved",
        12029 => "the server could not be reached",
        12030 | 12152 => "the connection closed unexpectedly",
        12157 => "the secure connection failed",
        12175 => "TLS certificate validation failed",
        12017 => "the request was cancelled",
        _ => return format!("{call}: WinHTTP error {error}"),
    };
    format!("{call}: {detail}")
}

/// Whether a chat request may be attempted at all, without touching the
/// network. The UI uses this to choose between the send button and the
/// disabled hint.
pub fn ready(settings: &ChatSettings) -> Result<Endpoint, String> {
    if !settings.enabled {
        return Err("Remote chat is disabled".into());
    }
    if settings.model.trim().is_empty() {
        return Err("No chat model name is configured".into());
    }
    super::endpoint(settings).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::ChatSettings;

    #[test]
    fn error_messages_are_actionable_and_never_contain_the_key() {
        let mut settings = ChatSettings {
            enabled: true,
            base_url: "https://api.example.com/v1".into(),
            model: "m".into(),
            api_key: "sk-must-not-appear".into(),
        };
        let endpoint = super::super::endpoint(&settings).unwrap();
        assert!(!endpoint.url.contains("sk-must-not-appear"));
        // Error mapping never echoes configuration values.
        for code in [12002, 12007, 12029, 12175] {
            let text = winhttp_error("call", code);
            assert!(!text.contains("sk-must-not-appear"), "{text}");
        }
        settings.api_key.clear();
        assert!(ready(&settings).is_ok());
    }

    #[test]
    fn ready_rejects_disabled_and_incomplete_configuration() {
        let mut settings = ChatSettings {
            enabled: false,
            base_url: "https://api.example.com/v1".into(),
            model: "m".into(),
            api_key: String::new(),
        };
        assert!(ready(&settings).is_err());
        settings.enabled = true;
        settings.model.clear();
        assert!(ready(&settings).is_err());
        settings.model = "m".into();
        settings.base_url = "http://remote.example.com/v1".into();
        assert!(ready(&settings).is_err());
        settings.base_url = "http://127.0.0.1:8080/v1".into();
        assert!(ready(&settings).is_ok());
    }

    #[test]
    fn wide_strings_are_nul_terminated() {
        assert_eq!(wide("a"), vec![b'a' as u16, 0]);
    }
}
