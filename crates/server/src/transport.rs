//! The WebSocket listener. It runs on background threads and only moves text
//! frames between sockets and the foreground session.

use std::io::{self, ErrorKind, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, http};

/// Playwright client minor versions whose protocol this server speaks.
pub const SUPPORTED_MINORS: &[&str] = &["1.64"];

pub(crate) enum Event {
    Opened {
        connection: u64,
        outgoing: mpsc::Sender<String>,
    },
    Message {
        connection: u64,
        text: String,
    },
    Closed {
        connection: u64,
    },
}

pub(crate) fn listen(
    addr: SocketAddr,
    token: Option<String>,
    events: async_channel::Sender<Event>,
) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;
    std::thread::Builder::new()
        .name("gpui-playwright-listener".into())
        .spawn(move || {
            let mut next = 0u64;
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                next += 1;
                let connection = next;
                let events = events.clone();
                let token = token.clone();
                let _ = std::thread::Builder::new()
                    .name(format!("gpui-playwright-conn-{connection}"))
                    .spawn(move || {
                        if let Err(err) = serve(stream, connection, token, events) {
                            log::debug!("gpui-playwright connection {connection}: {err}");
                        }
                    });
            }
        })?;
    Ok(local)
}

/// Reads the request head without consuming it, so plain HTTP probes can be
/// answered and WebSocket upgrades handed to tungstenite untouched.
fn peek_head(stream: &TcpStream) -> io::Result<String> {
    let mut buf = vec![0u8; 8192];
    for _ in 0..200 {
        let n = stream.peek(&mut buf)?;
        if n == 0 {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        if let Some(end) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&buf[..end]).into_owned());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(ErrorKind::InvalidData.into())
}

fn serve(
    mut stream: TcpStream,
    connection: u64,
    token: Option<String>,
    events: async_channel::Sender<Event>,
) -> Result<(), Box<dyn std::error::Error>> {
    let head = peek_head(&stream)?;
    if !head.to_ascii_lowercase().contains("upgrade: websocket") {
        // Health probe for test runners' `webServer.url`.
        let mut sink = vec![0u8; head.len() + 4];
        stream.read_exact(&mut sink)?;
        stream.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 7\r\nconnection: close\r\n\r\nRunning",
        )?;
        return Ok(());
    }

    let callback =
        move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
            if let Some(token) = &token
                && request.uri().path().trim_start_matches('/') != token
            {
                return Err(reject(http::StatusCode::FORBIDDEN, "invalid token"));
            }
            let agent = request
                .headers()
                .get("user-agent")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if let Some(message) = version_mismatch(agent) {
                return Err(reject(http::StatusCode::PRECONDITION_REQUIRED, &message));
            }
            Ok(response)
        };
    let mut socket = tungstenite::accept_hdr(stream, callback)?;
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(5)))?;

    let (outgoing, pending) = mpsc::channel::<String>();
    events.send_blocking(Event::Opened {
        connection,
        outgoing,
    })?;

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        loop {
            while let Ok(text) = pending.try_recv() {
                socket.write(Message::text(text))?;
            }
            socket.flush()?;
            match socket.read() {
                Ok(Message::Text(text)) => events.send_blocking(Event::Message {
                    connection,
                    text: text.to_string(),
                })?,
                Ok(Message::Close(_)) => return Ok(()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(err))
                    if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Ok(());
                }
                Err(err) => return Err(err.into()),
            }
        }
    })();
    let _ = events.send_blocking(Event::Closed { connection });
    result
}

fn reject(status: http::StatusCode, message: &str) -> ErrorResponse {
    let mut response = ErrorResponse::new(Some(message.to_owned()));
    *response.status_mut() = status;
    response
}

/// Mirrors Playwright's own check: clients send `Playwright/x.y.z` and a
/// server only accepts the minor versions it speaks.
fn version_mismatch(user_agent: &str) -> Option<String> {
    let version = user_agent
        .strip_prefix("Playwright/")?
        .split([' ', '/'])
        .next()?;
    let minor: String = version.split('.').take(2).collect::<Vec<_>>().join(".");
    if SUPPORTED_MINORS.contains(&minor.as_str()) {
        None
    } else {
        Some(format!(
            "Playwright version mismatch:\n  - gpui-playwright supports: v{}\n  - client version: v{minor}\n\nInstall a supported @playwright/test or playwright version.",
            SUPPORTED_MINORS.join(", v")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_minor_only() {
        assert_eq!(
            version_mismatch("Playwright/1.64.1 (x64; ubuntu 24.04) node/22.22"),
            None
        );
        assert!(version_mismatch("Playwright/1.63.0 (x64)").is_some());
        assert_eq!(version_mismatch("curl/8"), None);
    }
}
