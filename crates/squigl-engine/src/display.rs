//! Display modes for the magnifier: how the picture's tones are mapped on the way to
//! the screen, as a 256-entry table ([`lut`]) a front end applies on the GPU with one
//! lookup per pixel.
//!
//! There are two kinds of table:
//!
//! - [`Lut::Tone`] keeps the colour. Each RGB channel, at full range after the
//!   YUV->RGB conversion of `squigl_core::convert`, is replaced by `table[channel]`.
//! - [`Lut::Luma`] drops the colour for two-colour reading. The decoded luma byte,
//!   exactly as it is in the Y plane (BT.601 limited range, 16..=235), indexes an
//!   RGB colour, blending from the mode's ink (the dark strokes) to its paper (the
//!   light ground).
//!
//! Contrast, brightness, gamma and, in a two-colour mode, a hard threshold are baked
//! into the table, so the shader stays a single lookup and the result can be tested
//! here.

use serde::{Deserialize, Serialize};

/// How the picture is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayMode {
    /// The camera's colours.
    #[default]
    Normal,
    /// Black on white.
    Grey,
    /// The camera's colours inverted.
    Inverted,
    /// Yellow ink on black.
    YellowOnBlack,
    /// White ink on black.
    WhiteOnBlack,
    /// Black ink on yellow.
    BlackOnYellow,
    /// [`DisplayConfig::ink`] on [`DisplayConfig::paper`].
    Custom,
}

impl DisplayMode {
    pub const ALL: [DisplayMode; 7] = [
        DisplayMode::Normal,
        DisplayMode::Grey,
        DisplayMode::Inverted,
        DisplayMode::YellowOnBlack,
        DisplayMode::WhiteOnBlack,
        DisplayMode::BlackOnYellow,
        DisplayMode::Custom,
    ];

    /// The (ink, paper) colours of a two-colour mode; `None` for a mode that keeps
    /// the camera's colours.
    pub fn colours(self, cfg: &DisplayConfig) -> Option<([u8; 3], [u8; 3])> {
        const BLACK: [u8; 3] = [0, 0, 0];
        const WHITE: [u8; 3] = [255, 255, 255];
        const YELLOW: [u8; 3] = [255, 255, 0];
        match self {
            DisplayMode::Normal | DisplayMode::Inverted => None,
            DisplayMode::Grey => Some((BLACK, WHITE)),
            DisplayMode::YellowOnBlack => Some((YELLOW, BLACK)),
            DisplayMode::WhiteOnBlack => Some((WHITE, BLACK)),
            DisplayMode::BlackOnYellow => Some((BLACK, YELLOW)),
            DisplayMode::Custom => Some((cfg.ink, cfg.paper)),
        }
    }
}

/// The `[display]` section: the mode and the tone adjustments.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub mode: DisplayMode,
    /// [`DisplayMode::Custom`]'s ink colour (sRGB).
    pub ink: [u8; 3],
    /// [`DisplayMode::Custom`]'s paper colour (sRGB).
    pub paper: [u8; 3],
    /// The spread around mid-grey: 1 leaves it, 2 doubles it.
    pub contrast: f32,
    /// Added to the lightness, from -1 to 1.
    pub brightness: f32,
    /// Above 1 lightens the mid-tones, below 1 darkens them.
    pub gamma: f32,
    /// Two-colour modes only: lightness (0..1, after the adjustments above) at or
    /// above this becomes paper, below it ink, with nothing in between.
    pub threshold: Option<f32>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            mode: DisplayMode::Normal,
            ink: [0, 0, 0],
            paper: [255, 255, 255],
            contrast: 1.0,
            brightness: 0.0,
            gamma: 1.0,
            threshold: None,
        }
    }
}

/// A display mode as a lookup table; see the module docs for how each is applied.
// Built once per mode change and sent on; boxing the larger variant buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lut {
    /// Per RGB channel, full range in and out.
    Tone([u8; 256]),
    /// Luma byte (limited range) to an RGB colour.
    Luma([[u8; 3]; 256]),
}

impl Lut {
    /// The table as a 256x1 RGBA8 texture. A [`Lut::Tone`] entry is repeated in R, G
    /// and B.
    pub fn to_rgba(&self) -> Vec<u8> {
        match self {
            Lut::Tone(t) => t.iter().flat_map(|&v| [v, v, v, 255]).collect(),
            Lut::Luma(t) => t.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        }
    }
}

/// The adjustments, on a lightness in 0..=1.
fn adjust(cfg: &DisplayConfig, t: f32) -> f32 {
    let t = ((t - 0.5) * cfg.contrast + 0.5 + cfg.brightness).clamp(0.0, 1.0);
    t.powf(1.0 / cfg.gamma.max(0.05))
}

fn byte(t: f32) -> u8 {
    (t.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The table for `cfg`.
pub fn lut(cfg: &DisplayConfig) -> Lut {
    match cfg.mode.colours(cfg) {
        None => {
            let invert = cfg.mode == DisplayMode::Inverted;
            Lut::Tone(std::array::from_fn(|i| {
                let t = adjust(cfg, i as f32 / 255.0);
                byte(if invert { 1.0 - t } else { t })
            }))
        }
        Some((ink, paper)) => Lut::Luma(std::array::from_fn(|y| {
            let t = adjust(cfg, ((y as f32 - 16.0) / 219.0).clamp(0.0, 1.0));
            let t = match cfg.threshold {
                Some(at) => f32::from(u8::from(t >= at)),
                None => t,
            };
            std::array::from_fn(|c| {
                byte((ink[c] as f32 + (paper[c] as f32 - ink[c] as f32) * t) / 255.0)
            })
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(mode: DisplayMode) -> DisplayConfig {
        DisplayConfig {
            mode,
            ..DisplayConfig::default()
        }
    }

    fn luma(cfg: &DisplayConfig) -> [[u8; 3]; 256] {
        match lut(cfg) {
            Lut::Luma(t) => t,
            Lut::Tone(_) => panic!("{:?} is not a two-colour mode", cfg.mode),
        }
    }

    #[test]
    fn normal_is_the_identity_and_inverted_its_mirror() {
        let Lut::Tone(t) = lut(&with(DisplayMode::Normal)) else {
            panic!()
        };
        assert!(t.iter().enumerate().all(|(i, &v)| v as usize == i));
        let Lut::Tone(t) = lut(&with(DisplayMode::Inverted)) else {
            panic!()
        };
        assert!(t.iter().enumerate().all(|(i, &v)| v as usize == 255 - i));
    }

    #[test]
    fn two_colour_modes_run_from_ink_at_video_black_to_paper_at_video_white() {
        for (mode, ink, paper) in [
            (DisplayMode::Grey, [0, 0, 0], [255, 255, 255]),
            (DisplayMode::YellowOnBlack, [255, 255, 0], [0, 0, 0]),
            (DisplayMode::WhiteOnBlack, [255, 255, 255], [0, 0, 0]),
            (DisplayMode::BlackOnYellow, [0, 0, 0], [255, 255, 0]),
        ] {
            let t = luma(&with(mode));
            // Limited range: at and beyond the ends the table is flat.
            assert_eq!(t[0], ink, "{mode:?}");
            assert_eq!(t[16], ink, "{mode:?}");
            assert_eq!(t[235], paper, "{mode:?}");
            assert_eq!(t[255], paper, "{mode:?}");
            // In between, every step moves towards the paper.
            let dist =
                |c: [u8; 3]| -> i32 { (0..3).map(|i| (c[i] as i32 - ink[i] as i32).abs()).sum() };
            assert!(t.windows(2).all(|w| dist(w[1]) >= dist(w[0])), "{mode:?}");
        }
    }

    #[test]
    fn custom_uses_the_configured_colours() {
        let cfg = DisplayConfig {
            mode: DisplayMode::Custom,
            ink: [10, 200, 30],
            paper: [40, 0, 90],
            ..DisplayConfig::default()
        };
        let t = luma(&cfg);
        assert_eq!((t[16], t[235]), ([10, 200, 30], [40, 0, 90]));
    }

    #[test]
    fn a_threshold_leaves_only_ink_and_paper() {
        let cfg = DisplayConfig {
            threshold: Some(0.5),
            ..with(DisplayMode::YellowOnBlack)
        };
        let t = luma(&cfg);
        assert!(t.iter().all(|&c| c == [255, 255, 0] || c == [0, 0, 0]));
        // Mid-grey (Y 126 is lightness ~0.502) is paper; just below it is ink.
        assert_eq!(t[126], [0, 0, 0]);
        assert_eq!(t[124], [255, 255, 0]);
    }

    #[test]
    fn contrast_brightness_and_gamma_shape_the_tone() {
        let tone = |cfg: DisplayConfig| match lut(&cfg) {
            Lut::Tone(t) => t,
            Lut::Luma(_) => panic!(),
        };
        let normal = with(DisplayMode::Normal);
        let steep = tone(DisplayConfig {
            contrast: 2.0,
            ..normal
        });
        // Mid-grey stays; a quarter of the way down becomes black.
        assert!(steep[128].abs_diff(128) <= 1);
        assert_eq!(steep[63], 0);
        assert_eq!(steep[192], 255);
        let bright = tone(DisplayConfig {
            brightness: 0.2,
            ..normal
        });
        assert_eq!(bright[0], 51);
        assert_eq!(bright[255], 255);
        let light_mids = tone(DisplayConfig {
            gamma: 2.0,
            ..normal
        });
        assert!(light_mids[64] > 64 && light_mids[0] == 0 && light_mids[255] == 255);
    }

    #[test]
    fn the_texture_is_256_rgba_texels() {
        for mode in DisplayMode::ALL {
            let rgba = lut(&with(mode)).to_rgba();
            assert_eq!(rgba.len(), 1024, "{mode:?}");
            assert!(rgba.chunks(4).all(|px| px[3] == 255));
        }
    }

    #[test]
    fn the_config_is_written_with_kebab_case_modes() {
        let cfg = DisplayConfig {
            threshold: Some(0.4),
            ..with(DisplayMode::YellowOnBlack)
        };
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("mode = \"yellow-on-black\""), "{text}");
        assert_eq!(toml::from_str::<DisplayConfig>(&text).unwrap(), cfg);
        // Every field may be left out.
        assert_eq!(
            toml::from_str::<DisplayConfig>("").unwrap(),
            DisplayConfig::default()
        );
    }
}
