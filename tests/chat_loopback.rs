//! The chat transport against a loopback fixture.
//!
//! Every test here talks to a `std::net::TcpListener` on `127.0.0.1`, which is
//! exactly the one plain-http target the endpoint policy allows. Nothing
//! leaves the machine and no real provider is contacted: the server is a
//! scripted HTTP/1.1 responder that records what the client sent.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use delicium::app::chat::{self, ChatRequest, Message, Role, net};
use delicium::preferences::ChatSettings;

/// A recorded request from the chat client.
struct Recorded {
    request_line: String,
    headers: String,
    body: String,
}

/// Starts a one-shot HTTP server and returns its base URL and a handle that
/// yields the recorded request.
fn serve(response: &'static str, status: &'static str) -> (String, thread::JoinHandle<Recorded>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept the chat connection");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let recorded = read_request(&mut &stream);
        let mut stream = stream;
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        );
        stream.write_all(reply.as_bytes()).unwrap();
        stream.flush().unwrap();
        recorded
    });
    (format!("http://127.0.0.1:{port}/v1"), handle)
}

fn read_request(stream: &mut &TcpStream) -> Recorded {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut headers = String::new();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
        headers.push_str(&line);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).unwrap();
    Recorded {
        request_line,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

fn settings(base_url: &str) -> ChatSettings {
    ChatSettings {
        enabled: true,
        base_url: base_url.into(),
        model: "fixture-model".into(),
        api_key: "sk-loopback-fixture".into(),
    }
}

#[test]
fn a_loopback_reply_travels_the_real_transport() {
    let (base_url, server) = serve(
        r#"{"choices":[{"message":{"role":"assistant","content":"  Hello from the fixture!  "}}]}"#,
        "200 OK",
    );
    let config = settings(&base_url);
    let endpoint = chat::endpoint(&config).unwrap();
    assert_eq!(endpoint.host, "127.0.0.1");
    assert!(!endpoint.secure);

    let history = vec![Message {
        role: Role::User,
        content: "earlier turn".into(),
    }];
    let request = ChatRequest::build(&config, "You are Eous.", &history, "hello there");
    let body = request.to_json();
    let response = net::post_chat(&config, &endpoint, &body).unwrap();
    assert_eq!(
        chat::parse_reply(&response).unwrap(),
        "Hello from the fixture!"
    );

    let recorded = server.join().unwrap();
    assert!(
        recorded
            .request_line
            .starts_with("POST /v1/chat/completions "),
        "{}",
        recorded.request_line
    );
    assert!(
        recorded
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-loopback-fixture"),
        "{}",
        recorded.headers
    );
    let sent: serde_json::Value = serde_json::from_str(&recorded.body).unwrap();
    assert_eq!(sent["model"], "fixture-model");
    let messages = sent["messages"].as_array().unwrap();
    // System prompt, one context turn, the new user turn.
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["content"], "earlier turn");
    assert_eq!(messages[2]["content"], "hello there");
    // The key travels in the header only, never in the request line.
    assert!(!recorded.request_line.contains("sk-loopback-fixture"));
}

#[test]
fn an_error_status_is_reported_with_the_endpoint_message() {
    let (base_url, server) = serve(
        r#"{"error":{"message":"invalid api key"}}"#,
        "401 Unauthorized",
    );
    let config = settings(&base_url);
    let endpoint = chat::endpoint(&config).unwrap();
    let error = net::post_chat(&config, &endpoint, "{}").unwrap_err();
    assert!(error.contains("HTTP 401"), "{error}");
    assert!(error.contains("invalid api key"), "{error}");
    // The credential never leaks into the error text.
    assert!(!error.contains("sk-loopback-fixture"), "{error}");
    server.join().unwrap();
}

#[test]
fn redirects_are_not_followed() {
    // A redirect target that would answer if it were contacted. The client
    // must never reach it: a bearer token may only go to the configured host,
    // even when the configured host itself redirects.
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_port = target.local_addr().unwrap().port();
    let reached = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reached_clone = reached.clone();
    let target_thread = thread::spawn(move || {
        target.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Ok((_stream, _)) = target.accept() {
                reached_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                return;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
    });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let _ = read_request(&mut &stream);
        let reply = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target_port}/v1/chat/completions\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(reply.as_bytes()).unwrap();
    });

    let config = settings(&format!("http://127.0.0.1:{port}/v1"));
    let endpoint = chat::endpoint(&config).unwrap();
    let error = net::post_chat(&config, &endpoint, "{}").unwrap_err();
    assert!(error.contains("HTTP 302"), "{error}");
    server.join().unwrap();
    target_thread.join().unwrap();
    assert!(
        !reached.load(std::sync::atomic::Ordering::SeqCst),
        "the redirect target was contacted"
    );
}

#[test]
fn a_non_json_reply_is_an_error_not_a_pet_line() {
    let (base_url, server) = serve("<html>proxy error</html>", "200 OK");
    let config = settings(&base_url);
    let endpoint = chat::endpoint(&config).unwrap();
    let body = net::post_chat(&config, &endpoint, "{}").unwrap();
    assert!(chat::parse_reply(&body).is_err());
    server.join().unwrap();
}

#[test]
fn the_session_uses_the_transport_and_appends_the_reply() {
    let (base_url, server) = serve(
        r#"{"choices":[{"message":{"role":"assistant","content":"hi from remote"}}]}"#,
        "200 OK",
    );
    let mut session = delicium::app::chat::session::ChatSession::new(
        settings(&base_url),
        delicium::preferences::Resolved::En,
    );
    assert!(session.send(7, "Eous", "hello").is_none());
    assert_eq!(session.inflight(), Some(7));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let reply = loop {
        if let Some(reply) = session.poll() {
            break reply;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the remote reply never arrived"
        );
        thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(reply.remote);
    assert_eq!(reply.text, "hi from remote");
    assert_eq!(session.history(7).len(), 2);
    assert!(session.inflight().is_none());
    server.join().unwrap();
}
