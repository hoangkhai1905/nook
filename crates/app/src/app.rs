//! The eframe app: owns the shell, modules, tray, hotkeys and system
//! watcher, and keeps the OS window (position, region, material) in sync
//! with what the shell draws.

use crate::config::{AccentSource, Config, Material, MonitorMode};
use crate::notify::{self, Notices};
use crate::reveal::Reveal;
use crate::settings;
use crate::stats::{self, Stats};
use crate::tray::{Tray, TrayCommand};
use crate::update::{self, Updater};
use crate::watcher::{self, SystemState, WatcherControl};
use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, unbounded};
use egui::{Color32, FontFamily, FontId, Rect, Ui, pos2, vec2};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use nook_platform::backdrop::{Backdrop, Tint};
use nook_platform::hittest::{HitShape, HitTester};
use nook_platform::material::{self, Applied};
use nook_platform::notifications::{self, Notification, NotificationWatcher};
use nook_platform::single_instance::InstanceGuard;
use nook_platform::system::Battery;
use nook_platform::{Hwnd, startup, system, wallpaper, window};
use nook_ui::anim::{self, Motion};
use nook_ui::bento;
use nook_ui::geometry::Notch;
use nook_ui::module::{Badge, Module, Registry, Tab};
use nook_ui::paint;
use nook_ui::shell::{Shell, ShellMetrics, ShellOutput};
use nook_ui::shell_state::ShellTimings;
use nook_ui::theme::{self, FALLBACK_ACCENT, SIZE_BODY, SIZE_LABEL, Theme, ThemeMode};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Bento,
    Settings,
    /// A module's full-panel page (e.g. clipboard history), by module id.
    Module(&'static str),
}

/// A notification in the pill, with its app icon on the GPU.
struct Toast {
    n: Notification,
    icon: Option<egui::TextureHandle>,
}

/// What the pill draws for a notification.
struct NoticeView {
    icon: Option<egui::TextureId>,
    line: notify::Line,
    /// Further arrivals during this display ("+N").
    more: u32,
    /// The expanded card (`None`: the one-line pill, over fullscreen or an
    /// open panel).
    card: Option<notify::Card>,
}

/// Minimum gap between blur clip/tint pushes while the shell animates.
// ponytail: ~120 Hz ceiling, so on >120 Hz displays the blur edge can lag the
// painted edge by up to one throttled frame. Raise the rate or drop the
// throttle if that ever shows.
const BACKDROP_MIN_INTERVAL: Duration = Duration::from_millis(8);

/// How long to hold back a backdrop push, or `None` when one is due.
fn backdrop_wait(now: Instant, last_push: Option<Instant>, min: Duration) -> Option<Duration> {
    let left = min.checked_sub(now.saturating_duration_since(last_push?))?;
    (!left.is_zero()).then_some(left)
}

/// Shell outline (window px) and debug-overlay rect last sent to the OS.
type PushedShape = (Vec<(f32, f32)>, Option<Rect>);

struct Hotkeys {
    manager: GlobalHotKeyManager,
    toggle_id: Option<u32>,
    /// Module hotkeys and the module each belongs to.
    modules: Vec<(HotKey, &'static str)>,
    rx: Receiver<GlobalHotKeyEvent>,
}

impl Hotkeys {
    /// Registers a module's own shortcut, unless it has none, is hidden or
    /// the config turns it off (see `Config::module_hotkey`).
    fn add_module(&mut self, m: &dyn Module, cfg: &Config) {
        let Some(accel) = cfg.module_hotkey(m.id(), m.default_hotkey()) else {
            return;
        };
        match HotKey::from_str(accel).map(|hk| (self.manager.register(hk), hk)) {
            Ok((Ok(()), hk)) => self.modules.push((hk, m.id())),
            Ok((Err(e), _)) => tracing::warn!("hotkey {accel} for {} unavailable: {e}", m.id()),
            Err(e) => tracing::warn!("bad hotkey {accel:?} for {}: {e}", m.id()),
        }
    }

    /// Frees the shortcut of a module that was taken out (a plugin).
    fn remove_module(&mut self, id: &str) {
        for (hk, _) in self.modules.iter().filter(|(_, m)| *m == id) {
            if let Err(e) = self.manager.unregister(*hk) {
                tracing::warn!("releasing hotkey for {id}: {e}");
            }
        }
        self.modules.retain(|(_, m)| *m != id);
    }
}

pub struct NookApp {
    cfg: Config,
    hwnd: Hwnd,
    shell: Shell,
    registry: Registry,
    page: Page,
    /// The grid is in edit mode (the pencil in the tab bar).
    editing: bool,
    pinned_before_edit: bool,
    /// Rows the grid needed at the last frame, and for which tab: an opened
    /// tile or a resize can need more than the tab's own sizes say.
    bento_rows: (Tab, usize),
    tray: Option<Tray>,
    /// This run draws with OpenGL (else DirectX).
    on_gl: bool,
    tray_rx: Receiver<TrayCommand>,
    hotkeys: Option<Hotkeys>,
    sys: SystemState,
    sys_rx: Receiver<SystemState>,
    watcher: Arc<WatcherControl>,
    accent_tx: crossbeam_channel::Sender<Option<[u8; 3]>>,
    accent_rx: Receiver<Option<[u8; 3]>>,
    wallpaper_accent: Option<Color32>,
    system_accent: Option<Color32>,
    material: Applied,
    material_key: Option<(bool, bool, Material)>,
    /// Cross-fades the palette when the seed or light/dark changes.
    palette: theme::Fade,
    /// Created lazily the first time the glass material is wanted.
    backdrop: Option<Backdrop>,
    backdrop_failed: bool,
    /// Click-through outside the shape (the window has no region).
    hit: Option<HitTester>,
    /// Last shape pushed to the compositor and the hit tester (window px).
    pushed: Option<PushedShape>,
    /// Hover-to-reveal while hidden for a fullscreen app.
    reveal: Reveal,
    /// Windows toasts as they arrive; None if the listener is unavailable.
    notifications: Option<NotificationWatcher>,
    /// The notification the pill is showing.
    notices: Notices<Toast>,
    /// The pointer rested on the notification pill at the last frame.
    notice_hover: bool,
    /// Forces the next frame to push its shape to the OS.
    shape_dirty: bool,
    /// When the backdrop last got a clip/tint push (see `BACKDROP_MIN_INTERVAL`).
    backdrop_pushed_at: Option<Instant>,
    /// Whether the cursor was inside the shape at the previous frame.
    was_inside: bool,
    /// Z-layer last applied (`cfg.on_top`).
    applied_on_top: Option<bool>,
    /// Frames drawn; the window stays fully transparent for the first few
    /// so no uninitialized surface ever shows.
    frames: u32,
    /// When the shell last settled collapsed, and whether memory was trimmed since.
    collapsed_since: Option<f64>,
    trimmed: bool,
    /// Window rect in physical pixels: (x, y, w, h).
    anchor: Option<(i32, i32, i32, i32)>,
    activatable: bool,
    /// GitHub release checks and the in-place update.
    updater: Updater,
    /// The anonymous daily usage ping.
    stats: Stats,
    _instance: InstanceGuard,
    #[cfg(feature = "debug-overlay")]
    overlay: crate::overlay::DebugOverlay,
}

impl NookApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cfg: Config,
        instance: InstanceGuard,
    ) -> Result<Self> {
        let ctx = cc.egui_ctx.clone();
        theme::install_fonts(&ctx);
        log_gpu(cc);

        let hwnd = match cc.window_handle().context("window handle")?.as_raw() {
            RawWindowHandle::Win32(h) => Hwnd::from_raw(h.hwnd.get()),
            other => anyhow::bail!("unexpected window handle {other:?}"),
        };
        window::ensure_overlay(hwnd, false).context("overlay window styles")?;
        // DirectX draws on a composition visual above the GDI surface, which
        // must then stay transparent (OpenGL draws into it instead).
        window::set_gdi_clearing(cc.wgpu_render_state.is_some());
        if let Err(e) = window::fill_client(hwnd) {
            tracing::warn!("frameless client area: {e}");
        }
        // Invisible until the first frames are drawn.
        window::set_opacity(hwnd, 0).context("initial window opacity")?;

        let repaint = ctx.clone();
        let hit = match HitTester::spawn(hwnd, move |_| repaint.request_repaint()) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::error!("hit testing unavailable, the notch will not take clicks: {e}");
                None
            }
        };

        let on_gl = cc.wgpu_render_state.is_none();
        let (tray_tx, tray_rx) = unbounded();
        let tray = match Tray::new(
            tray_tx,
            ctx.clone(),
            startup::is_enabled(),
            cfg.on_top,
            on_gl,
        ) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::error!("tray icon unavailable: {e:#}");
                None
            }
        };

        let watcher = Arc::new(WatcherControl::default());
        watcher.set_monitor_mode(cfg.monitor);
        let sys = watcher::sample(hwnd, cfg.monitor == MonitorMode::Cursor);
        let (sys_tx, sys_rx) = unbounded();
        watcher::spawn(hwnd, watcher.clone(), sys_tx, ctx.clone()).context("spawning watcher")?;

        let repaint = ctx.clone();
        let notifications = NotificationWatcher::spawn(move || repaint.request_repaint())
            .inspect_err(|e| tracing::warn!("notifications: {e}"))
            .ok();

        let (accent_tx, accent_rx) = unbounded();
        spawn_wallpaper_accent(accent_tx.clone(), ctx.clone());

        nook_modules::plugins::set_dev_mode(cfg.plugin_dev);
        let mut registry = Registry::default();
        nook_modules::register_all(&mut registry);
        // Tools that ship with Nook; usage stats count only these by name.
        let builtin = registry
            .all()
            .map(|m| m.id())
            .filter(|id| !id.starts_with(nook_modules::plugins::ID_PREFIX))
            .collect();
        cfg.modules.apply(&mut registry);
        for m in registry.all_mut() {
            m.attach(&ctx);
        }
        let hotkeys = match register_hotkeys(&cfg, &registry, &ctx) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::error!("global hotkeys unavailable: {e:#}");
                None
            }
        };

        // Keep config and the Run key in agreement (the key is the truth).
        let mut cfg = cfg;
        cfg.launch_at_startup = startup::is_enabled();

        let timings = timings(&cfg);
        let mut shell = Shell::new(ShellMetrics::default(), timings);
        shell.placement = cfg.placement;
        // Dev flags (screenshots/demos): `--pinned` starts expanded and
        // pinned, `--settings` additionally opens the settings page.
        // `--page=<module id>` opens that module's page.
        let flag = |f: &str| std::env::args().any(|a| a == f);
        let module_page = std::env::args()
            .find_map(|a| a.strip_prefix("--page=").map(str::to_owned))
            .and_then(|id| registry.all().find(|m| m.id() == id).map(|m| m.id()));
        let page = if flag("--settings") {
            Page::Settings
        } else if let Some(id) = module_page {
            Page::Module(id)
        } else {
            Page::Bento
        };
        if flag("--pinned") || page != Page::Bento {
            shell.state.set_pinned(true, 0.0);
        }
        let updater = Updater::start(ctx.clone(), cfg.check_updates);
        // A missing id means a new install (or a config from before stats).
        let new_install = !stats::valid_id(&cfg.install_id);
        if new_install {
            cfg.install_id = stats::new_install_id();
            if let Err(e) = cfg.save() {
                tracing::error!("saving config: {e:#}");
            }
        }
        let stats = Stats::start(
            cfg.install_id.clone(),
            new_install,
            cfg.usage_stats,
            builtin,
        );
        Ok(Self {
            cfg,
            hwnd,
            shell,
            registry,
            page,
            editing: false,
            pinned_before_edit: false,
            bento_rows: (Tab::default(), 0),
            tray,
            tray_rx,
            hotkeys,
            sys,
            sys_rx,
            watcher,
            accent_tx,
            accent_rx,
            wallpaper_accent: None,
            system_accent: system::system_accent()
                .ok()
                .map(|[r, g, b]| Color32::from_rgb(r, g, b)),
            material: Applied::Opaque,
            material_key: None,
            palette: Default::default(),
            backdrop: None,
            backdrop_failed: false,
            hit,
            pushed: None,
            reveal: Reveal::default(),
            notifications,
            notices: Notices::default(),
            notice_hover: false,
            shape_dirty: true,
            backdrop_pushed_at: None,
            was_inside: false,
            applied_on_top: None,
            frames: 0,
            collapsed_since: None,
            trimmed: false,
            anchor: None,
            activatable: false,
            updater,
            stats,
            on_gl,
            _instance: instance,
            #[cfg(feature = "debug-overlay")]
            overlay: Default::default(),
        })
    }

    fn dark(&self) -> bool {
        match self.cfg.theme {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::Auto => !self.sys.light_theme,
        }
    }

    fn accent(&self) -> Color32 {
        match self.cfg.accent {
            AccentSource::Wallpaper => self.wallpaper_accent.unwrap_or(FALLBACK_ACCENT),
            AccentSource::System => self.system_accent.unwrap_or(FALLBACK_ACCENT),
            AccentSource::Custom => {
                theme::parse_hex(&self.cfg.accent_hex).unwrap_or(FALLBACK_ACCENT)
            }
        }
    }

    fn save_config(&self) {
        if let Err(e) = self.cfg.save() {
            tracing::error!("saving config: {e:#}");
        }
    }

    /// Enters or leaves the grid's edit mode. Editing pins the panel open
    /// (a drag near the edge must not fold it); leaving restores the pin.
    fn set_editing(&mut self, on: bool, ctx: &egui::Context) {
        if on == self.editing {
            return;
        }
        self.editing = on;
        let now = Self::now(ctx);
        if on {
            self.pinned_before_edit = self.shell.state.is_pinned();
            self.shell.state.set_pinned(true, now);
        } else {
            self.shell.state.set_pinned(self.pinned_before_edit, now);
        }
    }

    /// Applies edit-mode changes to the config and the grid, and saves.
    fn apply_layout_edits(&mut self, edits: Vec<bento::Edit>) {
        let modules = &mut self.cfg.modules;
        for edit in edits {
            match edit {
                bento::Edit::Reorder(ids) => {
                    modules.order = ids.into_iter().map(str::to_owned).collect();
                }
                bento::Edit::ToggleShown(id) => {
                    if modules.disabled.iter().any(|d| d == id) {
                        modules.disabled.retain(|d| d != id);
                    } else {
                        modules.disabled.push(id.to_owned());
                    }
                }
                bento::Edit::Resize(id, size) => {
                    let native = self
                        .registry
                        .all()
                        .find(|m| m.id() == id)
                        .map(|m| m.default_size());
                    if Some(size) == native {
                        modules.sizes.remove(id);
                    } else {
                        modules.sizes.insert(id.to_owned(), size);
                    }
                }
                bento::Edit::MoveTab(id, tab) => {
                    if tab == nook_modules::default_tab(id) {
                        modules.tabs.remove(id);
                    } else {
                        modules.tabs.insert(id.to_owned(), tab);
                    }
                    // Last in its new tab, which opens to show it.
                    modules.order.retain(|o| o != id);
                    modules.order.push(id.to_owned());
                    modules.last_tab = tab;
                }
            }
        }
        modules.apply(&mut self.registry);
        // Hiding or showing a tile frees or takes its shortcut right away.
        if let Some(hk) = &mut self.hotkeys {
            for m in self.registry.all() {
                hk.remove_module(m.id());
                hk.add_module(m, &self.cfg);
            }
        }
        self.save_config();
    }

    fn now(ctx: &egui::Context) -> f64 {
        ctx.input(|i| i.time)
    }

    /// After the shell has sat collapsed and untouched for a few seconds,
    /// hand the working set back to the OS. Repeats after each idle repaint
    /// (the pill clock ticks once a minute), never while the pointer is on it.
    fn trim_when_idle(&mut self, ctx: &egui::Context, progress: f32) {
        const AFTER: f64 = 3.0;
        let touched = ctx.input(|i| i.pointer.hover_pos().is_some())
            || self.registry.all().any(|m| m.overlay_open());
        if self.shell.state.is_open() || progress > 0.001 || touched {
            self.collapsed_since = None;
            self.trimmed = false;
            return;
        }
        let now = Self::now(ctx);
        let since = *self.collapsed_since.get_or_insert(now);
        if now - since >= AFTER {
            if let Err(e) = nook_platform::process::trim_working_set() {
                tracing::debug!("trim: {e}");
            }
            self.trimmed = true;
        } else if !self.trimmed {
            ctx.request_repaint_after(Duration::from_secs_f64(AFTER - (now - since) + 0.01));
        }
    }

    fn handle_tray(&mut self, ctx: &egui::Context, cmd: TrayCommand) {
        let now = Self::now(ctx);
        match cmd {
            TrayCommand::Settings => {
                self.page = Page::Settings;
                self.shell.state.set_pinned(true, now);
            }
            TrayCommand::TogglePin => self.shell.state.toggle_pin(now),
            TrayCommand::ToggleOnTop => {
                self.cfg.on_top = !self.cfg.on_top;
                self.save_config();
            }
            TrayCommand::ToggleStartup => {
                self.cfg.launch_at_startup = !self.cfg.launch_at_startup;
                self.apply_startup();
            }
            TrayCommand::Update => self.updater.install(),
            TrayCommand::RestartRenderer => {
                self.cfg.renderer = if self.on_gl {
                    crate::config::Renderer::Auto
                } else {
                    crate::config::Renderer::OpenGl
                };
                self.save_config();
                crate::RESTART_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            TrayCommand::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn apply_startup(&mut self) {
        if let Err(e) = startup::set_enabled(self.cfg.launch_at_startup) {
            tracing::error!("launch at startup: {e}");
            self.cfg.launch_at_startup = startup::is_enabled();
        }
        if let Some(t) = &self.tray {
            t.set_startup(self.cfg.launch_at_startup);
        }
        self.save_config();
    }

    /// Spans the window across the top of the target monitor's work area.
    /// The shell positions itself inside it (left, center or right).
    fn anchor_window(&mut self) {
        let Some(mon) = self.sys.monitor else {
            return;
        };
        let scale = window::dpi(self.hwnd) as f32 / 96.0;
        let h = (self.shell.window_size().y * scale).ceil() as i32;
        let rect = (mon.work.left, mon.work.top, mon.work.width(), h);
        if self.anchor != Some(rect) {
            match window::set_rect(self.hwnd, rect.0, rect.1, rect.2, rect.3) {
                Ok(()) => {
                    window::clear_gdi_surface(self.hwnd);
                    self.anchor = Some(rect);
                    self.shape_dirty = true;
                }
                Err(e) => tracing::warn!("anchoring window: {e}"),
            }
        }
    }

    /// Lets clicks through everywhere except on the shell (and keeps input
    /// while a button pressed on it is held). The hit tester decides; the window
    /// style can only be changed safely from this thread.
    fn apply_click_through(&mut self) {
        if let Some((client, window)) = window::ensure_client_fills(self.hwnd) {
            tracing::warn!("frame strip: client {client:?} smaller than window {window:?}, fixed");
        }
        let through = !self.hit.as_ref().is_none_or(HitTester::inside);
        // Checked against the real style every frame (one read, a write only
        // when it differs): winit rewrites the extended style when the
        // window is activated, which used to drop the bit while the cached
        // state said it was set, leaving the whole strip catching clicks.
        if let Err(e) = window::set_click_through(self.hwnd, through) {
            tracing::warn!("click-through: {e}");
        }
    }

    /// Keeps the z-order in line with `cfg.on_top`, and re-applies it if
    /// something (winit, the shell) changed the topmost bit behind our back
    /// or another always-on-top window came up over the notch.
    fn apply_layer(&mut self) {
        let drift = window::is_topmost(self.hwnd) != self.cfg.on_top
            || (self.cfg.on_top && self.sys.covered);
        if self.applied_on_top == Some(self.cfg.on_top) && !drift {
            return;
        }
        let host = self.backdrop.as_ref().map(Backdrop::host);
        match window::set_layer(self.hwnd, host, self.cfg.on_top) {
            Ok(()) => {
                self.applied_on_top = Some(self.cfg.on_top);
                self.sys.covered = false;
                tracing::info!("on top: {}", self.cfg.on_top);
            }
            Err(e) => tracing::warn!("window layer: {e}"),
        }
        if let Some(t) = &self.tray {
            t.set_on_top(self.cfg.on_top);
        }
    }

    fn apply_material(&mut self) {
        let dark = self.dark();
        let key = (dark, self.sys.transparency, self.cfg.material);
        if self.material_key == Some(key) {
            return;
        }
        self.material_key = Some(key);
        material::prepare(self.hwnd, dark);
        // The backdrop window also paints the surface fill, so it is used for
        // both materials; "Solid" only turns its blur layer off.
        if self.backdrop.is_none() && !self.backdrop_failed {
            match Backdrop::new(self.hwnd) {
                Ok(b) => self.backdrop = Some(b),
                Err(e) => {
                    tracing::warn!("composition backdrop unavailable, painting glass in GL: {e}");
                    self.backdrop_failed = true;
                }
            }
        }
        let blur = self.cfg.material == Material::Acrylic && self.sys.transparency;
        self.material = if blur && self.backdrop.is_some() {
            Applied::Acrylic
        } else {
            Applied::Opaque
        };
        self.shape_dirty = true;
        tracing::info!("backdrop material: {:?}", self.material);
    }

    /// Whether the backdrop (tint and clip) may be updated this frame; call
    /// once per frame. Otherwise a repaint is scheduled for when it may, so
    /// the final settled shape is always sent. A pending re-push
    /// (`shape_dirty`) is never held back.
    fn backdrop_due(&mut self, ctx: &egui::Context) -> bool {
        let now = Instant::now();
        if !self.shape_dirty
            && let Some(wait) = backdrop_wait(now, self.backdrop_pushed_at, BACKDROP_MIN_INTERVAL)
        {
            ctx.request_repaint_after(wait + Duration::from_millis(1));
            return false;
        }
        self.backdrop_pushed_at = Some(now);
        true
    }

    /// Pushes this frame's surface fill to the compositor.
    fn update_tint(&mut self, ctx: &egui::Context, theme: &Theme, out: &ShellOutput, due: bool) {
        if !due {
            return;
        }
        let Some(b) = &mut self.backdrop else {
            return;
        };
        let ppp = ctx.pixels_per_point();
        let (top, bottom) = (theme.glass_top, theme.glass_bottom);
        let unmult = |c: Color32| theme::with_alpha(c, out.opacity).to_srgba_unmultiplied();
        let rect = out.shape.rect;
        let tint = Tint {
            top: unmult(top),
            bottom: unmult(bottom),
            y0: rect.min.y * ppp,
            y1: rect.max.y * ppp,
            blur: self.material == Applied::Acrylic,
        };
        if let Err(e) = b.set_tint(tint) {
            tracing::warn!("backdrop tint: {e}");
        }
    }

    /// Pushes the shell's current shape to the compositor (sub-pixel clip
    /// of the glass) and to the hit tester (screen pixels, plus the debug
    /// overlay if enabled). Only does work when the shape changed.
    fn update_shape(
        &mut self,
        ctx: &egui::Context,
        shape: Option<Notch>,
        extra: Option<Rect>,
        due: bool,
    ) {
        let ppp = ctx.pixels_per_point();
        let outline: Vec<(f32, f32)> = shape
            .map(|s| s.outline().iter().map(|p| (p.x * ppp, p.y * ppp)).collect())
            .unwrap_or_default();
        // Before the hit-test early return: a throttled frame must still be
        // able to deliver the final shape once it has settled.
        if due && let Some(b) = &mut self.backdrop {
            if self.shape_dirty {
                b.invalidate_shape();
            }
            if let Err(e) = b.set_shape(&outline) {
                tracing::warn!("backdrop clip: {e}");
            }
        }
        let key = (outline, extra);
        if self.pushed.as_ref() == Some(&key) && !self.shape_dirty {
            return;
        }
        let (outline, extra) = key;
        if let Some(hit) = &self.hit {
            let (ox, oy) = self
                .anchor
                .map(|(x, y, _, _)| (x as f32, y as f32))
                .unwrap_or_default();
            let mut polygons = Vec::with_capacity(2);
            if let Some(s) = shape {
                // A point of slack so the anti-aliased edge still counts.
                polygons.push(
                    s.expand(1.0)
                        .outline()
                        .iter()
                        .map(|p| (ox + p.x * ppp, oy + p.y * ppp))
                        .collect(),
                );
            }
            if let Some(r) = extra {
                let (x0, y0) = (ox + r.min.x * ppp, oy + r.min.y * ppp);
                let (x1, y1) = (ox + r.max.x * ppp, oy + r.max.y * ppp);
                polygons.push(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]);
            }
            hit.set_shape(HitShape { polygons });
        }
        self.pushed = Some((outline, extra));
        self.shape_dirty = false;
    }

    /// Tells the hit tester where the resting pill is (screen pixels) while
    /// a hidden notch may reveal on hover, and nowhere otherwise.
    fn update_hot_zone(&self, ctx: &egui::Context, armed: bool) {
        let Some(hit) = &self.hit else { return };
        let mut polygons = Vec::new();
        if armed {
            let (ox, oy) = self
                .anchor
                .map(|(x, y, _, _)| (x as f32, y as f32))
                .unwrap_or_default();
            let ppp = ctx.pixels_per_point();
            // A point of slack: the top pixel row sits on the polygon's edge.
            let outline = self
                .shell
                .rest_shape(ctx.content_rect())
                .expand(1.0)
                .outline();
            polygons.push(
                outline
                    .iter()
                    .map(|p| (ox + p.x * ppp, oy + p.y * ppp))
                    .collect(),
            );
        }
        hit.set_hot_zone(HitShape { polygons });
    }

    /// Re-asserts the overlay styles (winit resets them on some state
    /// changes). Text input needs an activatable window; otherwise never
    /// steal focus.
    fn sync_window_styles(&mut self, ctx: &egui::Context) {
        // Only a text field (or a plugin game's canvas) needs the keyboard;
        // any clicked widget holds egui focus, and taking the foreground for
        // those would steal it from other windows (the screenshot editor,
        // the browser).
        let wanted = ctx.text_edit_focused() || nook_modules::plugins::keyboard_focused(ctx);
        let gained = wanted && !self.activatable;
        self.activatable = wanted;
        match window::ensure_overlay(self.hwnd, self.activatable) {
            Ok(true) => tracing::debug!("re-applied overlay window styles"),
            Ok(false) => {}
            Err(e) => tracing::warn!("overlay styles: {e}"),
        }
        // A text field just took focus: the window must be the foreground
        // one for the keys to arrive (the click that focused it allows it).
        if gained {
            window::activate(self.hwnd);
        }
    }

    /// Panel size the open page asks for, kept inside the monitor's work area.
    fn page_size(&self, ctx: &egui::Context) -> Option<egui::Vec2> {
        let m = self.shell.metrics;
        let want = match self.page {
            // The panel is as tall as the tab's rows; it springs when the
            // tab changes.
            Page::Bento => {
                let tab = self.cfg.modules.last_tab;
                let sizes = self.registry.tab_sizes(tab, self.editing);
                let mut h = bento::tab_height(&sizes, self.editing);
                if self.bento_rows.0 == tab {
                    h = h.max(bento::panel_height(self.bento_rows.1));
                }
                vec2(m.expanded.x, h)
            }
            Page::Settings => m.expanded + vec2(0.0, settings::extra_height(ctx, m.expanded.y)),
            Page::Module(id) => self.registry.all().find(|m| m.id() == id)?.page_size()?,
        };
        let fit = self.sys.monitor.map_or(want, |mon| {
            let ppp = ctx.pixels_per_point();
            let room_y = mon.work.height() as f32 / ppp / 1.08 - m.top_margin - 24.0;
            let room_x = mon.work.width() as f32 / ppp - 2.0 * m.side_margin - 24.0;
            vec2(want.x.min(room_x), want.y.min(room_y))
        });
        // The grid may be shorter than the regular panel.
        let min_y = if self.page == Page::Bento {
            0.0
        } else {
            m.expanded.y
        };
        Some(vec2(fit.x.max(m.expanded.x), fit.y.max(min_y)))
    }

    /// Puts plugin installs, updates and removals into effect while Nook
    /// runs: the registry, the grid layout and the hotkeys follow.
    /// Puts new toasts in the pill. Dropped: Nook's own, any while the panel
    /// is open, and any while Windows itself would hold toasts back
    /// (fullscreen, presentation, Do Not Disturb / Focus).
    fn take_notifications(&mut self, ctx: &egui::Context) {
        let Some(watcher) = &self.notifications else {
            return;
        };
        for n in watcher.poll() {
            if notifications::is_own(&n.app_id, &n.app_name)
                || self.shell.state.is_open()
                || !notifications::can_show_now()
            {
                continue;
            }
            tracing::debug!("notification from {}", n.app_name);
            let icon = n.icon.as_ref().map(|art| {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [art.width, art.height],
                    art.rgba.as_slice(),
                );
                ctx.load_texture(
                    "nook-notification-icon",
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.notices.push(Toast { n, icon }, Self::now(ctx));
            ctx.request_repaint();
        }
    }

    fn apply_plugin_changes(&mut self, ctx: &egui::Context) {
        let applied = nook_modules::plugins::apply_changes(&mut self.registry, ctx);
        if applied.added.is_empty() && applied.removed.is_empty() {
            return;
        }
        if let Some(hk) = &mut self.hotkeys {
            for id in &applied.removed {
                hk.remove_module(id);
            }
        }
        for &id in &applied.added {
            // A plugin hidden before and installed again should show.
            self.cfg.modules.disabled.retain(|d| d != id);
            if let (Some(hk), Some(m)) = (
                &mut self.hotkeys,
                self.registry.all().find(|m| m.id() == id),
            ) {
                hk.add_module(m, &self.cfg);
            }
        }
        self.cfg.modules.apply(&mut self.registry);
        self.save_config();
    }

    /// Hands files dropped on the notch to the module whose page is open if
    /// it takes them, else to the first visible module that does. Without
    /// the page check the always-accepting File Shelf would win while
    /// another module's page asks for a drop.
    fn route_dropped_files(&mut self, ctx: &egui::Context) {
        let paths: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if paths.is_empty() {
            return;
        }
        if nook_platform::files::take_own_drop(&paths) {
            tracing::info!(
                "{} file(s) dragged out and dropped back on Nook: ignored",
                paths.len()
            );
            return;
        }
        // On the Plugins page every drop is meant as a plugin: it installs,
        // or the page says why not; nothing goes on to the file shelf.
        if self.page == Page::Module(nook_modules::plugins::PLUGINS_ID) {
            tracing::info!("{} file(s) dropped on the Plugins page", paths.len());
            for p in &paths {
                nook_modules::plugins::install::submit_dropped(p);
            }
            return;
        }
        // Elsewhere, plugin archives (one plugin or a bundle) install and the
        // Plugins page opens with their progress; other files, ordinary
        // archives included, go to a module as before.
        let paths: Vec<std::path::PathBuf> = paths
            .into_iter()
            .filter(|p| !nook_modules::plugins::install::submit_archive(p))
            .collect();
        if paths.is_empty() {
            return;
        }
        let page_takes_files = match self.page {
            Page::Module(id) => self
                .registry
                .get_mut(id)
                .is_some_and(|m| m.accepts_files())
                .then_some(id),
            _ => None,
        };
        let target = match page_takes_files {
            Some(id) => self.registry.get_mut(id),
            None => self
                .registry
                .visible_mut()
                .into_iter()
                .find(|m| m.accepts_files()),
        };
        match target {
            Some(m) => {
                tracing::info!("{} file(s) dropped on {}", paths.len(), m.id());
                m.drop_files(paths);
            }
            None => tracing::info!("{} file(s) dropped, no module takes files", paths.len()),
        }
    }
}

/// Paints `text` so its ink (not its line box) starts at `x`, or ends at `x`
/// when `from_right`, and is vertically centered on `cy`. The font's line
/// box carries descender space and Phosphor glyphs are narrower than their
/// em box, so aligning line boxes leaves the pill visibly off-center.
fn ink_text(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    x: f32,
    cy: f32,
    from_right: bool,
) -> Rect {
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    let ink = galley.mesh_bounds;
    let dx = if from_right {
        x - ink.max.x
    } else {
        x - ink.min.x
    };
    let offset = vec2(dx, cy - ink.center().y);
    painter.galley(pos2(offset.x, offset.y), galley, color);
    ink.translate(offset)
}

/// [`ink_text`] for ticking digits: vertically centered on `cy` by ink, each
/// digit in a fixed-width slot, starting at `x`. Returns the line box.
fn ink_digits(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    x: f32,
    cy: f32,
) -> Rect {
    let ink = painter
        .layout_no_wrap(text.to_owned(), font.clone(), color)
        .mesh_bounds;
    paint::digit_slots(
        painter,
        text,
        font,
        color,
        pos2(x, cy - ink.center().y),
        egui::Align2::LEFT_TOP,
    )
}

fn icon_font(size: f32, fill: bool) -> FontId {
    let family = if fill {
        theme::FAMILY_ICON_FILL
    } else {
        theme::FAMILY_ICON_REGULAR
    };
    FontId::new(size, FontFamily::Name(family.into()))
}

/// Collapsed pill: live badges from modules (ring + label on the left,
/// glyphs and pulse dots on the right), inset by the same ink margin on
/// both sides.
fn draw_pill(badges: Vec<Badge>, battery: Option<Battery>, ui: &mut Ui, rect: Rect, theme: &Theme) {
    const PAD: f32 = 15.0;
    let painter = ui.painter();
    let cy = rect.center().y;
    let left = rect.min.x + PAD;
    let mut right = rect.max.x - PAD;
    let now = ui.input(|i| i.time);
    let clock = system::clock();

    // Left: a running timer's ring + countdown, otherwise the clock.
    let ring = badges.iter().find_map(|b| match b {
        Badge::Ring { progress, label } => Some((*progress, label.clone())),
        _ => None,
    });
    let left_end = match ring {
        Some((progress, label)) => {
            let r = 6.0;
            paint::ring(
                painter,
                pos2(left + r, cy),
                r,
                2.0,
                progress,
                theme::with_alpha(theme.text_dim, 0.35),
                theme.accent,
            );
            ink_digits(
                painter,
                &label,
                Theme::font_semibold(SIZE_BODY),
                theme.text,
                left + 2.0 * r + 7.0,
                cy,
            )
            .max
            .x
        }
        None => {
            ink_digits(
                painter,
                &clock.time,
                Theme::font_semibold(SIZE_BODY),
                theme.text,
                left,
                cy,
            )
            .max
            .x
        }
    };

    // Media: bars at the right edge, art and title between them and the
    // clock. Other right-side badges sit left of the bars.
    let media = media_badge(&badges);
    if media.is_some() {
        right = paint_bars(painter, right, cy, now, theme.text) - 8.0;
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }

    // Right: live glyphs (coffee, phone pulse...), otherwise battery or date.
    let mut any_right = false;
    for badge in &badges {
        match badge {
            Badge::Ring { .. } | Badge::Media { .. } => {}
            Badge::Glyph { icon } => {
                let ink = ink_text(
                    painter,
                    icon,
                    icon_font(14.0, true),
                    theme.accent_fg(),
                    right,
                    cy,
                    true,
                );
                right = ink.min.x - 6.0;
                any_right = true;
            }
            Badge::Pulse => {
                let phase = ((now * 1.6).sin() * 0.5 + 0.5) as f32;
                let c = pos2(right - 3.0, cy);
                painter.circle_filled(
                    c,
                    3.0 + 3.0 * phase,
                    theme::with_alpha(theme.accent, 0.35 * (1.0 - phase)),
                );
                painter.circle_filled(c, 3.0, theme.accent);
                right -= 12.0;
                any_right = true;
                ui.ctx().request_repaint_after(Duration::from_millis(33));
            }
        }
    }
    if let Some((title, art)) = media {
        let x = left_end + 10.0;
        let art_rect =
            Rect::from_min_size(pos2(x, cy - MEDIA_ART / 2.0), vec2(MEDIA_ART, MEDIA_ART));
        let text_x = art_rect.max.x + 6.0;
        // Nothing fits mid-spring until the pill has widened.
        if right - text_x > 24.0 {
            match art {
                Some(tex) => {
                    painter.add(
                        egui::epaint::RectShape::filled(art_rect, 5.0, Color32::WHITE)
                            .with_texture(*tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
                    );
                }
                None => {
                    painter.rect_filled(art_rect, 5.0, theme::with_alpha(theme.text_dim, 0.25));
                    painter.text(
                        art_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        nook_ui::icons::MUSIC_NOTES,
                        icon_font(12.0, false),
                        theme.text_dim,
                    );
                }
            }
            let mut job = egui::text::LayoutJob::single_section(
                title.to_owned(),
                egui::text::TextFormat::simple(Theme::font(SIZE_LABEL + 1.0), theme.text),
            );
            job.wrap = egui::text::TextWrapping {
                max_width: right - text_x,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = painter.layout_job(job);
            let pos = pos2(text_x, cy - galley.size().y / 2.0);
            painter.galley(pos, galley, theme.text);
        }
    } else if !any_right {
        match battery {
            Some(b) => {
                let text = ink_text(
                    painter,
                    &format!("{}%", b.percent),
                    Theme::font(SIZE_LABEL + 1.0),
                    theme.text_dim,
                    right,
                    cy,
                    true,
                );
                let (icon, color) = battery_icon(b, theme);
                ink_text(
                    painter,
                    icon,
                    icon_font(15.0, b.charging),
                    color,
                    text.min.x - 4.0,
                    cy,
                    true,
                );
            }
            None => {
                ink_text(
                    painter,
                    &clock.date,
                    Theme::font(SIZE_LABEL + 1.0),
                    theme.text_dim,
                    right,
                    cy,
                    true,
                );
            }
        }
    }

    // One wake-up when the minute changes; nothing in between.
    ui.ctx()
        .request_repaint_after(Duration::from_secs(u64::from(clock.secs_to_next_minute)));
}

/// One line: the title in the bright color, then " · body" dimmer, cut with
/// "…" at `max_width`.
fn notice_job(
    view: &NoticeView,
    color: Color32,
    dim: Color32,
    max_width: f32,
) -> egui::text::LayoutJob {
    let font = |semibold: bool| {
        if semibold {
            Theme::font_semibold(SIZE_LABEL + 1.0)
        } else {
            Theme::font(SIZE_LABEL + 1.0)
        }
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &view.line.title,
        0.0,
        egui::text::TextFormat::simple(font(true), color),
    );
    if let Some(body) = &view.line.body {
        job.append(
            &format!(" · {body}"),
            0.0,
            egui::text::TextFormat::simple(font(false), dim),
        );
    }
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job
}

/// Collapsed pill while a notification shows: `[icon] Title · body…` and a
/// "+N" for further arrivals at the right.
fn draw_notice(view: &NoticeView, ui: &mut Ui, rect: Rect, theme: &Theme) {
    const PAD: f32 = 15.0;
    let painter = ui.painter();
    let cy = rect.center().y;
    let mut right = rect.max.x - PAD;
    if view.more > 0 {
        let ink = ink_text(
            painter,
            &format!("+{}", view.more),
            Theme::font_semibold(SIZE_LABEL),
            theme.text_dim,
            right,
            cy,
            true,
        );
        right = ink.min.x - notify::COUNT_GAP;
    }
    let icon = Rect::from_min_size(
        pos2(rect.min.x + PAD, cy - notify::ICON / 2.0),
        vec2(notify::ICON, notify::ICON),
    );
    let text_x = icon.max.x + notify::ICON_GAP;
    // Nothing fits mid-spring until the pill has widened.
    if right - text_x <= 24.0 {
        return;
    }
    match view.icon {
        Some(tex) => {
            painter.add(
                egui::epaint::RectShape::filled(icon, 5.0, Color32::WHITE)
                    .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
            );
        }
        None => {
            painter.text(
                icon.center(),
                egui::Align2::CENTER_CENTER,
                nook_ui::icons::BELL,
                icon_font(14.0, false),
                theme.text_dim,
            );
        }
    }
    let galley = painter.layout_job(notice_job(view, theme.text, theme.text_dim, right - text_x));
    let pos = pos2(text_x, cy - galley.size().y / 2.0);
    painter.galley(pos, galley, theme.text);
}

/// The notification card hanging from the top edge: header (icon, app, "now",
/// close), the app logo with title and body, and "+N more". Laid out at the
/// full card size from the top-left of `rect`, so it stays put while the
/// shell springs open (the shell clips it). Returns whether X was clicked.
fn draw_card(
    view: &NoticeView,
    card: &notify::Card,
    ui: &mut Ui,
    rect: Rect,
    theme: &Theme,
) -> bool {
    const PAD: f32 = 16.0;
    const LOGO: f32 = 56.0;
    let r = &theme.roles;
    let tex = |painter: &egui::Painter, to: Rect, radius: f32, id: egui::TextureId| {
        painter.add(
            egui::epaint::RectShape::filled(to, radius, Color32::WHITE)
                .with_texture(id, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
        );
    };
    let painter = ui.painter().clone();

    // Header.
    let cy = rect.min.y + 12.0 + 9.0;
    let small = Rect::from_center_size(pos2(rect.min.x + PAD + 7.0, cy), vec2(14.0, 14.0));
    match view.icon {
        Some(id) => tex(&painter, small, 4.0, id),
        None => {
            painter.text(
                small.center(),
                egui::Align2::CENTER_CENTER,
                nook_ui::icons::BELL,
                icon_font(13.0, false),
                theme.text_dim,
            );
        }
    }
    let app = ink_text(
        &painter,
        &card.app,
        Theme::font(SIZE_LABEL + 1.0),
        theme.text_dim,
        small.max.x + 8.0,
        cy,
        false,
    );
    ink_text(
        &painter,
        "· now",
        Theme::font(SIZE_LABEL + 1.0),
        theme.text_faint,
        app.max.x + 6.0,
        cy,
        false,
    );
    let x = Rect::from_center_size(
        pos2(rect.min.x + notify::CARD_W - PAD - 8.0, cy),
        vec2(22.0, 22.0),
    );
    let x_resp = ui.interact(x, egui::Id::new("notice-x"), egui::Sense::click());
    if x_resp.hovered() {
        painter.circle_filled(x.center(), 11.0, r.surface_container_high);
    }
    painter.text(
        x.center(),
        egui::Align2::CENTER_CENTER,
        nook_ui::icons::X,
        icon_font(12.0, false),
        theme.text_dim,
    );

    // Logo (or the app's first letter) with the title and body beside it.
    let logo = Rect::from_min_size(
        pos2(rect.min.x + PAD, rect.min.y + 12.0 + 18.0 + 8.0),
        vec2(LOGO, LOGO),
    );
    match view.icon {
        Some(id) => tex(&painter, logo, 16.0, id),
        None => {
            painter.rect_filled(logo, 16.0, r.primary_container);
            let letter = card
                .app
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string();
            painter.text(
                logo.center(),
                egui::Align2::CENTER_CENTER,
                letter,
                Theme::font_semibold(24.0),
                r.on_primary_container,
            );
        }
    }
    let text_x = logo.max.x + 12.0;
    let width = rect.min.x + notify::CARD_W - PAD - text_x;
    let layout = |text: &str, font: FontId, color: Color32, rows: usize| {
        let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
        job.wrap = egui::text::TextWrapping {
            max_width: width,
            max_rows: rows,
            break_anywhere: false,
            overflow_character: Some('…'),
        };
        painter.layout_job(job)
    };
    let title = layout(
        &card.title,
        Theme::font_semibold(SIZE_BODY + 1.0),
        theme.text,
        1,
    );
    let title_h = title.size().y;
    painter.galley(pos2(text_x, logo.min.y), title, theme.text);
    if let Some(body) = &card.body {
        let galley = layout(body, Theme::font(SIZE_BODY), theme.text_dim, 2);
        painter.galley(
            pos2(text_x, logo.min.y + title_h + 2.0),
            galley,
            theme.text_dim,
        );
    }
    if view.more > 0 {
        ink_text(
            &painter,
            &format!("+{} more", view.more),
            Theme::font_semibold(SIZE_LABEL),
            r.primary,
            logo.min.x,
            logo.max.y + 14.0,
            false,
        );
    }
    x_resp.clicked()
}

/// Width the pill springs to while media plays.
const MEDIA_PILL_WIDTH: f32 = 300.0;

/// Pill width that fits the clock (`left_w`), the art, the title and the
/// bars, between the normal pill (`min_w`) and [`MEDIA_PILL_WIDTH`]; longer
/// titles end in "…". Mirrors the spacing in `draw_pill`.
fn media_pill_width(left_w: f32, title_w: f32, min_w: f32) -> f32 {
    const PAD: f32 = 15.0;
    let bars = BAR_COUNT as f32 * BAR_W + (BAR_COUNT - 1) as f32 * BAR_GAP;
    // +2: rounding slack so a title that fits isn't cut to "…".
    let content = PAD + left_w + 10.0 + MEDIA_ART + 6.0 + title_w + 2.0 + 8.0 + bars + PAD;
    content.clamp(min_w, MEDIA_PILL_WIDTH)
}
const MEDIA_ART: f32 = 22.0;
const BAR_COUNT: usize = 4;
const BAR_W: f32 = 2.5;
const BAR_GAP: f32 = 2.0;
const BAR_MIN: f32 = 3.0;
const BAR_MAX: f32 = 14.0;

/// The playing media in the badges: title and album art.
fn media_badge(badges: &[Badge]) -> Option<(&str, Option<&egui::TextureId>)> {
    badges.iter().find_map(|b| match b {
        Badge::Media { title, art } => Some((title.as_str(), art.as_ref())),
        _ => None,
    })
}

/// Height of visualizer bar `i` at time `t` seconds: a decorative sine mix,
/// always within `BAR_MIN..=BAR_MAX`.
fn bar_height(i: usize, t: f64) -> f32 {
    let k = i as f64;
    let wave = ((t * (5.0 + 1.7 * k) + k * 1.9).sin() * 0.5 + 0.5) as f32;
    BAR_MIN + (BAR_MAX - BAR_MIN) * wave
}

/// Paints the bars ending at `right` (x), centered on `cy`; returns their
/// left edge.
fn paint_bars(painter: &egui::Painter, right: f32, cy: f32, t: f64, color: Color32) -> f32 {
    let total = BAR_COUNT as f32 * BAR_W + (BAR_COUNT - 1) as f32 * BAR_GAP;
    let left = right - total;
    for i in 0..BAR_COUNT {
        let h = bar_height(i, t);
        let x = left + i as f32 * (BAR_W + BAR_GAP);
        painter.rect_filled(
            Rect::from_min_size(pos2(x, cy - h / 2.0), vec2(BAR_W, h)),
            BAR_W / 2.0,
            color,
        );
    }
    left
}

fn battery_icon(b: Battery, theme: &Theme) -> (&'static str, Color32) {
    use nook_ui::icons;
    if b.charging {
        return (icons::BATTERY_CHARGING, Color32::from_rgb(98, 214, 132));
    }
    let icon = match b.percent {
        90.. => icons::BATTERY_FULL,
        60.. => icons::BATTERY_HIGH,
        30.. => icons::BATTERY_MEDIUM,
        12.. => icons::BATTERY_LOW,
        _ => icons::BATTERY_WARNING,
    };
    let color = if b.percent < 12 {
        Color32::from_rgb(255, 99, 88)
    } else {
        theme.text_dim
    };
    (icon, color)
}

fn timings(cfg: &Config) -> ShellTimings {
    ShellTimings {
        hover_delay: cfg.hover_delay_ms as f64 / 1000.0,
        collapse_grace: cfg.collapse_grace_ms as f64 / 1000.0,
        ..ShellTimings::default()
    }
}

fn register_hotkeys(cfg: &Config, registry: &Registry, ctx: &egui::Context) -> Result<Hotkeys> {
    let manager = GlobalHotKeyManager::new().context("creating hotkey manager")?;
    let toggle_id = match HotKey::from_str(&cfg.toggle_hotkey) {
        Ok(hk) => match manager.register(hk) {
            Ok(()) => Some(hk.id()),
            Err(e) => {
                tracing::warn!("hotkey {} unavailable: {e}", cfg.toggle_hotkey);
                None
            }
        },
        Err(e) => {
            tracing::warn!("bad hotkey {:?}: {e}", cfg.toggle_hotkey);
            None
        }
    };
    let (tx, rx) = unbounded();
    let ctx = ctx.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if tx.send(e).is_ok() {
            ctx.request_repaint();
        }
    }));
    let mut hotkeys = Hotkeys {
        manager,
        toggle_id,
        modules: Vec::new(),
        rx,
    };
    for m in registry.all() {
        hotkeys.add_module(m, cfg);
    }
    Ok(hotkeys)
}

/// Which GPU and driver draw the window: the first thing to check when a
/// report says the panel is black or flickers.
fn log_gpu(cc: &eframe::CreationContext<'_>) {
    if let Some(rs) = &cc.wgpu_render_state {
        crate::diag::set_active_renderer("DirectX 12");
        let info = rs.adapter.get_info();
        tracing::info!(
            "gpu: {} ({:?}, {:?}), driver {} {}, format {:?}",
            info.name,
            info.backend,
            info.device_type,
            info.driver,
            info.driver_info,
            rs.target_format,
        );
    } else if let Some(gl) = &cc.gl {
        crate::diag::set_active_renderer("OpenGL");
        use eframe::glow::HasContext;
        // SAFETY: plain string queries on the current context.
        let (renderer, version, alpha) = unsafe {
            (
                gl.get_parameter_string(eframe::glow::RENDERER),
                gl.get_parameter_string(eframe::glow::VERSION),
                gl.get_framebuffer_attachment_parameter_i32(
                    eframe::glow::FRAMEBUFFER,
                    eframe::glow::BACK_LEFT,
                    eframe::glow::FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE,
                ),
            )
        };
        tracing::info!("gpu: {renderer} (OpenGL {version}), alpha bits {alpha}");
    }
}

fn spawn_wallpaper_accent(tx: crossbeam_channel::Sender<Option<[u8; 3]>>, ctx: egui::Context) {
    let spawned = std::thread::Builder::new()
        .name("nook-accent".into())
        .spawn(move || {
            let accent = match wallpaper::accent_from_wallpaper() {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("wallpaper accent: {e}");
                    None
                }
            };
            if tx.send(accent).is_ok() {
                ctx.request_repaint();
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("spawning accent thread: {e}");
    }
}

impl eframe::App for NookApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(cmd) = self.tray_rx.try_recv() {
            self.handle_tray(ctx, cmd);
        }
        if let Some(hk) = &self.hotkeys {
            let mut toggle = false;
            while let Ok(e) = hk.rx.try_recv() {
                if e.state != HotKeyState::Pressed {
                    continue;
                }
                if Some(e.id) == hk.toggle_id {
                    toggle = !toggle;
                } else if let Some((_, id)) = hk.modules.iter().find(|(k, _)| k.id() == e.id)
                    && let Some(m) = self.registry.get_mut(id)
                {
                    self.stats.tool(id);
                    m.on_hotkey();
                }
            }
            if toggle {
                let now = Self::now(ctx);
                // The hotkey always brings a notch that was sent behind
                // other windows back on top.
                if !self.cfg.on_top {
                    self.cfg.on_top = true;
                    self.save_config();
                }
                let st = &mut self.shell.state;
                if st.is_open() && st.is_pinned() {
                    st.set_open(false, now);
                } else {
                    st.set_pinned(true, now);
                }
            }
        }
        while let Ok(s) = self.sys_rx.try_recv() {
            if s.light_theme != self.sys.light_theme {
                self.system_accent = system::system_accent()
                    .ok()
                    .map(|[r, g, b]| Color32::from_rgb(r, g, b));
            }
            if s.wallpaper != self.sys.wallpaper {
                // New wallpaper (from the wallpaper page or elsewhere): new accent.
                spawn_wallpaper_accent(self.accent_tx.clone(), ctx.clone());
            }
            self.sys = s;
        }
        while let Ok(a) = self.accent_rx.try_recv() {
            self.wallpaper_accent = a.map(|[r, g, b]| Color32::from_rgb(r, g, b));
        }
        self.apply_plugin_changes(ctx);
        self.take_notifications(ctx);
        let mut show = None;
        for m in self.registry.all_mut() {
            m.on_tick();
            if m.take_close_request() {
                self.shell.state.set_open(false, Self::now(ctx));
            }
            if m.take_show_request() {
                show = Some(m.id());
            }
        }
        if let Some(id) = show {
            self.page = Page::Module(id);
            self.shell.state.set_pinned(true, Self::now(ctx));
        }
        anim::set_motion(
            ctx,
            Motion {
                reduced: !self.sys.animations,
            },
        );
        self.apply_material();
        self.anchor_window();
        self.apply_layer();
        self.apply_click_through();
        if let Some(b) = &mut self.backdrop
            && let Err(e) = b.follow(self.hwnd)
        {
            tracing::warn!("backdrop placement: {e}");
        }
        if let Some(t) = &self.tray {
            t.set_pinned(self.shell.state.is_pinned());
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        #[cfg(feature = "debug-overlay")]
        self.overlay.begin();
        let ctx = ui.ctx().clone();
        let dark = self.dark();
        let roles = self
            .palette
            .roles(&ctx, self.accent(), dark, Self::now(&ctx));
        let theme = Theme::from_roles(dark, roles, self.material.translucent(), &self.cfg.look());
        let fullscreen_hidden = self.cfg.hide_in_fullscreen && self.sys.fullscreen;
        let badges: Vec<Badge> = self
            .registry
            .visible()
            .filter_map(|m| m.collapsed_badge())
            .collect();
        // A notification takes over the pill until it expires or is clicked.
        let t = Self::now(&ctx);
        if let Some(at) = self.notices.tick(t, self.notice_hover) {
            ctx.request_repaint_after_secs(((at - t).max(0.0)) as f32 + 0.01);
        }
        // The card grows out of the closed pill, over a fullscreen app too;
        // with the panel open the one-line pill stays.
        let as_card = !self.shell.state.is_open();
        let notice = self.notices.current().map(|toast| NoticeView {
            icon: toast.icon.as_ref().map(egui::TextureHandle::id),
            line: notify::line(&toast.n, self.cfg.show_notification_text),
            more: self.notices.more(),
            card: as_card.then(|| notify::card(&toast.n, self.cfg.show_notification_text)),
        });
        self.shell.collapsed_height = notice.as_ref().filter(|v| v.card.is_some()).map(|v| {
            if v.more > 0 {
                notify::CARD_H_MORE
            } else {
                notify::CARD_H
            }
        });
        let measure = |text: &str, font: FontId| {
            ctx.layer_painter(egui::LayerId::background())
                .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
                .size()
                .x
        };
        let digits =
            |text: &str| paint::digit_slots_width(&ctx, text, &Theme::font_semibold(SIZE_BODY));
        // The pill widens to fit the notification or what's playing; the
        // reveal hot zone follows.
        self.shell.collapsed_width = if let Some(view) = &notice {
            if view.card.is_some() {
                Some(notify::CARD_W)
            } else {
                let text_w = ctx
                    .layer_painter(egui::LayerId::background())
                    .layout_job(notice_job(view, theme.text, theme.text_dim, f32::INFINITY))
                    .size()
                    .x;
                let count_w = match view.more {
                    0 => 0.0,
                    n => measure(&format!("+{n}"), Theme::font_semibold(SIZE_LABEL)),
                };
                Some(notify::pill_width(
                    text_w,
                    count_w,
                    self.shell.metrics.collapsed.x,
                ))
            }
        } else {
            media_badge(&badges).map(|(title, _)| {
                let left = match badges.iter().find_map(|b| match b {
                    Badge::Ring { label, .. } => Some(label.as_str()),
                    _ => None,
                }) {
                    // Ring (2r) plus its gap, then the countdown.
                    Some(label) => 19.0 + digits(label),
                    None => digits(&system::clock().time),
                };
                let title_w = measure(title, Theme::font(SIZE_LABEL + 1.0));
                media_pill_width(left, title_w, self.shell.metrics.collapsed.x)
            })
        };
        self.update_hot_zone(&ctx, fullscreen_hidden && self.cfg.reveal_on_hover);
        let now = ctx.input(|i| i.time);
        let revealed = self.reveal.update(
            now,
            fullscreen_hidden,
            self.cfg.reveal_on_hover,
            self.hit.as_ref().is_some_and(HitTester::in_hot_zone),
            self.shell.state.is_open(),
        );
        if let Some(at) = self.reveal.wake_at() {
            ctx.request_repaint_after_secs((at - now).max(0.0) as f32 + 0.001);
        }
        // A notification card drops down over a fullscreen app for its hold,
        // then the notch hides again.
        let card_up = notice.as_ref().is_some_and(|v| v.card.is_some());
        let hidden = fullscreen_hidden && !revealed && !card_up;

        let mut clicked = None;
        let mut page_request = None;
        let mut module_back = false;
        let mut page_background_clicked = false;
        let mut settings_out = settings::PageOutput::default();
        let mut grid_out = bento::BentoOutput::default();
        let mut update_clicked = false;
        let mut notice_closed = false;
        let update_status = self.updater.status();
        let editing = self.editing;
        let page = self.page;
        let battery = self.sys.battery;
        let composited = self.backdrop.is_some();
        // Stay open while typing in the panel or playing a plugin game (and
        // the window has focus) or while files are dragged over it.
        let typing = ctx.text_edit_focused() || nook_modules::plugins::keyboard_focused(&ctx);
        self.shell.hold_open =
            (typing && ctx.input(|i| i.focused)) || ctx.input(|i| !i.raw.hovered_files.is_empty());
        // Revealed over a fullscreen app: hover shows the pill, a click opens.
        self.shell.click_to_open = fullscreen_hidden && revealed;
        // A notification owns the closed pill: hover can't open the panel and
        // a click brings its app forward instead of pinning.
        self.shell.capture_pill_click = notice.is_some() && !hidden;
        self.route_dropped_files(&ctx);
        self.shell.expanded_override = self.page_size(&ctx);
        let registry = &mut self.registry;
        let cfg = &mut self.cfg;
        let out = self.shell.show(
            ui,
            &theme,
            hidden,
            composited,
            |ui, rect, _alpha| match &notice {
                Some(view) => match &view.card {
                    Some(card) => notice_closed = draw_card(view, card, ui, rect, &theme),
                    None => draw_notice(view, ui, rect, &theme),
                },
                None => draw_pill(badges, battery, ui, rect, &theme),
            },
            |ui, cctx| match page {
                Page::Bento => {
                    let mut tiles = registry.grid_mut(editing);
                    grid_out = bento::show(
                        ui,
                        cctx,
                        &theme,
                        &mut tiles,
                        editing,
                        &mut cfg.modules.last_tab,
                    );
                    clicked = grid_out.clicked;
                    // The Update chip sits in the middle of the bottom gutter.
                    let y = bento::gutter_y(cctx.panel);
                    if !editing && cctx.alpha > 0.5 && y < cctx.shape.max.y - 4.0 {
                        update_clicked = update::chip(
                            ui,
                            pos2(cctx.panel.center().x, y),
                            egui::Align2::CENTER_CENTER,
                            &update_status,
                            &theme,
                        )
                        .is_some_and(|c| c.clicked);
                    }
                    page_request = tiles
                        .iter_mut()
                        .filter(|t| !t.hidden)
                        .find_map(|t| t.module.take_page_request().then(|| t.module.id()));
                }
                Page::Module(id) => {
                    if let Some(m) = registry.get_mut(id) {
                        let title = m.name().to_owned();
                        let mut close = false;
                        let page_out = nook_ui::page::show(
                            ui,
                            cctx,
                            &theme,
                            id,
                            &title,
                            |ui, header, body| {
                                close = m.ui_page(ui, header, body, &theme);
                            },
                        );
                        module_back = page_out.back || close;
                        page_background_clicked = page_out.background_clicked;
                    } else {
                        module_back = true;
                    }
                }
                Page::Settings => {
                    settings_out = settings::page(ui, cctx, &theme, cfg, &update_status);
                }
            },
        );

        self.notice_hover = out.pointer_over && notice.is_some();
        if notice_closed {
            self.notices.take();
            self.notice_hover = false;
            ctx.request_repaint();
        }
        if out.pill_clicked
            && let Some(toast) = self.notices.take()
        {
            let n = &toast.n;
            self.notice_hover = false;
            ctx.request_repaint();
            if let Err(e) = notifications::bring_forward(&n.app_id, &n.hints()) {
                tracing::debug!("notification click: bringing {} forward: {e}", n.app_name);
            }
        }

        if let Some(id) = clicked {
            self.stats.tool(id);
        }
        self.stats.panel(self.shell.state.is_open());
        if grid_out.open_settings {
            self.page = Page::Settings;
        }
        if grid_out.tab_changed {
            self.save_config();
        }
        if page == Page::Bento && grid_out.rows > 0 {
            let seen = (self.cfg.modules.last_tab, grid_out.rows);
            if self.bento_rows != seen {
                self.bento_rows = seen;
                ctx.request_repaint();
            }
        }
        if settings_out.back || module_back {
            self.page = Page::Bento;
        }
        if page_background_clicked {
            self.shell.state.toggle_pin(Self::now(&ctx));
        }
        if settings_out.open_plugins {
            self.page = Page::Module(nook_modules::plugins::PLUGINS_ID);
        }
        if grid_out.toggle_edit {
            self.set_editing(!self.editing, &ctx);
        }
        if !grid_out.edits.is_empty() {
            self.apply_layout_edits(grid_out.edits);
        }
        if let Some(id) = page_request {
            self.page = Page::Module(id);
        }
        for m in self.registry.all_mut() {
            m.ui_overlay(&ctx, &theme);
        }
        if settings_out.startup_toggled {
            self.apply_startup();
        }
        if update_clicked || settings_out.install_update {
            self.updater.install();
        }
        if settings_out.check_update {
            self.updater.check_now();
        }
        if let Some(t) = &mut self.tray {
            t.set_update(update_status.offer().map(|r| r.version.as_str()));
        }
        // Setup is running and will start the new version; get out of its way.
        if update_status == update::Status::Installing {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if settings_out.changed {
            self.shell.timings = timings(&self.cfg);
            self.shell.placement = self.cfg.placement;
            self.watcher.set_monitor_mode(self.cfg.monitor);
            nook_modules::plugins::set_dev_mode(self.cfg.plugin_dev);
            self.stats.set_enabled(self.cfg.usage_stats);
            // Re-sample now so a monitor switch does not wait for the watcher.
            self.sys.monitor =
                watcher::sample(self.hwnd, self.cfg.monitor == MonitorMode::Cursor).monitor;
            self.save_config();
        }
        // Back to the grid once the panel has fully collapsed.
        if out.progress < 0.05 && !self.shell.state.is_open() {
            self.page = Page::Bento;
            self.editing = false;
            self.bento_rows.1 = 0;
            ctx.memory_mut(|m| m.stop_text_input());
        }

        // Live modules ask for their own repaint cadence.
        let expanded = self.shell.state.is_open();
        if let Some(next) = self
            .registry
            .visible()
            .filter_map(|m| m.next_repaint(expanded))
            .min()
        {
            ctx.request_repaint_after(next);
        }

        self.sync_window_styles(&ctx);
        // Activating the window for a text field can make winit rewrite the
        // extended style; fix the click-through bit before the frame ends.
        self.apply_click_through();
        self.trim_when_idle(&ctx, out.progress);

        #[cfg(feature = "debug-overlay")]
        let overlay_rect = Some(self.overlay.show(ui, &theme));
        #[cfg(not(feature = "debug-overlay"))]
        let overlay_rect: Option<Rect> = None;

        let due = self.backdrop_due(&ctx);
        self.update_tint(&ctx, &theme, &out, due);
        self.update_shape(&ctx, out.visible.then_some(out.shape), overlay_rect, due);

        // Reveal the window once real frames have been presented.
        self.frames = self.frames.saturating_add(1);
        if self.frames == 3 {
            window::ensure_shown(self.hwnd);
            // Showing the window gives it a fresh, white GDI surface.
            window::clear_gdi_surface(self.hwnd);
            if let Err(e) = window::set_opacity(self.hwnd, 255) {
                tracing::warn!("window opacity: {e}");
            }
        } else if self.frames < 3 {
            ctx.request_repaint();
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // The hit tester made the window click-through as the cursor left
        // the shape, so no more mouse events arrive: tell egui it is gone.
        let inside = self.hit.as_ref().is_none_or(HitTester::inside);
        if self.was_inside && !inside {
            let clicking = raw
                .events
                .iter()
                .any(|e| matches!(e, egui::Event::PointerButton { .. }));
            if clicking {
                // Let egui finish the click at its position first.
                ctx.request_repaint();
                return;
            }
            raw.events.push(egui::Event::PointerGone);
        }
        // Becoming interactive sends no mouse move until the cursor moves
        // again; report where it already is so a resting cursor hovers too.
        if !self.was_inside
            && inside
            && let (Ok((x, y)), Some((ox, oy, _, _))) =
                (nook_platform::monitor::cursor_pos(), self.anchor)
        {
            let ppp = ctx.pixels_per_point();
            raw.events.push(egui::Event::PointerMoved(pos2(
                (x - ox) as f32 / ppp,
                (y - oy) as f32 / ppp,
            )));
        }
        self.was_inside = inside;
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        for m in self.registry.all_mut() {
            m.shutdown();
        }
        self.watcher
            .stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.save_config();
        tracing::info!("bye");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_pill_fits_short_titles_and_caps_long_ones() {
        let short = media_pill_width(50.0, 40.0, 140.0);
        let longer = media_pill_width(50.0, 80.0, 140.0);
        assert!((140.0..MEDIA_PILL_WIDTH).contains(&short), "{short}");
        assert_eq!(longer - short, 40.0, "grows one-for-one with the title");
        assert_eq!(media_pill_width(50.0, 1000.0, 140.0), MEDIA_PILL_WIDTH);
        assert_eq!(media_pill_width(0.0, 0.0, 140.0), 140.0);
    }

    #[test]
    fn bars_stay_within_bounds() {
        for i in 0..BAR_COUNT {
            for step in 0..2000 {
                let h = bar_height(i, step as f64 * 0.016);
                assert!((BAR_MIN..=BAR_MAX).contains(&h), "{h}");
            }
        }
    }

    #[test]
    fn media_pill_shows_only_for_a_playing_badge() {
        assert!(media_badge(&[]).is_none());
        let ring = Badge::Ring {
            progress: 0.5,
            label: "1:00".into(),
        };
        assert!(media_badge(std::slice::from_ref(&ring)).is_none());
        let media = Badge::Media {
            title: "Song".into(),
            art: None,
        };
        // The timer ring stays in the badges and keeps the left slot.
        let badges = [ring, media];
        let (title, art) = media_badge(&badges).unwrap();
        assert_eq!(title, "Song");
        assert!(art.is_none());
    }

    #[test]
    fn backdrop_wait_throttles() {
        let t0 = Instant::now();
        let min = Duration::from_millis(8);
        assert_eq!(backdrop_wait(t0, None, min), None);
        let ms = Duration::from_millis;
        assert_eq!(backdrop_wait(t0 + ms(3), Some(t0), min), Some(ms(5)));
        assert_eq!(backdrop_wait(t0 + ms(8), Some(t0), min), None);
        assert_eq!(backdrop_wait(t0 + ms(20), Some(t0), min), None);
    }
}
