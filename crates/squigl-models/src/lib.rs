//! The bundled models: GLM-OCR as a [`Transcriber`] and PP-DocLayoutV3 as the block
//! detector, both on ONNX Runtime. [`models`] finds or downloads their files.

mod glmocr;
pub mod layout;
mod lifecycle;
pub mod models;

use anyhow::{anyhow, bail, Context, Result};
pub use glmocr::Device;
use glmocr::Model;
use lifecycle::{Lifecycle, OnDevice, State};
use ort::environment::Environment;
use ort::ep::{ExecutionProviderDispatch, WebGPU, CPU};
use ort::session::Session;
use squigl_engine::model::{ModelContext, ModelPhase};
use squigl_engine::transcribe::{Mode, Reading, Transcriber, Transcription};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

const VARIANT: &str = "q4f16";

/// The process-wide ONNX Runtime environment: `ort` allows exactly one, and both
/// models need it.
fn environment() -> Result<Environment> {
    static ENV: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| ort::init().build().map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| anyhow!("initialising ONNX Runtime: {e}"))
}

/// One model at a time on the runtime. The WebGPU provider is not safe to `run`
/// from two threads at once, even on two sessions: a block detection overlapping a
/// read segfaulted inside a TopK kernel. This is
/// <https://github.com/microsoft/onnxruntime/issues/32561> (open; a fix is in
/// progress as PR 29851) -- sequential runs are fine, concurrent ones are a silent
/// SIGSEGV. Held around every `run` and, to be safe, every load; drop it once the
/// pinned ONNX Runtime carries the fix.
pub static RUNTIME: Mutex<()> = Mutex::new(());

/// Takes [`RUNTIME`], surviving a panic elsewhere.
pub fn runtime_turn() -> std::sync::MutexGuard<'static, ()> {
    RUNTIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// Opens one graph on `device`. A WebGPU request fails here (not later) if the
/// provider cannot be registered.
fn open_session(path: &Path, device: Device) -> Result<Session> {
    let _turn = runtime_turn();
    let providers: Vec<ExecutionProviderDispatch> = match device {
        Device::WebGpu => vec![WebGPU::default().build().error_on_failure()],
        Device::Cpu => vec![CPU::default().build()],
    };
    let t = Instant::now();
    let session = Session::builder(&environment()?)?
        .with_execution_providers(providers)
        .map_err(|e| anyhow!("registering the {} execution provider: {e}", device.name()))?
        .commit_from_file(path)
        .with_context(|| format!("loading {}", path.display()))?;
    log::debug!(
        "loaded {} in {:.2}s",
        path.display(),
        t.elapsed().as_secs_f64()
    );
    Ok(session)
}

/// Set once the GPU device has been lost (a driver reset -- `VK_ERROR_DEVICE_LOST`
/// -- which a whole page at too large an image budget provokes). The WebGPU
/// provider does not recover the device, so from then on everything loads on the
/// CPU; a restart gets the GPU back.
pub static GPU_LOST: AtomicBool = AtomicBool::new(false);

/// Whether `e` is the GPU being lost. Records it if so.
pub(crate) fn note_gpu_loss(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}");
    let lost = text.contains("DEVICE_LOST")
        || (text.contains("lost") && (text.contains("Device") || text.contains("device")));
    if lost {
        GPU_LOST.store(true, Ordering::SeqCst);
        log::error!("the GPU device was lost; models reload on the CPU: {text}");
    }
    lost
}

/// Whether the graphics system offers a real GPU: an adapter on Direct3D 12, Metal
/// or Vulkan that is not a software rasteriser. Without one, WebGPU would run on
/// Microsoft's WARP or Mesa's llvmpipe -- a VM, a PC without its GPU driver -- far
/// slower than ONNX Runtime's own CPU kernels. Asked once.
fn gpu_present() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY));
        let infos: Vec<_> = adapters.iter().map(|a| a.get_info()).collect();
        let present = infos.iter().any(|i| i.device_type != wgpu::DeviceType::Cpu);
        let names: Vec<_> = infos
            .iter()
            .map(|i| format!("{} ({:?}, {:?})", i.name, i.device_type, i.backend))
            .collect();
        if present {
            log::debug!("GPU adapters: {}", names.join("; "));
        } else {
            log::info!(
                "no hardware GPU (adapters: {}); the models run on the CPU",
                if names.is_empty() {
                    "none".to_string()
                } else {
                    names.join("; ")
                }
            );
        }
        present
    })
}

/// The devices to try for a preference, in order -- the CPU alone once the GPU
/// has been lost, or when "auto" finds no real GPU to try.
fn attempts(device: DevicePref) -> &'static [Device] {
    if GPU_LOST.load(Ordering::SeqCst) {
        return &[Device::Cpu];
    }
    match device {
        DevicePref::Auto if !gpu_present() => &[Device::Cpu],
        DevicePref::Auto => &[Device::WebGpu, Device::Cpu],
        DevicePref::Webgpu => &[Device::WebGpu],
        DevicePref::Cpu => &[Device::Cpu],
    }
}

// ---------------------------------------------------------------------------

/// Which device to try: the config's own type, so the config can name it in a build
/// without the models.
pub use squigl_engine::transcribe::LocalDevice as DevicePref;

/// The bundled GLM-OCR. Prepared (found, downloaded if need be, loaded) on a thread:
/// at once when built eagerly, else on [`Transcriber::prepare`]; reads are refused
/// until it is ready. A lost GPU mid-read reloads it on the CPU.
pub struct LocalBackend {
    name: String,
    max_tokens: usize,
    life: Arc<Lifecycle<Model>>,
}

impl OnDevice for Model {
    fn device(&self) -> Device {
        Model::device(self)
    }
}

impl LocalBackend {
    pub fn new(
        name: String,
        device: DevicePref,
        max_tokens: u32,
        max_image_tokens: u32,
        ctx: &ModelContext,
    ) -> Self {
        let max_image_tokens = max_image_tokens as usize;
        let life = Lifecycle::new(&models::GLM_OCR, "GLM-OCR", ctx, move |dir, report| {
            load(dir, device, max_image_tokens, report)
        });
        Self {
            name,
            max_tokens: max_tokens as usize,
            life,
        }
    }
}

fn load(
    dir: &Path,
    device: DevicePref,
    max_image_tokens: usize,
    report: &dyn Fn(ModelPhase),
) -> Result<Model> {
    let mut last = None;
    for &d in attempts(device) {
        report(ModelPhase::Loading {
            device: d.name().to_string(),
        });
        match Model::load(dir, VARIANT, d, max_image_tokens) {
            Ok(m) => return Ok(m),
            Err(e) => {
                log::warn!("GLM-OCR on {}: {e:#}", d.name());
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("no device to try")))
}

impl Transcriber for LocalBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn status(&self) -> Option<String> {
        Some(self.life.phase.get().describe())
    }

    fn phase(&self) -> Option<ModelPhase> {
        Some(self.life.phase.get())
    }

    fn prepare(&self) {
        self.life.prepare();
    }

    fn cancel_prepare(&self) {
        self.life.cancel();
    }

    fn read(
        &self,
        png: &[u8],
        _mode: Mode,
        prompt: &str,
        _capture_px: (u32, u32),
    ) -> Result<Transcription> {
        let started = Instant::now();
        let img = image::load_from_memory(png)
            .context("decoding the crop")?
            .to_rgb8();
        let mut state = self.life.state.lock().unwrap();
        let model = match &mut *state {
            State::Ready(m) => m,
            State::Idle | State::Preparing => {
                bail!("model not ready yet: {}", self.life.not_ready())
            }
            State::Failed(e) => bail!("model unavailable: {e}"),
        };
        // The other model may have lost the GPU under this one.
        if model.device() == Device::WebGpu && GPU_LOST.load(Ordering::SeqCst) {
            self.life.reload(&mut state);
            bail!("the GPU was lost; the model is reloading on the CPU, try again shortly");
        }
        let out = {
            let _turn = runtime_turn();
            model.generate(&img, prompt, self.max_tokens)
        };
        let out = match out {
            Ok(out) => out,
            Err(e) if note_gpu_loss(&e) => {
                self.life.reload(&mut state);
                bail!(
                    "the GPU was lost mid-read (a driver reset); the model is reloading on \
                     the CPU, try again shortly. A smaller region or image budget avoids it."
                );
            }
            Err(e) => return Err(e),
        };
        let (readings, silent) = if out.text.is_empty() {
            (Vec::new(), 1)
        } else {
            (
                vec![Reading {
                    text: out.text,
                    count: 1,
                    truncated: out.truncated,
                    tokens: (!out.tokens.is_empty()).then_some(out.tokens),
                }],
                0,
            )
        };
        Ok(Transcription {
            backend: self.name.clone(),
            readings,
            silent,
            samples: 1,
            elapsed: started.elapsed(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Built lazily, the model only says where it is -- on disk or not, whichever
    /// this machine has -- and refuses to read; nothing is downloaded or loaded.
    #[test]
    fn a_lazy_backend_waits_to_be_asked() {
        let told = Arc::new(AtomicUsize::new(0));
        let t = Arc::clone(&told);
        let ctx = ModelContext {
            eager: false,
            notify: Arc::new(move || {
                t.fetch_add(1, Ordering::Relaxed);
            }),
        };
        let backend = LocalBackend::new("built in".into(), DevicePref::Cpu, 16, 64, &ctx);
        let phase = backend.phase().unwrap();
        assert!(
            phase == ModelPhase::Installed
                || phase
                    == ModelPhase::NotInstalled {
                        size: models::GLM_OCR.total_size()
                    },
            "{phase:?}"
        );
        assert!(phase.is_idle());
        let mut png = Vec::new();
        image::RgbImage::new(1, 1)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let err = backend
            .read(&png, Mode::Crop, "", (1, 1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not ready"), "{err}");
        assert_eq!(told.load(Ordering::Relaxed), 0, "nothing happened to tell");
    }
}
