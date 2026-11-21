//! Hover-to-reveal for a notch hidden by a fullscreen app (`reveal_on_hover`).
//!
//! The cursor must rest in the hot zone (where the pill normally sits) for
//! [`DWELL`] before the notch shows up as a plain pill; the normal hover
//! logic takes over from there. It hides again once the pointer has left the
//! zone and the panel is closed.

/// How long the cursor must rest in the hot zone before the notch reveals.
pub const DWELL: f64 = 0.4;

#[derive(Default)]
pub struct Reveal {
    /// When the cursor entered the hot zone while still hidden.
    since: Option<f64>,
    revealed: bool,
}

impl Reveal {
    /// Whether the notch should show despite `hidden_for_fullscreen`.
    /// `enabled` is the setting, `in_zone` the cursor in the hot zone and
    /// `shell_open` the panel open (or about to be).
    pub fn update(
        &mut self,
        now: f64,
        hidden_for_fullscreen: bool,
        enabled: bool,
        in_zone: bool,
        shell_open: bool,
    ) -> bool {
        if !hidden_for_fullscreen || !enabled {
            *self = Self::default();
        } else if self.revealed {
            self.revealed = in_zone || shell_open;
        } else if !in_zone {
            self.since = None;
        } else if now - *self.since.get_or_insert(now) >= DWELL {
            self.revealed = true;
            self.since = None;
        }
        self.revealed
    }

    /// When a frame is needed to finish the dwell.
    pub fn wake_at(&self) -> Option<f64> {
        self.since.map(|s| s + DWELL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_dwell_does_not_reveal_long_one_does() {
        let mut r = Reveal::default();
        assert!(!r.update(0.0, true, true, true, false));
        assert_eq!(r.wake_at(), Some(DWELL));
        assert!(!r.update(0.39, true, true, true, false));
        assert!(r.update(0.4, true, true, true, false));
        assert_eq!(r.wake_at(), None);
    }

    #[test]
    fn leaving_before_the_dwell_resets_it() {
        let mut r = Reveal::default();
        r.update(0.0, true, true, true, false);
        assert!(!r.update(0.3, true, true, false, false));
        assert_eq!(r.wake_at(), None);
        // Re-entering restarts the clock.
        assert!(!r.update(0.5, true, true, true, false));
        assert!(!r.update(0.8, true, true, true, false));
        assert!(r.update(0.9, true, true, true, false));
    }

    #[test]
    fn stays_while_in_zone_or_open_then_hides() {
        let mut r = Reveal::default();
        r.update(0.0, true, true, true, false);
        assert!(r.update(0.4, true, true, true, false));
        assert!(r.update(1.0, true, true, true, false));
        // Pointer on the panel, away from the zone: still revealed.
        assert!(r.update(2.0, true, true, false, true));
        // Panel closed and pointer gone: hidden again, and needs a new dwell.
        assert!(!r.update(3.0, true, true, false, false));
        assert!(!r.update(3.1, true, true, true, false));
    }

    #[test]
    fn setting_off_never_reveals() {
        let mut r = Reveal::default();
        for t in [0.0, 0.5, 1.0] {
            assert!(!r.update(t, true, false, true, false));
        }
        assert_eq!(r.wake_at(), None);
    }

    #[test]
    fn not_fullscreen_is_irrelevant_and_resets() {
        let mut r = Reveal::default();
        r.update(0.0, true, true, true, false);
        assert!(r.update(0.4, true, true, true, false));
        assert!(!r.update(0.5, false, true, true, true));
        // A new fullscreen session starts from scratch.
        assert!(!r.update(0.6, true, true, true, false));
    }
}
