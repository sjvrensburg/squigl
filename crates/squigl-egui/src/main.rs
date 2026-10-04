//! squigl: use an Android phone as a document camera on the desktop -- live view,
//! drag a region to zoom, capture, save. The phone side and the decode pipeline are
//! the `squigl-core` library; this crate is the window and the reconnect policy.

// A window, not a console program: on Windows a release build opens no console
// beside it. (A debug build keeps one, for the log.)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod panes;
mod settings;

use anyhow::{Context, Result};
use clap::Parser;
use squigl_core::cameras::is_usable_size;
use squigl_core::convert::Rotation;
use squigl_core::decode::Backend;
use squigl_core::{ConnectOptions, Facing};
use squigl_engine::engine::{BackendFactory, DetectorFactory, Engine, EngineDeps, EngineOptions};
use squigl_engine::erase;
use squigl_engine::geometry::Crop;
use squigl_engine::stream::{Resolution, SourceSpec, StreamConfig};
use squigl_engine::transcribe::BackendConfig;
use std::path::PathBuf;

/// Live view, crop and capture an Android phone's camera.
#[derive(Parser, Debug)]
#[command(name = "squigl", version)]
struct Args {
    /// ADB serial of the device to use (autodetected if omitted and only one is attached).
    #[arg(long)]
    serial: Option<String>,

    /// Use the phone over Wi-Fi: `adb connect` to HOST[:PORT] instead of USB. See
    /// `squigl-cli --tcpip` for the one-time switch.
    #[arg(long, value_name = "HOST[:PORT]", conflicts_with = "serial")]
    connect: Option<String>,

    /// Which camera to start with (switchable in the window).
    #[arg(long, value_enum, default_value = "back")]
    facing: FacingArg,

    /// Capture resolution, e.g. 1280x720, or `max` for the largest the camera offers
    /// that the decoder can handle. Defaults to `max`: this is a document camera.
    #[arg(long, default_value = "max")]
    resolution: String,

    /// H.264 decoder; `ffmpeg` (if compiled in) lifts openh264's ~3840x2160 ceiling.
    #[arg(long, value_enum, default_value_t = DecoderArg::default())]
    decoder: DecoderArg,

    /// Requested max frame rate.
    #[arg(long)]
    fps: Option<u32>,

    /// H.264 bitrate in megabits per second.
    #[arg(long, default_value_t = 30)]
    bitrate: u32,

    /// Also write every frame to this v4l2loopback device (e.g. /dev/video10), so the
    /// same stream is a webcam for other apps while the window is open.
    #[cfg(target_os = "linux")]
    #[arg(long, value_name = "/dev/videoN")]
    device: Option<PathBuf>,

    /// Open an image (a scanned or photographed page) instead of using the phone.
    /// Dropping a file on the window does the same; "Use phone" goes back.
    #[arg(long, value_name = "FILE")]
    open: Option<PathBuf>,

    /// Play a recording made with `squigl-cli --record`, looping, instead of using a
    /// phone: for demos, and for trying the window with no phone attached.
    #[arg(long, value_name = "FILE", conflicts_with = "open")]
    replay: Option<PathBuf>,

    /// How to draw: `glow` (OpenGL, the default on Linux) or `wgpu` (the default on
    /// Windows and macOS). For a machine where the default does not work.
    #[arg(long, hide = true, value_enum)]
    renderer: Option<RendererArg>,

    /// Development aid: start on synthetic colour bars instead of the phone.
    #[arg(long, hide = true, conflicts_with_all = ["open", "replay"])]
    test_pattern: bool,

    /// Camera zoom ratio at startup (the phone's own zoom; a slider in the window
    /// changes it later).
    #[arg(long)]
    zoom: Option<f32>,

    /// Turn the picture clockwise by this many degrees at startup (a phone on a
    /// stand is usually mounted sideways). Also changeable in the window.
    #[arg(long, value_parser = ["0", "90", "180", "270"], default_value = "0")]
    rotate: String,

    /// Development aid: start with this crop selected, in view pixels.
    #[arg(long, hide = true, value_name = "X,Y,W,H")]
    dev_crop: Option<String>,

    /// Development aid: capture the first frame and erase a straight brush stroke on
    /// it, from X1,Y1 to X2,Y2 with radius R, in view pixels, as painting over the
    /// Zoom pane does. Repeatable.
    #[arg(long, hide = true, value_name = "X1,Y1,X2,Y2,R")]
    dev_erase: Vec<String>,

    /// Download the built-in models (transcription and block detection) into
    /// DIR/<model name>/ (verified against the checksums compiled into this binary)
    /// and exit. For packaging, or for a machine that is offline later: a `models/`
    /// directory next to the executable is used without any download.
    #[cfg(feature = "local-model")]
    #[arg(long, value_name = "DIR")]
    fetch_model: Option<PathBuf>,

    /// Where captures are saved. Defaults to ~/Pictures/squigl.
    #[arg(long)]
    save_dir: Option<PathBuf>,

    /// Development aid: after this many seconds, write a PNG screenshot of the window
    /// to --screenshot-path and exit.
    #[arg(long, hide = true, requires = "screenshot_path")]
    screenshot_after: Option<f32>,

    #[arg(long, hide = true)]
    screenshot_path: Option<PathBuf>,

    /// Development aid: read the crop (or page) with the first backend as soon as a
    /// frame arrives.
    #[arg(long, hide = true)]
    dev_read: bool,

    /// Development aid: with --dev-read, ask the next backend too once the first
    /// answer is in.
    #[arg(long, hide = true, requires = "dev_read")]
    dev_second: bool,

    /// Development aid: start with the Settings window open.
    #[arg(long, hide = true)]
    dev_settings: bool,

    /// Development aid: detect blocks as soon as the detector and a frame are ready.
    #[arg(long, hide = true)]
    dev_detect: bool,

    /// Development aid: with --dev-detect, read every block with the first backend
    /// once they are found.
    #[arg(long, hide = true, requires = "dev_detect")]
    dev_read_all: bool,

    /// Development aid: 3 s after the first frame, set this zoom over the control
    /// channel (the live path, as the slider does).
    #[arg(long, hide = true)]
    dev_zoom: Option<f32>,
}

/// How the window draws: OpenGL (`glow`) or wgpu (Direct3D 12, Metal, Vulkan).
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum RendererArg {
    Glow,
    Wgpu,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum FacingArg {
    Front,
    Back,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum DecoderArg {
    Openh264,
    #[cfg(feature = "ffmpeg")]
    Ffmpeg,
}

impl Default for DecoderArg {
    fn default() -> Self {
        match Backend::default() {
            Backend::Openh264 => DecoderArg::Openh264,
            #[cfg(feature = "ffmpeg")]
            Backend::Ffmpeg => DecoderArg::Ffmpeg,
        }
    }
}

impl From<DecoderArg> for Backend {
    fn from(d: DecoderArg) -> Self {
        match d {
            DecoderArg::Openh264 => Backend::Openh264,
            #[cfg(feature = "ffmpeg")]
            DecoderArg::Ffmpeg => Backend::Ffmpeg,
        }
    }
}

fn main() -> Result<()> {
    let result = run();
    if let Err(e) = &result {
        fatal(&format!("{e:#}"));
    }
    result
}

/// Says why the window could not start. On Windows a release build has no console
/// (`windows_subsystem`), so the message would otherwise go nowhere.
fn fatal(message: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (text, title) = (
            wide(&format!("Squigl could not start:\n\n{message}")),
            wide("Squigl"),
        );
        // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    let _ = message; // printed by `main`'s returned error
}

fn run() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,ort=warn"))
        .init();
    let args = Args::parse();

    #[cfg(feature = "local-model")]
    if let Some(dir) = &args.fetch_model {
        for spec in squigl_models::models::ALL {
            let target = dir.join(spec.name);
            // Each whole per cent once.
            let last = std::cell::Cell::new(None);
            let report = |p: squigl_engine::model::ModelPhase| {
                let line = p.describe();
                if last.replace(Some(line.clone())).as_ref() != Some(&line) {
                    eprintln!("{}: {line}", spec.name);
                }
            };
            spec.download_into(&target, &report, &Default::default())?;
            println!("{}", target.display());
        }
        return Ok(());
    }

    let decoder = Backend::from(args.decoder);
    let resolution = match args.resolution.as_str() {
        "max" => Resolution::Max,
        "default" => Resolution::PhoneDefault,
        s => {
            let (w, h) = s
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                .with_context(|| {
                    format!("--resolution {s:?} must be WIDTHxHEIGHT, max or default")
                })?;
            anyhow::ensure!(
                is_usable_size(w, h, decoder),
                "{w}x{h} is not usable with the {} decoder (see `squigl-cli --list-sizes`)",
                decoder.name()
            );
            Resolution::Fixed(w, h)
        }
    };
    let source = match (args.open, args.replay) {
        (Some(path), _) => SourceSpec::Image(path),
        (None, Some(path)) => SourceSpec::Replay(path),
        (None, None) if args.test_pattern => SourceSpec::TestPattern,
        (None, None) => SourceSpec::Phone,
    };
    let config = StreamConfig {
        source,
        options: ConnectOptions {
            serial: args.serial,
            tcp_address: args.connect,
            facing: match args.facing {
                FacingArg::Back => Facing::Back,
                FacingArg::Front => Facing::Front,
            },
            resolution: None,
            max_fps: args.fps,
            bitrate_bps: Some(args.bitrate.saturating_mul(1_000_000)),
            decoder,
            zoom: args.zoom.filter(|&z| z > 1.0),
            torch: false,
            control: true,
        },
        resolution,
        #[cfg(target_os = "linux")]
        tee_device: args.device,
        #[cfg(not(target_os = "linux"))]
        tee_device: None,
    };
    let save_dir = args
        .save_dir
        .unwrap_or_else(squigl_engine::paths::pictures_dir);

    let rotation = match args.rotate.as_str() {
        "90" => Rotation::Cw90,
        "180" => Rotation::Cw180,
        "270" => Rotation::Cw270,
        _ => Rotation::None,
    };
    let dev_crop = args.dev_crop.as_deref().map(parse_crop).transpose()?;
    let dev_erase = args
        .dev_erase
        .iter()
        .map(|s| {
            let v: Vec<f32> = s
                .split(',')
                .map(|n| n.trim().parse())
                .collect::<Result<_, _>>()?;
            anyhow::ensure!(v.len() == 5, "--dev-erase wants X1,Y1,X2,Y2,R, got {s:?}");
            Ok(erase::Stroke::line([v[0], v[1]], [v[2], v[3]], v[4]))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let dev_read = args.dev_read;
    let dev_second = args.dev_second;
    let dev_detect = args.dev_detect;
    let dev_settings = args.dev_settings;
    let dev_read_all = args.dev_read_all;
    let dev_zoom = args.dev_zoom;
    let screenshot = args
        .screenshot_after
        .zip(args.screenshot_path)
        .map(|(secs, path)| (std::time::Duration::from_secs_f32(secs), path));

    let gui_config = squigl_engine::config::Config::load_or_create().unwrap_or_else(|e| {
        log::error!("{e:#}; no transcription backends available");
        squigl_engine::config::Config {
            backends: Vec::new(),
            ..Default::default()
        }
    });
    // The engine builds the HTTP backends; the built-in model is this binary's.
    let backend_factory: BackendFactory = Box::new(|backend, ctx| match backend {
        #[cfg(feature = "local-model")]
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
        #[cfg(not(feature = "local-model"))]
        BackendConfig::Local { name, .. } => {
            let _ = ctx; // what a built-in model would be prepared with
            log::warn!("backend {name:?} needs a build with the local-model feature");
            None
        }
        other => other.build(),
    });
    #[cfg(feature = "local-model")]
    let detector_factory: DetectorFactory = Box::new(|layout, ctx| {
        layout.enabled.then(|| {
            std::sync::Arc::new(squigl_models::layout::LayoutService::new(
                layout.device,
                layout.threshold,
                ctx,
            )) as _
        })
    });
    #[cfg(not(feature = "local-model"))]
    let detector_factory: DetectorFactory = Box::new(|_, _| None);
    #[cfg(feature = "math")]
    let typesetter: Option<std::sync::Arc<dyn squigl_engine::typeset::Typesetter>> =
        Some(std::sync::Arc::new(squigl_math::Renderer::new()));
    #[cfg(not(feature = "math"))]
    let typesetter: Option<std::sync::Arc<dyn squigl_engine::typeset::Typesetter>> = None;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Squigl")
            .with_inner_size([1400.0, 800.0]),
        // A detached pane is an immediate viewport, painted and swapped inside the
        // main window's pass: with vsync every swap waits for a refresh, so each
        // detached window divided the frame rate. egui only repaints on request (a
        // camera frame, input), so without vsync it does not spin.
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: false,
            ..Default::default()
        },
        wgpu_options: eframe::WgpuConfiguration {
            surface: eframe::SurfaceConfig {
                present_mode: eframe::wgpu::PresentMode::AutoNoVsync,
                ..eframe::SurfaceConfig::LOW_LATENCY
            },
            ..Default::default()
        },
        // OpenGL where it is at home. On Windows a machine without a proper GPU
        // driver (a VM, a basic display adapter) offers OpenGL 1.1, which egui_glow
        // refuses; wgpu's Direct3D 12 falls back to Microsoft's software rasteriser
        // there. On macOS OpenGL is deprecated; wgpu draws with Metal.
        renderer: match args.renderer {
            Some(RendererArg::Glow) => eframe::Renderer::Glow,
            Some(RendererArg::Wgpu) => eframe::Renderer::Wgpu,
            None if cfg!(any(windows, target_os = "macos")) => eframe::Renderer::Wgpu,
            None => eframe::Renderer::Glow,
        },
        ..Default::default()
    };
    eframe::run_native(
        "squigl",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let engine = Engine::new(
                gui_config,
                config,
                EngineDeps {
                    backends: backend_factory,
                    detector: detector_factory,
                },
                // The window loads the models at once, as it always has.
                EngineOptions {
                    eager_models: true,
                    ..EngineOptions::default()
                },
                std::sync::Arc::new(move || ctx.request_repaint()),
            );
            let mut app = app::App::new(engine, save_dir, typesetter, screenshot);
            app.set_rotation(rotation);
            app.set_crop(dev_crop);
            app.set_dev_erase(dev_erase);
            app.set_dev_read(dev_read, dev_second);
            app.set_dev_detect(dev_detect, dev_read_all);
            app.set_settings_open(dev_settings);
            app.set_dev_zoom(dev_zoom);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

/// `X,Y,W,H` in view pixels, for the development flags.
fn parse_crop(s: &str) -> anyhow::Result<Crop> {
    let v: Vec<usize> = s
        .split(',')
        .map(|n| n.trim().parse())
        .collect::<Result<_, _>>()?;
    anyhow::ensure!(v.len() == 4, "wanted X,Y,W,H, got {s:?}");
    Ok(Crop {
        x: v[0],
        y: v[1],
        w: v[2],
        h: v[3],
    })
}
