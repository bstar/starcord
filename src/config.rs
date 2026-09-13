//! `config.toml`, and the template written beside it on a first run.
//!
//! Every field has a default, and a file that omits a table gets the whole
//! table's defaults. That is not politeness: the file is hand-edited, the
//! program is a chat client that people leave running for days, and a missing
//! key should cost a preference rather than a startup.
//!
//! Nothing here is ever a secret. The token lives in the keyring or in the
//! mode-0600 credentials file and nowhere else, so this file can be 0644,
//! copied between machines, and pasted into a bug report.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The whole of `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: Ui,
    pub layout: Layout,
    pub chat: Chat,
    pub media: Media,
    pub notify: Notify,
    pub compose: Compose,
    pub channels: Channels,
}

/// How the whole window looks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    /// A theme id, or `"system"` to follow the desktop.
    pub theme: String,
    /// Blank columns and rows kept around the whole layout, for terminals
    /// whose window has no padding of its own.
    pub padding_x: u16,
    pub padding_y: u16,
    /// `auto`, `kitty`, `blocks` or `off`.
    pub graphics: String,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: "catppuccin-mocha".into(),
            padding_x: 0,
            padding_y: 0,
            graphics: "auto".into(),
        }
    }
}

/// How the guild rail is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuildsStyle {
    /// A narrow column of icons or initials.
    Rail,
    /// A full-width list with names.
    List,
    Hidden,
}

impl Default for GuildsStyle {
    fn default() -> Self {
        Self::Rail
    }
}

impl GuildsStyle {
    pub fn name(self) -> &'static str {
        match self {
            GuildsStyle::Rail => "rail",
            GuildsStyle::List => "list",
            GuildsStyle::Hidden => "hidden",
        }
    }
}

/// The dock, as it is remembered between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub guilds: GuildsStyle,
    /// Width of the column holding channels and DMs.
    pub left_cols: u16,
    pub members_cols: u16,
    /// Percentage of the left column the DM list takes.
    pub dms_share: u16,
    pub show_channels: bool,
    pub show_dms: bool,
    pub show_members: bool,
    pub zen: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            guilds: GuildsStyle::Rail,
            left_cols: 26,
            members_cols: 24,
            dms_share: 40,
            show_channels: true,
            show_dms: true,
            show_members: true,
            zen: false,
        }
    }
}

/// How much of a timestamp a message header carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Timestamps {
    Off,
    /// `14:32`.
    Short,
    /// `2026-09-13 14:32`.
    Full,
}

impl Default for Timestamps {
    fn default() -> Self {
        Self::Short
    }
}

impl Timestamps {
    pub fn name(self) -> &'static str {
        match self {
            Timestamps::Off => "off",
            Timestamps::Short => "short",
            Timestamps::Full => "full",
        }
    }

    /// The next one round, for the key that cycles them.
    pub fn next(self) -> Self {
        match self {
            Timestamps::Off => Timestamps::Short,
            Timestamps::Short => Timestamps::Full,
            Timestamps::Full => Timestamps::Off,
        }
    }
}

/// Whether a spoiler is covered until asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Spoilers {
    Hidden,
    Shown,
}

impl Default for Spoilers {
    fn default() -> Self {
        Self::Hidden
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Chat {
    pub show_avatars: bool,
    pub timestamps: Timestamps,
    /// How far apart two messages by the same person can be and still be drawn
    /// as one block.
    pub group_window_secs: u64,
    /// Rows an inline picture may take. `0` draws a chip instead.
    pub max_image_rows: u16,
    pub emoji_images: bool,
    pub show_embeds: bool,
    pub spoilers: Spoilers,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            show_avatars: true,
            timestamps: Timestamps::Short,
            group_window_secs: 420,
            max_image_rows: 12,
            emoji_images: true,
            show_embeds: true,
            spoilers: Spoilers::Hidden,
        }
    }
}

/// When an animated picture is allowed to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Animate {
    Always,
    /// Only in the focused panel, which is what keeps a scrollback of GIFs
    /// from costing a core.
    Focused,
    Never,
}

impl Default for Animate {
    fn default() -> Self {
        Self::Focused
    }
}

impl Animate {
    pub fn name(self) -> &'static str {
        match self {
            Animate::Always => "always",
            Animate::Focused => "focused",
            Animate::Never => "never",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Animate::Always => Animate::Focused,
            Animate::Focused => Animate::Never,
            Animate::Never => Animate::Always,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Media {
    pub animate: Animate,
    pub cache_mb: u64,
    /// Where `s` in the media viewer puts a file. `~` is expanded.
    pub save_dir: String,
    /// argv, never a shell line: a file name with a space in it is a file name
    /// with a space in it, not two arguments.
    pub player: Vec<String>,
}

impl Default for Media {
    fn default() -> Self {
        Self {
            animate: Animate::Focused,
            cache_mb: 256,
            save_dir: "~/Downloads".into(),
            player: vec!["mpv".into()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notify {
    pub enabled: bool,
    /// Only DMs, not every mention in every server.
    pub dms_only: bool,
    pub bell: bool,
    /// A desktop notification as well as the in-terminal one. Off by default:
    /// it puts a message's author and text on somebody else's screen.
    pub desktop: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Self {
            enabled: true,
            dms_only: false,
            bell: true,
            desktop: false,
        }
    }
}

/// Which key sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SendKey {
    Enter,
    CtrlEnter,
}

impl Default for SendKey {
    fn default() -> Self {
        Self::Enter
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Compose {
    pub send_key: SendKey,
    /// How tall the composer may grow before it scrolls.
    pub max_rows: u16,
    pub typing_indicator: bool,
}

impl Default for Compose {
    fn default() -> Self {
        Self {
            send_key: SendKey::Enter,
            max_rows: 10,
            typing_indicator: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Channels {
    /// Voice channels in the channel list. Off, because this client cannot
    /// join one and a row that does nothing is worse than no row.
    pub show_voice: bool,
}

impl Default for Channels {
    fn default() -> Self {
        Self { show_voice: false }
    }
}

impl Config {
    /// Read the file, or the defaults if there is not one.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Write a commented starting file, if there is not one already.
    ///
    /// A template rather than a serialised `Config`, which would be a correct
    /// file that teaches nobody anything: what a reader needs is the list of
    /// what can be changed and what the words mean, and a bare dump carries
    /// neither. Returns whether it created the file.
    pub fn write_template(path: &Path) -> Result<bool> {
        if path.exists() {
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, TEMPLATE).with_context(|| format!("writing {}", path.display()))?;
        Ok(true)
    }

    /// `~/Downloads` and the like, as a real path.
    pub fn save_dir(&self) -> PathBuf {
        expand_tilde(&self.media.save_dir)
    }
}

fn expand_tilde(text: &str) -> PathBuf {
    let Some(rest) = text.strip_prefix('~') else {
        return PathBuf::from(text);
    };
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
        return PathBuf::from(text);
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    if rest.is_empty() {
        PathBuf::from(home)
    } else {
        PathBuf::from(home).join(rest)
    }
}

const TEMPLATE: &str = r#"# STAR/CORD configuration.
#
# Everything STAR/CORD keeps lives under one directory -- this file, the
# session, the themes and the media cache. $STARCORD_DIR relocates all of it.
#
# There is no token in here and there never will be one. It lives in the OS
# keyring, or in credentials.toml at mode 0600 when there is no keyring, so
# this file is safe to copy between machines and safe to paste into a bug
# report.

[ui]
# "system" follows the desktop. Or name one of the sixteen built-in themes;
# `t` and `T` cycle through them while it is running.
theme = "catppuccin-mocha"
# How pictures are drawn: auto, kitty, blocks, or off. "auto" asks the
# terminal, which is right nearly everywhere; insist on kitty over ssh or
# inside a multiplexer, where the question sometimes goes unanswered.
graphics = "auto"
# Blank cells around the whole layout, for a terminal whose window has none.
padding_x = 0
padding_y = 0

[layout]
# The server rail: "rail" is a narrow strip of icons, "list" a full column of
# names, "hidden" neither. Alt+G toggles it whichever this says.
guilds = "rail"
# The column holding the channel list and the DM list, and the member list on
# the far side. Both are dragged by their seams and written back here.
left_cols = 26
members_cols = 24
# Percentage of the left column the DM list takes.
dms_share = 40
show_channels = true
show_dms = true
show_members = true
# Alt+Z: chat, composer and the status line, nothing else.
zen = false

[chat]
show_avatars = true
# off, short (14:32) or full (2026-09-13 14:32).
timestamps = "short"
# How far apart two messages from the same person can be and still be drawn as
# one block, in seconds.
group_window_secs = 420
# Rows an inline picture may take. 0 draws a chip with the file name instead.
max_image_rows = 12
emoji_images = true
show_embeds = true
# "hidden" covers a spoiler until it is asked for; "shown" never covers one.
spoilers = "hidden"

[media]
# When an animated picture may move: always, focused, or never.
animate = "focused"
cache_mb = 256
save_dir = "~/Downloads"
# Arguments, never a shell line. The file is appended to this list.
player = ["mpv"]

[notify]
enabled = true
# Only direct messages, rather than every mention in every server.
dms_only = false
bell = true
# A desktop notification as well. Off by default: it puts somebody's name and
# words on a screen that may not be yours alone.
desktop = false

[compose]
# "enter" sends and shift+enter makes a newline; "ctrl-enter" swaps them.
send_key = "enter"
# How tall the composer may grow before it starts scrolling.
max_rows = 10
typing_indicator = true

[channels]
# Voice channels in the channel list. Off, because this client cannot join one
# and a row that does nothing is worse than no row at all.
show_voice = false
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_the_defaults() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c, Config::default());
    }

    /// The reason every table carries `#[serde(default)]`: somebody editing
    /// one key should not have to write the other six.
    #[test]
    fn a_partial_table_keeps_the_rest_of_its_defaults() {
        let c: Config = toml::from_str("[ui]\npadding_x = 2\n").unwrap();
        assert_eq!(c.ui.padding_x, 2);
        assert_eq!(c.ui.theme, Config::default().ui.theme);
        assert_eq!(c.chat, Chat::default());
    }

    /// The template is the file a first run writes, so it has to parse and it
    /// has to mean what the defaults mean. A template that drifted from the
    /// defaults would silently change the program for new installations only.
    #[test]
    fn the_template_parses_as_the_defaults() {
        let parsed: Config = toml::from_str(TEMPLATE).expect("the template must parse");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn the_template_is_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        assert!(Config::write_template(&path).unwrap());
        std::fs::write(&path, "[ui]\npadding_x = 7\n").unwrap();
        assert!(!Config::write_template(&path).unwrap());
        assert_eq!(Config::load(&path).unwrap().ui.padding_x, 7);
    }

    #[test]
    fn a_missing_file_loads_as_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::load(&dir.path().join("nothing.toml")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn the_save_directory_expands_a_tilde() {
        let home = std::env::var_os("HOME").unwrap_or_default();
        if home.is_empty() {
            return;
        }
        let c = Config::default();
        let dir = c.save_dir();
        assert!(dir.is_absolute(), "{dir:?}");
        assert!(dir.ends_with("Downloads"));
        assert_eq!(expand_tilde("/tmp/x"), PathBuf::from("/tmp/x"));
    }
}
