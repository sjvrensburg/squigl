//! Roadmap spike S1: frames from Rust into a Tauri webview, three ways -- a custom
//! URI scheme, an IPC response, a localhost WebSocket -- pulled by the page, drawn
//! with WebGL2, timed. See ../README.md.
//!
//! Configured by environment variables, so `run.sh` can sweep them:
//! `S1_TRANSPORT` (scheme | ipc | ws), `S1_BYTES` (frame size; the luma plane of a
//! frame about that big), `S1_SECONDS` (measured time, after a 2 s warm-up),
//! `S1_INFLIGHT` (requests the page keeps open, 1 or 2), `S1_FPS` (the rate the page
//! asks at; 0, the default, as fast as it can), `S1_OUT` (where the result JSON goes;
//! it is also printed).

use serde::Serialize;
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Header: magic, frame number, send time (Unix ms, f64), width, height.
const HEADER: usize = 32;
const MAGIC: &[u8; 8] = b"S1FRAME1";

struct Config {
    transport: String,
    bytes: usize,
    seconds: f64,
    inflight: u32,
    fps: f64,
    out: Option<String>,
}

impl Config {
    fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Self {
            transport: var("S1_TRANSPORT").unwrap_or_else(|| "ws".into()),
            bytes: var("S1_BYTES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(3_000_000),
            seconds: var("S1_SECONDS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10.0),
            inflight: var("S1_INFLIGHT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            fps: var("S1_FPS").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            out: var("S1_OUT"),
        }
    }
}

/// A synthetic luma source: a fixed gradient, with the frame number burned into the
/// top rows as 32 black or white squares and a bar that moves with it.
struct Source {
    width: usize,
    height: usize,
    base: Vec<u8>,
    seq: AtomicU64,
}

/// The square's edge, in pixels, of each burned-in bit.
const BIT: usize = 16;

impl Source {
    fn new(bytes: usize) -> Self {
        // 4:3, rounded to 16.
        let width = (((bytes as f64 * 4.0 / 3.0).sqrt() as usize) / 16 * 16).max(32 * BIT);
        let height = (bytes / width / 16 * 16).max(BIT * 2);
        let base = (0..height)
            .flat_map(|y| (0..width).map(move |x| (16 + (x + y) % 220) as u8))
            .collect();
        Self {
            width,
            height,
            base,
            seq: AtomicU64::new(0),
        }
    }

    fn next(&self) -> Vec<u8> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let (w, h) = (self.width, self.height);
        let mut out = Vec::with_capacity(HEADER + w * h);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&seq.to_le_bytes());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            * 1000.0;
        out.extend_from_slice(&now.to_le_bytes());
        out.extend_from_slice(&(w as u32).to_le_bytes());
        out.extend_from_slice(&(h as u32).to_le_bytes());
        out.extend_from_slice(&self.base);
        let plane = &mut out[HEADER..];
        for bit in 0..32 {
            let v = if seq >> bit & 1 == 1 { 235 } else { 16 };
            for y in 0..BIT {
                let row = &mut plane[y * w + bit * BIT..][..BIT];
                row.fill(v);
            }
        }
        let bar = (seq as usize * 8) % w;
        for y in BIT..h {
            plane[y * w + bar] = 235;
        }
        out
    }
}

#[derive(Serialize)]
struct Setup {
    transport: String,
    seconds: f64,
    inflight: u32,
    fps: f64,
    width: usize,
    height: usize,
    bytes: usize,
    ws_port: u16,
}

struct App {
    config: Config,
    source: Arc<Source>,
    ws_port: u16,
    started: Instant,
    cpu_at_start: std::sync::Mutex<Option<(f64, Instant)>>,
}

#[tauri::command]
fn setup(app: tauri::State<'_, App>) -> Setup {
    Setup {
        transport: app.config.transport.clone(),
        seconds: app.config.seconds,
        inflight: app.config.inflight,
        fps: app.config.fps,
        width: app.source.width,
        height: app.source.height,
        bytes: HEADER + app.source.width * app.source.height,
        ws_port: app.ws_port,
    }
}

#[tauri::command]
fn next_frame(app: tauri::State<'_, App>) -> tauri::ipc::Response {
    tauri::ipc::Response::new(app.source.next())
}

/// The page says the measured period starts: note the CPU time so far.
#[tauri::command]
fn measure_start(app: tauri::State<'_, App>) {
    *app.cpu_at_start.lock().unwrap() = Some((cpu_seconds(), Instant::now()));
}

/// The page's measurements: written out with the CPU used meanwhile, then exit.
#[tauri::command]
fn report(app: tauri::State<'_, App>, handle: tauri::AppHandle, result: serde_json::Value) {
    let mut result = result;
    if let Some((cpu0, t0)) = *app.cpu_at_start.lock().unwrap() {
        let cores = (cpu_seconds() - cpu0) / t0.elapsed().as_secs_f64();
        result["cpu_cores"] = serde_json::json!((cores * 100.0).round() / 100.0);
    }
    result["wall_s"] = serde_json::json!(app.started.elapsed().as_secs_f64());
    let line = result.to_string();
    println!("{line}");
    if let Some(path) = &app.config.out {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("opening S1_OUT");
        writeln!(f, "{line}").unwrap();
    }
    handle.exit(0);
}

/// CPU seconds used by this process and every descendant (WebKit's web and network
/// processes), from /proc. Linux only; 0 elsewhere.
fn cpu_seconds() -> f64 {
    #[cfg(target_os = "linux")]
    {
        let ticks = 100.0; // USER_HZ
        let me = std::process::id();
        let mut parents = std::collections::HashMap::new();
        let mut cpu = std::collections::HashMap::new();
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // Fields after the parenthesised command name.
            let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
                continue;
            };
            let f: Vec<&str> = rest.split_whitespace().collect();
            let (Some(ppid), Some(ut), Some(st)) = (f.get(1), f.get(11), f.get(12)) else {
                continue;
            };
            parents.insert(pid, ppid.parse::<u32>().unwrap_or(0));
            cpu.insert(
                pid,
                (ut.parse::<f64>().unwrap_or(0.0) + st.parse::<f64>().unwrap_or(0.0)) / ticks,
            );
        }
        let descends = |mut pid: u32| loop {
            if pid == me {
                return true;
            }
            match parents.get(&pid) {
                Some(&p) if p != 0 && p != pid => pid = p,
                _ => return false,
            }
        };
        cpu.iter()
            .filter(|(&pid, _)| descends(pid))
            .map(|(_, s)| s)
            .sum()
    }
    #[cfg(not(target_os = "linux"))]
    {
        0.0
    }
}

/// One client at a time: each text message asks for a frame, answered in order.
fn serve_ws(listener: TcpListener, source: Arc<Source>) {
    for stream in listener.incoming().flatten() {
        let source = Arc::clone(&source);
        std::thread::spawn(move || {
            stream.set_nodelay(true).ok();
            let Ok(mut ws) = tungstenite::accept(stream) else {
                return;
            };
            while let Ok(msg) = ws.read() {
                if msg.is_close() {
                    break;
                }
                if msg.is_text() && ws.send(source.next().into()).is_err() {
                    break;
                }
            }
        });
    }
}

fn main() {
    let config = Config::from_env();
    let source = Arc::new(Source::new(config.bytes));
    let listener = TcpListener::bind("127.0.0.1:0").expect("binding the WebSocket");
    let ws_port = listener.local_addr().unwrap().port();
    {
        let source = Arc::clone(&source);
        std::thread::spawn(move || serve_ws(listener, source));
    }
    let scheme_source = Arc::clone(&source);
    tauri::Builder::default()
        .manage(App {
            config,
            source,
            ws_port,
            started: Instant::now(),
            cpu_at_start: std::sync::Mutex::new(None),
        })
        .register_asynchronous_uri_scheme_protocol("frame", move |_ctx, _request, responder| {
            let source = Arc::clone(&scheme_source);
            std::thread::spawn(move || {
                let response = tauri::http::Response::builder()
                    .header("Content-Type", "application/octet-stream")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(source.next())
                    .unwrap();
                responder.respond(response);
            });
        })
        .invoke_handler(tauri::generate_handler![
            setup,
            next_frame,
            measure_start,
            report
        ])
        .run(tauri::generate_context!())
        .expect("running the spike");
}
