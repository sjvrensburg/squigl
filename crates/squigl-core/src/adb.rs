//! Minimal ADB transport, implemented by shelling out to the system `adb` binary.
//!
//! A pure-Rust USB ADB client (e.g. the `adb_client` crate) was considered to avoid the
//! system-`adb` dependency, but its public API only exposes `forward`/`reverse` tunnel
//! setup through a connection to a *running adb server* (`adb start-server`, i.e. the
//! same daemon the `adb` binary talks to) rather than direct USB stream opening to an
//! arbitrary `localabstract:` service. Reimplementing that raw USB protocol ourselves
//! is a much larger undertaking than this MVP justifies, so we shell out to `adb`
//! instead -- it is a single, common dependency (`android-tools`/`platform-tools`) and
//! this is how effectively every scrcpy-like tool works.

use crate::error::{Error, Result};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const DEVICE_SERVER_PATH: &str = "/data/local/tmp/scrcpy-server.jar";

/// Default port adbd listens on once switched to TCP/IP mode (`adb tcpip 5555`).
pub const DEFAULT_TCP_PORT: u16 = 5555;

/// An Android device addressed by ADB serial -- a USB serial, or `host:port` for a
/// device reached over TCP/IP (see [`AdbDevice::connect_tcp`]).
#[derive(Debug, Clone)]
pub struct AdbDevice {
    pub serial: Option<String>,
}

impl AdbDevice {
    /// Picks the sole device in the `device` (authorized, online) state, or fails if
    /// zero or more than one are present. Offline/unauthorized entries are ignored,
    /// as is the stale `host:port` entry adb keeps after a Wi-Fi device goes away.
    pub fn autodetect() -> Result<Self> {
        let out = adb(&[], &["devices"])?;
        let serials: Vec<&str> = out
            .lines()
            .skip(1)
            .filter_map(|l| {
                let mut cols = l.split_whitespace();
                match (cols.next(), cols.next()) {
                    (Some(serial), Some("device")) => Some(serial),
                    _ => None,
                }
            })
            .collect();
        match serials.len() {
            0 => Err(Error::NoDevice),
            1 => Ok(Self {
                serial: Some(serials[0].to_string()),
            }),
            _ => Err(Error::AdbCommand(format!(
                "multiple devices attached ({serials:?}); pass a serial explicitly"
            ))),
        }
    }

    pub fn with_serial(serial: impl Into<String>) -> Self {
        Self {
            serial: Some(serial.into()),
        }
    }

    /// Connects to a device over TCP/IP (`adb connect host:port`; the port defaults
    /// to [`DEFAULT_TCP_PORT`]) and returns it. The phone must already be in TCP/IP
    /// mode, see [`Self::enable_tcpip`]. Safe to call repeatedly: an existing
    /// connection is reported as "already connected" and reused.
    pub fn connect_tcp(address: &str) -> Result<Self> {
        let address = if address.contains(':') {
            address.to_string()
        } else {
            format!("{address}:{DEFAULT_TCP_PORT}")
        };
        let out = adb(&[], &["connect", &address])?;
        // `adb connect` exits 0 even on failure; the verdict is in the text.
        let ok = out.contains("connected to");
        if !ok {
            return Err(Error::AdbCommand(format!(
                "`adb connect {address}` failed: {}",
                out.trim()
            )));
        }
        Ok(Self::with_serial(address))
    }

    /// Switches this (USB-attached) device's adbd to listen on TCP `port` and returns
    /// the `host:port` address to use with [`Self::connect_tcp`], using the phone's
    /// Wi-Fi IPv4 address. The USB cable can be unplugged afterwards; `adb usb`
    /// switches back.
    pub fn enable_tcpip(&self, port: u16) -> Result<String> {
        let ip = self.wifi_ipv4()?;
        let port_str = port.to_string();
        let out = adb(&self.args(&[]), &["tcpip", &port_str])?;
        if !out.contains("restarting in TCP mode") {
            return Err(Error::AdbCommand(format!(
                "unexpected `adb tcpip {port}` output: {}",
                out.trim()
            )));
        }
        Ok(format!("{ip}:{port}"))
    }

    /// The phone's IPv4 address on `wlan0` (Wi-Fi), parsed from `ip addr`.
    pub fn wifi_ipv4(&self) -> Result<String> {
        let out = adb(&self.args(&[]), &["shell", "ip -4 -o addr show wlan0"])?;
        parse_ipv4_from_ip_addr(&out).ok_or_else(|| {
            Error::AdbCommand(format!(
                "could not find the phone's Wi-Fi IPv4 address (is Wi-Fi on?); \
                 `ip -4 -o addr show wlan0` printed: {}",
                out.trim()
            ))
        })
    }

    fn args<'a>(&'a self, rest: &'a [&'a str]) -> Vec<&'a str> {
        let mut v = Vec::with_capacity(rest.len() + 2);
        if let Some(serial) = &self.serial {
            v.push("-s");
            v.push(serial.as_str());
        }
        v.extend_from_slice(rest);
        v
    }

    /// Pushes the embedded scrcpy-server jar to the device's tmp dir.
    pub fn push_server_jar(&self, jar: &[u8]) -> Result<()> {
        let tmp = std::env::temp_dir().join("squigl-scrcpy-server.jar");
        std::fs::write(&tmp, jar)?;
        let tmp_str = tmp.to_string_lossy().to_string();
        adb(&self.args(&[]), &["push", &tmp_str, DEVICE_SERVER_PATH])?;
        Ok(())
    }

    /// Sets up `adb forward tcp:<local_port> localabstract:scrcpy_<scid>` and returns the
    /// local TCP port scrcpy's server will be reachable on. Port 0 asks adb to pick a free one.
    pub fn forward(&self, scid_hex8: &str) -> Result<u16> {
        let remote = format!("localabstract:scrcpy_{scid_hex8}");
        let out = adb(&self.args(&[]), &["forward", "tcp:0", &remote])?;
        out.trim()
            .parse::<u16>()
            .map_err(|_| Error::AdbCommand(format!("unexpected `adb forward` output: {out:?}")))
    }

    pub fn remove_forward(&self, port: u16) {
        let local = format!("tcp:{port}");
        let _ = adb(&self.args(&[]), &["forward", "--remove", &local]);
    }

    /// Starts the scrcpy server on-device as a background shell process and returns the
    /// child handle for the local `adb shell` process driving it (killing it stops the
    /// remote server too, since it's the foreground process of that shell).
    ///
    /// `server_args` are the `key=value` options passed to the server (must include
    /// `scid=<hex8>` matching the socket name used in [`Self::forward`]).
    pub fn start_server(&self, server_args: &[String]) -> Result<Child> {
        let cmd = server_command(server_args);
        let mut argv = self.args(&[]);
        argv.push("shell");
        argv.push(&cmd);
        spawn_adb(&argv)
    }

    /// Runs the scrcpy server to completion (for one-shot modes such as
    /// `list_camera_sizes=true`) and returns everything it printed.
    pub fn run_server_once(&self, server_args: &[String]) -> Result<String> {
        let cmd = server_command(server_args);
        adb(&self.args(&[]), &["shell", &cmd])
    }
}

fn server_command(server_args: &[String]) -> String {
    format!(
        "CLASSPATH={DEVICE_SERVER_PATH} app_process / com.genymobile.scrcpy.Server {} {}",
        env!("SCRCPY_SERVER_VERSION"),
        server_args.join(" ")
    )
}

/// The adb executable's file name on this OS.
const ADB_EXE: &str = if cfg!(windows) { "adb.exe" } else { "adb" };

/// Where to look for adb, in order: `$SQUIGL_ADB`; next to the running executable
/// (where an installer puts the one it ships); `platform-tools` under
/// `$ANDROID_HOME` and `$ANDROID_SDK_ROOT` (an Android SDK); then plain `adb`, found
/// on `PATH`. `env` reads an environment variable.
fn candidates(env: impl Fn(&str) -> Option<OsString>, exe_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(p) = env("SQUIGL_ADB").filter(|p| !p.is_empty()) {
        found.push(PathBuf::from(p));
    }
    if let Some(dir) = exe_dir {
        found.push(dir.join(ADB_EXE));
    }
    for sdk in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(root) = env(sdk).filter(|p| !p.is_empty()) {
            found.push(PathBuf::from(root).join("platform-tools").join(ADB_EXE));
        }
    }
    found.push(PathBuf::from(ADB_EXE));
    found
}

/// A process for `program`. On Windows it gets no console window of its own: a GUI
/// app spawning adb would otherwise flash one up every time.
fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// The adb to use: the first that runs (`adb version`) of `$SQUIGL_ADB`, `adb` next
/// to the running executable, `platform-tools/adb` under `$ANDROID_HOME` or
/// `$ANDROID_SDK_ROOT`, and `adb` on `PATH`. Found
/// once and remembered; not finding it is not remembered, so installing adb while
/// squigl runs is picked up on the next try.
pub fn locate() -> Result<PathBuf> {
    static FOUND: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    if let Some(path) = FOUND.get() {
        return Ok(path.clone());
    }
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent);
    let path = candidates(|v| std::env::var_os(v), exe_dir)
        .into_iter()
        .find(|p| {
            command(p)
                .arg("version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
        .ok_or(Error::AdbNotFound)?;
    log::debug!("using adb at {}", path.display());
    Ok(FOUND.get_or_init(|| path).clone())
}

fn adb(pre_args: &[&str], args: &[&str]) -> Result<String> {
    let bin = locate()?;
    let mut full = Vec::with_capacity(pre_args.len() + args.len());
    full.extend_from_slice(pre_args);
    full.extend_from_slice(args);
    log::debug!("adb {}", full.join(" "));
    let output = command(&bin).args(&full).output()?;
    if !output.status.success() {
        return Err(Error::AdbCommand(format!(
            "`adb {}` failed: {}",
            full.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn spawn_adb(args: &[&str]) -> Result<Child> {
    let bin = locate()?;
    log::debug!("adb {} (background)", args.join(" "));
    // Deliberately left in our process group: a terminal Ctrl-C then also reaches the
    // `adb shell` child, so the on-device server dies even if we exit without
    // running `CameraSession`'s Drop (e.g. a second, hard Ctrl-C).
    Ok(command(&bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?)
}

/// Extracts the address from `ip -4 -o addr show wlan0` output such as
/// `24: wlan0    inet 192.168.1.23/24 brd 192.168.1.255 scope global wlan0\       valid_lft ...`.
fn parse_ipv4_from_ip_addr(out: &str) -> Option<String> {
    let mut words = out.split_whitespace();
    while let Some(w) = words.next() {
        if w == "inet" {
            let cidr = words.next()?;
            return Some(cidr.split('/').next()?.to_string());
        }
    }
    None
}

/// Generates an 8 hex-digit session id, used both as the `scid=` server argument and the
/// `scrcpy_<scid>` local socket name, matching scrcpy's own convention (letting multiple
/// concurrent sessions on one device coexist).
pub fn random_scid_hex8() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    // scrcpy parses scid with Integer.parseInt(value, 16) into a *signed* 32-bit int,
    // so it must fit in 31 bits -- mask off the top bit or values with the high hex
    // digit >= 8 overflow and the server aborts with NumberFormatException.
    let mixed = (nanos ^ (pid << 32)) as u32 & 0x7fff_ffff;
    format!("{mixed:08x}")
}

/// Best-effort: ensure the writer side of a spawned child's stdin is flushed/closed so
/// `adb shell` doesn't hang waiting for input.
pub fn close_stdin(child: &mut Child) {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adb_is_looked_for_in_order() {
        let env = |v: &str| match v {
            "SQUIGL_ADB" => Some(OsString::from("/opt/adb/adb")),
            "ANDROID_HOME" => Some(OsString::from("/sdk")),
            // Set but empty: skipped.
            "ANDROID_SDK_ROOT" => Some(OsString::new()),
            _ => None,
        };
        let found = candidates(env, Some(Path::new("/app")));
        assert_eq!(
            found,
            [
                PathBuf::from("/opt/adb/adb"),
                Path::new("/app").join(ADB_EXE),
                Path::new("/sdk").join("platform-tools").join(ADB_EXE),
                PathBuf::from(ADB_EXE),
            ]
        );
        // With nothing set, only PATH.
        assert_eq!(candidates(|_| None, None), [PathBuf::from(ADB_EXE)]);
    }

    #[test]
    fn parses_wlan_address() {
        let out = "24: wlan0    inet 192.168.1.23/24 brd 192.168.1.255 scope global wlan0\\       valid_lft forever preferred_lft forever\n";
        assert_eq!(
            parse_ipv4_from_ip_addr(out).as_deref(),
            Some("192.168.1.23")
        );
        assert_eq!(parse_ipv4_from_ip_addr(""), None);
        assert_eq!(
            parse_ipv4_from_ip_addr("Device 'wlan0' does not exist."),
            None
        );
    }
}
