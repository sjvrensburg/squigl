//! The engine host: one thread owns the [`Engine`] (which is single-threaded by
//! design) and serves everything else through one channel -- the window's commands,
//! its event subscription, the frame transport's plane requests -- plus the
//! engine's own wake-ups. After each message it pumps the engine and forwards the
//! events: the stream slice at most every [`STREAM_INTERVAL`] (its zoom and status
//! fields can change at frame rate), everything else at once.

use serde::{Deserialize, Serialize};
use squigl_core::convert::{PlaneFormat, Planes};
use squigl_engine::config::Config;
use squigl_engine::display::{self, Lut};
use squigl_engine::engine::{
    Command, Engine, EngineDeps, EngineOptions, EngineState, Event, Reply, StreamSlice, Versioned,
};
use squigl_engine::geometry::Crop;
use squigl_engine::stream::StreamConfig;
use squigl_engine::view::{FrameRef, Placement, PlaneHeader, Viewport};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::ipc::Channel;

/// The least time between two stream slices sent to the window.
const STREAM_INTERVAL: Duration = Duration::from_millis(250);

/// What the window asks the transport for: its viewport, which frame, which planes.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct FrameRequest {
    pub viewport: Viewport,
    #[serde(default)]
    pub frame: FrameRef,
    #[serde(default)]
    pub format: PlaneFormat,
}

/// What comes back with the planes: what was sampled, and where it goes.
#[derive(Debug, Clone, Serialize)]
pub struct FrameHeader {
    #[serde(flatten)]
    pub planes: PlaneHeader,
    /// The sampled region in view (rotated) space.
    pub view_region: Crop,
    /// The whole view's size.
    pub view: (usize, usize),
    /// Where the viewport puts the view.
    pub placement: Placement,
}

pub struct FrameReply {
    pub header: FrameHeader,
    pub planes: Planes,
}

/// The display mode's table, for the shader.
#[derive(Debug, Clone, Serialize)]
pub struct LutReply {
    /// `tone` (per RGB channel) or `luma` (Y byte to a colour).
    pub kind: &'static str,
    /// 256 RGBA texels.
    pub rgba: Vec<u8>,
}

enum Request {
    Command(Command, Sender<Result<Reply, String>>),
    Subscribe(Channel<Event>),
    Frame(FrameRequest, Sender<Option<FrameReply>>),
    Lut(Sender<LutReply>),
    Reference(usize, usize, Sender<Option<[u8; 3]>>),
    /// A phone that paired over the network, to show.
    Pair(Box<squigl_core::WebrtcSource>),
    Wake,
    /// Stop the engine (ending a phone session cleanly) and the thread; answered
    /// when done.
    Stop(Sender<()>),
}

/// A handle to the host thread; cheap to clone, usable from any thread.
#[derive(Clone)]
pub struct Host {
    tx: Sender<Request>,
}

impl Host {
    /// Starts the host thread, which builds the engine (`deps` is made there: the
    /// engine and its factories stay on that one thread).
    pub fn start(
        config: Config,
        stream: StreamConfig,
        deps: impl FnOnce() -> EngineDeps + Send + 'static,
        options: EngineOptions,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let wake_tx = tx.clone();
        std::thread::Builder::new()
            .name("squigl-engine".into())
            .spawn(move || {
                let wake_tx = std::sync::Mutex::new(wake_tx);
                let engine = Engine::new(
                    config,
                    stream,
                    deps(),
                    options,
                    Arc::new(move || {
                        let _ = wake_tx.lock().unwrap().send(Request::Wake);
                    }),
                );
                serve(engine, rx);
            })
            .expect("spawning the engine thread");
        Self { tx }
    }

    pub fn command(&self, command: Command) -> Result<Reply, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Request::Command(command, tx))
            .map_err(|_| "the engine has stopped".to_string())?;
        rx.recv()
            .unwrap_or_else(|_| Err("the engine has stopped".into()))
    }

    /// Sends every slice to `channel` now, and every event from then on.
    pub fn subscribe(&self, channel: Channel<Event>) {
        let _ = self.tx.send(Request::Subscribe(channel));
    }

    /// The planes for a viewport; `None` before there is a frame.
    pub fn frame(&self, request: FrameRequest) -> Option<FrameReply> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Request::Frame(request, tx)).ok()?;
        rx.recv().ok().flatten()
    }

    /// Stops the engine and waits for it: the app is quitting.
    pub fn stop(&self) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Request::Stop(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
    }

    pub fn lut(&self) -> Option<LutReply> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Request::Lut(tx)).ok()?;
        rx.recv().ok()
    }

    /// Shows a phone that paired over the network ([`Engine::pair`]).
    pub fn pair(&self, session: squigl_core::WebrtcSource) {
        let _ = self.tx.send(Request::Pair(Box::new(session)));
    }

    /// The colour the shown frame should have on screen at view pixel (`x`, `y`)
    /// ([`Engine::displayed_pixel`]); `None` before a frame or outside the view.
    pub fn reference(&self, x: usize, y: usize) -> Option<[u8; 3]> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Request::Reference(x, y, tx)).ok()?;
        rx.recv().ok().flatten()
    }
}

/// Every slice, as events: what a new subscriber starts from. (Taken apart whole,
/// so a slice added to the engine cannot be left out here.)
fn snapshot(state: &EngineState) -> Vec<Event> {
    let EngineState {
        stream,
        capture,
        blocks,
        reading,
        models,
        config,
        speech,
        voices,
    } = state;
    vec![
        Event::Stream(stream.clone()),
        Event::Capture(capture.clone()),
        Event::Blocks(blocks.clone()),
        Event::Reading(reading.clone()),
        Event::Models(models.clone()),
        Event::Config(config.clone()),
        Event::Speech(speech.clone()),
        Event::Voices(voices.clone()),
    ]
}

fn frame(engine: &Engine, request: &FrameRequest) -> Option<FrameReply> {
    let shown = engine.frame(request.frame)?;
    let view = engine.rotation().rotated_size(shown.width, shown.height);
    let view_request = request
        .viewport
        .request(view, request.frame, request.format);
    let planes = engine.render_planes(&view_request)?;
    Some(FrameReply {
        header: FrameHeader {
            planes: planes.header,
            view_region: view_request.region,
            view,
            placement: request.viewport.placement(view),
        },
        planes: planes.planes,
    })
}

fn lut(engine: &Engine) -> LutReply {
    let table = display::lut(&engine.config().display);
    LutReply {
        kind: match table {
            Lut::Tone(_) => "tone",
            Lut::Luma(_) => "luma",
        },
        rgba: table.to_rgba(),
    }
}

fn serve(mut engine: Engine, rx: Receiver<Request>) {
    let mut subscribers: Vec<Channel<Event>> = Vec::new();
    // A stream slice waiting for its turn, and when the last one went.
    let mut held: Option<Versioned<StreamSlice>> = None;
    let mut stream_sent = Instant::now() - STREAM_INTERVAL;
    loop {
        let wait = if held.is_some() {
            STREAM_INTERVAL.saturating_sub(stream_sent.elapsed())
        } else {
            Duration::from_secs(1)
        };
        match rx.recv_timeout(wait) {
            Ok(Request::Command(command, reply)) => {
                let _ = reply.send(engine.handle(command).map_err(|e| format!("{e:#}")));
            }
            Ok(Request::Subscribe(channel)) => {
                engine.pump(Instant::now());
                let alive = snapshot(engine.state())
                    .into_iter()
                    .all(|event| channel.send(event).is_ok());
                if alive {
                    subscribers.push(channel);
                }
            }
            Ok(Request::Frame(request, reply)) => {
                let _ = reply.send(frame(&engine, &request));
            }
            Ok(Request::Lut(reply)) => {
                let _ = reply.send(lut(&engine));
            }
            Ok(Request::Reference(x, y, reply)) => {
                let _ = reply.send(engine.displayed_pixel(FrameRef::Shown, x, y));
            }
            Ok(Request::Pair(session)) => engine.pair(*session),
            Ok(Request::Wake) | Err(RecvTimeoutError::Timeout) => {}
            Ok(Request::Stop(done)) => {
                engine.stop();
                let _ = done.send(());
                return;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for event in engine.pump(Instant::now()) {
            match event {
                Event::Stream(slice) => held = Some(slice),
                other => subscribers.retain(|s| s.send(other.clone()).is_ok()),
            }
        }
        if held.is_some() && stream_sent.elapsed() >= STREAM_INTERVAL {
            let slice = held.take().expect("checked");
            subscribers.retain(|s| s.send(Event::Stream(slice.clone())).is_ok());
            stream_sent = Instant::now();
        }
    }
    engine.stop();
}
