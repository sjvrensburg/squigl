//! squigl-desktop: Squigl's desktop app, a Tauri 2 window over `squigl-engine`. The
//! engine runs on its own thread ([`host`]); the web UI drives it with
//! [`dispatch`]ed commands, follows it through the events [`subscribe`] streams,
//! and pulls frames over a localhost WebSocket ([`transport`]).

// A window, not a console program: on Windows a release build opens no console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod host;
mod transport;

use clap::Parser;
use host::{Host, LutReply};
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
}

/// The development aids the page acts on.
#[derive(Debug, Clone, Default, serde::Serialize)]
struct DevOptions {
    keys: Vec<String>,
    snapshot_after_ms: Option<u64>,
    stats: bool,
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

#[tauri::command]
fn lut(host: State<'_, Host>) -> Result<LutReply, String> {
    host.lut().ok_or_else(|| "the engine has stopped".into())
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let dev = DevOptions {
        keys: args
            .dev_keys
            .as_deref()
            .map(|k| k.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        snapshot_after_ms: args.dev_snapshot_after.map(|s| (s * 1000.0) as u64),
        stats: args.dev_stats,
        snapshot_path: args.dev_snapshot_path,
    };
    let source = match (args.open, args.replay) {
        (Some(path), _) => SourceSpec::Image(path),
        (None, Some(path)) => SourceSpec::Replay(path),
        (None, None) if args.test_pattern => SourceSpec::TestPattern,
        (None, None) => SourceSpec::Phone,
    };
    let config = Config::load_or_create().unwrap_or_else(|e| {
        log::error!("{e:#}; using the defaults");
        Config::default()
    });
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
    // The magnifier needs no models yet; reading arrives in a later phase.
    let host = Host::start(
        config,
        stream,
        EngineDeps::remote_only,
        EngineOptions::default(),
    );
    let frames = transport::start(host.clone())?;
    log::info!("frames on ws://127.0.0.1:{}", frames.port);

    let app = tauri::Builder::default()
        .on_page_load(|webview, payload| {
            log::debug!(
                "page {:?}: {}",
                payload.event(),
                webview.url().map(|u| u.to_string()).unwrap_or_default()
            );
        })
        .manage(host)
        .manage(frames)
        .manage(dev)
        .invoke_handler(tauri::generate_handler![
            dispatch,
            subscribe,
            endpoint,
            lut,
            page_log,
            dev_options,
            dev_save_snapshot
        ])
        .build(tauri::generate_context!())?;
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            handle.state::<Host>().stop();
        }
    });
    Ok(())
}
