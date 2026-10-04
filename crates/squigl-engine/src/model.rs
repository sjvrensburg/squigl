//! Built-in models getting ready: the [`ModelPhase`] a model is in, the
//! [`ModelContext`] a front end builds one with (load at once or on request, and who
//! to wake), and the [`PhaseCell`] a model keeps its phase in.
//!
//! The models themselves live outside the engine (`squigl-models`); they report
//! through [`crate::transcribe::Transcriber::phase`] and
//! [`crate::layout::BlockDetector::phase`].

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where a built-in model is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "kebab-case")]
pub enum ModelPhase {
    /// Not on this machine: preparing it downloads `size` bytes.
    NotInstalled {
        size: u64,
    },
    /// On disk, not loaded: preparing it loads it.
    Installed,
    /// Looking for its files.
    Locating,
    Downloading {
        done: u64,
        total: u64,
    },
    /// Checking a downloaded file against its recorded checksum.
    Verifying,
    Loading {
        device: String,
    },
    Ready {
        device: String,
    },
    /// The GPU was lost under it; it is loading again on the CPU.
    Reloading,
    Failed {
        error: String,
    },
}

impl ModelPhase {
    pub fn is_ready(&self) -> bool {
        matches!(self, ModelPhase::Ready { .. })
    }

    /// Whether preparing it would do anything: it is neither ready nor on its way.
    pub fn is_idle(&self) -> bool {
        matches!(
            self,
            ModelPhase::NotInstalled { .. } | ModelPhase::Installed | ModelPhase::Failed { .. }
        )
    }

    /// A line for a person.
    pub fn describe(&self) -> String {
        match self {
            ModelPhase::NotInstalled { size } => {
                format!("not downloaded ({} MB)", size.div_ceil(1_000_000))
            }
            ModelPhase::Installed => "downloaded, not loaded".to_string(),
            ModelPhase::Locating => "locating model".to_string(),
            ModelPhase::Downloading { done, total } => {
                format!("downloading {}%", done * 100 / (*total).max(1))
            }
            ModelPhase::Verifying => "verifying download".to_string(),
            ModelPhase::Loading { device } => format!("loading model on {device}"),
            ModelPhase::Ready { device } => format!("ready on {device}"),
            ModelPhase::Reloading => "GPU lost — reloading on the CPU".to_string(),
            ModelPhase::Failed { error } => format!("unavailable: {error}"),
        }
    }
}

/// Called whenever a model's phase moves on, from the model's own thread: a front
/// end's repaint, the engine's waker.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// How a front end wants its built-in models.
#[derive(Clone)]
pub struct ModelContext {
    /// Prepare (find, download if needed, load) as soon as built. The egui window
    /// does; a front end that asks first leaves it to [`crate::transcribe::Transcriber::prepare`].
    pub eager: bool,
    pub notify: Notify,
}

impl ModelContext {
    /// Eager, telling no one: for tools and tests.
    pub fn eager() -> Self {
        Self {
            eager: true,
            notify: Arc::new(|| {}),
        }
    }
}

/// The least time between two notifications of download progress alone.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// A model's phase, shared between the model's thread and whoever reads it, with
/// the notifications coalesced: every change of kind is told at once, download
/// progress at most every 250 ms or every whole per cent.
pub struct PhaseCell {
    inner: Mutex<(ModelPhase, Option<Instant>)>,
    notify: Notify,
}

impl PhaseCell {
    pub fn new(phase: ModelPhase, notify: Notify) -> Self {
        Self {
            inner: Mutex::new((phase, None)),
            notify,
        }
    }

    pub fn get(&self) -> ModelPhase {
        self.inner.lock().unwrap().0.clone()
    }

    pub fn set(&self, phase: ModelPhase) {
        let tell = {
            let mut inner = self.inner.lock().unwrap();
            let (old, told) = &mut *inner;
            let tell = match (&*old, &phase) {
                (a, b) if a == b => false,
                (
                    ModelPhase::Downloading { done: a, total },
                    ModelPhase::Downloading { done: b, .. },
                ) => {
                    let percent = |d: u64| d * 100 / (*total).max(1);
                    percent(*a) != percent(*b)
                        || told.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL)
                }
                _ => true,
            };
            // Without a notification the stored phase still moves, so a reader sees
            // the latest; only the last one told is remembered.
            *old = phase;
            if tell {
                *told = Some(Instant::now());
            }
            tell
        };
        if tell {
            (self.notify)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counted() -> (PhaseCell, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let cell = PhaseCell::new(
            ModelPhase::NotInstalled { size: 1000 },
            Arc::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            }),
        );
        (cell, count)
    }

    #[test]
    fn download_progress_is_told_per_whole_percent() {
        let (cell, told) = counted();
        cell.set(ModelPhase::Locating);
        assert_eq!(told.load(Ordering::Relaxed), 1);
        // 1000 steps of a thousandth: 101 distinct percentages (0..=100), and the
        // first step is a change of kind.
        for done in 0..=1000 {
            cell.set(ModelPhase::Downloading { done, total: 1000 });
        }
        let n = told.load(Ordering::Relaxed);
        assert!((101..=110).contains(&n), "told {n} times");
        assert_eq!(
            cell.get(),
            ModelPhase::Downloading {
                done: 1000,
                total: 1000
            }
        );
        // The same phase again is not news.
        cell.set(ModelPhase::Downloading {
            done: 1000,
            total: 1000,
        });
        cell.set(ModelPhase::Ready {
            device: "CPU".into(),
        });
        cell.set(ModelPhase::Ready {
            device: "CPU".into(),
        });
        assert_eq!(told.load(Ordering::Relaxed), n + 1);
    }

    #[test]
    fn phases_describe_themselves_and_serialise_tagged() {
        let p = ModelPhase::Downloading {
            done: 50,
            total: 200,
        };
        assert_eq!(p.describe(), "downloading 25%");
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#"{"phase":"downloading","done":50,"total":200}"#);
        assert!(ModelPhase::Installed.is_idle());
        assert!(!ModelPhase::Locating.is_idle());
        assert!(ModelPhase::Ready {
            device: "WebGPU".into()
        }
        .is_ready());
    }
}
