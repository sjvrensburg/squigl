//! The "policy" half of the WebRTC camera source: an HTTPS server serving the phone
//! browser's capture page and a minimal WHIP-shaped ingest endpoint
//! ([`phone_cam4linux::webrtc_source`] handles the WebRTC/ICE/DTLS side once an offer
//! is accepted). LAN-only: no auth, no STUN/TURN, one session at a time.
//!
//! Self-signed and generated fresh each run (browsers require a secure context for
//! `getUserMedia`, and a LAN IP isn't a domain a real CA will certify) -- the phone's
//! browser will show a certificate warning once per server restart, which is expected
//! and must be accepted to proceed.

use anyhow::{Context, Result};
use phone_cam4linux::decode::Backend;
use phone_cam4linux::WebrtcSource;
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tiny_http::{Header, Method, Response, Server, SslConfig};

const CAPTURE_PAGE: &str = include_str!("webrtc_capture.html");

/// Runs the capture-page/WHIP HTTPS server until `stop` is raised. Accepted sessions
/// stream straight to `device` (opened lazily at whatever size the browser's camera
/// negotiated); only one session runs at a time, a second `POST /whip` while one is
/// active is rejected with 503 so the phone can retry after reloading.
pub fn run(
    device: &Path,
    bind: IpAddr,
    port: u16,
    decoder: Backend,
    stop: &Arc<AtomicBool>,
) -> Result<()> {
    let cert = generate_cert(bind).context("generating a self-signed TLS certificate")?;
    let server = Server::https(
        (bind, port),
        SslConfig {
            certificate: cert.cert_pem.into_bytes(),
            private_key: cert.key_pem.into_bytes(),
        },
    )
    .map_err(|e| anyhow::anyhow!("starting HTTPS server on {bind}:{port}: {e}"))?;

    log::info!(
        "open https://{bind}:{port}/ in the phone's browser (same network as this machine); \
         accept the self-signed certificate warning, then tap \"Start streaming\""
    );

    let busy = Arc::new(AtomicBool::new(false));

    while !stop.load(Ordering::Relaxed) {
        let request = match server.recv_timeout(Duration::from_millis(500)) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => {
                log::warn!("HTTP server error: {e}");
                continue;
            }
        };
        handle_request(request, device, bind, decoder, &busy, stop);
    }
    Ok(())
}

fn handle_request(
    request: tiny_http::Request,
    device: &Path,
    bind: IpAddr,
    decoder: Backend,
    busy: &Arc<AtomicBool>,
    stop: &Arc<AtomicBool>,
) {
    match (request.method(), request.url()) {
        (Method::Get, "/") => {
            let header = html_header();
            let _ = request.respond(Response::from_string(CAPTURE_PAGE).with_header(header));
        }
        (Method::Post, "/whip") => handle_whip(request, device, bind, decoder, busy, stop),
        _ => {
            let _ = request.respond(Response::from_string("not found").with_status_code(404));
        }
    }
}

fn handle_whip(
    mut request: tiny_http::Request,
    device: &Path,
    bind: IpAddr,
    decoder: Backend,
    busy: &Arc<AtomicBool>,
    stop: &Arc<AtomicBool>,
) {
    if busy.swap(true, Ordering::SeqCst) {
        let _ = request.respond(
            Response::from_string(
                "a streaming session is already active; end it before starting another",
            )
            .with_status_code(503),
        );
        return;
    }

    let mut offer_sdp = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut offer_sdp) {
        busy.store(false, Ordering::SeqCst);
        let _ = request
            .respond(Response::from_string(format!("reading offer: {e}")).with_status_code(400));
        return;
    }

    match WebrtcSource::accept_offer(&offer_sdp, bind, decoder) {
        Ok((mut session, answer_sdp)) => {
            let device = device.to_path_buf();
            let stop = Arc::clone(stop);
            let busy = Arc::clone(busy);
            std::thread::spawn(move || {
                log::info!("WebRTC session started");
                if let Err(e) = session.run_to_v4l2(&device, &stop) {
                    log::warn!("WebRTC session ended: {e}");
                } else {
                    log::info!("WebRTC session ended");
                }
                busy.store(false, Ordering::SeqCst);
            });
            let header = Header::from_bytes(&b"Content-Type"[..], &b"application/sdp"[..]).unwrap();
            let _ = request.respond(
                Response::from_string(answer_sdp)
                    .with_status_code(201)
                    .with_header(header),
            );
        }
        Err(e) => {
            busy.store(false, Ordering::SeqCst);
            let _ = request.respond(Response::from_string(format!("{e}")).with_status_code(400));
        }
    }
}

fn html_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap()
}

struct Cert {
    cert_pem: String,
    key_pem: String,
}

/// A fresh self-signed certificate valid for `bind`'s IP (the SAN a browser checks
/// against the address it connected to).
fn generate_cert(bind: IpAddr) -> Result<Cert> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec![bind.to_string()])
            .context("generating self-signed certificate")?;
    Ok(Cert {
        cert_pem: cert.pem(),
        key_pem: signing_key.serialize_pem(),
    })
}

/// Guesses this machine's LAN-facing IP (the one that would be used to reach the
/// public internet) without sending any traffic -- `connect` on a UDP socket just
/// picks a route, it doesn't require the peer to be reachable.
pub fn detect_lan_ip() -> Result<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").context("binding a probe socket")?;
    socket
        .connect("8.8.8.8:80")
        .context("resolving the outbound route")?;
    Ok(socket.local_addr()?.ip())
}
