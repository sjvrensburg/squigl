//! Pairs a phone's browser with squigl: an HTTPS server for the capture page and a
//! minimal WHIP-shaped ingest endpoint, plus the QR code that leads a phone there.
//! [`squigl_core::webrtc_source`] handles the WebRTC/ICE/DTLS side once an offer is
//! accepted; what happens to the session is the caller's (the CLI streams it to a
//! V4L2 device, the desktop app hands it to its engine).
//!
//! - **A token per start.** The address carries a random `?t=`, and the page and
//!   `/whip` answer only with it, so someone else on the network cannot push a
//!   picture into squigl, nor even find the page, without seeing the QR code.
//! - **A self-signed certificate.** Browsers need a secure context for
//!   `getUserMedia`, and a LAN address is not something a real CA will certify, so
//!   the phone's browser warns once. With [`PairingOptions::cert_dir`] the
//!   certificate is kept and reused, so it warns once per phone rather than on every
//!   start (and again only when the address changes).
//! - **Every address at once.** The LAN's and any overlay network's
//!   ([`addresses()`]: Tailscale, ZeroTier, Nebula), each its own listener and
//!   [`Offer`], since a phone on another network can still reach an overlay
//!   address. The WebRTC media goes over the address the offer came in at: a host
//!   ICE candidate there, no STUN/TURN.

use anyhow::{Context, Result};
use squigl_core::decode::Backend;
use squigl_core::WebrtcSource;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server, SslConfig};

mod addresses;
mod tailscale;

pub use addresses::{addresses, classify, Address, Network};

const CAPTURE_PAGE: &str = include_str!("capture.html");

/// The port asked for unless the caller says otherwise. Firefox remembers a
/// certificate exception per host and port, so a stable port matters as much as a
/// stable certificate.
pub const DEFAULT_PORT: u16 = 8443;

/// An SDP offer is a few kilobytes; anything far larger is not one.
const MAX_OFFER: u64 = 64 * 1024;

pub struct PairingOptions {
    /// Where the phone may reach this machine, best first ([`addresses()`], or one
    /// given outright). The page, `/whip` and the WebRTC media listen on each.
    pub addresses: Vec<Address>,
    /// The page's port; where it is taken, any free one.
    pub port: u16,
    pub decoder: Backend,
    /// Where to keep the certificate between starts; `None` makes a fresh one each
    /// time.
    pub cert_dir: Option<PathBuf>,
    /// For a Tailscale address, Tailscale's own certificate for this machine's name
    /// when the tailnet has HTTPS on (`tailscale cert`), so the browser does not
    /// warn. Issuing it puts the name in the public Certificate Transparency logs;
    /// the tailnet's admin agreed to that in turning HTTPS on.
    pub tailscale_https: bool,
}

/// One way to reach the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub network: Network,
    /// The address to open on the phone, token included.
    pub url: String,
    /// Whether the certificate is one the browser trusts (no warning).
    pub trusted: bool,
}

/// What becomes of an accepted session. `Err` turns the phone away (503, with the
/// message) before it is sent an answer.
pub type OnSession = Box<dyn FnMut(WebrtcSource) -> Result<(), String> + Send>;

/// A running pairing server; it stops when stopped or dropped. Sessions it handed
/// over keep running: they have their own sockets.
pub struct PairingServer {
    offers: Vec<Offer>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

/// A server bound to one address, and how it is offered.
struct Listener {
    server: Server,
    bind: IpAddr,
    offer: Offer,
}

impl PairingServer {
    pub fn start(
        options: PairingOptions,
        on_session: impl FnMut(WebrtcSource) -> Result<(), String> + Send + 'static,
    ) -> Result<Self> {
        anyhow::ensure!(
            !options.addresses.is_empty(),
            "no network address to pair at"
        );
        let names: Vec<String> = options.addresses.iter().map(|a| a.ip.to_string()).collect();
        let own = certificate(options.cert_dir.as_deref(), &names)?;
        let tailscale = match &options.cert_dir {
            Some(dir)
                if options.tailscale_https
                    && options
                        .addresses
                        .iter()
                        .any(|a| a.network == Network::Tailscale) =>
            {
                tailscale::certificate(dir)
            }
            _ => None,
        };
        let token = token();
        let mut listeners = Vec::new();
        let mut failures = Vec::new();
        for address in &options.addresses {
            let (cert, host, trusted) = match &tailscale {
                Some(ts) if address.network == Network::Tailscale => {
                    (&ts.cert, ts.name.clone(), true)
                }
                _ => (&own, host(address.ip), false),
            };
            let ssl = || SslConfig {
                certificate: cert.cert_pem.clone().into_bytes(),
                private_key: cert.key_pem.clone().into_bytes(),
            };
            let server = Server::https((address.ip, options.port), ssl()).or_else(|e| {
                if options.port == 0 {
                    return Err(e);
                }
                log::info!(
                    "port {} is not free at {} ({e}); using another",
                    options.port,
                    address.ip
                );
                Server::https((address.ip, 0), ssl())
            });
            match server {
                Ok(server) => {
                    let port = server.server_addr().to_ip().map_or(0, |a| a.port());
                    listeners.push(Listener {
                        server,
                        bind: address.ip,
                        offer: Offer {
                            network: address.network,
                            url: format!("https://{host}:{port}/?t={token}"),
                            trusted,
                        },
                    });
                }
                Err(e) => failures.push(format!("{}: {e}", address.ip)),
            }
        }
        for failure in &failures {
            log::warn!("pairing cannot listen at {failure}");
        }
        anyhow::ensure!(
            !listeners.is_empty(),
            "pairing could not listen anywhere ({})",
            failures.join("; ")
        );
        Ok(Self::serve(
            listeners,
            token,
            options.decoder,
            Box::new(on_session),
        ))
    }

    fn serve(
        listeners: Vec<Listener>,
        token: String,
        decoder: Backend,
        on_session: OnSession,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let on_session = Arc::new(Mutex::new(on_session));
        let mut offers = Vec::new();
        let mut threads = Vec::new();
        for Listener {
            server,
            bind,
            offer,
        } in listeners
        {
            log::info!("pairing on {} at {}", offer.network.label(), offer.url);
            offers.push(offer);
            let handler = Handler {
                token: token.clone(),
                bind,
                decoder,
                on_session: Arc::clone(&on_session),
            };
            let stop = Arc::clone(&stop);
            threads.push(
                std::thread::Builder::new()
                    .name("squigl-pairing".into())
                    .spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            match server.recv_timeout(Duration::from_millis(200)) {
                                Ok(Some(request)) => handler.handle(request),
                                Ok(None) => {}
                                Err(e) => log::warn!("pairing server: {e}"),
                            }
                        }
                    })
                    .expect("spawning a pairing server's thread"),
            );
        }
        Self {
            offers,
            stop,
            threads,
        }
    }

    /// Every way to reach the server, best first.
    pub fn offers(&self) -> &[Offer] {
        &self.offers
    }

    /// The best address to open on the phone, token included.
    pub fn url(&self) -> &str {
        &self.offers[0].url
    }

    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for PairingServer {
    fn drop(&mut self) {
        self.halt();
    }
}

struct Handler {
    token: String,
    bind: IpAddr,
    decoder: Backend,
    on_session: Arc<Mutex<OnSession>>,
}

impl Handler {
    fn handle(&self, request: Request) {
        let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
        let path = path.to_string();
        let authorised = query
            .split('&')
            .filter_map(|pair| pair.strip_prefix("t="))
            .any(|t| same(t, &self.token));
        let response = match (request.method(), path.as_str(), authorised) {
            (Method::Get, "/", true) => {
                Response::from_string(CAPTURE_PAGE).with_header(header("text/html; charset=utf-8"))
            }
            (Method::Get, "/", false) => Response::from_string(EXPIRED)
                .with_status_code(403)
                .with_header(header("text/html; charset=utf-8")),
            (Method::Post, "/whip", true) => return self.whip(request),
            (Method::Post, "/whip", false) => {
                Response::from_string("this pairing code is not current").with_status_code(403)
            }
            _ => Response::from_string("not found").with_status_code(404),
        };
        let _ = request.respond(response);
    }

    fn whip(&self, mut request: Request) {
        let mut offer = String::new();
        if let Err(e) = request
            .as_reader()
            .take(MAX_OFFER)
            .read_to_string(&mut offer)
        {
            let _ = request.respond(
                Response::from_string(format!("reading offer: {e}")).with_status_code(400),
            );
            return;
        }
        let response = match WebrtcSource::accept_offer(&offer, self.bind, self.decoder) {
            Ok((session, answer)) => match (self.on_session.lock().unwrap())(session) {
                Ok(()) => {
                    log::info!("a phone paired");
                    Response::from_string(answer)
                        .with_status_code(201)
                        .with_header(header("application/sdp"))
                }
                Err(reason) => Response::from_string(reason).with_status_code(503),
            },
            Err(e) => Response::from_string(e.to_string()).with_status_code(400),
        };
        let _ = request.respond(response);
    }
}

const EXPIRED: &str = "<!doctype html><meta charset=utf-8>\
<meta name=viewport content=\"width=device-width, initial-scale=1\">\
<title>Squigl</title><h1>This pairing code has expired</h1>\
<p>Scan the code Squigl is showing now.</p>";

fn header(content_type: &str) -> Header {
    Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).unwrap()
}

/// Compares in time independent of where they differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// 128 random bits as hex.
fn token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An IP as a URL's host: IPv6 in brackets.
fn host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

/// `text` (an [`Offer`]'s URL) as a QR code: an SVG document, dark modules on
/// white (whatever the theme: a camera needs the contrast), with the quiet zone.
pub fn qr_svg(text: &str) -> String {
    use qrcode::render::svg;
    match qrcode::QrCode::new(text.as_bytes()) {
        Ok(code) => code
            .render::<svg::Color>()
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .quiet_zone(true)
            .build(),
        // Only text past a QR code's capacity (some 3 KB) fails; an address is not.
        Err(e) => {
            log::warn!("no QR code for {text}: {e}");
            String::new()
        }
    }
}

pub(crate) struct Cert {
    pub(crate) cert_pem: String,
    pub(crate) key_pem: String,
}

/// The certificate for `names` (its subject alternative names, what a browser
/// checks against the address it went to): the one kept in `dir` if it was made for
/// the same names, else a new one, kept there for next time.
fn certificate(dir: Option<&Path>, names: &[String]) -> Result<Cert> {
    let Some(dir) = dir else {
        return generate(names);
    };
    let files = [
        dir.join("pairing.crt"),
        dir.join("pairing.key"),
        dir.join("pairing.names"),
    ];
    let read = |i: usize| std::fs::read_to_string(&files[i]);
    if let (Ok(cert_pem), Ok(key_pem), Ok(kept)) = (read(0), read(1), read(2)) {
        if kept.lines().eq(names.iter().map(String::as_str)) {
            return Ok(Cert { cert_pem, key_pem });
        }
    }
    let cert = generate(names)?;
    let keep = || -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&files[0], &cert.cert_pem)?;
        write_private(&files[1], &cert.key_pem)?;
        std::fs::write(&files[2], names.join("\n"))
    };
    if let Err(e) = keep() {
        log::warn!(
            "could not keep the pairing certificate in {}: {e}",
            dir.display()
        );
    }
    Ok(cert)
}

fn generate(names: &[String]) -> Result<Cert> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(names.to_vec())
            .context("generating a self-signed TLS certificate")?;
    Ok(Cert {
        cert_pem: cert.pem(),
        key_pem: signing_key.serialize_pem(),
    })
}

/// Writes a file only its owner can read (on Unix; elsewhere the user's profile
/// directory already is).
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(contents.as_bytes())
}

/// Guesses this machine's LAN-facing IP (the one that would be used to reach the
/// public internet) without sending any traffic -- `connect` on a UDP socket just
/// picks a route, it doesn't require the peer to be reachable.
pub fn detect_lan_ip() -> Result<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").context("binding a probe socket")?;
    socket
        .connect("8.8.8.8:80")
        .context("finding the route out (is this machine on a network?)")?;
    Ok(socket.local_addr()?.ip())
}

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;
