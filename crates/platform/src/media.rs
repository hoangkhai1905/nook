//! Now-playing info and transport controls through the system media
//! transport controls (SMTC, `GlobalSystemMediaTransportControlsSessionManager`).
//!
//! All WinRT work happens on one worker thread. It subscribes to the
//! manager's and the current session's change events, so it sleeps until
//! something actually changes (plus a slow safety refresh) and costs nothing
//! while the music is idle. Snapshots go to the UI through a channel, and
//! `on_change` wakes the UI.

use crate::{PlatformError, Result, Win32Context};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationInvokePattern,
    IUIAutomationLegacyIAccessiblePattern, IUIAutomationSelectionItemPattern,
    TreeScope_Descendants, UIA_ControlTypePropertyId, UIA_InvokePatternId,
    UIA_LegacyIAccessiblePatternId, UIA_SelectionItemPatternId, UIA_TabItemControlTypeId,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, SW_RESTORE, ShowWindow, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

/// Album art, decoded and downscaled to at most [`ART_SIZE`] square.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Art {
    pub width: usize,
    pub height: usize,
    /// Unpremultiplied RGBA8.
    pub rgba: Vec<u8>,
}

pub const ART_SIZE: u32 = 96;

/// What is playing right now.
#[derive(Clone, Debug)]
pub struct NowPlaying {
    /// Cleaned-up app id, e.g. "Spotify" or "chrome".
    pub source: String,
    /// The session's raw `SourceAppUserModelId`, for finding its window.
    pub app_id: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_toggle: bool,
    /// Position and duration in seconds when the app reports a timeline.
    pub timeline: Option<(f64, f64)>,
    /// When `timeline` was sampled; a playing position advances from here.
    pub captured: Instant,
    /// Shared between snapshots of the same track.
    pub art: Option<Arc<Art>>,
}

impl NowPlaying {
    /// Position and duration (seconds) extrapolated to now.
    pub fn position(&self) -> Option<(f64, f64)> {
        let (pos, dur) = self.timeline?;
        let pos = if self.playing {
            pos + self.captured.elapsed().as_secs_f64()
        } else {
            pos
        };
        Some((pos.clamp(0.0, dur), dur))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    PlayPause,
    Next,
    Previous,
}

enum Msg {
    Wake,
    Command(MediaCommand),
}

/// Handle to the worker thread. Dropping it stops the worker.
pub struct MediaWatcher {
    tx: Sender<Msg>,
    rx: Receiver<Option<NowPlaying>>,
}

impl MediaWatcher {
    /// Starts the worker. `on_change` runs (on the worker thread) after each
    /// new snapshot.
    pub fn spawn(on_change: impl Fn() + Send + 'static) -> Result<Self> {
        let (tx, worker_rx) = mpsc::channel();
        let (snap_tx, rx) = mpsc::channel();
        let wake = tx.clone();
        std::thread::Builder::new()
            .name("nook-media".into())
            .spawn(move || {
                if let Err(e) = run(&worker_rx, &wake, &snap_tx, &on_change) {
                    tracing::warn!("media controls unavailable: {e}");
                }
            })
            .map_err(|e| PlatformError::Other(format!("spawning media thread: {e}")))?;
        Ok(Self { tx, rx })
    }

    /// Latest snapshot since the last call, if any (`Some(None)` means the
    /// session went away).
    pub fn poll(&self) -> Option<Option<NowPlaying>> {
        self.rx.try_iter().last()
    }

    pub fn send(&self, cmd: MediaCommand) {
        let _ = self.tx.send(Msg::Command(cmd));
    }
}

/// The current session and the event subscriptions on it.
struct Watched {
    id: String,
    session: Session,
    /// Event registration tokens: properties, playback, timeline.
    tokens: [i64; 3],
}

impl Watched {
    fn new(session: Session, id: String, wake: &Sender<Msg>) -> Result<Self> {
        let w = wake.clone();
        let props = session
            .MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                let _ = w.send(Msg::Wake);
                Ok(())
            }))
            .api("MediaPropertiesChanged")?;
        let w = wake.clone();
        let playback = session
            .PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                let _ = w.send(Msg::Wake);
                Ok(())
            }))
            .api("PlaybackInfoChanged")?;
        let w = wake.clone();
        let timeline = session
            .TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                let _ = w.send(Msg::Wake);
                Ok(())
            }))
            .api("TimelinePropertiesChanged")?;
        Ok(Self {
            id,
            session,
            tokens: [props, playback, timeline],
        })
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        let [props, playback, timeline] = self.tokens;
        let _ = self.session.RemoveMediaPropertiesChanged(props);
        let _ = self.session.RemovePlaybackInfoChanged(playback);
        let _ = self.session.RemoveTimelinePropertiesChanged(timeline);
    }
}

/// Hash of the thumbnail's bytes: the art is decoded again only when it
/// changes (see `snapshot` for why it isn't keyed on the track).
type ArtKey = u64;

fn run(
    rx: &Receiver<Msg>,
    wake: &Sender<Msg>,
    out: &Sender<Option<NowPlaying>>,
    on_change: &dyn Fn(),
) -> Result<()> {
    // SAFETY: initializes WinRT for this worker thread only.
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.api("RoInitialize")?;
    let manager = Manager::RequestAsync()
        .api("RequestAsync")?
        .join()
        .api("RequestAsync.join")?;
    let w = wake.clone();
    manager
        .CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
            let _ = w.send(Msg::Wake);
            Ok(())
        }))
        .api("CurrentSessionChanged")?;
    // An app opening or closing its session may change who is shown even
    // when Windows' current session does not.
    let w = wake.clone();
    manager
        .SessionsChanged(&TypedEventHandler::new(move |_, _| {
            let _ = w.send(Msg::Wake);
            Ok(())
        }))
        .api("SessionsChanged")?;

    let mut watched: Option<Watched> = None;
    let mut art: Option<(ArtKey, Option<Arc<Art>>)> = None;
    loop {
        let session = active_session(&manager);
        let id = session
            .as_ref()
            .and_then(|s| s.SourceAppUserModelId().ok())
            .map(|h| h.to_string());
        if watched.as_ref().map(|w| &w.id) != id.as_ref() {
            watched = match (session, id) {
                (Some(s), Some(id)) => Watched::new(s, id, wake)
                    .inspect_err(|e| tracing::debug!("media session events: {e}"))
                    .ok(),
                _ => None,
            };
        }
        let snapshot = watched
            .as_ref()
            .and_then(|w| match snapshot(&w.session, &w.id, &mut art) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::debug!("media snapshot: {e}");
                    None
                }
            });
        if out.send(snapshot).is_err() {
            return Ok(());
        }
        on_change();

        // Sleep until something changes; a slow refresh covers apps that
        // forget to raise events.
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(msg) => {
                // Events come in bursts (title, art and playback together).
                std::thread::sleep(Duration::from_millis(40));
                for msg in std::iter::once(msg).chain(rx.try_iter()) {
                    if let (Msg::Command(cmd), Some(w)) = (msg, &watched)
                        && let Err(e) = command(&w.session, cmd)
                    {
                        tracing::debug!("media command {cmd:?}: {e}");
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// The session to show: Windows' current one unless it is paused while
/// another app plays (see [`pick_session`]).
fn active_session(manager: &Manager) -> Option<Session> {
    let current = manager.GetCurrentSession().ok();
    let Ok(all) = manager.GetSessions() else {
        return current;
    };
    let all: Vec<Session> = all.into_iter().collect();
    let playing: Vec<bool> = all
        .iter()
        .map(|s| {
            s.GetPlaybackInfo()
                .and_then(|i| i.PlaybackStatus())
                .is_ok_and(|st| st == Status::Playing)
        })
        .collect();
    let id_of = |s: &Session| s.SourceAppUserModelId().ok().map(|h| h.to_string());
    let current_id = current.as_ref().and_then(id_of);
    let cur_idx = current_id
        .as_ref()
        .and_then(|c| all.iter().position(|s| id_of(s).as_ref() == Some(c)));
    match pick_session(&playing, cur_idx) {
        Some(i) => all.into_iter().nth(i),
        None => current,
    }
}

fn command(session: &Session, cmd: MediaCommand) -> Result<()> {
    let op = match cmd {
        MediaCommand::PlayPause => session.TryTogglePlayPauseAsync(),
        MediaCommand::Next => session.TrySkipNextAsync(),
        MediaCommand::Previous => session.TrySkipPreviousAsync(),
    }
    .api("TryControlAsync")?;
    op.join().api("TryControlAsync.join")?;
    Ok(())
}

fn snapshot(
    session: &Session,
    id: &str,
    art_cache: &mut Option<(ArtKey, Option<Arc<Art>>)>,
) -> Result<NowPlaying> {
    let props = session
        .TryGetMediaPropertiesAsync()
        .api("TryGetMediaPropertiesAsync")?
        .join()
        .api("TryGetMediaPropertiesAsync.join")?;
    let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
    let artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();

    let info = session.GetPlaybackInfo().api("GetPlaybackInfo")?;
    let playing = info.PlaybackStatus().ok() == Some(Status::Playing);
    let controls = info.Controls().api("PlaybackInfo.Controls")?;

    // Browsers hand out the previous track's thumbnail for a moment after
    // the title changes and fire another properties event once the real one
    // is in, so the art follows the thumbnail's bytes, not the track name.
    let art = match read_thumbnail(&props) {
        Ok(bytes) => {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&bytes, &mut h);
            let key = std::hash::Hasher::finish(&h);
            match art_cache {
                Some((k, art)) if *k == key => art.clone(),
                _ => {
                    let art = decode_art(&bytes)
                        .inspect_err(|e| tracing::debug!("album art: {e}"))
                        .ok()
                        .map(Arc::new);
                    *art_cache = Some((key, art.clone()));
                    art
                }
            }
        }
        Err(e) => {
            tracing::debug!("album art: {e}");
            *art_cache = None;
            None
        }
    };

    Ok(NowPlaying {
        source: clean_source(id),
        app_id: id.to_owned(),
        title,
        artist,
        playing,
        can_previous: controls.IsPreviousEnabled().unwrap_or(false),
        can_next: controls.IsNextEnabled().unwrap_or(false),
        can_toggle: controls.IsPlayPauseToggleEnabled().unwrap_or(false),
        timeline: timeline(session, playing),
        captured: Instant::now(),
        art,
    })
}

/// 100 ns ticks per second (WinRT `TimeSpan` / `DateTime` units).
const TICKS: f64 = 10_000_000.0;

fn timeline(session: &Session, playing: bool) -> Option<(f64, f64)> {
    let t = session.GetTimelineProperties().ok()?;
    let start = t.StartTime().ok()?.Duration;
    let end = t.EndTime().ok()?.Duration;
    let pos = t.Position().ok()?.Duration;
    let dur = (end - start) as f64 / TICKS;
    if dur <= 0.0 {
        return None;
    }
    let mut pos = (pos - start) as f64 / TICKS;
    // The position is as of `LastUpdatedTime`; carry it forward to now.
    if playing && let Ok(updated) = t.LastUpdatedTime() {
        let since = (now_ticks() - updated.UniversalTime) as f64 / TICKS;
        if since > 0.0 {
            pos += since;
        }
    }
    Some((pos.clamp(0.0, dur), dur))
}

/// Now as a WinRT `DateTime` (100 ns ticks since 1601-01-01 UTC).
fn now_ticks() -> i64 {
    const EPOCH_DIFF: i64 = 11_644_473_600;
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (unix.as_secs() as i64 + EPOCH_DIFF) * 10_000_000 + i64::from(unix.subsec_nanos() / 100)
}

fn read_thumbnail(
    props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties,
) -> Result<Vec<u8>> {
    read_stream(&props.Thumbnail().api("Thumbnail")?)
}

/// Reads all bytes behind a WinRT stream reference (album art, app logos).
pub(crate) fn read_stream(thumb: &IRandomAccessStreamReference) -> Result<Vec<u8>> {
    let stream = thumb
        .OpenReadAsync()
        .api("OpenReadAsync")?
        .join()
        .api("OpenReadAsync.join")?;
    let size = u32::try_from(stream.Size().api("Size")?)
        .map_err(|_| PlatformError::Other("album art too large".into()))?;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).api("GetInputStreamAt")?)
        .api("CreateDataReader")?;
    reader
        .LoadAsync(size)
        .api("LoadAsync")?
        .join()
        .api("LoadAsync.join")?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes).api("ReadBytes")?;
    Ok(bytes)
}

pub(crate) fn decode_art(bytes: &[u8]) -> Result<Art> {
    let image = image::load_from_memory(bytes)
        .map_err(|e| PlatformError::Other(format!("decoding album art: {e}")))?
        .thumbnail(ART_SIZE, ART_SIZE)
        .to_rgba8();
    Ok(Art {
        width: image.width() as usize,
        height: image.height() as usize,
        rgba: image.into_raw(),
    })
}

/// "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify" -> "Spotify",
/// "chrome.exe" -> "chrome". Opaque ids (mostly hex, e.g. some browsers'
/// per-tab ids) come back empty rather than as noise.
pub fn clean_source(id: &str) -> String {
    let name = id.rsplit('!').next().unwrap_or(id);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    let name = name.rsplit(['\\', '/']).next().unwrap_or(name);
    // Only hex digits (and GUID punctuation): "foobar2000" is mostly hex
    // by count yet a real name.
    if name.len() >= 8
        && name
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, '-' | '{' | '}'))
    {
        return String::new();
    }
    name.to_owned()
}

/// True if a window belongs to the media app `app_id`: its explicit
/// AppUserModelID equals it (Chromium sets one per window, PWAs included), or
/// the window's executable name matches the cleaned id ("msedge" for
/// "MSEdge" or "msedge.exe").
fn app_matches(app_id: &str, explicit: Option<&str>, exe_stem: Option<&str>) -> bool {
    if explicit.is_some_and(|e| e.eq_ignore_ascii_case(app_id) || same_package(app_id, e)) {
        return true;
    }
    let name = clean_source(app_id);
    !name.is_empty() && exe_stem.is_some_and(|s| s.eq_ignore_ascii_case(&name))
}

/// Both are packaged ids ("Family!App") of the same package family: the
/// Media Player's session says "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"
/// while its window may carry another entry of the package, and its exe
/// ("Microsoft.Media.Player") bears no resemblance to either.
fn same_package(a: &str, b: &str) -> bool {
    match (a.split_once('!'), b.split_once('!')) {
        (Some((x, _)), Some((y, _))) => x.eq_ignore_ascii_case(y),
        _ => false,
    }
}

/// Executables of browsers whose tabs are searched through UI Automation
/// (Chromium forks expose the same tab strip as Edge; Firefox its own).
/// Other apps are never scanned: walking an Electron app's tree through UI
/// Automation switches its accessibility mode on for nothing.
fn is_browser(exe_stem: &str) -> bool {
    const BROWSERS: [&str; 10] = [
        "chrome",
        "msedge",
        "brave",
        "opera",
        "vivaldi",
        "firefox",
        "chromium",
        "arc",
        "librewolf",
        "waterfox",
    ];
    BROWSERS.iter().any(|b| exe_stem.eq_ignore_ascii_case(b))
}

/// Index of the session to show: the current one (Windows' own idea of the
/// active session, which follows the most recent playback) if it is playing,
/// else the first playing one, else the current one.
fn pick_session(playing: &[bool], current: Option<usize>) -> Option<usize> {
    match current {
        Some(c) if playing.get(c) == Some(&true) => Some(c),
        _ => playing.iter().position(|&p| p).or(current),
    }
}

/// Index of the window to focus among `(belongs to the app, title)`
/// candidates in z-order: the first app window whose title contains a hint
/// (hints in priority order), else the first app window.
fn pick_window(candidates: &[(bool, String)], hints: &[&str]) -> Option<usize> {
    let first = candidates.iter().position(|c| c.0)?;
    let by_title = hints.iter().find_map(|h| {
        let needle = h.trim().to_lowercase();
        if needle.is_empty() {
            return None;
        }
        candidates
            .iter()
            .position(|c| c.0 && c.1.to_lowercase().contains(&needle))
    });
    Some(by_title.unwrap_or(first))
}

/// Index of the first tab whose name contains a hint, trying hints in
/// priority order (case-insensitive, so "<title> - YouTube - Audio playing"
/// still matches).
fn pick_tab(names: &[String], hints: &[&str]) -> Option<usize> {
    hints.iter().find_map(|h| {
        let needle = h.trim().to_lowercase();
        if needle.is_empty() {
            return None;
        }
        names
            .iter()
            .position(|n| n.to_lowercase().contains(&needle))
    })
}

/// Selects the browser tab matching a hint in `hwnd` through UI Automation.
/// True if a tab was found and selected.
fn select_tab(uia: &IUIAutomation, hwnd: HWND, hints: &[&str]) -> windows::core::Result<bool> {
    // SAFETY: COM calls on interfaces owned by this thread.
    unsafe {
        let root = uia.ElementFromHandle(hwnd)?;
        let cond = uia.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(UIA_TabItemControlTypeId.0),
        )?;
        let mut tabs = root.FindAll(TreeScope_Descendants, &cond)?;
        if tabs.Length()? == 0 {
            // Chromium builds its accessibility tree on the first UI
            // Automation request; give it a moment, then ask again.
            std::thread::sleep(Duration::from_millis(300));
            tabs = root.FindAll(TreeScope_Descendants, &cond)?;
        }
        let n = tabs.Length()?;
        let mut names = Vec::new();
        for i in 0..n {
            names.push(
                tabs.GetElement(i)?
                    .CurrentName()
                    .map(|b| b.to_string())
                    .unwrap_or_default(),
            );
        }
        let Some(i) = pick_tab(&names, hints) else {
            return Ok(false);
        };
        let tab = tabs.GetElement(i as i32)?;
        if let Ok(p) =
            tab.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
        {
            return p.Select().map(|()| true);
        }
        if let Ok(p) = tab.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) {
            return p.Invoke().map(|()| true);
        }
        let p = tab.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
            UIA_LegacyIAccessiblePatternId,
        )?;
        p.DoDefaultAction().map(|()| true)
    }
}

/// Brings a window of the app `app_id` to the front (restoring it if
/// minimized), first selecting the tab matching one of `hints` (priority
/// order) when the app is a browser; else the window whose title matches a
/// hint, else its first window. Returns false when the app has no suitable
/// window. The UI Automation search and the focusing run on a short-lived
/// thread (UIA is slow and needs COM), so a true result only means "working
/// on it".
pub fn focus_app(app_id: &str, hints: &[&str]) -> Result<bool> {
    let mut found: Vec<(HWND, String)> = Vec::new();
    // SAFETY: the callback only runs during the call and `found` outlives it.
    unsafe { EnumWindows(Some(collect), LPARAM(&raw mut found as isize)) }.api("EnumWindows")?;
    // Per window: belongs to the app, and is a browser window.
    let mut flags: Vec<(bool, bool)> = Vec::with_capacity(found.len());
    let candidates: Vec<(bool, String)> = found
        .iter()
        .map(|(h, t)| {
            let explicit = explicit_app_id(*h);
            let stem = exe_stem(*h);
            let mine = app_matches(app_id, explicit.as_deref(), stem.as_deref());
            flags.push((mine, mine && stem.as_deref().is_some_and(is_browser)));
            (mine, t.clone())
        })
        .collect();
    let Some(fallback) = pick_window(&candidates, hints) else {
        return Ok(false);
    };
    // HWND isn't Send; the thread gets raw values.
    let app_windows: Vec<isize> = found
        .iter()
        .zip(&flags)
        .filter(|(_, f)| f.1)
        .map(|((h, _), _)| h.0 as isize)
        .collect();
    let fallback = found[fallback].0.0 as isize;
    let hints: Vec<String> = hints.iter().map(|h| (*h).to_owned()).collect();
    std::thread::spawn(move || {
        let target = if app_windows.is_empty() {
            fallback
        } else {
            select_hinted_tab(&app_windows, &hints).unwrap_or(fallback)
        };
        let hwnd = HWND(target as *mut _);
        // SAFETY: plain calls; stale handles just fail.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
        }
        // Nook never activates, so the foreground lock needs the same input
        // attachment trick as the embedded phone window (it attaches the
        // calling thread, so any thread works).
        crate::embed::give_focus(crate::window::Hwnd::from_raw(target));
    });
    Ok(true)
}

/// First app window (z-order) with a tab matching a hint, after selecting
/// that tab. None when there is no such tab or UI Automation is unavailable.
fn select_hinted_tab(windows: &[isize], hints: &[String]) -> Option<isize> {
    // SAFETY: COM is initialised for this short-lived thread only.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED).ok();
        let uia: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let hints: Vec<&str> = hints.iter().map(String::as_str).collect();
        windows
            .iter()
            .copied()
            .find(|&h| select_tab(&uia, HWND(h as *mut _), &hints).unwrap_or(false))
    }
}

/// `EnumWindows` callback: visible, titled, non-tool, uncloaked windows.
unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `Vec` passed by `focus_app`.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<(HWND, String)>) };
    // SAFETY: read-only window queries into local buffers.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool()
            || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
        {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        );
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        if cloaked == 0 && n > 0 {
            out.push((hwnd, String::from_utf16_lossy(&buf[..n])));
        }
    }
    true.into()
}

/// The window's explicit AppUserModelID, if it set one.
fn explicit_app_id(hwnd: HWND) -> Option<String> {
    // SAFETY: property store of a window; the PWSTR is freed after copying.
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let value = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let s = PropVariantToStringAlloc(&value).ok()?;
        let id = s.to_string().ok().filter(|id| !id.is_empty());
        CoTaskMemFree(Some(s.0 as *const _));
        id
    }
}

/// File stem of the window's process executable ("msedge").
fn exe_stem(hwnd: HWND) -> Option<String> {
    // SAFETY: query-only process handle, closed right after.
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let file = path.rsplit(['\\', '/']).next()?;
        Some(file.strip_suffix(".exe").unwrap_or(file).to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_tab_matches_the_playing_title() {
        let tabs: Vec<String> = [
            "Facebook",
            "TẬP 5 END | Trên Mạng Là Chị - YouTube - Audio playing",
            "nhạt-fine - YouTube Music",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(pick_tab(&tabs, &["TẬP 5 END | Trên Mạng Là Chị"]), Some(1));
        assert_eq!(
            pick_tab(&tabs, &["  NHẠT-FINE "]),
            Some(2),
            "trimmed, any case"
        );
        assert_eq!(pick_tab(&tabs, &[""]), None);
        assert_eq!(pick_tab(&tabs, &["   "]), None);
        assert_eq!(pick_tab(&tabs, &["not open anywhere"]), None);
        assert_eq!(pick_tab(&[], &["x"]), None);
        assert_eq!(pick_tab(&tabs, &[]), None);
        // Hints are tried in order: the first one that matches a tab wins.
        assert_eq!(
            pick_tab(&tabs, &["nope", "youtube music", "facebook"]),
            Some(2)
        );
        assert_eq!(pick_tab(&tabs, &["facebook", "youtube music"]), Some(0));
    }

    #[test]
    fn window_ownership() {
        assert!(app_matches("MSEdge", None, Some("msedge")));
        assert!(app_matches("chrome.exe", None, Some("Chrome")));
        assert!(app_matches(
            "SpotifyAB.SpotifyMusic_x!Spotify",
            None,
            Some("spotify")
        ));
        assert!(app_matches("Chrome.PWA", Some("chrome.pwa"), Some("other")));
        assert!(!app_matches("MSEdge", None, Some("chrome")));
        assert!(!app_matches(
            "F0DC299D809B9700",
            None,
            Some("F0DC299D809B9700")
        ));
        assert!(!app_matches("MSEdge", None, None));
    }

    #[test]
    fn packaged_and_classic_apps_match() {
        // Windows Media Player: packaged, exe name unrelated to the id.
        let zune = "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic";
        assert!(app_matches(
            zune,
            Some("Microsoft.ZuneMusic_8wekyb3d8bbwe!App"),
            Some("Microsoft.Media.Player")
        ));
        assert!(!app_matches(
            zune,
            Some("Other.Pkg_abc!App"),
            Some("Microsoft.Media.Player")
        ));
        // Classic ids are exe names; the window's exe matches.
        assert!(app_matches("Spotify.exe", None, Some("Spotify")));
        assert!(app_matches("vlc.exe", None, Some("vlc")));
        assert!(app_matches("foobar2000.exe", None, Some("foobar2000")));
        // Firefox: an opaque hash id, matched through the window's own.
        assert!(app_matches(
            "308046B0AF4A39CB",
            Some("308046B0AF4A39CB"),
            Some("firefox")
        ));
    }

    #[test]
    fn only_browsers_are_scanned_for_tabs() {
        for b in ["chrome", "MSEdge", "brave", "opera", "vivaldi", "firefox"] {
            assert!(is_browser(b), "{b}");
        }
        for n in ["Spotify", "vlc", "foobar2000", "Microsoft.Media.Player"] {
            assert!(!is_browser(n), "{n}");
        }
    }

    #[test]
    fn session_choice() {
        // The current session wins while it plays.
        assert_eq!(pick_session(&[true, true], Some(1)), Some(1));
        // Paused current, another plays: show the player.
        assert_eq!(pick_session(&[false, true], Some(0)), Some(1));
        // Nothing plays: stay on the current one.
        assert_eq!(pick_session(&[false, false], Some(1)), Some(1));
        // No current but something plays.
        assert_eq!(pick_session(&[false, true], None), Some(1));
        assert_eq!(pick_session(&[false], None), None);
        assert_eq!(pick_session(&[], None), None);
    }

    #[test]
    fn window_choice() {
        let c = |a: bool, t: &str| (a, t.to_owned());
        let wins = [
            c(false, "Song - Notes"),
            c(true, "Inbox"),
            c(true, "Song - YouTube"),
        ];
        assert_eq!(pick_window(&wins, &["song"]), Some(2));
        assert_eq!(pick_window(&wins, &["nothing"]), Some(1));
        assert_eq!(pick_window(&wins, &[""]), Some(1));
        assert_eq!(pick_window(&wins, &[]), Some(1));
        assert_eq!(pick_window(&wins, &["", "nothing", "inbox"]), Some(1));
        assert_eq!(pick_window(&[c(false, "Song")], &["Song"]), None);
        assert_eq!(pick_window(&[], &["Song"]), None);
    }

    #[test]
    fn source_names() {
        assert_eq!(
            clean_source("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"),
            "Spotify"
        );
        assert_eq!(clean_source("chrome.exe"), "chrome");
        assert_eq!(clean_source("C:\\Apps\\foo.exe"), "foo");
        assert_eq!(clean_source("F0DC299D809B9700"), "");
        assert_eq!(clean_source("foobar2000.exe"), "foobar2000");
    }

    #[test]
    fn paused_position_does_not_advance() {
        let np = NowPlaying {
            source: String::new(),
            app_id: String::new(),
            title: String::new(),
            artist: String::new(),
            playing: false,
            can_previous: false,
            can_next: false,
            can_toggle: false,
            timeline: Some((30.0, 100.0)),
            captured: Instant::now() - Duration::from_secs(5),
            art: None,
        };
        assert_eq!(np.position(), Some((30.0, 100.0)));
        let playing = NowPlaying {
            playing: true,
            ..np
        };
        let (pos, _) = playing.position().unwrap();
        assert!(pos >= 35.0);
    }
}
