//! The settings file, `gui.toml` in the [config directory](crate::paths::config_dir)
//! (`~/.config/squigl/gui.toml` on Linux): the transcription backends, the block
//! detector, the prompts, the window, ink enhancement, and the magnifier's display
//! mode and view. Every front end reads and writes this one file, so each keeps the
//! sections it does not use.

use crate::display::DisplayConfig;
use crate::transcribe::{
    BackendConfig, LocalDevice, Mode, CROP_PROMPT, FORMULA_PROMPT, PAGE_PROMPT,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The built-in block detector (PP-DocLayoutV3; needs the `local-model` build
/// feature).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LayoutConfig {
    pub enabled: bool,
    pub device: LocalDevice,
    /// Minimum detection score. Handwriting scores lower than the printed pages the
    /// model was trained on.
    pub threshold: f32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            device: LocalDevice::default(),
            threshold: 0.4,
        }
    }
}

/// The instructions sent with an image. Sent by the OpenAI-compatible and built-in
/// backends; the hint API has its own on the workbench side.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PromptsConfig {
    /// For a boxed region (a word or a line).
    pub crop: String,
    /// For a block the layout model called a formula.
    pub formula: String,
    /// For a whole page.
    pub page: String,
}

impl Default for PromptsConfig {
    fn default() -> Self {
        Self {
            crop: CROP_PROMPT.into(),
            formula: FORMULA_PROMPT.into(),
            page: PAGE_PROMPT.into(),
        }
    }
}

impl PromptsConfig {
    pub fn for_mode(&self, mode: Mode) -> &str {
        match mode {
            Mode::Crop => &self.crop,
            Mode::Formula => &self.formula,
            Mode::Page => &self.page,
        }
    }
}

/// The window itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiConfig {
    /// Everything in the window scaled by this (1.0 = the desktop's own size).
    pub scale: f32,
    /// The readings' text size in points (scaled by `scale` like everything else).
    pub reading_size: f32,
    /// A token at or above this probability is shown as steady (no tint). Workbench
    /// default: 0.92.
    pub steady_threshold: f32,
    /// A token at or above this (but below `steady_threshold`) is "wavering"
    /// (amber); below it, "hesitant" (red). Workbench default: 0.6.
    pub wavering_threshold: f32,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            scale: 1.0,
            reading_size: 20.0,
            steady_threshold: 0.92,
            wavering_threshold: 0.6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub backends: Vec<BackendConfig>,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub prompts: PromptsConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub enhance: crate::enhance::EnhanceConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub magnifier: MagnifierConfig,
    #[serde(default)]
    pub desktop: DesktopConfig,
}

impl Config {
    /// `gui.toml` in the [config directory](crate::paths::config_dir).
    pub fn path() -> PathBuf {
        crate::paths::config_dir().join("gui.toml")
    }

    /// The file's contents, or -- if there is no file -- the defaults, written out so
    /// there is something to edit.
    pub fn load_or_create() -> Result<Self> {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let config = Self::default();
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, Self::default_text())
                    .with_context(|| format!("writing {}", path.display()))?;
                log::info!("wrote default backends to {}", path.display());
                Ok(config)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Writes the file; the Settings window's Save.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    /// Writes the settings to `path` (creating its directory).
    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.text()).with_context(|| format!("writing {}", path.display()))
    }

    fn default_text() -> String {
        Self::default().text()
    }

    fn text(&self) -> String {
        format!(
            "# squigl settings: edit here or in the window's Settings. Each [[backends]]\n\
             # entry is one choice in the window; the first is selected at startup.\n\
             # [layout] is the block detector, [prompts] what the models are asked,\n\
             # [ui] the egui window, [display] and [magnifier] the magnifier's view,\n\
             # [desktop] the desktop app.\n\n{}",
            toml::to_string_pretty(self).expect("config serialises")
        )
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backends: vec![
                BackendConfig::Local {
                    name: "GLM-OCR (built in)".into(),
                    device: LocalDevice::default(),
                    max_tokens: 1024,
                    max_image_tokens: 2048,
                },
                BackendConfig::HintApi {
                    name: "workbench GLM-OCR".into(),
                    base_url: "http://127.0.0.1:8093".into(),
                    member: "glm-ocr".into(),
                    samples: 3,
                },
                BackendConfig::OpenAi {
                    name: "llama-server :8099".into(),
                    base_url: "http://127.0.0.1:8099/v1".into(),
                    model: "local".into(),
                    api_key: None,
                    samples: 1,
                    temperature: 0.0,
                    max_tokens: 1024,
                },
            ],
            layout: LayoutConfig::default(),
            prompts: PromptsConfig::default(),
            ui: UiConfig::default(),
            enhance: crate::enhance::EnhanceConfig::default(),
            display: DisplayConfig::default(),
            magnifier: MagnifierConfig::default(),
            desktop: DesktopConfig::default(),
        }
    }
}

/// The `[magnifier]` section: how the live view is magnified.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MagnifierConfig {
    /// The magnification at start: 1 fits the whole picture in the window.
    pub magnification: f32,
    /// The most it goes to.
    pub max_magnification: f32,
    /// Smooth (bilinear) rather than blocky (nearest-neighbour) pixels when
    /// magnified.
    pub smooth: bool,
    /// A horizontal guide line across the middle of the view, to keep one's place.
    pub reading_line: bool,
}

impl Default for MagnifierConfig {
    fn default() -> Self {
        Self {
            magnification: 1.0,
            max_magnification: 30.0,
            smooth: true,
            reading_line: false,
        }
    }
}

/// The desktop app's look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    /// Yellow text and outlines on black.
    HighContrastYellow,
    /// White text and outlines on black.
    HighContrastWhite,
}

/// The `[desktop]` section: the desktop app's own settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopConfig {
    pub theme: Theme,
    /// Shortcuts that are a single key with no modifier (Space, R, +). They can be
    /// turned off, for someone who presses keys by accident or uses speech input.
    pub single_key_shortcuts: bool,
    /// Keys moved from their defaults: action name to key (the `KeyboardEvent.key`
    /// value). The app knows the actions and their default keys.
    pub shortcuts: BTreeMap<String, String>,
    /// Pair over Tailscale with Tailscale's own certificate for this machine (when
    /// the tailnet has HTTPS on), so the phone's browser shows no warning. Off
    /// unless chosen: getting one puts the machine's tailnet name in the public
    /// Certificate Transparency logs.
    pub tailscale_https: bool,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            single_key_shortcuts: true,
            shortcuts: BTreeMap::new(),
            tailscale_https: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplayMode;

    #[test]
    fn default_config_round_trips_through_toml() {
        let text = Config::default_text();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed, Config::default());
        assert!(matches!(parsed.backends[0], BackendConfig::Local { .. }));
        assert_eq!(
            parsed.backends[1].build().unwrap().name(),
            "workbench GLM-OCR"
        );
        // Optional fields may be left out.
        let minimal: Config = toml::from_str(
            "[[backends]]\nkind = \"open-ai\"\nname = \"x\"\nbase_url = \"http://h/v1\"\nmodel = \"m\"\n",
        )
        .unwrap();
        assert!(matches!(
            minimal.backends[0],
            BackendConfig::OpenAi { samples: 1, .. }
        ));
    }

    /// Every section survives a write and a read, so one front end saving the file
    /// keeps what another set.
    #[test]
    fn every_section_round_trips() {
        let mut cfg = Config::default();
        cfg.layout.threshold = 0.3;
        cfg.prompts.page = "read it".into();
        cfg.ui.scale = 1.5;
        cfg.enhance.strength = 0.25;
        cfg.display = DisplayConfig {
            mode: DisplayMode::Custom,
            ink: [1, 2, 3],
            paper: [250, 240, 230],
            contrast: 1.4,
            brightness: -0.1,
            gamma: 1.2,
            threshold: Some(0.45),
        };
        cfg.magnifier = MagnifierConfig {
            magnification: 4.0,
            max_magnification: 16.0,
            smooth: false,
            reading_line: true,
        };
        cfg.desktop = DesktopConfig {
            theme: Theme::HighContrastYellow,
            single_key_shortcuts: false,
            shortcuts: [("freeze".to_string(), "Enter".to_string())].into(),
            tailscale_https: true,
        };
        let parsed: Config = toml::from_str(&cfg.text()).unwrap();
        assert_eq!(parsed, cfg);
    }

    /// A file written before [display] and [magnifier] existed still loads.
    #[test]
    fn an_older_file_gets_the_new_sections_default() {
        let parsed: Config = toml::from_str("[ui]\nscale = 1.25\n").unwrap();
        assert_eq!(parsed.ui.scale, 1.25);
        assert_eq!(parsed.display, DisplayConfig::default());
        assert_eq!(parsed.magnifier, MagnifierConfig::default());
    }
}
