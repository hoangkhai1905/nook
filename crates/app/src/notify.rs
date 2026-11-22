//! Incoming notifications in the pill: what is showing, for how long, how
//! wide the pill gets, and what its single line says.

use nook_platform::notifications::Notification;

/// How long a notification stays up after it arrives (or after the pointer
/// leaves it).
pub const SHOW_FOR: f64 = 5.0;
/// The widest the pill grows for a notification.
pub const PILL_MAX_WIDTH: f32 = 360.0;
/// Side padding, icon, and the gap after it; mirrors `draw_notice`.
const PAD: f32 = 15.0;
pub const ICON: f32 = 18.0;
pub const ICON_GAP: f32 = 8.0;
/// Gap before the "+N" count.
pub const COUNT_GAP: f32 = 8.0;

/// The newest notification and how many more came while it was up. No queue.
pub struct Notices<T> {
    current: Option<T>,
    /// When it goes away (while not hovered).
    deadline: f64,
    /// Arrivals during this display beyond the one showing.
    more: u32,
    hovering: bool,
}

impl<T> Default for Notices<T> {
    fn default() -> Self {
        Self {
            current: None,
            deadline: 0.0,
            more: 0,
            hovering: false,
        }
    }
}

impl<T> Notices<T> {
    /// A notification arrived: it replaces the one showing and restarts the
    /// timer; the replaced one counts toward "+N".
    pub fn push(&mut self, item: T, now: f64) {
        self.more = if self.current.is_some() {
            self.more + 1
        } else {
            0
        };
        self.current = Some(item);
        self.deadline = now + SHOW_FOR;
    }

    /// Advances the timer. While the pointer is on the notification it stays
    /// up; the full time restarts when the pointer leaves. Returns when to
    /// look again (`None`: nothing is waiting on the clock).
    pub fn tick(&mut self, now: f64, hovered: bool) -> Option<f64> {
        self.current.as_ref()?;
        if hovered {
            self.hovering = true;
            return None;
        }
        if self.hovering {
            self.hovering = false;
            self.deadline = now + SHOW_FOR;
        }
        if now >= self.deadline {
            self.take();
            return None;
        }
        Some(self.deadline)
    }

    /// Dismisses the notification showing and returns it.
    pub fn take(&mut self) -> Option<T> {
        self.more = 0;
        self.hovering = false;
        self.current.take()
    }

    pub fn current(&self) -> Option<&T> {
        self.current.as_ref()
    }

    pub fn more(&self) -> u32 {
        self.more
    }
}

/// The pill's text: a title and, when text is shown, the body's first line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub title: String,
    pub body: Option<String>,
}

pub fn line(n: &Notification, show_text: bool) -> Line {
    let title = if n.title.is_empty() {
        &n.app_name
    } else {
        &n.title
    };
    Line {
        title: title.clone(),
        body: show_text
            .then(|| first_line(&n.body))
            .filter(|b| !b.is_empty()),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// The notification card: app name, title, and body (up to two lines).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub app: String,
    pub title: String,
    pub body: Option<String>,
}

/// Card size: fixed width, height without and with the "+N more" row.
pub const CARD_W: f32 = 360.0;
pub const CARD_H: f32 = 112.0;
pub const CARD_H_MORE: f32 = 130.0;

/// With text off the card shows only the app (name and logo).
pub fn card(n: &Notification, show_text: bool) -> Card {
    let title = if show_text && !n.title.is_empty() {
        n.title.clone()
    } else {
        n.app_name.clone()
    };
    let body = n
        .body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Card {
        app: n.app_name.clone(),
        title,
        body: (show_text && !body.is_empty()).then_some(body),
    }
}

/// Pill width for a line `text_w` wide and a "+N" `count_w` wide (0 for
/// none), between the normal pill (`min_w`) and [`PILL_MAX_WIDTH`]; longer
/// text ends in "…".
pub fn pill_width(text_w: f32, count_w: f32, min_w: f32) -> f32 {
    let count = if count_w > 0.0 {
        COUNT_GAP + count_w
    } else {
        0.0
    };
    // +2: rounding slack so text that fits isn't cut to "…".
    let content = PAD + ICON + ICON_GAP + text_w + 2.0 + count + PAD;
    content.clamp(min_w, PILL_MAX_WIDTH.max(min_w))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(title: &str, body: &str) -> Notification {
        Notification {
            app_name: "Slack".into(),
            app_id: "Slack".into(),
            title: title.into(),
            body: body.into(),
            extra: Vec::new(),
            icon: None,
        }
    }

    #[test]
    fn shows_for_five_seconds_then_goes() {
        let mut n = Notices::default();
        assert_eq!(n.tick(0.0, false), None);
        n.push("a", 10.0);
        assert_eq!(n.tick(10.0, false), Some(15.0));
        assert_eq!(n.tick(14.9, false), Some(15.0));
        assert!(n.current().is_some());
        assert_eq!(n.tick(15.0, false), None);
        assert!(n.current().is_none());
    }

    #[test]
    fn hover_keeps_it_up_and_the_time_restarts_on_leave() {
        let mut n = Notices::default();
        n.push("a", 0.0);
        assert_eq!(n.tick(4.0, true), None);
        assert_eq!(n.tick(60.0, true), None, "still up long after 5 s");
        assert!(n.current().is_some());
        // Pointer leaves at 60: another full 5 s.
        assert_eq!(n.tick(60.0, false), Some(65.0));
        assert!(n.current().is_some());
        assert_eq!(n.tick(65.0, false), None);
        assert!(n.current().is_none());
    }

    #[test]
    fn bursts_show_the_newest_with_a_count() {
        let mut n = Notices::default();
        n.push("a", 0.0);
        assert_eq!(n.more(), 0);
        n.push("b", 1.0);
        n.push("c", 2.0);
        assert_eq!(n.current(), Some(&"c"));
        assert_eq!(n.more(), 2);
        // A new arrival restarts the five seconds.
        assert_eq!(n.tick(3.0, false), Some(7.0));
        // After it is gone the count starts over.
        assert_eq!(n.tick(7.0, false), None);
        n.push("d", 20.0);
        assert_eq!(n.more(), 0);
        // Dismissing (a click) clears it too.
        n.push("e", 21.0);
        assert_eq!(n.take(), Some("e"));
        assert_eq!(n.more(), 0);
        assert!(n.current().is_none());
    }

    #[test]
    fn line_is_title_and_first_body_line() {
        let l = line(&notification("Alice", "\n  hello\nsecond line"), true);
        assert_eq!(l.title, "Alice");
        assert_eq!(l.body.as_deref(), Some("hello"));
        // Text off: sender only.
        assert_eq!(line(&notification("Alice", "hello"), false).body, None);
        // No title: the app name stands in; no body: none.
        let l = line(&notification("", ""), true);
        assert_eq!((l.title.as_str(), l.body), ("Slack", None));
    }

    #[test]
    fn card_hides_text_when_off() {
        let n = notification("Alice", "hi\n\n there");
        let c = card(&n, true);
        assert_eq!(
            (c.title.as_str(), c.body.as_deref()),
            ("Alice", Some("hi there"))
        );
        let c = card(&n, false);
        assert_eq!((c.title.as_str(), c.body), ("Slack", None));
        assert_eq!(card(&notification("", ""), true).title, "Slack");
    }

    #[test]
    fn width_stays_between_the_pill_and_the_cap() {
        let min = 140.0;
        assert_eq!(pill_width(0.0, 0.0, min), min);
        assert_eq!(pill_width(10_000.0, 0.0, min), PILL_MAX_WIDTH);
        assert_eq!(pill_width(10_000.0, 20.0, min), PILL_MAX_WIDTH);
        let a = pill_width(100.0, 0.0, min);
        let b = pill_width(140.0, 0.0, min);
        assert!(min < a && a < b && b < PILL_MAX_WIDTH, "{a} {b}");
        // The count needs room of its own.
        assert!(pill_width(100.0, 20.0, min) > a);
        // A cap below the minimum never shrinks the pill.
        assert_eq!(pill_width(500.0, 0.0, 400.0), 400.0);
    }
}
