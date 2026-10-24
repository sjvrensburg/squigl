//! Pairing a phone's browser from the window (`squigl_pairing`): the server
//! runs only while the pairing dialog is open; a phone that pairs is queued
//! for the app's pump to hand to the engine (the GPUI counterpart of the
//! Tauri host's `Pair` request).

use squigl_core::decode::Backend;
use squigl_core::WebrtcSource;
use squigl_pairing::{Address, PairingOptions, PairingServer, DEFAULT_PORT};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct Pairing {
    server: Mutex<Option<PairingServer>>,
    /// The `tailscale_https` the running server was started with, if any.
    tailscale: Mutex<Option<bool>>,
    bind: Vec<IpAddr>,
    decoder: Backend,
    cert_dir: PathBuf,
    sessions: Arc<Mutex<Vec<WebrtcSource>>>,
}

/// One way to pair, as the dialog shows it.
pub struct Offer {
    pub network: String,
    pub overlay: bool,
    pub url: String,
    pub qr_svg: String,
    pub trusted: bool,
}

impl Pairing {
    pub fn new(bind: Vec<IpAddr>, decoder: Backend, cert_dir: PathBuf) -> Self {
        Self {
            server: Mutex::new(None),
            tailscale: Mutex::new(None),
            bind,
            decoder,
            cert_dir,
            sessions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The queue paired sessions arrive in (for `--dev-fake-phone`).
    pub fn sessions_queue(&self) -> Arc<Mutex<Vec<WebrtcSource>>> {
        Arc::clone(&self.sessions)
    }

    /// The newest paired phone waiting to be shown, if any: an older one
    /// queued ahead of it is dropped (the desktop shows the latest pairing).
    pub fn take_newest(&self) -> Option<WebrtcSource> {
        let mut sessions = self.sessions.lock().unwrap();
        let newest = sessions.pop();
        sessions.clear();
        newest
    }

    /// Starts the server (or keeps the running one) and says how to reach it.
    pub fn start(&self, tailscale_https: bool) -> Result<Vec<Offer>, String> {
        let mut server = self.server.lock().unwrap();
        // A changed setting restarts the server with it in force.
        if *self.tailscale.lock().unwrap() == Some(tailscale_https) && server.is_some() {
            // Already running as asked; fall through to the offers.
        } else if server.is_some() {
            if let Some(old) = server.take() {
                old.stop();
            }
        }
        if server.is_none() {
            *self.tailscale.lock().unwrap() = Some(tailscale_https);
            let options = PairingOptions {
                addresses: if self.bind.is_empty() {
                    squigl_pairing::addresses()
                } else {
                    self.bind.iter().copied().map(Address::given).collect()
                },
                port: DEFAULT_PORT,
                decoder: self.decoder,
                cert_dir: Some(self.cert_dir.clone()),
                tailscale_https,
            };
            let queue = Arc::clone(&self.sessions);
            let started = PairingServer::start(options, move |session| {
                queue.lock().unwrap().push(session);
                Ok(())
            })
            .map_err(|e| format!("{e:#}"))?;
            *server = Some(started);
        }
        let server = server.as_ref().expect("started above");
        Ok(server
            .offers()
            .iter()
            .map(|o| Offer {
                network: o.network.label().to_string(),
                overlay: o.network.is_overlay(),
                url: o.url.clone(),
                qr_svg: squigl_pairing::qr_svg(&o.url),
                trusted: o.trusted,
            })
            .collect())
    }

    pub fn stop(&self) {
        *self.tailscale.lock().unwrap() = None;
        if let Some(server) = self.server.lock().unwrap().take() {
            server.stop();
            log::info!("pairing closed");
        }
    }
}
