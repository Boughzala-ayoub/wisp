//! Ghostty-compatible configuration.
//!
//! Wisp reads Ghostty's config syntax, so an existing Ghostty config works:
//! `key = value` lines, `#` comments at the start of a line, repeatable keys
//! that append (`palette`, `keybind`, `font-family`), `key =` to reset a key to
//! its default, and `config-file` includes. Keys Wisp doesn't implement yet
//! produce a note rather than an error, since most come from a macOS or Linux
//! config. Command-line flags use the same keys: `wisp --font-size=14`.
//!
//! Themes use the same syntax. A theme applies before the config, so colors
//! set in the config override the theme's.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use libghostty_vt::style::{PaletteIndex, RgbColor};

use crate::bindings::Bindings;

include!(concat!(env!("OUT_DIR"), "/themes.rs"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightClick {
    CopyOrPaste,
    Copy,
    Paste,
    Ignore,
}

/// Everything Wisp reads from the config, with Ghostty's defaults except
/// where Windows conventions differ (noted on the field).
#[derive(Clone, Debug)]
pub struct Config {
    /// The first installed family is the primary font; the rest are tried, in
    /// order, for characters it lacks, before the system fallback.
    pub font_family: Vec<String>,
    pub font_size: f32,
    /// OpenType features like `-calt` or `ss01`, as (tag, value).
    pub font_features: Vec<([u8; 4], u32)>,
    pub background: RgbColor,
    pub foreground: RgbColor,
    pub palette: Vec<(u8, RgbColor)>,
    pub cursor_color: Option<RgbColor>,
    /// Color of the text under a block cursor; the background color if unset.
    pub cursor_text: Option<RgbColor>,
    pub selection_background: Option<RgbColor>,
    pub selection_foreground: Option<RgbColor>,
    pub cursor_style: CursorStyle,
    /// `None` blinks unless a program says otherwise, Ghostty's default.
    pub cursor_style_blink: Option<bool>,
    pub background_opacity: f32,
    pub background_blur: bool,
    /// Left/right and top/bottom padding, in points.
    pub window_padding_x: (f32, f32),
    pub window_padding_y: (f32, f32),
    /// Initial size in cells.
    pub window_width: u16,
    pub window_height: u16,
    pub title: Option<String>,
    /// The shell to run. `None` picks PowerShell.
    pub command: Option<String>,
    /// Scrollback in bytes, as in Ghostty.
    pub scrollback_limit: usize,
    pub bindings: Bindings,
    pub copy_on_select: bool,
    /// Ghostty defaults to a context menu; Windows terminals copy or paste.
    pub right_click_action: RightClick,
    pub clipboard_paste_protection: bool,
    pub clipboard_write: bool,
    pub mouse_hide_while_typing: bool,
    pub selection_clear_on_typing: bool,
    pub confirm_close_surface: bool,
    /// Where the config was read from, if a file existed.
    pub path: Option<PathBuf>,
    /// Problems worth showing the user: bad values, missing files.
    pub errors: Vec<String>,
    /// Keys Wisp doesn't support yet. Logged, not shown.
    pub unsupported: Vec<String>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            font_family: Vec::new(),
            font_size: 12.0,
            font_features: Vec::new(),
            background: RgbColor { r: 0x28, g: 0x2c, b: 0x34 },
            foreground: RgbColor { r: 0xff, g: 0xff, b: 0xff },
            palette: Vec::new(),
            cursor_color: None,
            cursor_text: None,
            selection_background: None,
            selection_foreground: None,
            cursor_style: CursorStyle::Block,
            cursor_style_blink: None,
            background_opacity: 1.0,
            background_blur: false,
            window_padding_x: (2.0, 2.0),
            window_padding_y: (2.0, 2.0),
            window_width: 100,
            window_height: 30,
            title: None,
            command: None,
            scrollback_limit: 10_000_000,
            bindings: Bindings::default(),
            copy_on_select: false,
            right_click_action: RightClick::CopyOrPaste,
            clipboard_paste_protection: true,
            clipboard_write: true,
            mouse_hide_while_typing: false,
            selection_clear_on_typing: true,
            confirm_close_surface: true,
            path: None,
            errors: Vec::new(),
            unsupported: Vec::new(),
        }
    }
}

/// One `key = value` line, remembered with where it came from.
#[derive(Clone, Debug)]
struct Line {
    key: String,
    value: String,
    /// The file's directory, for resolving relative paths in the value.
    dir: PathBuf,
    /// "file:line", for error messages.
    origin: String,
}

impl Config {
    /// The config file, then command-line flags, then the theme they name.
    pub fn load() -> Config {
        let path = default_path();
        let args: Vec<String> = std::env::args().skip(1).collect();
        Config::load_from(path.as_deref(), &args, system_prefers_dark())
    }

    /// `dark` picks the half of a `light:...,dark:...` theme.
    pub fn load_from(path: Option<&Path>, args: &[String], dark: bool) -> Config {
        let mut config = Config::default();
        let mut lines = Vec::new();
        let mut seen = HashSet::new();
        if let Some(path) = path.filter(|p| p.is_file()) {
            config.path = Some(path.to_path_buf());
            read_file(path, &mut lines, &mut seen, &mut config.errors);
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        for (i, arg) in args.iter().enumerate() {
            match arg.strip_prefix("--").and_then(|a| a.split_once('=')) {
                Some((key, value)) => lines.push(Line {
                    key: key.trim().to_owned(),
                    value: unquote(value.trim()).to_owned(),
                    dir: cwd.clone(),
                    origin: format!("argument {}", i + 1),
                }),
                None => config.errors.push(format!("argument {}: expected --key=value, got {arg:?}", i + 1)),
            }
        }

        // The theme goes first so the config's own colors win over it.
        if let Some(theme) = lines.iter().rev().find(|l| l.key == "theme").map(|l| l.value.clone()) {
            match theme_text(&theme, dark, &cwd) {
                Ok(text) => {
                    for line in parse(&text, Path::new(""), &format!("theme {theme:?}")) {
                        if line.key == "theme" || line.key == "config-file" {
                            config.errors.push(format!("{}: themes can't set {}", line.origin, line.key));
                        } else {
                            config.apply(&line);
                        }
                    }
                }
                Err(e) => config.errors.push(e),
            }
        }
        for line in lines.iter().filter(|l| l.key != "theme") {
            config.apply(line);
        }
        config
    }

    fn apply(&mut self, line: &Line) {
        let Line { key, value, origin, .. } = line;
        // `key =` with nothing after it restores the default.
        if value.is_empty() {
            self.reset(key);
            return;
        }
        let result: Result<(), String> = (|| {
            match key.as_str() {
                "font-family" => self.font_family.push(value.clone()),
                "font-size" => self.font_size = number(value)?,
                "font-feature" => self.font_features.push(font_feature(value)?),
                "background" => self.background = color(value)?,
                "foreground" => self.foreground = color(value)?,
                "palette" => {
                    let (index, rgb) = libghostty_vt::style::Palette::parse_palette_entry(value)
                        .map_err(|_| format!("expected N=COLOR, got {value:?}"))?;
                    self.palette.push((index.0, rgb));
                }
                "cursor-color" => self.cursor_color = Some(color(value)?),
                "cursor-text" => self.cursor_text = Some(color(value)?),
                "selection-background" => self.selection_background = Some(color(value)?),
                "selection-foreground" => self.selection_foreground = Some(color(value)?),
                "cursor-style" => {
                    self.cursor_style = match value.as_str() {
                        "block" => CursorStyle::Block,
                        "bar" => CursorStyle::Bar,
                        "underline" => CursorStyle::Underline,
                        "block_hollow" => CursorStyle::BlockHollow,
                        _ => return Err(format!("unknown cursor style {value:?}")),
                    }
                }
                "cursor-style-blink" => self.cursor_style_blink = Some(boolean(value)?),
                "background-opacity" => self.background_opacity = number::<f32>(value)?.clamp(0.0, 1.0),
                // Ghostty takes true, false or a blur radius; Wisp blurs with the system backdrop.
                "background-blur" => self.background_blur = value != "false" && value != "0",
                "window-padding-x" => self.window_padding_x = pair(value)?,
                "window-padding-y" => self.window_padding_y = pair(value)?,
                "window-width" => self.window_width = number::<u16>(value)?.max(10),
                "window-height" => self.window_height = number::<u16>(value)?.max(4),
                "title" => self.title = Some(value.clone()),
                "command" => self.command = Some(value.clone()),
                "scrollback-limit" => self.scrollback_limit = number(value)?,
                "keybind" => self.bindings.parse_line(value)?,
                "copy-on-select" => self.copy_on_select = value != "false",
                "right-click-action" => {
                    self.right_click_action = match value.as_str() {
                        // No context menu yet; copy-or-paste is the Windows habit.
                        "copy-or-paste" | "context-menu" => RightClick::CopyOrPaste,
                        "copy" => RightClick::Copy,
                        "paste" => RightClick::Paste,
                        "ignore" => RightClick::Ignore,
                        _ => return Err(format!("unknown right-click action {value:?}")),
                    }
                }
                "clipboard-paste-protection" => self.clipboard_paste_protection = boolean(value)?,
                // ponytail: "ask" allows without asking; a prompt can come with a settings UI.
                "clipboard-write" => self.clipboard_write = value != "deny",
                "mouse-hide-while-typing" => self.mouse_hide_while_typing = boolean(value)?,
                "selection-clear-on-typing" => self.selection_clear_on_typing = boolean(value)?,
                "confirm-close-surface" => self.confirm_close_surface = value != "false",
                "config-file" | "theme" => {} // handled while reading
                _ => self.unsupported.push(format!("{origin}: {key}")),
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.errors.push(format!("{origin}: {key}: {e}"));
        }
    }

    fn reset(&mut self, key: &str) {
        let d = Config::default();
        match key {
            "font-family" => self.font_family.clear(),
            "font-size" => self.font_size = d.font_size,
            "font-feature" => self.font_features.clear(),
            "background" => self.background = d.background,
            "foreground" => self.foreground = d.foreground,
            "palette" => self.palette.clear(),
            "cursor-color" => self.cursor_color = None,
            "cursor-text" => self.cursor_text = None,
            "selection-background" => self.selection_background = None,
            "selection-foreground" => self.selection_foreground = None,
            "cursor-style" => self.cursor_style = d.cursor_style,
            "cursor-style-blink" => self.cursor_style_blink = None,
            "background-opacity" => self.background_opacity = d.background_opacity,
            "command" => self.command = None,
            "title" => self.title = None,
            "keybind" => self.bindings = Bindings::default(),
            _ => {}
        }
    }

    /// The font families to try, the configured ones first.
    pub fn font_families(&self) -> Vec<String> {
        let mut families = self.font_family.clone();
        families.push("Cascadia Code".into());
        families
    }

    /// The 256-color palette: libghostty's defaults with the config's entries on top.
    pub fn palette_over(&self, mut base: libghostty_vt::style::Palette) -> libghostty_vt::style::Palette {
        for &(index, rgb) in &self.palette {
            base.set(PaletteIndex(index), rgb);
        }
        base
    }

    /// Scrollback in lines. Ghostty limits it in bytes; a cell is about 12 bytes.
    pub fn scrollback_lines(&self, columns: u16) -> usize {
        // ponytail: an estimate; libghostty-vt's byte limit isn't in the Rust bindings yet.
        (self.scrollback_limit / (columns.max(1) as usize * 12)).clamp(1_000, 1_000_000)
    }
}

/// `%XDG_CONFIG_HOME%\wisp\config.ghostty`, else `%LOCALAPPDATA%\wisp\...`.
/// Like Ghostty, the older name `config` is used if it's the one that exists.
pub fn default_path() -> Option<PathBuf> {
    let dir = config_dir()?;
    let current = dir.join("config.ghostty");
    let legacy = dir.join("config");
    Some(if !current.exists() && legacy.exists() { legacy } else { current })
}

pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
    Some(PathBuf::from(base).join("wisp"))
}

/// Whether Windows is set to dark mode for apps.
pub fn system_prefers_dark() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut light = 1u32;
    let mut size = 4u32;
    let found = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut light).cast()),
            Some(&mut size),
        )
    };
    found.is_ok() && light == 0
}

/// Reads a file's lines, then the files it includes, depth first, as Ghostty does.
fn read_file(path: &Path, lines: &mut Vec<Line>, seen: &mut HashSet<PathBuf>, errors: &mut Vec<String>) {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(canonical) {
        errors.push(format!("{}: included more than once (a cycle?)", path.display()));
        return;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            errors.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let parsed = parse(&text, &dir, &path.display().to_string());
    let includes: Vec<Line> = parsed.iter().filter(|l| l.key == "config-file").cloned().collect();
    lines.extend(parsed);
    // Included files apply after the file that includes them.
    for include in includes {
        let (optional, name) = match include.value.strip_prefix('?') {
            Some(rest) => (true, rest),
            None => (false, include.value.as_str()),
        };
        let target = resolve(name, &include.dir);
        if target.is_file() {
            read_file(&target, lines, seen, errors);
        } else if !optional {
            errors.push(format!("{}: config-file {} not found", include.origin, target.display()));
        }
    }
}

fn parse(text: &str, dir: &Path, name: &str) -> Vec<Line> {
    text.lines()
        .enumerate()
        .filter_map(|(i, raw)| {
            let line = raw.trim();
            // Only a `#` at the start of a line is a comment, so `background = #282c34` works.
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=').unwrap_or((line, ""));
            Some(Line {
                key: key.trim().to_owned(),
                value: unquote(value.trim()).to_owned(),
                dir: dir.to_path_buf(),
                origin: format!("{name}:{}", i + 1),
            })
        })
        .collect()
}

fn unquote(value: &str) -> &str {
    value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value)
}

/// Expands `~` and makes relative paths relative to `dir`.
fn resolve(path: &str, dir: &Path) -> PathBuf {
    let expanded = match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(rest),
        None => PathBuf::from(path),
    };
    if expanded.is_absolute() { expanded } else { dir.join(expanded) }
}

/// Finds a theme by path or name: the user's themes folder first, then the
/// bundled collection. `light:A,dark:B` picks one by the system setting.
fn theme_text(theme: &str, dark: bool, cwd: &Path) -> Result<String, String> {
    if theme.contains("light:") || theme.contains("dark:") {
        let pick = theme
            .split(',')
            .filter_map(|part| part.trim().split_once(':'))
            .find(|(mode, _)| *mode == if dark { "dark" } else { "light" })
            .map(|(_, name)| name.trim().to_owned());
        return match pick {
            Some(name) => theme_text(&name, dark, cwd),
            None => Err(format!("theme {theme:?} has no {} variant", if dark { "dark" } else { "light" })),
        };
    }
    let as_path = resolve(theme, cwd);
    if Path::new(theme).is_absolute() || theme.starts_with('~') {
        return std::fs::read_to_string(&as_path).map_err(|e| format!("theme {}: {e}", as_path.display()));
    }
    if let Some(user) = config_dir().map(|d| d.join("themes").join(theme))
        && let Ok(text) = std::fs::read_to_string(user)
    {
        return Ok(text);
    }
    THEMES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(theme))
        .map(|(_, text)| text.to_string())
        .ok_or_else(|| format!("theme {theme:?} not found"))
}

fn color(value: &str) -> Result<RgbColor, String> {
    // Ghostty accepts hex with or without the #, and X11 color names.
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap();
        return Ok(RgbColor { r: byte(0), g: byte(2), b: byte(4) });
    }
    RgbColor::parse(value).map_err(|_| format!("{value:?} is not a color"))
}

fn number<T: std::str::FromStr>(value: &str) -> Result<T, String> {
    value.parse().map_err(|_| format!("{value:?} is not a number"))
}

fn boolean(value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("expected true or false, got {value:?}")),
    }
}

/// `2` or `2,4`.
fn pair(value: &str) -> Result<(f32, f32), String> {
    match value.split_once(',') {
        Some((a, b)) => Ok((number(a.trim())?, number(b.trim())?)),
        None => {
            let v = number(value)?;
            Ok((v, v))
        }
    }
}

/// `-calt`, `+liga`, `ss01`, `calt=0`.
fn font_feature(value: &str) -> Result<([u8; 4], u32), String> {
    let (name, setting) = if let Some(n) = value.strip_prefix('-') {
        (n, 0)
    } else if let Some(n) = value.strip_prefix('+') {
        (n, 1)
    } else if let Some((n, v)) = value.split_once('=') {
        (n.trim(), number(v.trim())?)
    } else {
        (value, 1)
    };
    let tag: [u8; 4] = name.as_bytes().try_into().map_err(|_| format!("{name:?} is not a 4-letter feature tag"))?;
    Ok((tag, setting))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        let dir = std::env::temp_dir().join(format!("wisp-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("config-{}", text.len()));
        std::fs::write(&path, text).unwrap();
        Config::load_from(Some(&path), &[], true)
    }

    #[test]
    fn reads_ghostty_syntax() {
        let c = config(
            "# a comment\n\
             font-family = \"JetBrains Mono\"\n\
             font-family = Symbols Nerd Font Mono\n\
             font-size=14\n\
             background = #1e1e2e\n\
             foreground = cdd6f4\n\
             palette = 1=#f38ba8\n\
             cursor-style = bar\n\
             window-padding-x = 4,8\n\
             font-feature = -calt\n\
             keybind = ctrl+shift+t=new_tab\n",
        );
        assert_eq!(c.font_family, ["JetBrains Mono", "Symbols Nerd Font Mono"]);
        assert_eq!(c.font_size, 14.0);
        assert_eq!(c.background, RgbColor { r: 0x1e, g: 0x1e, b: 0x2e });
        assert_eq!(c.foreground, RgbColor { r: 0xcd, g: 0xd6, b: 0xf4 });
        assert_eq!(c.palette, [(1, RgbColor { r: 0xf3, g: 0x8b, b: 0xa8 })]);
        assert_eq!(c.cursor_style, CursorStyle::Bar);
        assert_eq!(c.window_padding_x, (4.0, 8.0));
        assert_eq!(c.font_features, [(*b"calt", 0)]);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
    }

    #[test]
    fn empty_value_resets_and_hash_in_value_is_kept() {
        let c = config("font-size = 20\nfont-size =\nbackground = #000000\n");
        assert_eq!(c.font_size, 12.0);
        assert_eq!(c.background, RgbColor { r: 0, g: 0, b: 0 });
    }

    #[test]
    fn bad_values_are_errors_and_unknown_keys_are_notes() {
        let c = config("font-size = big\nmacos-titlebar-style = tabs\ncursor-style = triangle\n");
        assert_eq!(c.errors.len(), 2, "{:?}", c.errors);
        assert_eq!(c.unsupported.len(), 1);
        assert!(c.unsupported[0].ends_with("macos-titlebar-style"));
    }

    #[test]
    fn themes_apply_first_so_config_colors_win() {
        let c = config("theme = Catppuccin Mocha\nforeground = #ffffff\n");
        assert_eq!(c.background, RgbColor { r: 0x1e, g: 0x1e, b: 0x2e }, "from the theme");
        assert_eq!(c.foreground, RgbColor { r: 0xff, g: 0xff, b: 0xff }, "the config overrides the theme");
        assert_eq!(c.palette.len(), 16);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
    }

    #[test]
    fn light_dark_themes_follow_the_system() {
        let dir = std::env::temp_dir().join(format!("wisp-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config");
        std::fs::write(&path, "theme = light:Catppuccin Latte,dark:Catppuccin Mocha\n").unwrap();
        let dark = Config::load_from(Some(&path), &[], true);
        let light = Config::load_from(Some(&path), &[], false);
        assert_eq!(dark.background, RgbColor { r: 0x1e, g: 0x1e, b: 0x2e });
        assert_eq!(light.background, RgbColor { r: 0xef, g: 0xf1, b: 0xf5 });
    }

    #[test]
    fn includes_load_after_the_file_and_cycles_stop() {
        let dir = std::env::temp_dir().join(format!("wisp-include-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main"), "font-size = 10\nconfig-file = extra\nconfig-file = ?missing\n").unwrap();
        std::fs::write(dir.join("extra"), "font-size = 16\nconfig-file = main\n").unwrap();
        let c = Config::load_from(Some(&dir.join("main")), &[], true);
        assert_eq!(c.font_size, 16.0, "the included file applies after the one including it");
        assert_eq!(c.errors.len(), 1, "only the cycle is reported, not the optional file: {:?}", c.errors);
    }

    #[test]
    fn command_line_flags_override_the_file() {
        let path = std::env::temp_dir().join(format!("wisp-args-test-{}", std::process::id()));
        std::fs::write(&path, "font-size = 10\n").unwrap();
        let c = Config::load_from(Some(&path), &["--font-size=18".into(), "oops".into()], true);
        assert_eq!(c.font_size, 18.0);
        assert_eq!(c.errors.len(), 1);
    }

    #[test]
    fn every_bundled_theme_parses_cleanly() {
        assert!(THEMES.len() > 700);
        for (name, _) in THEMES {
            let c = Config::load_from(None, &[format!("--theme={name}")], true);
            assert!(c.errors.is_empty(), "theme {name}: {:?}", c.errors);
            assert!(c.unsupported.is_empty(), "theme {name} uses keys Wisp ignores: {:?}", c.unsupported);
        }
    }
}
