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

    /// Its position in [`Pane::ALL`]: 0, 1, 2.
    fn index(self) -> usize {
        Pane::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }
}

#[derive(Debug)]
pub struct Panes {
    active: Pane,
    maximised: Option<Pane>,
    /// Per pane, indexed by [`Pane::index`]: shown in its own window.
    detached: [bool; 3],
}

impl Default for Panes {
    fn default() -> Self {
        Self {
            active: Pane::Preview,
            maximised: None,
            detached: [false; 3],
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

    /// Makes `pane` active; restores it if it is the maximised one, else maximises
    /// it. A detached pane (shown in its own window) cannot be maximised here, so
    /// it only becomes active.
    pub fn toggle_maximise(&mut self, pane: Pane) {
        self.active = pane;
        if self.detached[pane.index()] {
            return;
        }
        match self.maximised {
            Some(p) if p == pane => self.maximised = None,
            _ => self.maximised = Some(pane),
        }
    }

    /// Whether `pane` is drawn in the camera window: it is not detached and no pane
    /// is maximised, or it is this one.
    pub fn shown(&self, pane: Pane) -> bool {
        !self.detached[pane.index()] && (self.maximised.is_none() || self.maximised == Some(pane))
    }

    /// Whether `pane` is drawn in its own window rather than in the camera window.
    pub fn is_detached(&self, pane: Pane) -> bool {
        self.detached[pane.index()]
    }

    /// Detaches `pane`, or attaches it back. Detaching the maximised pane restores
    /// the layout.
    pub fn toggle_detached(&mut self, pane: Pane) {
        let i = pane.index();
        self.detached[i] = !self.detached[i];
        if self.detached[i] && self.maximised == Some(pane) {
            self.maximised = None;
        }
    }

    /// The detached panes, in [`Pane::ALL`] order.
    pub fn detached(&self) -> Vec<Pane> {
        Pane::ALL
            .into_iter()
            .filter(|p| self.detached[p.index()])
            .collect()
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

    #[test]
    fn detaching_zoom_hides_only_it() {
        let mut panes = Panes::default();
        panes.toggle_detached(Pane::Zoom);
        assert!(panes.is_detached(Pane::Zoom));
        assert!(!panes.shown(Pane::Zoom));
        assert!(panes.shown(Pane::Preview));
        assert!(panes.shown(Pane::Reading));
        assert_eq!(panes.detached(), vec![Pane::Zoom]);
    }

    #[test]
    fn detaching_maximised_pane_restores_layout() {
        let mut panes = Panes::default();
        panes.toggle_maximise(Pane::Zoom);
        panes.toggle_detached(Pane::Zoom);
        assert_eq!(panes.maximised(), None);
        assert!(panes.shown(Pane::Preview));
        assert!(panes.shown(Pane::Reading));
        assert!(!panes.shown(Pane::Zoom));
    }

    #[test]
    fn toggling_detached_twice_returns_to_all() {
        let mut panes = Panes::default();
        panes.toggle_detached(Pane::Zoom);
        panes.toggle_detached(Pane::Zoom);
        assert!(!panes.is_detached(Pane::Zoom));
        assert!(panes.shown(Pane::Preview));
        assert!(panes.shown(Pane::Zoom));
        assert!(panes.shown(Pane::Reading));
        assert!(panes.detached().is_empty());
    }

    #[test]
    fn a_detached_pane_cannot_be_maximised() {
        let mut panes = Panes::default();
        panes.toggle_detached(Pane::Zoom);
        panes.toggle_maximise(Pane::Zoom);
        assert_eq!(panes.maximised(), None);
        assert_eq!(panes.active(), Pane::Zoom);
        assert!(panes.shown(Pane::Preview));
        assert!(panes.shown(Pane::Reading));
    }
}
