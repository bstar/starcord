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
    pub chat: Chat,
    pub media: Media,
    pub notify: Notify,
    pub compose: Compose,
    pub channels: Channels,
    pub auth: Auth,
    pub gifs: Gifs,
}

/// Where the token is kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Store {
    /// The keyring if there is one, the mode-0600 file if there is not.
    #[default]
    Auto,
    /// Refuse to write a file; a machine with no keyring asks every time.
    Keyring,
    File,
    /// Keep nothing. Every run asks.
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    pub store: Store,
}

/// Which service the GIF picker asks, and how.
///
/// Configuration rather than a constant because Discord proxies a third party
/// here and has announced a change of provider. A client that hard-coded one
/// would stop returning results on the day that happens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gifs {
    pub provider: String,
    /// `gif`, `mp4` or `tinygif`.
    pub media_format: String,
    pub locale: String,
}

impl Default for Gifs {
    fn default() -> Self {
        Self {
            provider: "tenor".into(),
            media_format: "gif".into(),
            locale: "en-US".into(),
        }
    }
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
    /// The tallest an open list in the column may grow to, in rows.
    ///
    /// A ceiling rather than a size: a list shorter than this is only as tall
    /// as it has entries, and one longer than it scrolls. Eight is what leaves
    /// a readable conversation under it on a twenty-four-row terminal.
    pub list_rows: u16,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: "catppuccin-mocha".into(),
            padding_x: 0,
            padding_y: 0,
            graphics: "auto".into(),
            list_rows: 8,
        }
    }
}

/// How much of a timestamp a message header carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Timestamps {
    Off,
    /// `14:32`.
    #[default]
    Short,
    /// `2026-09-13 14:32`.
    Full,
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Spoilers {
    #[default]
    Hidden,
    Shown,
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Animate {
    Always,
    /// Only in the focused panel, which is what keeps a scrollback of GIFs
    /// from costing a core.
    #[default]
    Focused,
    Never,
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
    /// How large the on-disk cache may grow before the oldest files are swept.
    pub cache_max_mib: u64,
    /// The largest attachment worth downloading. Twenty-five is what an
    /// account without Nitro may upload, so it is the size of the largest file
    /// most people will ever be sent.
    pub max_attachment_mib: u64,
    /// Where `s` in the media viewer puts a file. `~` is expanded.
    pub save_dir: String,
    /// argv, never a shell line: a file name with a space in it is a file name
    /// with a space in it, not two arguments.
    pub player: Vec<String>,
    /// The same for a picture. Empty is the desktop's own opener, which is
    /// what a click on a photograph should reach.
    #[serde(default)]
    pub viewer: Vec<String>,
}

impl Default for Media {
    fn default() -> Self {
        Self {
            animate: Animate::Focused,
            cache_max_mib: 512,
            max_attachment_mib: 25,
            save_dir: "~/Downloads".into(),
            // `--` so that a file called `-x` is a file rather than an option.
            player: vec!["mpv".into(), "--".into()],
            viewer: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notify {
    pub enabled: bool,
    /// Say nothing about the channel already on screen while the terminal has
    /// focus: the reader is looking at it.
    pub only_when_unfocused: bool,
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
            only_when_unfocused: true,
            dms_only: false,
            bell: true,
            desktop: false,
        }
    }
}

/// Which key sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SendKey {
    #[default]
    Enter,
    CtrlEnter,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Channels {
    /// Voice channels in the channel list. Off -- the derived default -- because
    /// this client cannot join one, and a row that does nothing when it is
    /// activated is worse than no row at all.
    pub show_voice: bool,
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

    /// What the core is told, out of what the file says.
    ///
    /// The two structs are separate on purpose — nothing under `src/discord/`
    /// reads a config file, and nothing here knows what a gateway is — so this
    /// is the one place the words in `config.toml` become the core's settings.
    /// A key added to one and not carried across here is a key that does
    /// nothing, which is the failure this function exists to make visible in
    /// one screenful.
    pub fn core(&self) -> crate::discord::handle::DiscordConfig {
        use crate::discord::auth::StorePreference;
        crate::discord::handle::DiscordConfig {
            store: match self.auth.store {
                Store::Auto => StorePreference::Auto,
                Store::Keyring => StorePreference::Keyring,
                Store::File => StorePreference::File,
                Store::None => StorePreference::None,
            },
            media: crate::discord::media::MediaConfig {
                cache_max_mib: self.media.cache_max_mib,
                max_attachment_mib: self.media.max_attachment_mib,
                player: self.media.player.clone(),
                viewer: self.media.viewer.clone(),
            },
            gifs: crate::discord::http::route::GifProvider {
                name: self.gifs.provider.clone(),
                media_format: self.gifs.media_format.clone(),
                locale: self.gifs.locale.clone(),
            },
            notify: crate::discord::notify::NotifyConfig {
                // The core's `enabled` is about the *desktop* notification,
                // which is the only kind it delivers. `[notify] enabled` is
                // about all of them, and the bell and the line in the status
                // bar are the terminal's own: they are drawn here whatever the
                // desktop is doing, which is why the two keys are anded rather
                // than one of them carried across.
                enabled: self.notify.enabled && self.notify.desktop,
                only_when_unfocused: self.notify.only_when_unfocused,
                dms_only: self.notify.dms_only,
            },
            locale: self.gifs.locale.clone(),
            ..Default::default()
        }
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
# "blocks" draws two pixels to a cell and works in any terminal; "off" draws
# no picture at all and leaves the chip that names the file.
graphics = "auto"
# Blank cells around the whole layout, for a terminal whose window has none.
padding_x = 0
padding_y = 0
# The tallest an open list -- servers, channels or members -- may grow to. A
# shorter list is only as tall as it has entries; a longer one scrolls. The
# conversation gets whatever is left.
list_rows = 8

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
# How large the media cache may grow, in mebibytes, before the oldest files
# are swept.
cache_max_mib = 512
# The largest attachment worth downloading. Twenty-five is what an account
# without Nitro may upload.
max_attachment_mib = 25
save_dir = "~/Downloads"
# Arguments, never a shell line. The file is appended to this list, and `--`
# is what keeps a file called `-x` a file rather than an option.
player = ["mpv", "--"]
# What a click on a picture opens it with. Empty is the desktop's own opener
# -- `open` on macOS, `xdg-open` elsewhere -- which hands the file to whatever
# you already look at pictures with. Name a program the same way as `player`
# to choose one instead.
viewer = []

[notify]
enabled = true
# Say nothing about the channel already on screen while the terminal has
# focus: you are looking at it.
only_when_unfocused = true
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

[auth]
# Where the token is kept: "auto" uses the OS keyring if there is one and a
# mode-0600 file if there is not, "keyring" refuses to write a file, "file"
# always writes one, and "none" keeps nothing and asks every run.
store = "auto"

[gifs]
# Which service Discord proxies for the picker, and in what shape. This is
# configuration rather than a constant because Discord has announced a change
# of provider, and a client that hard-coded one would stop returning results
# on the day it happens.
provider = "tenor"
media_format = "gif"
locale = "en-US"
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

    /// A file written by 0.0.1 still loads. The window had a `[layout]` table
    /// then; there is nothing it could mean now, and a client that refused to
    /// start over a table it no longer has is a worse answer than one that
    /// ignores it. Nothing here declares `deny_unknown_fields`, which is what
    /// makes that true rather than merely intended.
    #[test]
    fn an_old_layout_table_is_ignored() {
        let text = "\
[ui]
padding_x = 2

[layout]
guilds = \"rail\"
left_cols = 26
members_cols = 24
dms_share = 40
show_channels = true
show_dms = true
show_members = true
zen = false
";
        let c: Config = toml::from_str(text).expect("an old file still parses");
        assert_eq!(c.ui.padding_x, 2);
        assert_eq!(c.ui.list_rows, Ui::default().list_rows);
    }

    /// The one number the column's arithmetic reads out of the file.
    #[test]
    fn the_list_ceiling_has_a_default_and_can_be_set() {
        assert_eq!(Ui::default().list_rows, 8);
        let c: Config = toml::from_str("[ui]\nlist_rows = 3\n").unwrap();
        assert_eq!(c.ui.list_rows, 3);
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

    /// Everything the file says that the core has to be told reaches it.
    ///
    /// The two structs are separate on purpose, and the cost of that is that
    /// a key can be added to one and silently not carried to the other. This
    /// is the test that says it was: every non-default value set here comes
    /// back out the far side.
    #[test]
    fn every_setting_the_core_reads_is_carried_across() {
        let cfg = Config {
            auth: Auth { store: Store::File },
            media: Media {
                cache_max_mib: 77,
                max_attachment_mib: 9,
                player: vec!["mpv".into(), "--no-config".into()],
                ..Media::default()
            },
            gifs: Gifs {
                provider: "klipy".into(),
                media_format: "tinygif".into(),
                locale: "fr".into(),
            },
            notify: Notify {
                enabled: true,
                desktop: true,
                only_when_unfocused: false,
                dms_only: true,
                ..Notify::default()
            },
            ..Config::default()
        };
        let core = cfg.core();

        assert_eq!(core.store, crate::discord::auth::StorePreference::File);
        assert_eq!(core.media.cache_max_mib, 77);
        assert_eq!(core.media.max_attachment_mib, 9);
        assert_eq!(core.media.player, vec!["mpv", "--no-config"]);
        assert_eq!(core.gifs.name, "klipy");
        assert_eq!(core.gifs.media_format, "tinygif");
        assert_eq!(core.gifs.locale, "fr");
        assert_eq!(core.locale, "fr");
        assert!(core.notify.enabled, "[notify] desktop turns it on");
        assert!(!core.notify.only_when_unfocused);
        assert!(core.notify.dms_only);

        // And the default, which is a client that beeps and writes a line but
        // puts nothing on somebody else's screen.
        let quiet = Config::default().core();
        assert!(
            !quiet.notify.enabled,
            "[notify] desktop is off by default, so the core delivers nothing"
        );
    }

    /// And the defaults agree, so a file that says nothing gets the same
    /// client as a file that writes out the template.
    #[test]
    fn the_defaults_are_the_cores_defaults() {
        let core = Config::default().core();
        let theirs = crate::discord::handle::DiscordConfig::default();
        assert_eq!(core.media.cache_max_mib, theirs.media.cache_max_mib);
        assert_eq!(
            core.media.max_attachment_mib,
            theirs.media.max_attachment_mib
        );
        assert_eq!(core.media.player, theirs.media.player);
        assert_eq!(core.gifs.name, theirs.gifs.name);
        assert_eq!(core.gifs.media_format, theirs.gifs.media_format);
        assert_eq!(core.gifs.locale, theirs.gifs.locale);
        assert_eq!(core.notify.enabled, Config::default().notify.desktop);
        assert_eq!(
            core.notify.only_when_unfocused,
            theirs.notify.only_when_unfocused
        );
        assert_eq!(core.notify.dms_only, theirs.notify.dms_only);
        assert_eq!(core.store, theirs.store);
    }
}
