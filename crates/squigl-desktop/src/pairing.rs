//! Pairing a phone's browser from the window ([`squigl_pairing`]): the server runs
//! only while the pairing dialog is open, and a phone that pairs becomes the
//! engine's source ([`Host::pair`]). Its session outlives the dialog.

use crate::host::Host;
use serde::Serialize;
use squigl_core::decode::Backend;
use squigl_pairing::{Address, PairingOptions, PairingServer, DEFAULT_PORT};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Pairing {
    server: Mutex<Option<PairingServer>>,
    /// Where to listen; every LAN and overlay address when empty.
    bind: Vec<IpAddr>,
    decoder: Backend,
    /// Where the certificate is kept: beside the settings.
    cert_dir: PathBuf,
}

/// One way to pair, as the dialog shows it.
#[derive(Serialize)]
pub struct Offer {
    /// The network, as a person would name it ("Wi-Fi", "Tailscale").
    pub network: String,
    pub overlay: bool,
    pub url: String,
    pub qr_svg: String,
    /// Whether the browser will take the certificate without a warning.
    pub trusted: bool,
}

impl Pairing {
    pub fn new(bind: Vec<IpAddr>, decoder: Backend, cert_dir: PathBuf) -> Self {
        Self {
            server: Mutex::new(None),
            bind,
            decoder,
            cert_dir,
        }
    }

    /// Starts the server (or keeps the running one) and says how to reach it.
    /// `tailscale_https` is the `[desktop]` setting (see `squigl_pairing`).
    pub fn start(&self, host: &Host, tailscale_https: bool) -> Result<Vec<Offer>, String> {
        let mut server = self.server.lock().unwrap();
        if server.is_none() {
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
            let host = host.clone();
            let started = PairingServer::start(options, move |session| {
                host.pair(session);
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
        if let Some(server) = self.server.lock().unwrap().take() {
            server.stop();
            log::info!("pairing closed");
        }
    }
}
