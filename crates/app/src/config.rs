//! User configuration in `%APPDATA%\Nook\config.toml`.

use anyhow::{Context, Result};
use nook_ui::module::TileSize;
use nook_ui::shell::Placement;
use nook_ui::theme::{Look, ThemeMode, parse_hex};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccentSource {
    /// Dominant color of the desktop wallpaper.
    #[default]
    Wallpaper,
    /// Windows accent color.
    System,
    /// `accent_hex`.
    Custom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonitorMode {
    #[default]
    Primary,
    /// Follow the monitor under the mouse cursor.
    Cursor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Material {
    /// Blurred acrylic backdrop where supported.
    #[default]
    Acrylic,
    /// Opaque glass, no backdrop blur.
    Solid,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Renderer {
    /// DirectX 12, falling back to OpenGL when it cannot start.
    #[default]
    Auto,
    /// DirectX 12 through a DirectComposition swapchain.
    DirectX,
    /// OpenGL. Some drivers drop its alpha and the window comes out black.
    OpenGl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrainSize {
    /// One physical pixel per speck.
    #[default]
    Fine,
    Medium,
    Coarse,
}

impl GrainSize {
    pub const ALL: [GrainSize; 3] = [GrainSize::Fine, GrainSize::Medium, GrainSize::Coarse];

    pub fn pixels(self) -> f32 {
        match self {
            GrainSize::Fine => 1.0,
            GrainSize::Medium => 2.0,
            GrainSize::Coarse => 3.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModulesConfig {
    /// Tile order by module id. Unknown ids are ignored; unlisted modules
    /// are appended.
    pub order: Vec<String>,
    pub disabled: Vec<String>,
    /// Tile sizes picked in the editor; modules not listed use their own.
    pub sizes: std::collections::BTreeMap<String, TileSize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeMode,
    pub accent: AccentSource,
    pub accent_hex: String,
    pub monitor: MonitorMode,
    pub material: Material,
    /// Glass opacity over the blurred backdrop, in percent (Glass material).
    pub glass_opacity: u8,
    /// `#RRGGBB` the panel background is tinted toward; empty for neutral.
    pub background_hex: String,
    /// Film-grain strength in percent; 0 turns the grain off.
    pub grain: u8,
    pub grain_size: GrainSize,
    /// Top-left corner, top center or top-right corner.
    pub placement: Placement,
    /// Above all windows (true) or behind them.
    pub on_top: bool,
    /// Fade the notch out while a fullscreen app (game, video) is in front
    /// on its monitor. Off: the notch is always visible.
    pub hide_in_fullscreen: bool,
    pub hover_delay_ms: u32,
    pub collapse_grace_ms: u32,
    pub launch_at_startup: bool,
    /// Global shortcut that opens and pins the panel.
    pub toggle_hotkey: String,
    /// Plugin dev mode: plugins restart when their files are saved.
    pub plugin_dev: bool,
    /// Look for a new release on GitHub at start and every few hours.
    pub check_updates: bool,
    /// Send the anonymous daily "opened" ping (see `stats.rs`).
    pub usage_stats: bool,
    /// GPU backend; read at startup only.
    pub renderer: Renderer,
    /// Random id the stats ping counts installs by; made on first run.
    pub install_id: String,
    pub modules: ModulesConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Auto,
            accent: AccentSource::Wallpaper,
            accent_hex: "#7C5CFF".into(),
            monitor: MonitorMode::Primary,
            material: Material::Acrylic,
            glass_opacity: DEFAULT_GLASS_OPACITY,
            background_hex: String::new(),
            grain: DEFAULT_GRAIN,
            grain_size: GrainSize::Fine,
            placement: Placement::Center,
            on_top: true,
            hide_in_fullscreen: false,
            hover_delay_ms: 120,
            collapse_grace_ms: 250,
            launch_at_startup: false,
            toggle_hotkey: "ctrl+alt+N".into(),
            plugin_dev: false,
            check_updates: true,
            usage_stats: true,
            renderer: Renderer::Auto,
            install_id: String::new(),
            modules: ModulesConfig {
                order: nook_modules::DEFAULT_ORDER
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
                disabled: Vec::new(),
                sizes: Default::default(),
            },
        }
    }
}

pub const DEFAULT_GLASS_OPACITY: u8 = 70;
pub const DEFAULT_GRAIN: u8 = 35;

impl Config {
    /// The styling settings as the theme takes them.
    pub fn look(&self) -> Look {
        Look {
            glass_opacity: self.glass_opacity.clamp(20, 100) as f32 / 100.0,
            tint: parse_hex(&self.background_hex),
            grain: self.grain.min(100) as f32 / 100.0,
            grain_px: self.grain_size.pixels(),
        }
    }

    /// Puts the styling settings (not theme, accent or material) back to
    /// their defaults.
    pub fn reset_look(&mut self) {
        let d = Config::default();
        self.glass_opacity = d.glass_opacity;
        self.background_hex = d.background_hex;
        self.grain = d.grain;
        self.grain_size = d.grain_size;
    }
}

pub fn dir() -> Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA").context("APPDATA is not set")?;
    Ok(PathBuf::from(appdata).join("Nook"))
}

pub fn path() -> Result<PathBuf> {
    Ok(dir()?.join("config.toml"))
}

impl Config {
    /// Loads the config, writing defaults on first run. A malformed file is
    /// kept as `config.toml.bad` and defaults are used.
    pub fn load() -> Result<Self> {
        let path = path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_lenient(&text) {
                Ok((cfg, dropped)) => {
                    if !dropped.is_empty() {
                        // The app only saves on a settings change, not on exit,
                        // so without this the bad values would sit in the file
                        // (and warn) until then. Keep the original beside it.
                        let _ = std::fs::copy(&path, path.with_extension("toml.bad"));
                        cfg.save()?;
                    }
                    Ok(cfg)
                }
                Err(e) => {
                    tracing::error!("config parse error, using defaults: {e}");
                    let _ = std::fs::rename(&path, path.with_extension("toml.bad"));
                    let cfg = Config::default();
                    cfg.save()?;
                    Ok(cfg)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                cfg.save()?;
                Ok(cfg)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let dir = dir()?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let text = toml::to_string_pretty(self).context("serializing config")?;
        let path = path()?;
        // Write-then-rename so a crash never leaves a half-written file.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }
}

/// Parses `text` keeping every valid value. Invalid ones fall back to their
/// default and are returned as `key.path = value` strings. Only text that is
/// not TOML at all is an error.
fn parse_lenient(text: &str) -> Result<(Config, Vec<String>), toml::de::Error> {
    let user: toml::Table = text.parse()?;
    let mut merged = toml::Table::try_from(Config::default()).expect("default config serializes");
    let mut dropped = Vec::new();
    merge(&mut merged, &[], user, &mut dropped);
    let cfg = toml::Value::Table(merged).try_into().unwrap_or_default();
    Ok((cfg, dropped))
}

/// Inserts each `user` entry under `path` in `root` if `root` still
/// deserializes as a `Config`; a rejected table is retried key by key.
fn merge(root: &mut toml::Table, path: &[String], user: toml::Table, dropped: &mut Vec<String>) {
    for (key, val) in user {
        let mut here = path.to_vec();
        here.push(key.clone());
        let table = here[..here.len() - 1]
            .iter()
            .fold(&mut *root, |t, k| t[k].as_table_mut().expect("table"));
        let old = table.insert(key.clone(), val.clone());
        if toml::Value::Table(root.clone())
            .try_into::<Config>()
            .is_ok()
        {
            continue;
        }
        let table = here[..here.len() - 1]
            .iter()
            .fold(&mut *root, |t, k| t[k].as_table_mut().expect("table"));
        // Put the default back (or nothing, for keys the default omits).
        match old {
            Some(o) => table.insert(key, o),
            None => table.remove(&key),
        };
        match (val, table.get(here.last().expect("key"))) {
            (toml::Value::Table(sub), Some(toml::Value::Table(_))) => {
                merge(root, &here, sub, dropped)
            }
            (val, _) => {
                tracing::warn!("config: ignoring invalid {} = {val}", here.join("."));
                dropped.push(format!("{} = {val}", here.join(".")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).expect("serialize");
        let back: Config = toml::from_str(&text).expect("parse");
        assert_eq!(cfg, back);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let cfg: Config = toml::from_str("theme = \"dark\"").expect("parse");
        assert_eq!(cfg.theme, ThemeMode::Dark);
        assert_eq!(cfg.hover_delay_ms, 120);
        assert_eq!(cfg.placement, Placement::Center);
        assert!(cfg.on_top);
    }

    #[test]
    fn module_sizes_round_trip() {
        let text = "[modules]\norder = [\"timer\"]\n[modules.sizes]\ntimer = \"wide\"\n";
        let cfg: Config = toml::from_str(text).expect("parse");
        assert_eq!(cfg.modules.sizes.get("timer"), Some(&TileSize::Wide));
        let back: Config =
            toml::from_str(&toml::to_string_pretty(&cfg).expect("ser")).expect("parse");
        assert_eq!(cfg, back);
    }

    #[test]
    fn look_settings_parse_and_map() {
        let text =
            "grain = 0\ngrain_size = \"coarse\"\nbackground_hex = \"#336699\"\nglass_opacity = 5";
        let cfg: Config = toml::from_str(text).expect("parse");
        let look = cfg.look();
        assert_eq!(look.grain, 0.0);
        assert_eq!(look.grain_px, 3.0);
        assert!(look.tint.is_some());
        // Clamped so the glass never disappears entirely.
        assert_eq!(look.glass_opacity, 0.2);
        let old: Config = toml::from_str("theme = \"dark\"").expect("parse");
        assert_eq!(old.grain, DEFAULT_GRAIN);
        assert_eq!(old.look().tint, None);
    }

    #[test]
    fn renderer_parses_and_defaults_to_auto() {
        let cfg: Config = toml::from_str("renderer = \"opengl\"").expect("parse");
        assert_eq!(cfg.renderer, Renderer::OpenGl);
        let old: Config = toml::from_str("theme = \"dark\"").expect("parse");
        assert_eq!(old.renderer, Renderer::Auto);
    }

    #[test]
    fn placement_is_lowercase_in_toml() {
        let cfg: Config = toml::from_str("placement = \"right\"\non_top = false").expect("parse");
        assert_eq!(cfg.placement, Placement::Right);
        assert!(!cfg.on_top);
    }

    fn lenient(text: &str) -> (Config, Vec<String>) {
        parse_lenient(text).expect("valid toml")
    }

    #[test]
    fn bad_enum_keeps_other_fields() {
        let (cfg, dropped) = lenient(
            "material = \"bogus\"
hover_delay_ms = 300",
        );
        assert_eq!(cfg.material, Material::Acrylic);
        assert_eq!(cfg.hover_delay_ms, 300);
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].starts_with("material"));
    }

    #[test]
    fn bad_tile_size_keeps_other_sizes_and_order() {
        let text = "[modules]
order = [\"timer\", \"notes\"]
[modules.sizes]
timer = \"wide\"
notes = \"huge??\"
";
        let (cfg, dropped) = lenient(text);
        assert_eq!(cfg.modules.order, ["timer", "notes"]);
        assert_eq!(cfg.modules.sizes.len(), 1);
        assert_eq!(cfg.modules.sizes.get("timer"), Some(&TileSize::Wide));
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].starts_with("modules.sizes.notes"));
    }

    #[test]
    fn wrong_type_resets_only_that_field() {
        let (cfg, dropped) = lenient(
            "glass_opacity = 300
grain = 10
hover_delay_ms = 1.5",
        );
        assert_eq!(cfg.glass_opacity, DEFAULT_GLASS_OPACITY);
        assert_eq!(cfg.grain, 10);
        assert_eq!(cfg.hover_delay_ms, 120);
        assert_eq!(dropped.len(), 2);
    }

    #[test]
    fn valid_file_round_trips_unchanged() {
        let mut cfg = Config {
            theme: ThemeMode::Dark,
            ..Config::default()
        };
        cfg.modules.sizes.insert("timer".into(), TileSize::Wide);
        let (back, dropped) = lenient(&toml::to_string_pretty(&cfg).expect("ser"));
        assert!(dropped.is_empty());
        assert_eq!(back, cfg);
    }

    #[test]
    fn broken_syntax_is_an_error() {
        assert!(parse_lenient("theme = \"dark").is_err());
    }
}
