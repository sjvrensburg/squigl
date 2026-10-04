//! Where squigl keeps its files on each OS, from the `directories` crate:
//!
//! | | Linux | Windows | macOS |
//! |---|---|---|---|
//! | config | `$XDG_CONFIG_HOME/squigl` (`~/.config/squigl`) | `%APPDATA%\squigl\config` | `~/Library/Application Support/squigl` |
//! | cache | `$XDG_CACHE_HOME/squigl` (`~/.cache/squigl`) | `%LOCALAPPDATA%\squigl\cache` | `~/Library/Caches/squigl` |
//! | saved images | `$XDG_PICTURES_DIR/squigl` (`~/Pictures/squigl`) | `Pictures\squigl` | `~/Pictures/squigl` |
//!
//! The Linux paths are the ones squigl used before it ran anywhere else. With no home
//! directory at all, each falls back to a relative `squigl/...`.

use directories::{ProjectDirs, UserDirs};
use std::path::PathBuf;

fn project() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "squigl")
}

/// Where `gui.toml` lives.
pub fn config_dir() -> PathBuf {
    project().map_or_else(|| PathBuf::from("squigl"), |p| p.config_dir().to_path_buf())
}

/// Where downloads (the built-in models) are kept.
pub fn cache_dir() -> PathBuf {
    project().map_or_else(|| PathBuf::from("squigl"), |p| p.cache_dir().to_path_buf())
}

/// Where saved images go by default.
pub fn pictures_dir() -> PathBuf {
    let pictures = UserDirs::new().and_then(|u| {
        u.picture_dir()
            .map(PathBuf::from)
            .or_else(|| Some(u.home_dir().join("Pictures")))
    });
    pictures.unwrap_or_default().join("squigl")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// The paths squigl used on Linux before this module, for whatever the
    /// environment is: an existing config and model cache must still be found.
    #[test]
    fn linux_paths_are_unchanged() {
        let old = |xdg: &str, fallback: &str| {
            std::env::var_os(xdg)
                .filter(|v| PathBuf::from(v).is_absolute())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))
                .unwrap()
                .join("squigl")
        };
        assert_eq!(config_dir(), old("XDG_CONFIG_HOME", ".config"));
        assert_eq!(cache_dir(), old("XDG_CACHE_HOME", ".cache"));
    }
}
