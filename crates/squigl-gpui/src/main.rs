//! squigl-gpui: Squigl's desktop app as a native GPUI window over
//! `squigl-engine` -- GPU-composited (Vulkan on Linux), no webview, no IPC,
//! no frame socket. The engine runs on the window thread; its workers call
//! the waker, which the window's pump loop collects.
//!
//! Linux only. System packages (Fedora names): `libxkbcommon-x11-devel`,
//! `wayland-devel`, `vulkan-loader-devel`, `fontconfig-devel` to build;
//! `libspeechd-devel` + `alsa-lib-devel` for the `speech` feature.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod detach;
mod dev_voice;
mod frame;
mod guidance;
mod interact;
mod pairing;
mod render;
mod zoom;

use app::AppView;
use clap::Parser;
use gpui_kit::AppContext;
use pairing::Pairing;
use squigl_core::{ConnectOptions, Facing};
use squigl_engine::config::Config;
use squigl_engine::engine::{Engine, EngineDeps, EngineOptions};
use squigl_engine::stream::{Resolution, SourceSpec, StreamConfig};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// The phone (or a file) as a magnifier, with handwriting reading.
#[derive(Parser, Debug)]
#[command(name = "squigl-gpui", version)]
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
    /// Turn the picture clockwise by this many degrees at startup.
    #[arg(long, value_parser = ["0", "90", "180", "270"], default_value = "0")]
    rotate: String,
    /// Development aid: pair phones at these addresses (comma-separated).
    #[arg(long, hide = true, value_name = "IP,...", value_delimiter = ',')]
    dev_pairing_bind: Vec<std::net::IpAddr>,
    /// Development aid: pair a phone played by str0m (colour bars, zoom+torch).
    #[arg(long, hide = true)]
    dev_fake_phone: bool,
    /// Development aid: start from the default settings and save them to FILE.
    #[arg(long, hide = true, value_name = "FILE")]
    dev_config: Option<PathBuf>,
    /// Development aid: read with one OpenAI-compatible backend at URL.
    #[arg(long, hide = true, value_name = "URL")]
    dev_backend: Option<String>,
    /// Development aid: read aloud with a voice that says nothing.
    #[arg(long, hide = true)]
    dev_fake_voice: bool,
    /// Development aid: pop this pane into its own window at startup.
    #[arg(long, hide = true, value_parser = ["preview", "zoom", "reading"])]
    dev_detach: Option<String>,
}

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

fn voice(wake: squigl_engine::engine::Waker) -> Option<Box<dyn squigl_engine::speech::Voice>> {
    #[cfg(feature = "speech")]
    {
        #[cfg(feature = "local-model")]
        let kokoro = Some(Arc::new(squigl_models::kokoro::KokoroService::new(
            &squigl_engine::model::ModelContext {
                eager: false,
                notify: wake.clone(),
            },
        )));
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
                    Arc::new(squigl_models::layout::LayoutService::new(
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

fn init_logging() {
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if let Some(path) = std::env::var_os("SQUIGL_LOG_FILE") {
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
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
    let args = Args::parse();
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
        resolution: Resolution::Max,
        tee_device: None,
    };
    let decoder = stream.options.decoder;
    let pairing = Arc::new(Pairing::new(
        args.dev_pairing_bind,
        decoder,
        config_file
            .parent()
            .map_or_else(squigl_engine::paths::config_dir, Into::into),
    ));
    let spoken = dev_voice::DevSpoken::default();
    let fake_voice = args.dev_fake_voice.then(|| spoken.clone());
    let wake_flag = Arc::new(AtomicBool::new(true));
    let rotation = match args.rotate.as_str() {
        "90" => squigl_core::convert::Rotation::Cw90,
        "180" => squigl_core::convert::Rotation::Cw180,
        "270" => squigl_core::convert::Rotation::Cw270,
        _ => squigl_core::convert::Rotation::None,
    };

    if args.dev_fake_phone {
        let queue = pairing.sessions_queue();
        match squigl_pairing::testing::FakePhone::connect((640, 480), move |offer| {
            let local = std::net::Ipv4Addr::LOCALHOST.into();
            let (session, answer) = squigl_core::WebrtcSource::accept_offer(offer, local, decoder)
                .map_err(|e| e.to_string())?;
            queue.lock().unwrap().push(session);
            Ok(answer)
        }) {
            Ok(phone) => std::mem::forget(phone),
            Err(e) => log::error!("the fake phone did not pair: {e}"),
        }
    }

    let dev_detach = args.dev_detach.clone();
    let wake_for_engine = Arc::clone(&wake_flag);
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets) // full Lucide catalog: the default bundle lacks our toolbar icons
        .run(move |cx| {
            gpui_kit::init(cx);
            let pairing = Arc::clone(&pairing);
            let wake_flag = Arc::clone(&wake_flag);
            let engine = Engine::new(
                config.clone(),
                stream.clone(),
                engine_deps(fake_voice.clone()),
                EngineOptions {
                    eager_models: false,
                    config_file: Some(config_file.clone()),
                },
                Arc::new(move || wake_for_engine.store(true, std::sync::atomic::Ordering::Relaxed)),
            );
            // Startup rotation (the engine starts unrotated).
            // Applied as a command once the view exists, below.
            gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, move |window, cx| {
                cx.new(|cx| {
                    let mut view = AppView::new(engine, pairing, wake_flag, window, cx);
                    if rotation != squigl_core::convert::Rotation::None {
                        view.cmd(squigl_engine::engine::Command::SetRotation { rotation }, cx);
                    }
                    if let Some(pane) = &dev_detach {
                        view.detach_pending = Some(match pane.as_str() {
                            "zoom" => "zoom",
                            "reading" => "reading",
                            _ => "preview",
                        });
                    }
                    if config.magnifier.magnification != 1.0 {
                        view.set_start_magnification(config.magnifier.magnification);
                    }
                    view
                })
            })
            .expect("failed to open window");
        });
    Ok(())
}
