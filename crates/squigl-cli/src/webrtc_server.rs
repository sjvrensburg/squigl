//! `--webrtc`: [`squigl_pairing`]'s server, with each session streamed to the V4L2
//! device (opened lazily at whatever size the browser's camera negotiated). One
//! session at a time: a second phone is turned away (503) while one streams.

use anyhow::Result;
use squigl_core::decode::Backend;
use squigl_pairing::{PairingOptions, PairingServer};
use squigl_v4l2::LazyV4l2Sink;
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub use squigl_pairing::detect_lan_ip;

/// Serves the capture page until `stop` is raised.
pub fn run(
    device: &Path,
    bind: IpAddr,
    port: u16,
    decoder: Backend,
    stop: &Arc<AtomicBool>,
) -> Result<()> {
    let busy = Arc::new(AtomicBool::new(false));
    let options = PairingOptions {
        bind,
        port,
        decoder,
        cert_dir: None,
    };
    let server = PairingServer::start(options, {
        let (device, stop) = (device.to_path_buf(), Arc::clone(stop));
        move |mut session| {
            if busy.swap(true, Ordering::SeqCst) {
                return Err(
                    "a streaming session is already active; end it before starting another"
                        .to_string(),
                );
            }
            let (device, stop, busy) = (device.clone(), Arc::clone(&stop), Arc::clone(&busy));
            std::thread::spawn(move || {
                log::info!("WebRTC session started");
                let mut sink = LazyV4l2Sink::new(device);
                match session.run(&mut sink, &stop) {
                    Ok(()) => log::info!("WebRTC session ended"),
                    Err(e) => log::warn!("WebRTC session ended: {e}"),
                }
                busy.store(false, Ordering::SeqCst);
            });
            Ok(())
        }
    })?;

    log::info!(
        "open {} in the phone's browser (same network as this machine); \
         accept the self-signed certificate warning, then tap \"Start streaming\"",
        server.url()
    );
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }
    server.stop();
    Ok(())
}
