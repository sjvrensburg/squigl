//! Pairing a phone's browser from the window ([`squigl_pairing`]): the server runs
//! only while the pairing dialog is open, and a phone that pairs becomes the
//! engine's source ([`Host::pair`]). Its session outlives the dialog.

use crate::host::Host;
use serde::Serialize;
use squigl_core::decode::Backend;
use squigl_pairing::{PairingOptions, PairingServer, DEFAULT_PORT};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Pairing {
    server: Mutex<Option<PairingServer>>,
    /// Where to listen; the LAN address when `None`.
    bind: Option<IpAddr>,
    decoder: Backend,
    /// Where the certificate is kept: beside the settings.
    cert_dir: PathBuf,
}

/// What the dialog shows.
#[derive(Serialize)]
pub struct PairingInfo {
    pub url: String,
    pub qr_svg: String,
}

impl Pairing {
    pub fn new(bind: Option<IpAddr>, decoder: Backend, cert_dir: PathBuf) -> Self {
        Self {
            server: Mutex::new(None),
            bind,
            decoder,
            cert_dir,
        }
    }

    /// Starts the server (or keeps the running one) and says how to reach it.
    pub fn start(&self, host: &Host) -> Result<PairingInfo, String> {
        let mut server = self.server.lock().unwrap();
        if server.is_none() {
            let bind = match self.bind {
                Some(bind) => bind,
                None => squigl_pairing::detect_lan_ip().map_err(|e| format!("{e:#}"))?,
            };
            let options = PairingOptions {
                bind,
                port: DEFAULT_PORT,
                decoder: self.decoder,
                cert_dir: Some(self.cert_dir.clone()),
            };
            let host = host.clone();
            let started = PairingServer::start(options, move |session| {
                host.pair(session);
                Ok(())
            })
            .map_err(|e| format!("{e:#}"))?;
            log::info!("pairing at {}", started.url());
            *server = Some(started);
        }
        let server = server.as_ref().expect("started above");
        Ok(PairingInfo {
            url: server.url().to_string(),
            qr_svg: server.qr_svg(),
        })
    }

    pub fn stop(&self) {
        if let Some(server) = self.server.lock().unwrap().take() {
            server.stop();
            log::info!("pairing closed");
        }
    }
}
