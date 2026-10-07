//! Tailscale's own certificate for this machine, when its tailnet has HTTPS on: a
//! Let's Encrypt certificate for `machine.tailnet.ts.net`, which a phone on the
//! tailnet reaches through MagicDNS and its browser trusts, so it shows no warning.
//! Asked of the `tailscale` command (`tailscale cert`), which keeps and renews it;
//! on Linux that needs the user to be the tailnet's operator (`sudo tailscale set
//! --operator=$USER`) or root, else it fails and the self-signed one serves.

use crate::Cert;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) struct TailscaleCert {
    /// This machine's name on the tailnet, which the certificate is for.
    pub name: String,
    pub cert: Cert,
}

/// The certificate, kept in `dir`; `None` (and why, in the log) when Tailscale is
/// not running, HTTPS is off for the tailnet, or the certificate cannot be had.
pub(crate) fn certificate(dir: &Path) -> Option<TailscaleCert> {
    let status = run(&["status", "--json"], Duration::from_secs(5))?;
    let Some(name) = cert_name(&status) else {
        log::info!("Tailscale: HTTPS is off for this tailnet, so its address gets the self-signed certificate");
        return None;
    };
    let (crt, key) = (dir.join("tailscale.crt"), dir.join("tailscale.key"));
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!("Tailscale certificate: {}: {e}", dir.display());
        return None;
    }
    // Issuing one takes a while (Let's Encrypt); a kept one is quick.
    run(
        &[
            "cert",
            "--cert-file",
            &crt.to_string_lossy(),
            "--key-file",
            &key.to_string_lossy(),
            &name,
        ],
        Duration::from_secs(60),
    )?;
    let cert_pem = std::fs::read_to_string(&crt).ok()?;
    let key_pem = std::fs::read_to_string(&key).ok()?;
    log::info!("Tailscale: using the certificate for {name}");
    Some(TailscaleCert {
        name,
        cert: Cert { cert_pem, key_pem },
    })
}

/// This machine's name, from `tailscale status --json`, if the tailnet will
/// certify it (`CertDomains` lists it) and Tailscale is running.
fn cert_name(status: &str) -> Option<String> {
    let status: serde_json::Value = serde_json::from_str(status).ok()?;
    if status["BackendState"] != "Running" {
        return None;
    }
    let name = status["Self"]["DNSName"].as_str()?.trim_end_matches('.');
    let domains = status["CertDomains"].as_array()?;
    domains
        .iter()
        .any(|d| d.as_str() == Some(name))
        .then(|| name.to_string())
}

/// Runs `tailscale` with `args`, killing it after `limit`; its output if it
/// succeeded.
fn run(args: &[&str], limit: Duration) -> Option<String> {
    let mut command = Command::new("tailscale");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            log::debug!("tailscale: {e}");
            return None;
        }
    };
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                log::warn!("tailscale {}: no answer within {limit:?}", args[0]);
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        log::info!(
            "tailscale {}: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_taken_only_when_the_tailnet_certifies_it() {
        let status = |state: &str, domains: &str| {
            format!(
                r#"{{"BackendState":"{state}","Self":{{"DNSName":"desk.tail1234.ts.net."}},"CertDomains":{domains}}}"#
            )
        };
        assert_eq!(
            cert_name(&status("Running", r#"["desk.tail1234.ts.net"]"#)).as_deref(),
            Some("desk.tail1234.ts.net")
        );
        assert_eq!(cert_name(&status("Running", "null")), None);
        assert_eq!(
            cert_name(&status("Running", r#"["other.tail1234.ts.net"]"#)),
            None
        );
        assert_eq!(
            cert_name(&status("Stopped", r#"["desk.tail1234.ts.net"]"#)),
            None
        );
        assert_eq!(cert_name("not json"), None);
    }
}
