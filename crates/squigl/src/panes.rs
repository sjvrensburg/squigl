//! Which of the camera window's panes are shown and which one is active.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Preview,
    Zoom,
    Reading,
}

impl Pane {
    pub const ALL: [Pane; 3] = [Pane::Preview, Pane::Zoom, Pane::Reading];
    /// "Preview", "Zoom", "Reading".
    pub fn title(self) -> &'static str {
        match self {
            Pane::Preview => "Preview",
            Pane::Zoom => "Zoom",
            Pane::Reading => "Reading",
        }
    }
}

#[derive(Debug)]
pub struct Panes {
    active: Pane,
    maximised: Option<Pane>,
}

impl Default for Panes {
    fn default() -> Self {
        Self {
            active: Pane::Preview,
            maximised: None,
        }
    }
}

impl Panes {
    pub fn active(&self) -> Pane {
        self.active
    }

    pub fn set_active(&mut self, pane: Pane) {
        self.active = pane;
    }

    pub fn maximised(&self) -> Option<Pane> {
        self.maximised
    }

    /// Makes `pane` active; restores it if it is the maximised one, else maximises it.
    pub fn toggle_maximise(&mut self, pane: Pane) {
        self.active = pane;
        match self.maximised {
            Some(p) if p == pane => self.maximised = None,
            _ => self.maximised = Some(pane),
        }
    }

    /// Whether `pane` is drawn in the camera window: no pane is maximised, or it is
    /// this one.
    pub fn shown(&self, pane: Pane) -> bool {
        self.maximised.is_none() || self.maximised == Some(pane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_show_all_panes() {
        let panes = Panes::default();
        assert_eq!(panes.active(), Pane::Preview);
        assert_eq!(panes.maximised(), None);
        assert!(panes.shown(Pane::Preview));
        assert!(panes.shown(Pane::Zoom));
        assert!(panes.shown(Pane::Reading));
    }

    #[test]
    fn toggle_maximise_zoom_shows_only_zoom() {
        let mut panes = Panes::default();
        panes.toggle_maximise(Pane::Zoom);
        assert_eq!(panes.maximised(), Some(Pane::Zoom));
        assert_eq!(panes.active(), Pane::Zoom);
        assert!(panes.shown(Pane::Zoom));
        assert!(!panes.shown(Pane::Preview));
        assert!(!panes.shown(Pane::Reading));
    }

    #[test]
    fn toggling_zoom_again_restores_all() {
        let mut panes = Panes::default();
        panes.toggle_maximise(Pane::Zoom);
        panes.toggle_maximise(Pane::Zoom);
        assert_eq!(panes.maximised(), None);
        assert!(panes.shown(Pane::Preview));
        assert!(panes.shown(Pane::Zoom));
        assert!(panes.shown(Pane::Reading));
    }

    #[test]
    fn switching_maximised_pane_moves_it() {
        let mut panes = Panes::default();
        panes.toggle_maximise(Pane::Zoom);
        panes.toggle_maximise(Pane::Reading);
        assert_eq!(panes.maximised(), Some(Pane::Reading));
        assert_eq!(panes.active(), Pane::Reading);
        assert!(panes.shown(Pane::Reading));
        assert!(!panes.shown(Pane::Zoom));
        assert!(!panes.shown(Pane::Preview));
    }
}
