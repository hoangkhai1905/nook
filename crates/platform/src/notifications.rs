//! Incoming Windows toast notifications, through the system notification
//! listener (`UserNotificationListener`).
//!
//! One worker thread reads the current toasts and hands the ones it has not
//! seen before to the UI through a channel; `on_change` wakes the UI. It
//! subscribes to `NotificationChanged` and falls back to polling once a
//! second when an unpackaged app is not allowed to (see `run`). Notification
//! text is never logged, only app names and counts.

use crate::media::{self, Art};
use crate::{PlatformError, Result, Win32Context};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;
use windows::Foundation::{Size, TypedEventHandler};
use windows::Storage::Streams::IRandomAccessStreamReference;
use windows::UI::Notifications::Management::{
    UserNotificationListener as Listener, UserNotificationListenerAccessStatus as Access,
};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::Shell::{
    QUERY_USER_NOTIFICATION_STATE, QUNS_ACCEPTS_NOTIFICATIONS, SHQueryUserNotificationState,
};
use windows::core::Interface;

/// How often the worker polls when it has no change event to wait for.
const POLL: Duration = Duration::from_secs(1);
/// With events: a slower safety refresh in case one is never raised.
const SAFETY_POLL: Duration = Duration::from_secs(3);

/// A toast that just arrived.
#[derive(Clone, Debug)]
pub struct Notification {
    /// The app's display name ("Slack").
    pub app_name: String,
    /// The app's AppUserModelID, for finding or launching it.
    pub app_id: String,
    pub title: String,
    pub body: String,
    /// Further text lines (often the site of a browser notification), for
    /// finding the right tab.
    pub extra: Vec<String>,
    pub icon: Option<Arc<Art>>,
}

impl Notification {
    /// Window/tab title hints in priority order: the extra lines (site
    /// attribution), then the title.
    pub fn hints(&self) -> Vec<&str> {
        self.extra
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(self.title.as_str()))
            .collect()
    }
}

/// Handle to the worker thread. Dropping it stops the worker.
pub struct NotificationWatcher {
    rx: Receiver<Notification>,
    // Closing this ends the worker's wait.
    _tx: Sender<()>,
}

impl NotificationWatcher {
    /// Starts the worker. `on_change` runs (on the worker thread) after new
    /// notifications were queued.
    pub fn spawn(on_change: impl Fn() + Send + 'static) -> Result<Self> {
        let (tx, worker_rx) = mpsc::channel();
        let (out, rx) = mpsc::channel();
        let wake = tx.clone();
        std::thread::Builder::new()
            .name("nook-notifications".into())
            .spawn(move || {
                if let Err(e) = run(&worker_rx, &wake, &out, &on_change) {
                    tracing::warn!("notifications unavailable: {e}");
                }
            })
            .map_err(|e| PlatformError::Other(format!("spawning notification thread: {e}")))?;
        Ok(Self { rx, _tx: tx })
    }

    /// Notifications that arrived since the last call, oldest first.
    pub fn poll(&self) -> Vec<Notification> {
        self.rx.try_iter().collect()
    }
}

fn run(
    rx: &Receiver<()>,
    wake: &Sender<()>,
    out: &Sender<Notification>,
    on_change: &dyn Fn(),
) -> Result<()> {
    // SAFETY: initializes WinRT for this worker thread only.
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.api("RoInitialize")?;
    let listener = Listener::Current().api("UserNotificationListener")?;
    let mut status = listener.GetAccessStatus().api("GetAccessStatus")?;
    if status == Access::Unspecified {
        status = listener
            .RequestAccessAsync()
            .api("RequestAccessAsync")?
            .join()
            .api("RequestAccessAsync.join")?;
        tracing::info!("notification access requested: {status:?}");
    }
    if status != Access::Allowed {
        return Err(PlatformError::Other(format!(
            "notification access is {status:?}"
        )));
    }

    // Unpackaged apps may not be allowed to subscribe: poll then.
    let w = wake.clone();
    let wait = match listener.NotificationChanged(&TypedEventHandler::new(move |_, _| {
        let _ = w.send(());
        Ok(())
    })) {
        Ok(_) => {
            tracing::info!("notifications: change events");
            SAFETY_POLL
        }
        Err(e) => {
            tracing::info!("notifications: polling, no change events ({e})");
            POLL
        }
    };

    let mut seen = Seen::default();
    loop {
        match fetch(&listener, &mut seen) {
            Ok(fresh) if !fresh.is_empty() => {
                tracing::debug!("{} new notification(s)", fresh.len());
                for n in fresh {
                    if out.send(n).is_err() {
                        return Ok(());
                    }
                }
                on_change();
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("notifications: {e}"),
        }
        match rx.recv_timeout(wait) {
            Ok(()) => {
                // Events come in bursts.
                std::thread::sleep(Duration::from_millis(50));
                while rx.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// Reads the current toasts and returns those not seen before, oldest first.
fn fetch(listener: &Listener, seen: &mut Seen) -> Result<Vec<Notification>> {
    let items = listener
        .GetNotificationsAsync(NotificationKinds::Toast)
        .api("GetNotificationsAsync")?
        .join()
        .api("GetNotificationsAsync.join")?;
    let items: Vec<UserNotification> = items.into_iter().collect();
    let ids: Vec<u32> = items.iter().map(|n| n.Id().unwrap_or_default()).collect();
    let mut fresh: Vec<(i64, Notification)> = Vec::new();
    for i in seen.fresh(&ids) {
        let n = &items[i];
        let created = n
            .CreationTime()
            .map(|t| t.UniversalTime)
            .unwrap_or_default();
        match read(n) {
            Ok(n) => fresh.push((created, n)),
            Err(e) => tracing::debug!("notification {}: {e}", ids[i]),
        }
    }
    fresh.sort_by_key(|(created, _)| *created);
    Ok(fresh.into_iter().map(|(_, n)| n).collect())
}

fn read(n: &UserNotification) -> Result<Notification> {
    let app = n.AppInfo().api("AppInfo")?;
    let display = app.DisplayInfo().api("DisplayInfo")?;
    let app_name = display
        .DisplayName()
        .map(|h| h.to_string())
        .unwrap_or_default();
    let app_id = app
        .AppUserModelId()
        .map(|h| h.to_string())
        .ok()
        .filter(|id| !id.is_empty())
        .or_else(|| app.PackageFamilyName().ok().map(|h| h.to_string()))
        .unwrap_or_default();

    let texts: Vec<String> = n
        .Notification()
        .and_then(|n| n.Visual())
        .and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric()?))
        .and_then(|b| b.GetTextElements())
        .map(|els| {
            els.into_iter()
                .filter_map(|t| t.Text().ok())
                .map(|t| t.to_string())
                .collect()
        })
        .unwrap_or_default();
    let Texts { title, body, extra } = split_texts(texts);

    let icon = display
        .GetLogo(Size {
            Width: 32.0,
            Height: 32.0,
        })
        .and_then(|logo| logo.cast::<IRandomAccessStreamReference>())
        .api("GetLogo")
        .and_then(|logo| media::read_stream(&logo))
        .and_then(|bytes| media::decode_art(&bytes))
        .inspect_err(|e| tracing::debug!("notification icon of {app_name}: {e}"))
        .ok()
        .map(Arc::new);

    Ok(Notification {
        app_name,
        app_id,
        title,
        body,
        extra,
        icon,
    })
}

/// A toast's text lines by role.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Texts {
    pub title: String,
    pub body: String,
    pub extra: Vec<String>,
}

/// First non-empty line is the title, the second the body, the rest extras.
pub fn split_texts(texts: Vec<String>) -> Texts {
    let mut lines = texts
        .into_iter()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    Texts {
        title: lines.next().unwrap_or_default(),
        body: lines.next().unwrap_or_default(),
        extra: lines.collect(),
    }
}

/// Which notification ids are new since the last look.
#[derive(Default)]
struct Seen {
    ids: HashSet<u32>,
    primed: bool,
}

impl Seen {
    /// Indices into `current` of ids not seen before. The first call only
    /// records what is already there (toasts from before startup are not
    /// news); later calls forget ids that went away.
    fn fresh(&mut self, current: &[u32]) -> Vec<usize> {
        let first = !self.primed;
        self.primed = true;
        let new = current
            .iter()
            .enumerate()
            .filter(|(_, id)| !self.ids.contains(id))
            .map(|(i, _)| i)
            .collect();
        self.ids = current.iter().copied().collect();
        if first { Vec::new() } else { new }
    }
}

/// Whether the shell state allows showing a notification: only when it would
/// itself show a toast (not fullscreen, presentation, Do Not Disturb/Focus).
pub fn accepts(state: QUERY_USER_NOTIFICATION_STATE) -> bool {
    state == QUNS_ACCEPTS_NOTIFICATIONS
}

/// [`accepts`] for the system right now; shown when the query fails.
pub fn can_show_now() -> bool {
    // SAFETY: read-only shell query.
    let state = unsafe { SHQueryUserNotificationState() }.unwrap_or(QUNS_ACCEPTS_NOTIFICATIONS);
    accepts(state)
}

/// True for toasts Nook raised itself.
pub fn is_own(app_id: &str, app_name: &str) -> bool {
    app_name.eq_ignore_ascii_case("nook") || app_id.to_ascii_lowercase().contains("nook")
}

/// An AppUserModelID safe to hand to Explorer: plain id characters only, no
/// path tricks.
fn valid_aumid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._!-{}\\".contains(c))
}

/// Brings the notification's app forward (selecting the tab matching `hints`
/// in a browser); an app without a window is launched from its AppUserModelID.
pub fn bring_forward(app_id: &str, hints: &[&str]) -> Result<()> {
    if media::focus_app(app_id, hints)? {
        return Ok(());
    }
    if !valid_aumid(app_id) {
        return Err(PlatformError::Other(
            "no window and no launchable id".into(),
        ));
    }
    // One argument, no shell: the id cannot inject anything.
    std::process::Command::new("explorer.exe")
        .arg(format!("shell:AppsFolder\\{app_id}"))
        .spawn()
        .map(|_| ())
        .map_err(|e| PlatformError::Other(format!("launching {app_id}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Shell::{
        QUNS_BUSY, QUNS_NOT_PRESENT, QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };

    #[test]
    fn startup_snapshot_is_not_news_but_later_arrivals_are() {
        let mut seen = Seen::default();
        assert!(seen.fresh(&[1, 2, 3]).is_empty(), "already there at start");
        assert!(seen.fresh(&[1, 2, 3]).is_empty(), "nothing changed");
        assert_eq!(seen.fresh(&[1, 2, 3, 7, 5]), vec![3, 4]);
        assert!(seen.fresh(&[1, 2, 3, 7, 5]).is_empty(), "reported once");
        // Dismissed ones go away without being news.
        assert!(seen.fresh(&[7]).is_empty());
        assert_eq!(seen.fresh(&[7, 8]), vec![1]);
    }

    #[test]
    fn empty_start_then_first_arrival_is_news() {
        let mut seen = Seen::default();
        assert!(seen.fresh(&[]).is_empty());
        assert_eq!(seen.fresh(&[4]), vec![0]);
    }

    #[test]
    fn text_roles_by_order() {
        let t = |v: &[&str]| split_texts(v.iter().map(|s| (*s).to_owned()).collect());
        assert_eq!(
            t(&["Alice", "Hello there", "facebook.com", "more"]),
            Texts {
                title: "Alice".into(),
                body: "Hello there".into(),
                extra: vec!["facebook.com".into(), "more".into()],
            }
        );
        // Blank lines don't take a role.
        assert_eq!(t(&["  ", "Title", "", "Body"]).body, "Body");
        assert_eq!(t(&["Only title"]).body, "");
        assert_eq!(t(&[]), Texts::default());
    }

    #[test]
    fn hints_put_the_site_before_the_title() {
        let n = Notification {
            app_name: "Chrome".into(),
            app_id: "Chrome".into(),
            title: "New message".into(),
            body: String::new(),
            extra: vec!["messenger.com".into()],
            icon: None,
        };
        assert_eq!(n.hints(), vec!["messenger.com", "New message"]);
    }

    #[test]
    fn only_a_quiet_desktop_shows_notifications() {
        assert!(accepts(QUNS_ACCEPTS_NOTIFICATIONS));
        for s in [
            QUNS_NOT_PRESENT,
            QUNS_BUSY,
            QUNS_RUNNING_D3D_FULL_SCREEN,
            QUNS_PRESENTATION_MODE,
            QUNS_QUIET_TIME,
        ] {
            assert!(!accepts(s), "{s:?}");
        }
    }

    #[test]
    fn nook_toasts_are_ignored() {
        assert!(is_own("com.nook.app", "Nook"));
        assert!(is_own("whatever", "NOOK"));
        assert!(!is_own("Chrome", "Google Chrome"));
    }

    #[test]
    fn launch_ids_are_plain() {
        assert!(valid_aumid("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"));
        assert!(valid_aumid("Chrome"));
        assert!(valid_aumid(
            "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\cmd.exe"
        ));
        assert!(!valid_aumid(""));
        assert!(!valid_aumid("a & calc"));
        assert!(!valid_aumid("a\"b"));
        assert!(!valid_aumid("a|b"));
        assert!(!valid_aumid("..\\..\\evil"));
    }
}
