//! The frame transport: a WebSocket on `127.0.0.1` (roadmap spike S1 chose it --
//! the only one of the three measured that kept a 12 MB frame at 30 fps on under
//! one core).
//!
//! A connection must carry this launch's random token (`?token=…`) and come from
//! the app's own webview (its `Origin`); anything else is refused at the handshake.
//! The page sends a [`FrameRequest`] as JSON text and gets back either the text
//! `{"none":true}` (no frame yet) or one binary message: the header's length (`u32`
//! LE), the [`crate::host::FrameHeader`] as JSON, then the Y, U and V planes. It asks
//! again when it has drawn and there is something new, so nothing queues up.

use crate::host::{FrameReply, FrameRequest, Host};
use std::net::{TcpListener, TcpStream};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::Message;

/// Where the page connects, for this launch.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

/// The origins of the app's own webview: `tauri://localhost` (Linux, macOS),
/// `http(s)://tauri.localhost` (Windows), and Vite's dev server.
const ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
    "http://localhost:1420",
];

fn token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Starts serving frames from `host`; returns where to connect.
pub fn start(host: Host) -> std::io::Result<Endpoint> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = Endpoint {
        port: listener.local_addr()?.port(),
        token: token(),
    };
    let token = endpoint.token.clone();
    std::thread::Builder::new()
        .name("squigl-frames".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let (host, token) = (host.clone(), token.clone());
                std::thread::spawn(move || {
                    if let Err(e) = serve(stream, &host, &token) {
                        log::debug!("frame connection ended: {e}");
                    }
                });
            }
        })?;
    Ok(endpoint)
}

/// Whether a handshake carries `token` and an allowed origin.
fn allowed(request: &Request, token: &str) -> bool {
    let has_token = request
        .uri()
        .query()
        .unwrap_or("")
        .split('&')
        .any(|pair| pair == format!("token={token}"));
    let origin = request
        .headers()
        .get("origin")
        .and_then(|o| o.to_str().ok())
        .unwrap_or("");
    has_token && ORIGINS.contains(&origin)
}

fn serve(stream: TcpStream, host: &Host, token: &str) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    // Owned: a failed handshake's error keeps the callback, and the error must
    // outlive this call.
    let token = token.to_owned();
    // The signature is tungstenite's `Callback`; its error type is not ours to box.
    #[allow(clippy::result_large_err)]
    let check = move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        if allowed(request, &token) {
            Ok(response)
        } else {
            log::warn!(
                "refused a frame connection from {:?}",
                request.headers().get("origin")
            );
            let mut refusal = ErrorResponse::new(Some("forbidden".into()));
            *refusal.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
            Err(refusal)
        }
    };
    let mut ws = tungstenite::accept_hdr(stream, check)?;
    loop {
        let message = ws.read()?;
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => return Ok(()),
            _ => continue,
        };
        let request: FrameRequest = serde_json::from_str(&text)?;
        match host.frame(request) {
            Some(reply) => ws.send(Message::Binary(encode(&reply)?.into()))?,
            None => ws.send(Message::Text(r#"{"none":true}"#.into()))?,
        }
    }
}

/// The binary message: header length, header JSON, Y, U, V.
fn encode(reply: &FrameReply) -> anyhow::Result<Vec<u8>> {
    let header = serde_json::to_vec(&reply.header)?;
    let p = &reply.planes;
    let mut out = Vec::with_capacity(4 + header.len() + p.y.len() + p.u.len() + p.v.len());
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&p.y);
    out.extend_from_slice(&p.u);
    out.extend_from_slice(&p.v);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(uri: &str, origin: Option<&str>) -> Request {
        let mut r = Request::builder().uri(uri);
        if let Some(o) = origin {
            r = r.header("origin", o);
        }
        r.body(()).unwrap()
    }

    #[test]
    fn only_the_apps_own_webview_with_the_token_gets_in() {
        let t = "abc123";
        assert!(allowed(
            &request("/?token=abc123", Some("tauri://localhost")),
            t
        ));
        assert!(allowed(
            &request("/?x=1&token=abc123", Some("http://tauri.localhost")),
            t
        ));
        assert!(!allowed(
            &request("/?token=abc123", Some("http://evil.example")),
            t
        ));
        assert!(!allowed(&request("/?token=abc123", None), t));
        assert!(!allowed(
            &request("/?token=abc12", Some("tauri://localhost")),
            t
        ));
        assert!(!allowed(&request("/", Some("tauri://localhost")), t));
    }

    #[test]
    fn tokens_are_128_random_bits_in_hex() {
        let (a, b) = (token(), token());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
