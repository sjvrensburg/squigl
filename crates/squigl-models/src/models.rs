//! Where the bundled models' files live, and how they get there.
//!
//! The hundreds of megabytes of graphs are not embedded in the binary (that would
//! relink them on every build and put them in git); each model is looked up, in
//! order, in `$SQUIGL_MODEL_DIR/<name>/`, a `models/<name>/` directory next to the
//! AppImage (when running as one) or next to the executable (how a release tarball
//! ships them), and the cache's `models/<name>/` (see `squigl_engine::paths`;
//! `~/.cache/squigl/models/<name>/` on Linux). If none has it, it is downloaded from a
//! pinned Hugging Face revision -- into `$SQUIGL_MODEL_DIR` when that is set, else
//! into the cache -- each file checked against the sha256 recorded here before it is
//! used.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use squigl_engine::model::ModelPhase;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// What a download stopped by its cancel flag fails with.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("download cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub struct ModelFile {
    pub path: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    /// Where it comes from, when not `repo_url/path` (a model drawing on more than
    /// one repository).
    pub url: Option<&'static str>,
}

/// One downloadable model: its directory name and its file manifest.
pub struct ModelSpec {
    /// The directory the files live under (`models/<name>/`).
    pub name: &'static str,
    /// `https://huggingface.co/<repo>/resolve/<revision>`.
    pub repo_url: &'static str,
    pub files: &'static [ModelFile],
}

/// `onnx-community/GLM-OCR-ONNX`, q4f16 variant, at a pinned revision.
pub const GLM_OCR: ModelSpec = ModelSpec {
    name: "glm-ocr-onnx-q4f16",
    repo_url: "https://huggingface.co/onnx-community/GLM-OCR-ONNX/resolve/aea46198f09e3aa2b63422dd234f1cc66afffe52",
    files: &[
        ModelFile {
            path: "config.json",
            size: 2022,
            sha256: "8bf81b89d42ae98917084dfeaca5a1dc20ac20d6145f088ec2aac8840c1e4b9f",
            url: None,
        },
        ModelFile {
            path: "preprocessor_config.json",
            size: 366,
            sha256: "0d4b3cf5190e6b5b53ac52b1880b111163888bfc975db396561ee78887942fe4",
            url: None,
        },
        ModelFile {
            path: "tokenizer.json",
            size: 5_420_559,
            sha256: "c3e229a66a06267194e62055f70cf5580af83b3582f46928fefee8cb9618f499",
            url: None,
        },
        ModelFile {
            path: "onnx/vision_encoder_q4f16.onnx",
            size: 474_559,
            sha256: "083d41123710a5ceb725d3f48fc8170f3e85d9289e7530454c86c1961254055b",
            url: None,
        },
        ModelFile {
            path: "onnx/vision_encoder_q4f16.onnx_data",
            size: 262_272_000,
            sha256: "b09ae1abca6bd2d229c63bc2dc4d09bba272c0627b270804f66267b0bac17ee1",
            url: None,
        },
        ModelFile {
            path: "onnx/embed_tokens_q4f16.onnx",
            size: 1060,
            sha256: "b56ef40c21191aa1fdd4e7251679347ed45dd8473605e9539caeed6b781e41f7",
            url: None,
        },
        ModelFile {
            path: "onnx/embed_tokens_q4f16.onnx_data",
            size: 52_740_096,
            sha256: "4b82f4062c1cf676e29126c6826c93d262872c1efad8e24fc78476be4245a966",
            url: None,
        },
        ModelFile {
            path: "onnx/decoder_model_merged_q4f16.onnx",
            size: 377_830,
            sha256: "6510318b0b3f1458c38a8678ebb2ca6868e83753cef92d72da8cb926aa82e0b8",
            url: None,
        },
        ModelFile {
            path: "onnx/decoder_model_merged_q4f16.onnx_data",
            size: 336_844_800,
            sha256: "82af470f508000dcc3914c36d102f60c39b12f4be0f016b333e5e78b2e865bc8",
            url: None,
        },
    ],
};

/// PaddlePaddle's official ONNX export of PP-DocLayoutV3 (Apache-2.0), at a pinned
/// revision.
pub const DOC_LAYOUT: ModelSpec = ModelSpec {
    name: "pp-doclayoutv3-onnx",
    repo_url: "https://huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx/resolve/46bbdf188bb0a772c08aed74882ce7e51a8f1ea6",
    files: &[ModelFile {
        path: "inference.onnx",
        size: 130_502_049,
        sha256: "45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba",
        url: None,
    }],
};

/// Kokoro-82M (onnx-community's export, fp16) with five American English voices,
/// and what squigl-misaki needs to give it phonemes: Misaki's dictionaries (its
/// GitHub repository, pinned) and its fallback network for other words.
pub const KOKORO: ModelSpec = ModelSpec {
    name: "kokoro",
    repo_url: "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/1939ad2a8e416c0acfeecc08a694d14ef25f2231",
    files: &[
        ModelFile {
            path: "onnx/model_fp16.onnx",
            size: 163_234_740,
            sha256: "ba4527a874b42b21e35f468c10d326fdff3c7fc8cac1f85e9eb6c0dfc35c334a",
            url: None,
        },
        ModelFile {
            path: "voices/af_heart.bin",
            size: 522_240,
            sha256: "d583ccff3cdca2f7fae535cb998ac07e9fcb90f09737b9a41fa2734ec44a8f0b",
            url: None,
        },
        ModelFile {
            path: "voices/af_bella.bin",
            size: 522_240,
            sha256: "f69d836209b78eb8c66e75e3cda491e26ea838a3674257e9d4e5703cbaf55c8b",
            url: None,
        },
        ModelFile {
            path: "voices/af_sarah.bin",
            size: 522_240,
            sha256: "4409fbc125afabacc615d94db5398d847006a737b0247d6892b7a9a0007a2f0a",
            url: None,
        },
        ModelFile {
            path: "voices/am_michael.bin",
            size: 522_240,
            sha256: "1d1f21dd8da39c30705cd4c75d039d265e9bc4a2a93ed09bc9e1b1225eb95ba1",
            url: None,
        },
        ModelFile {
            path: "voices/am_adam.bin",
            size: 522_240,
            sha256: "162b035ed91cfc48b6046982184c645f72edcdd1b82843347f605d7bf7b15716",
            url: None,
        },
        ModelFile {
            path: "misaki/us_gold.json",
            size: 3_000_469,
            sha256: "dc414872a49a28ae6c141463d502fd945f3b2fde040484fdc47d00cc4612686f",
            url: Some("https://raw.githubusercontent.com/hexgrad/misaki/fba1236595f2d2bf21d414ba6e57d25256afada3/misaki/data/us_gold.json"),
        },
        ModelFile {
            path: "misaki/us_silver.json",
            size: 3_099_517,
            sha256: "de8f67be911bb6c659187b4a65fd966b6a30e56350e0f790d763210b053ac475",
            url: Some("https://raw.githubusercontent.com/hexgrad/misaki/fba1236595f2d2bf21d414ba6e57d25256afada3/misaki/data/us_silver.json"),
        },
        ModelFile {
            path: "misaki-fallback/config.json",
            size: 1_257,
            sha256: "8deb3537fb29c63cd9f20d75515ae06e4c92f1b6db0703a2d45bca95b33a53a4",
            url: Some("https://huggingface.co/PeterReid/graphemes_to_phonemes_en_us/resolve/a5631b285d18d59483c32c0c3379cb9fac924f4b/config.json"),
        },
        ModelFile {
            path: "misaki-fallback/model.safetensors",
            size: 3_011_692,
            sha256: "dc4a02e62d4fcb4bb4097ecf00db89b8e1a12a549a52ab6adfbba220b80a55c5",
            url: Some("https://huggingface.co/PeterReid/graphemes_to_phonemes_en_us/resolve/a5631b285d18d59483c32c0c3379cb9fac924f4b/model.safetensors"),
        },
    ],
};

/// Everything `--fetch-model` fetches: the egui window's models (it does not read
/// aloud, so not Kokoro).
pub const ALL: &[&ModelSpec] = &[&GLM_OCR, &DOC_LAYOUT];

/// The directories a model named `name` is searched for in, in order.
fn candidate_dirs(name: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("SQUIGL_MODEL_DIR") {
        dirs.push(PathBuf::from(d).join(name));
    }
    // Inside an AppImage the executable's own directory is read-only; the models
    // go next to the AppImage file, whose path the runtime passes in $APPIMAGE.
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        if let Some(dir) = Path::new(&appimage).parent() {
            dirs.push(dir.join("models").join(name));
        }
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(exe_dir.join("models").join(name));
    }
    dirs.push(squigl_engine::paths::cache_dir().join("models").join(name));
    dirs
}

/// Where a download lands: the directory the user named, else the cache.
fn download_dir(name: &str) -> PathBuf {
    match std::env::var_os("SQUIGL_MODEL_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join(name),
        None => squigl_engine::paths::cache_dir().join("models").join(name),
    }
}

impl ModelSpec {
    /// Sum of all file sizes: the download, and the "is it complete" test.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Every file present at its recorded size. Content is verified at download
    /// time; hashing 650 MB on each start is not worth the seconds.
    fn is_complete(&self, dir: &Path) -> bool {
        self.files
            .iter()
            .all(|f| std::fs::metadata(dir.join(f.path)).is_ok_and(|m| m.len() == f.size))
    }

    /// Where the model already is, if anywhere: no download.
    pub fn locate(&self) -> Option<PathBuf> {
        candidate_dirs(self.name)
            .into_iter()
            .find(|dir| self.is_complete(dir))
    }

    /// Finds the model, downloading it into the cache if no candidate has it.
    /// `report` is told each phase; raising `cancel` stops a download with
    /// [`Cancelled`].
    pub fn ensure(&self, report: &dyn Fn(ModelPhase), cancel: &AtomicBool) -> Result<PathBuf> {
        report(ModelPhase::Locating);
        if let Some(dir) = self.locate() {
            log::info!("{} found at {}", self.name, dir.display());
            return Ok(dir);
        }
        let dir = download_dir(self.name);
        self.download_into(&dir, report, cancel)?;
        Ok(dir)
    }

    /// Downloads the model into `dir` (files already present at the right size are
    /// kept), verifying every file's SHA-256. `squigl --fetch-model DIR` for
    /// scripts and release packaging.
    pub fn download_into(
        &self,
        dir: &Path,
        report: &dyn Fn(ModelPhase),
        cancel: &AtomicBool,
    ) -> Result<()> {
        log::info!(
            "downloading {} ({} MB) to {}",
            self.name,
            self.total_size() / 1_000_000,
            dir.display()
        );
        let agent = ureq::Agent::config_builder()
            .timeout_global(None)
            .build()
            .new_agent();
        let mut done: u64 = 0;
        let total = self.total_size();
        for file in self.files {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            let target = dir.join(file.path);
            if std::fs::metadata(&target).is_ok_and(|m| m.len() == file.size) {
                done += file.size;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let url = file
                .url
                .map_or_else(|| format!("{}/{}", self.repo_url, file.path), String::from);
            let part = dir.join(format!("{}.part", file.path));
            // Anonymous downloads are rate-limited (CI runners share addresses); a
            // Hugging Face token lifts that. The files themselves are public.
            let mut request = agent.get(&url);
            // Only ever to Hugging Face: a file from elsewhere must not get the token.
            let token = std::env::var_os("HF_TOKEN")
                .filter(|t| !t.is_empty() && url.starts_with("https://huggingface.co/"));
            if let Some(token) = &token {
                request = request.header(
                    "authorization",
                    format!("Bearer {}", token.to_string_lossy()),
                );
            }
            // An invalid token is refused (401) even for public files, so say when
            // one was sent: a stale HF_TOKEN in the environment is the likely cause.
            let mut response = request.call().with_context(|| {
                if token.is_some() {
                    format!(
                        "downloading {} (with HF_TOKEN from the environment)",
                        file.path
                    )
                } else {
                    format!("downloading {}", file.path)
                }
            })?;
            let mut reader = response.body_mut().as_reader();
            let mut out = std::fs::File::create(&part)?;
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 1 << 20];
            let mut written: u64 = 0;
            let mut last_report = Instant::now();
            loop {
                if cancel.load(Ordering::Relaxed) {
                    drop(out);
                    let _ = std::fs::remove_file(&part);
                    return Err(Cancelled.into());
                }
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                hasher.update(&buf[..n]);
                written += n as u64;
                if last_report.elapsed().as_millis() > 200 {
                    report(ModelPhase::Downloading {
                        done: done + written,
                        total,
                    });
                    last_report = Instant::now();
                }
            }
            drop(out);
            report(ModelPhase::Verifying);
            let digest = format!("{:x}", hasher.finalize());
            if written != file.size || digest != file.sha256 {
                let _ = std::fs::remove_file(&part);
                bail!(
                    "{} downloaded wrong: {written} bytes, sha256 {digest} (expected {} bytes, {})",
                    file.path,
                    file.size,
                    file.sha256
                );
            }
            std::fs::rename(&part, &target)?;
            done += file.size;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_are_consistent() {
        assert_eq!(GLM_OCR.files.len(), 9);
        assert_eq!(GLM_OCR.total_size(), 658_133_292);
        assert_eq!(DOC_LAYOUT.total_size(), 130_502_049);
        assert_eq!(KOKORO.total_size(), 174_958_875);
        for spec in ALL.iter().chain([&&KOKORO]) {
            assert!(spec.files.iter().all(|f| f.sha256.len() == 64));
            assert!(!spec.is_complete(Path::new("/nonexistent")));
        }
    }

    #[test]
    fn a_raised_cancel_flag_stops_a_download_before_any_request() {
        let dir = std::env::temp_dir().join(format!("squigl-cancel-{}", std::process::id()));
        let tiny = ModelSpec {
            name: "tiny",
            // Never contacted: the flag is checked before each file's request.
            repo_url: "http://127.0.0.1:9",
            files: &[ModelFile {
                path: "f",
                size: 1,
                sha256: "0000000000000000000000000000000000000000000000000000000000000000",
                url: None,
            }],
        };
        let result = tiny.download_into(&dir, &|_| {}, &AtomicBool::new(true));
        let _ = std::fs::remove_dir_all(&dir);
        let err = result.unwrap_err();
        assert!(err.is::<Cancelled>(), "{err:#}");
        assert!(!dir.join("f").exists());
    }
}
