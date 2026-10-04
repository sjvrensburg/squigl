//! The life of a built-in model: not prepared, preparing on a thread (find,
//! download if need be, load), ready, or failed. The [`ModelPhase`] goes to the
//! front end through a [`PhaseCell`], and a download can be cancelled.

use crate::models::{Cancelled, ModelSpec};
use crate::Device;
use anyhow::Result;
use squigl_engine::model::{ModelContext, ModelPhase, PhaseCell};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) enum State<T> {
    /// Not prepared: lazily built and not asked yet, or a download was cancelled.
    Idle,
    Preparing,
    Ready(Box<T>),
    Failed(String),
}

/// A loaded model, which knows what it runs on.
pub(crate) trait OnDevice: Send + 'static {
    fn device(&self) -> Device;
}

/// Loads the model from its directory, reporting each device it tries.
type Loader<T> = dyn Fn(&Path, &dyn Fn(ModelPhase)) -> Result<T> + Send + Sync;

pub(crate) struct Lifecycle<T> {
    spec: &'static ModelSpec,
    /// For the log: "GLM-OCR".
    what: &'static str,
    pub state: Mutex<State<T>>,
    pub phase: PhaseCell,
    cancel: AtomicBool,
    load: Box<Loader<T>>,
}

/// What a model that is not being prepared is: on disk or not.
fn idle_phase(spec: &ModelSpec) -> ModelPhase {
    match spec.locate() {
        Some(_) => ModelPhase::Installed,
        None => ModelPhase::NotInstalled {
            size: spec.total_size(),
        },
    }
}

impl<T: OnDevice> Lifecycle<T> {
    /// Starts preparing at once when `ctx` is eager.
    pub fn new(
        spec: &'static ModelSpec,
        what: &'static str,
        ctx: &ModelContext,
        load: impl Fn(&Path, &dyn Fn(ModelPhase)) -> Result<T> + Send + Sync + 'static,
    ) -> Arc<Self> {
        let this = Arc::new(Self {
            spec,
            what,
            state: Mutex::new(State::Idle),
            phase: PhaseCell::new(idle_phase(spec), ctx.notify.clone()),
            cancel: AtomicBool::new(false),
            load: Box::new(load),
        });
        if ctx.eager {
            this.prepare();
        }
        this
    }

    /// Starts preparing, unless it is ready or already on its way.
    pub fn prepare(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, State::Preparing | State::Ready(_)) {
            return;
        }
        *state = State::Preparing;
        drop(state);
        self.spawn();
    }

    /// Drops a model whose GPU is gone and loads it again (on the CPU, see
    /// [`crate::attempts`]). `state` is the caller's lock on the state.
    pub fn reload(self: &Arc<Self>, state: &mut State<T>) {
        *state = State::Preparing;
        self.phase.set(ModelPhase::Reloading);
        self.spawn();
    }

    /// Stops a download in progress; the model goes back to not installed.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Why it cannot be used yet, for a refused request.
    pub fn not_ready(&self) -> String {
        self.phase.get().describe()
    }

    fn spawn(self: &Arc<Self>) {
        self.cancel.store(false, Ordering::SeqCst);
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name(format!("squigl-{}", me.spec.name))
            .spawn(move || {
                let report = |p: ModelPhase| me.phase.set(p);
                let result = me
                    .spec
                    .ensure(&report, &me.cancel)
                    .and_then(|dir| (me.load)(&dir, &report));
                let mut state = me.state.lock().unwrap();
                match result {
                    Ok(model) => {
                        let device = model.device().name().to_string();
                        log::info!("{} ready on {device}", me.what);
                        *state = State::Ready(Box::new(model));
                        me.phase.set(ModelPhase::Ready { device });
                    }
                    Err(e) if e.is::<Cancelled>() => {
                        log::info!("{}: download cancelled", me.what);
                        *state = State::Idle;
                        me.phase.set(idle_phase(me.spec));
                    }
                    Err(e) => {
                        log::error!("{} unavailable: {e:#}", me.what);
                        *state = State::Failed(format!("{e:#}"));
                        me.phase.set(ModelPhase::Failed {
                            error: format!("{e:#}"),
                        });
                    }
                }
            })
            .expect("spawning a model thread");
    }
}
