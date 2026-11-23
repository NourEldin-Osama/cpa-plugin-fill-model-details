//! Minimal HTTP client for CPA's own Management API on 127.0.0.1.
//!
//! Only the port comes from the caller; the address is always 127.0.0.1.
//! Requests use HTTP/1.1 with `Connection: close`, so the reply ends when CPA
//! closes the connection.
//!
//! Requests with a body send `Expect: 100-continue` and wait before sending
//! the body. When CPA refuses early (for example 412 for an old version tag),
//! it answers before reading the body. Without the wait, the unread body makes
//! the connection reset and the refusal is lost.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::error::PluginError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for "100 Continue" before sending the body anyway.
const CONTINUE_TIMEOUT: Duration = Duration::from_secs(5);
const HEAD_END: &[u8] = b"\r\n\r\n";

#[derive(Debug)]
pub struct LoopbackResponse {
    pub status_code: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl LoopbackResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub fn send(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<LoopbackResponse, PluginError> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let request_head = build_request_head(port, method, path, headers, body.map(<[u8]>::len))?;
    stream.write_all(request_head.as_bytes())?;
    stream.flush()?;

    let mut received = Vec::new();
    if let Some(body_bytes) = body {
        stream.set_read_timeout(Some(CONTINUE_TIMEOUT))?;
        let first_status = read_first_head(&mut stream, &mut received)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        match first_status {
            // CPA wants the body.
            Some(100) => {
                remove_first_head(&mut received);
                stream.write_all(body_bytes)?;
                stream.flush()?;
            }
            // CPA already gave its final answer; the body is not wanted.
            Some(_) => {}
            // No answer in time: send the body, as HTTP clients normally do.
            None => {
                stream.write_all(body_bytes)?;
                stream.flush()?;
            }
        }
    }

    stream.read_to_end(&mut received)?;
    parse_response(&received)
}

fn build_request_head(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body_length: Option<usize>,
) -> Result<String, PluginError> {
    if !is_safe_header_text(method) || !is_safe_header_text(path) {
        return Err(PluginError::new(
            "invalid_request",
            "method or path has line breaks",
        ));
    }
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n"
    );
    for (name, value) in headers {
        if !is_safe_header_text(name) || !is_safe_header_text(value) {
            return Err(PluginError::new(
                "invalid_request",
                "header has line breaks",
            ));
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(length) = body_length {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {length}\r\n"));
        head.push_str("Expect: 100-continue\r\n");
    }
    head.push_str("\r\n");
    Ok(head)
}

fn is_safe_header_text(text: &str) -> bool {
    !text.contains('\r') && !text.contains('\n')
}

/// Reads until the first reply head is complete and returns its status code.
/// Returns `None` when nothing complete arrives before the read timeout.
fn read_first_head(
    stream: &mut TcpStream,
    received: &mut Vec<u8>,
) -> Result<Option<u16>, PluginError> {
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(head_end) = find_subsequence(received, HEAD_END) {
            return Ok(status_code_of(&received[..head_end]));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(read_count) => received.extend_from_slice(&chunk[..read_count]),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn remove_first_head(received: &mut Vec<u8>) {
    if let Some(head_end) = find_subsequence(received, HEAD_END) {
        received.drain(..head_end + HEAD_END.len());
    }
}

fn status_code_of(head: &[u8]) -> Option<u16> {
    let head_text = std::str::from_utf8(head).ok()?;
    let status_line = head_text.split("\r\n").next()?;
    status_line.split_whitespace().nth(1)?.parse::<u16>().ok()
}

fn parse_response(raw_response: &[u8]) -> Result<LoopbackResponse, PluginError> {
    let mut remaining = raw_response;
    loop {
        let head_end = find_subsequence(remaining, HEAD_END)
            .ok_or_else(|| PluginError::new("invalid_response", "reply has no header end"))?;
        let status_code = status_code_of(&remaining[..head_end])
            .ok_or_else(|| PluginError::new("invalid_response", "reply has no status code"))?;
        // Skip interim replies such as "100 Continue".
        if (100..200).contains(&status_code) {
            remaining = &remaining[head_end + HEAD_END.len()..];
            continue;
        }
        let head = std::str::from_utf8(&remaining[..head_end])
            .map_err(|_| PluginError::new("invalid_response", "reply headers are not UTF-8"))?;
        let headers: Vec<(String, String)> = head
            .split("\r\n")
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect();
        let mut body = remaining[head_end + HEAD_END.len()..].to_vec();
        let is_chunked = headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Transfer-Encoding") && value.eq_ignore_ascii_case("chunked")
        });
        if is_chunked {
            body = decode_chunked(&body)?;
        }
        return Ok(LoopbackResponse {
            status_code,
            headers,
            body,
        });
    }
}

fn decode_chunked(mut remaining: &[u8]) -> Result<Vec<u8>, PluginError> {
    let mut decoded = Vec::new();
    loop {
        let line_end = find_subsequence(remaining, b"\r\n")
            .ok_or_else(|| PluginError::new("invalid_response", "chunk size line is cut"))?;
        let size_text = std::str::from_utf8(&remaining[..line_end])
            .map_err(|_| PluginError::new("invalid_response", "chunk size is not text"))?;
        let size_hex = size_text.split(';').next().unwrap_or("").trim();
        let chunk_size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| PluginError::new("invalid_response", "chunk size is not hex"))?;
        remaining = &remaining[line_end + 2..];
        if chunk_size == 0 {
            return Ok(decoded);
        }
        if remaining.len() < chunk_size + 2 {
            return Err(PluginError::new("invalid_response", "chunk is cut"));
        }
        decoded.extend_from_slice(&remaining[..chunk_size]);
        remaining = &remaining[chunk_size + 2..];
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Port part of a `host:port` value, for example `127.0.0.1:8317` or `[::1]:8317`.
pub fn port_from_host(host: &str) -> Option<u16> {
    let (_, port_text) = host.trim().rsplit_once(':')?;
    port_text.parse::<u16>().ok()
}

/// Port of an `http://host:port/...` URL, such as an Origin or Referer value.
pub fn port_from_url(url: &str) -> Option<u16> {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = without_scheme.split('/').next().unwrap_or("");
    port_from_host(authority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    /// Starts a one-shot server on a free local port and returns that port.
    fn serve_once(handler: impl FnOnce(TcpStream) + Send + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a free port");
        let port = listener.local_addr().expect("local address").port();
        thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept one client");
            handler(stream);
        });
        port
    }

    /// Reads a request head and returns its Content-Length, if any.
    fn read_request_head(reader: &mut BufReader<TcpStream>) -> usize {
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("read a request line");
            if line == "\r\n" || line.is_empty() {
                return content_length;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("Content-Length")
            {
                content_length = value.trim().parse().expect("numeric length");
            }
        }
    }

    #[test]
    fn early_refusal_is_read_and_body_is_not_sent() {
        let port = serve_once(|stream| {
            let mut reader = BufReader::new(stream);
            read_request_head(&mut reader);
            let mut stream = reader.into_inner();
            stream
                .write_all(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .expect("write the refusal");
        });
        let body = vec![b'x'; 300_000];
        let response =
            send(port, "PUT", "/config", &[], Some(&body)).expect("refusal should be read");
        assert_eq!(response.status_code, 412);
        assert_eq!(response.body, b"{}");
    }

    #[test]
    fn body_is_sent_after_continue() {
        let port = serve_once(|stream| {
            let mut reader = BufReader::new(stream);
            let content_length = read_request_head(&mut reader);
            reader
                .get_mut()
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .expect("write continue");
            let mut body = vec![0_u8; content_length];
            reader.read_exact(&mut body).expect("read the body");
            // Answer with the number of body bytes the server received.
            let reply_body = body.len().to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nETag: \"new\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply_body.len(),
                reply_body
            );
            let mut stream = reader.into_inner();
            stream.write_all(reply.as_bytes()).expect("write the reply");
        });
        let response =
            send(port, "PUT", "/config", &[], Some(b"{\"a\":1}")).expect("reply should parse");
        assert_eq!(response.status_code, 200);
        assert_eq!(response.header("etag"), Some("\"new\""));
        assert_eq!(response.body, b"7");
    }

    #[test]
    fn parses_status_headers_and_body() {
        let raw =
            b"HTTP/1.1 200 OK\r\nETag: \"abc\"\r\nContent-Type: application/json\r\n\r\n{\"a\":1}";
        let response = parse_response(raw).expect("reply should parse");
        assert_eq!(response.status_code, 200);
        assert_eq!(response.header("etag"), Some("\"abc\""));
        assert_eq!(response.body, b"{\"a\":1}");
    }

    #[test]
    fn skips_interim_reply() {
        let raw = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\nX: 1\r\n\r\n";
        let response = parse_response(raw).expect("reply should parse");
        assert_eq!(response.status_code, 204);
        assert_eq!(response.header("x"), Some("1"));
    }

    #[test]
    fn decodes_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let response = parse_response(raw).expect("reply should parse");
        assert_eq!(response.body, b"Wikipedia");
    }

    #[test]
    fn rejects_header_injection() {
        let headers = vec![("X-Test".to_string(), "a\r\nEvil: 1".to_string())];
        assert!(build_request_head(8317, "GET", "/", &headers, None).is_err());
    }

    #[test]
    fn reads_ports() {
        assert_eq!(port_from_host("127.0.0.1:8317"), Some(8317));
        assert_eq!(port_from_host("[::1]:8318"), Some(8318));
        assert_eq!(port_from_host("localhost"), None);
        assert_eq!(
            port_from_url("http://127.0.0.1:8318/v0/resource/plugins/x/page"),
            Some(8318)
        );
        assert_eq!(port_from_url("http://localhost"), None);
    }
}
