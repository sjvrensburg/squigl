//! squigl-desktop: Squigl's desktop app, a Tauri 2 window over `squigl-engine`. The
//! engine runs on its own thread ([`host`]); the web UI drives it with
//! [`dispatch`]ed commands, follows it through the events [`subscribe`] streams,
//! and pulls frames over a localhost WebSocket ([`transport`]).

// A window, not a console program: on Windows a release build opens no console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod dev_voice;
mod host;
mod pairing;
mod transport;

use clap::Parser;
use host::{Host, LutReply};
use pairing::Pairing;
use squigl_core::{ConnectOptions, Facing};
use squigl_engine::config::Config;
use squigl_engine::engine::{Command, EngineDeps, EngineOptions, Event, Reply};
use squigl_engine::stream::{Resolution, SourceSpec, StreamConfig};
use std::path::PathBuf;
use tauri::ipc::Channel;
use tauri::{Manager, RunEvent, State};
use transport::Endpoint;

/// The phone (or a file) as a magnifier.
#[derive(Parser, Debug)]
#[command(name = "squigl-desktop", version)]
struct Args {
    /// ADB serial of the phone (autodetected if only one is attached).
    #[arg(long)]
    serial: Option<String>,

    /// Use the phone over Wi-Fi: `adb connect` to HOST[:PORT].
    #[arg(long, value_name = "HOST[:PORT]", conflicts_with = "serial")]
    connect: Option<String>,

    /// Open an image instead of using the phone.
    #[arg(long, value_name = "FILE")]
    open: Option<PathBuf>,

    /// Play a recording made with `squigl-cli --record`, looping.
    #[arg(long, value_name = "FILE", conflicts_with = "open")]
    replay: Option<PathBuf>,

    /// Development aid: synthetic colour bars instead of the phone.
    #[arg(long, hide = true, conflicts_with_all = ["open", "replay"])]
    test_pattern: bool,

    /// Development aid: shortcut keys the page presses, one every half second
    /// once the first frame is drawn, space-separated (`Space` for the space bar).
    #[arg(long, hide = true, value_name = "KEYS")]
    dev_keys: Option<String>,

    /// Development aid: after this many seconds, write the picture (the canvas) to
    /// --dev-snapshot-path as a PNG and quit.
    #[arg(long, hide = true, value_name = "SECS", requires = "dev_snapshot_path")]
    dev_snapshot_after: Option<f32>,

    #[arg(long, hide = true, value_name = "FILE")]
    dev_snapshot_path: Option<PathBuf>,

    /// Development aid: the page logs the frames it drew every 5 s.
    #[arg(long, hide = true)]
    dev_stats: bool,

    /// Development aid: draw with the Canvas2D fallback even where WebGL2 works.
    #[arg(long, hide = true)]
    dev_canvas2d: bool,

    /// Development aid: the page offers `window.squiglProbe` (the last frame's
    /// header, drawn pixels and the engine's reference for them) to a WebDriver test.
    #[arg(long, hide = true)]
    dev_probe: bool,

    /// Development aid: pair phones at these addresses (comma-separated), not
    /// this machine's own (127.0.0.1 lets a browser on this machine pair). One
    /// switch, not repeated: msedgedriver keeps only the last of a repeated one.
    #[arg(long, hide = true, value_name = "IP,...", value_delimiter = ',')]
    dev_pairing_bind: Vec<std::net::IpAddr>,

    /// Development aid: pair a phone played by str0m (colour bars, with a camera
    /// that has zoom and a torch) at start; `dev_phone_camera` says what it has
    /// been asked to do.
    #[arg(long, hide = true)]
    dev_fake_phone: bool,

    /// Development aid: zoom the page by this factor, as the OS text size would.
    #[arg(long, hide = true, value_name = "FACTOR")]
    dev_text_scale: Option<f64>,

    /// Development aid: open the window at this size (logical pixels), not maximised.
    #[arg(long, hide = true, value_name = "WxH", value_parser = parse_size)]
    dev_window_size: Option<(f64, f64)>,

    /// Development aid: start from the default settings and save them to FILE,
    /// leaving the real configuration alone.
    #[arg(long, hide = true, value_name = "FILE")]
    dev_config: Option<PathBuf>,

    /// Development aid: read with one OpenAI-compatible backend at URL (its
    /// `/v1` base) in place of the configured ones.
    #[arg(long, hide = true, value_name = "URL")]
    dev_backend: Option<String>,

    /// Development aid: read aloud with a voice that says nothing and keeps what it
    /// was given (`dev_spoken`), in place of the system's speech.
    #[arg(long, hide = true)]
    dev_fake_voice: bool,
}

/// The development aids the page acts on.
#[derive(Debug, Clone, Default, serde::Serialize)]
struct DevOptions {
    keys: Vec<String>,
    snapshot_after_ms: Option<u64>,
    stats: bool,
    canvas2d: bool,
    probe: bool,
    #[serde(skip)]
    snapshot_path: Option<PathBuf>,
}

#[tauri::command]
fn dispatch(host: State<'_, Host>, command: Command) -> Result<Reply, String> {
    host.command(command)
}

#[tauri::command]
fn subscribe(host: State<'_, Host>, channel: Channel<Event>) {
    host.subscribe(channel);
}

#[tauri::command]
fn endpoint(endpoint: State<'_, Endpoint>) -> Endpoint {
    endpoint.inner().clone()
}

/// An image the page has as bytes (pasted, or picked with a file input, which gives
/// no path): written to the cache -- replacing the last one -- and shown.
#[tauri::command]
fn open_image(host: State<'_, Host>, request: tauri::ipc::Request<'_>) -> Result<Reply, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("the image must come as raw bytes".into());
    };
    let dir = squigl_engine::paths::cache_dir().join("opened");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    for old in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let _ = std::fs::remove_file(old.path());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = dir.join(format!("{stamp}.img"));
    std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
    host.command(Command::UseSource {
        source: SourceSpec::Image(path),
    })
}

/// Opens pairing (the dialog is showing): each address and its QR code. Async, so
/// a slow `tailscale cert` holds up no window thread.
#[tauri::command]
async fn pairing_start(
    host: State<'_, Host>,
    pairing: State<'_, Pairing>,
    tailscale_https: bool,
) -> Result<Vec<pairing::Offer>, String> {
    pairing.start(&host, tailscale_https)
}

/// Closes pairing (the dialog is gone); a paired phone keeps streaming.
#[tauri::command]
fn pairing_stop(pairing: State<'_, Pairing>) {
    pairing.stop();
}

/// The page's errors and warnings, into the app's log (the web inspector is not at
/// hand in a release build).
#[tauri::command]
fn page_log(level: String, message: String) {
    match level.as_str() {
        "error" => log::error!(target: "page", "{message}"),
        "warn" => log::warn!(target: "page", "{message}"),
        _ => log::info!(target: "page", "{message}"),
    }
}

#[tauri::command]
fn dev_options(dev: State<'_, DevOptions>) -> DevOptions {
    dev.inner().clone()
}

/// The page's snapshot (PNG bytes as the raw request body): written, then quit.
#[tauri::command]
fn dev_save_snapshot(
    dev: State<'_, DevOptions>,
    app: tauri::AppHandle,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let (Some(path), tauri::ipc::InvokeBody::Raw(png)) = (&dev.snapshot_path, request.body())
    else {
        return Err("no snapshot was asked for, or it came as JSON".into());
    };
    std::fs::write(path, png).map_err(|e| format!("writing {}: {e}", path.display()))?;
    log::info!("wrote {}", path.display());
    app.exit(0);
    Ok(())
}

/// The engine's colours for view pixels of the shown frame, for `--dev-probe`.
#[tauri::command]
fn dev_reference(host: State<'_, Host>, points: Vec<(usize, usize)>) -> Vec<Option<[u8; 3]>> {
    points
        .into_iter()
        .map(|(x, y)| host.reference(x, y))
        .collect()
}

/// The phone `--dev-fake-phone` paired, while it streams.
#[derive(Default)]
struct DevPhone(std::sync::Mutex<Option<squigl_pairing::testing::FakePhone>>);

/// The fake phone's camera now (its zoom and torch), for a test.
#[tauri::command]
fn dev_phone_camera(phone: State<'_, DevPhone>) -> Option<squigl_core::RemoteCamera> {
    phone.0.lock().unwrap().as_ref().map(|p| p.camera())
}

/// Pairs a fake phone straight into the engine (no pairing server: its offer is
/// answered here).
fn pair_fake_phone(host: &Host, decoder: squigl_core::decode::Backend) -> DevPhone {
    let phone = squigl_pairing::testing::FakePhone::connect((640, 480), |offer| {
        let local = std::net::Ipv4Addr::LOCALHOST.into();
        let (session, answer) = squigl_core::WebrtcSource::accept_offer(offer, local, decoder)
            .map_err(|e| e.to_string())?;
        host.pair(session);
        Ok(answer)
    });
    match phone {
        Ok(phone) => DevPhone(std::sync::Mutex::new(Some(phone))),
        Err(e) => {
            log::error!("the fake phone did not pair: {e}");
            DevPhone::default()
        }
    }
}

/// Where Save puts pictures and readings: the pictures folder's `squigl`.
#[tauri::command]
fn save_dir() -> String {
    squigl_engine::paths::pictures_dir()
        .to_string_lossy()
        .into_owned()
}

/// A reading as text and maths runs, the maths as MathML (`squigl_engine::math`).
#[tauri::command]
fn math_parts(text: String) -> Vec<squigl_engine::math::Part> {
    squigl_engine::math::parts(&text)
}

/// The backends, detector and voice: the HTTP backends always, the built-in models
/// when built with them (prepared only on request: the person agrees to the
/// download), the system's speech when built with it.
fn engine_deps(fake_voice: Option<dev_voice::DevSpoken>) -> EngineDeps {
    EngineDeps {
        voice: match fake_voice {
            Some(spoken) => {
                Box::new(move |wake| Some(Box::new(dev_voice::FakeVoice::new(spoken, wake)) as _))
            }
            None => Box::new(voice),
        },
        ..model_deps()
    }
}

/// What `--dev-fake-voice` was given to say, in order (empty without it).
#[tauri::command]
fn dev_spoken(spoken: State<'_, dev_voice::DevSpoken>) -> Vec<String> {
    spoken.0.lock().unwrap().clone()
}

/// Reading aloud, if this build has it: Kokoro (with the built-in models; its
/// model downloads only when the person agrees, like the others) and the system's
/// speech.
fn voice(wake: squigl_engine::engine::Waker) -> Option<Box<dyn squigl_engine::speech::Voice>> {
    #[cfg(feature = "speech")]
    {
        #[cfg(feature = "local-model")]
        let kokoro = Some(std::sync::Arc::new(
            squigl_models::kokoro::KokoroService::new(&squigl_engine::model::ModelContext {
                eager: false,
                notify: wake.clone(),
            }),
        ));
        match squigl_speech::Voices::start(
            wake,
            #[cfg(feature = "local-model")]
            kokoro,
        ) {
            Ok(v) => {
                log::info!("reading aloud ready");
                return Some(Box::new(v));
            }
            Err(e) => log::warn!("no reading aloud: {e:#}"),
        }
    }
    #[cfg(not(feature = "speech"))]
    let _ = wake;
    None
}

fn model_deps() -> EngineDeps {
    #[cfg(feature = "local-model")]
    {
        use squigl_engine::transcribe::BackendConfig;
        EngineDeps {
            backends: Box::new(|backend, ctx| match backend {
                BackendConfig::Local {
                    name,
                    device,
                    max_tokens,
                    max_image_tokens,
                } => Some(Box::new(squigl_models::LocalBackend::new(
                    name.clone(),
                    *device,
                    *max_tokens,
                    *max_image_tokens,
                    ctx,
                ))),
                other => other.build(),
            }),
            detector: Box::new(|layout, ctx| {
                layout.enabled.then(|| {
                    std::sync::Arc::new(squigl_models::layout::LayoutService::new(
                        layout.device,
                        layout.threshold,
                        ctx,
                    )) as _
                })
            }),
            voice: Box::new(|_| None),
        }
    }
    #[cfg(not(feature = "local-model"))]
    EngineDeps::remote_only()
}

#[tauri::command]
fn lut(host: State<'_, Host>) -> Result<LutReply, String> {
    host.lut().ok_or_else(|| "the engine has stopped".into())
}

fn parse_size(s: &str) -> Result<(f64, f64), String> {
    let (w, h) = s.split_once('x').ok_or("expected WxH")?;
    let num = |v: &str| v.parse::<f64>().map_err(|e| format!("{v}: {e}"));
    Ok((num(w)?, num(h)?))
}

/// WebView2's `WEBVIEW2_USER_DATA_FOLDER` and `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`,
/// applied to the window. wry sets its own data folder and arguments, and with
/// them in place msedgedriver (WebDriver, for the end-to-end tests), which passes
/// its remote-debugging switch and scoped folder in these variables, never finds
/// the browser's DevTools port. Elsewhere, and when they are unset, nothing changes.
fn webview2_from_environment<R: tauri::Runtime, M: Manager<R>>(
    builder: tauri::WebviewWindowBuilder<'_, R, M>,
) -> tauri::WebviewWindowBuilder<'_, R, M> {
    if !cfg!(windows) {
        return builder;
    }
    let mut builder = builder;
    if let Some(folder) = std::env::var_os("WEBVIEW2_USER_DATA_FOLDER") {
        builder = builder.data_directory(PathBuf::from(folder));
    }
    if let Ok(extra) = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") {
        // wry's own defaults, which giving any arguments replaces.
        let args =
            format!("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection {extra}");
        builder = builder.additional_browser_args(&args);
    }
    builder
}

/// The OS's text size as a zoom for the page. Windows keeps it apart from the
/// display scale (Settings > Accessibility > Text size, 100-225 %) and WebView2 does
/// not apply it. WebKitGTK already follows GNOME's text scaling, which arrives as the
/// device pixel ratio; macOS has no system-wide text size.
fn os_text_scale() -> f64 {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        let percent: Option<u32> = winreg::RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Microsoft\Accessibility")
            .and_then(|key| key.get_value("TextScaleFactor"))
            .ok();
        if let Some(percent) = percent {
            return f64::from(percent.clamp(100, 225)) / 100.0;
        }
    }
    1.0
}

/// The command line. On Windows an argument the app does not know is logged and
/// skipped rather than fatal: a release build has no console to show clap's error
/// in, and msedgedriver (WebDriver, for the end-to-end tests) starts the app with
/// Chromium's switches (`--remote-debugging-port=0`, `--user-data-dir=...`).
fn parse_args() -> Args {
    if cfg!(windows) {
        parse_lenient(std::env::args().collect()).unwrap_or_else(|e| e.exit())
    } else {
        Args::parse()
    }
}

/// Parses `argv`, dropping (with a warning) each argument clap does not know.
fn parse_lenient(mut argv: Vec<String>) -> Result<Args, clap::Error> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    loop {
        let e = match Args::try_parse_from(&argv) {
            Ok(args) => return Ok(args),
            Err(e) if e.kind() == ErrorKind::UnknownArgument => e,
            Err(e) => return Err(e),
        };
        let Some(ContextValue::String(bad)) = e.get(ContextKind::InvalidArg) else {
            return Err(e);
        };
        // Reported as `--name` even when given as `--name=value`.
        let name = bad.split('=').next().unwrap_or(bad);
        let Some(at) = argv
            .iter()
            .position(|a| a == name || a.starts_with(&format!("{name}=")))
        else {
            return Err(e);
        };
        log::warn!("ignoring the unknown argument {}", argv[at]);
        argv.remove(at);
    }
}

/// Logging to stderr, or to the file `SQUIGL_LOG_FILE` names (appended): a release
/// build on Windows has no console, so its log is otherwise lost. Panics are logged.
fn init_logging() {
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if let Some(path) = std::env::var_os("SQUIGL_LOG_FILE") {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(file) => {
                builder.target(env_logger::Target::Pipe(Box::new(file)));
            }
            Err(e) => eprintln!("cannot log to {}: {e}", path.to_string_lossy()),
        }
    }
    builder.init();
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("{info}");
        default(info);
    }));
}

fn main() -> anyhow::Result<()> {
    init_logging();
    // What WebView2 is told from outside (msedgedriver passes its settings so).
    for (name, value) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("WEBVIEW2_") {
            log::info!("{} = {}", name.to_string_lossy(), value.to_string_lossy());
        }
    }
    let args = parse_args();
    let dev = DevOptions {
        keys: args
            .dev_keys
            .as_deref()
            .map(|k| k.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        snapshot_after_ms: args.dev_snapshot_after.map(|s| (s * 1000.0) as u64),
        stats: args.dev_stats,
        canvas2d: args.dev_canvas2d,
        probe: args.dev_probe,
        snapshot_path: args.dev_snapshot_path,
    };
    let source = match (args.open, args.replay) {
        (Some(path), _) => SourceSpec::Image(path),
        (None, Some(path)) => SourceSpec::Replay(path),
        (None, None) if args.test_pattern => SourceSpec::TestPattern,
        (None, None) => SourceSpec::Phone,
    };
    let (mut config, config_file) = match args.dev_config {
        Some(file) => (Config::default(), file),
        None => (
            Config::load_or_create().unwrap_or_else(|e| {
                log::error!("{e:#}; using the defaults");
                Config::default()
            }),
            Config::path(),
        ),
    };
    if let Some(base_url) = args.dev_backend {
        config.backends = vec![squigl_engine::transcribe::BackendConfig::OpenAi {
            name: "test backend".into(),
            base_url,
            model: "test".into(),
            api_key: None,
            samples: 1,
            temperature: 0.0,
            max_tokens: 256,
        }];
    }
    let stream = StreamConfig {
        source,
        options: ConnectOptions {
            serial: args.serial,
            tcp_address: args.connect,
            facing: Facing::Back,
            control: true,
            ..ConnectOptions::default()
        },
        // A magnifier wants the most pixels the decoder can take.
        resolution: Resolution::Max,
        tee_device: None,
    };
    let stream_decoder = stream.options.decoder;
    let pairing = Pairing::new(
        args.dev_pairing_bind,
        stream.options.decoder,
        config_file
            .parent()
            .map_or_else(squigl_engine::paths::config_dir, Into::into),
    );
    // The magnifier needs no models yet; reading arrives in a later phase.
    let spoken = dev_voice::DevSpoken::default();
    let fake_voice = args.dev_fake_voice.then(|| spoken.clone());
    let host = Host::start(
        config,
        stream,
        move || engine_deps(fake_voice),
        EngineOptions {
            config_file: Some(config_file),
            ..EngineOptions::default()
        },
    );
    let dev_phone = if args.dev_fake_phone {
        pair_fake_phone(&host, stream_decoder)
    } else {
        DevPhone::default()
    };
    let frames = transport::start(host.clone())?;
    log::info!("frames on ws://127.0.0.1:{}", frames.port);

    let text_scale = args.dev_text_scale.unwrap_or_else(os_text_scale);
    let window_size = args.dev_window_size;
    let app = tauri::Builder::default()
        .setup(move |app| {
            // The window is made here, not from the config, so that WebView2's own
            // settings can be honoured (see `webview2_from_environment`).
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("tauri.conf.json has no main window"))?;
            let mut builder = tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?;
            if let Some((w, h)) = window_size {
                builder = builder.maximized(false).inner_size(w, h);
            }
            let window = webview2_from_environment(builder).build()?;
            if text_scale != 1.0 {
                log::info!("text scale {text_scale}: zooming the page");
                window.set_zoom(text_scale)?;
            }
            Ok(())
        })
        .on_page_load(|webview, payload| {
            log::debug!(
                "page {:?}: {}",
                payload.event(),
                webview.url().map(|u| u.to_string()).unwrap_or_default()
            );
        })
        .manage(host)
        .manage(frames)
        .manage(pairing)
        .manage(dev)
        .manage(dev_phone)
        .manage(spoken)
        .invoke_handler(tauri::generate_handler![
            dispatch,
            subscribe,
            endpoint,
            lut,
            open_image,
            pairing_start,
            pairing_stop,
            page_log,
            save_dir,
            math_parts,
            dev_options,
            dev_save_snapshot,
            dev_reference,
            dev_phone_camera,
            dev_spoken
        ])
        .build(tauri::generate_context!())?;
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            handle.state::<Host>().stop();
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("squigl-desktop")
            .chain(args.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn unknown_switches_are_skipped_and_known_ones_kept() {
        let args = parse_lenient(argv(&[
            "--remote-debugging-port=0",
            "--replay",
            "a.sqrec",
            "--no-first-run",
            "--user-data-dir=C:\\x",
            "--dev-probe",
            "data:,",
        ]))
        .unwrap();
        assert_eq!(args.replay, Some(PathBuf::from("a.sqrec")));
        assert!(args.dev_probe);
    }

    #[test]
    fn other_mistakes_still_fail() {
        assert!(parse_lenient(argv(&["--replay"])).is_err());
    }
}
