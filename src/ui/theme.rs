//! STAR/CORD's theme: the shared one, plus the roles a chat window needs.
//!
//! The sixteen built-in theme files are STAR/KIT's and are shared with
//! STAR/AMP. None of them says anything about a message list, and they should
//! not have to: a theme is eight colours and a base16 scheme, and everything
//! else is derived from those. So `[chat]` is derived here, out of the same
//! palette the core resolved, and a file that *does* state a `[chat]` table
//! has the last word.
//!
//! Derivation rather than a table of literals is what keeps a theme honest.
//! Sixteen files times nineteen roles is three hundred colours nobody would
//! ever check; one rule per role, run over sixteen palettes and asserted
//! legible by a test, is three hundred colours that are all correct.

use std::ops::Deref;

use serde::{Deserialize, Serialize};
use starkit::theme::color::Rgb;
use starkit::theme::{pick, Registry, Resolve, Theme as Core, ThemeFile};

/// The `[chat]` table, as a theme file may state it.
///
/// Every field optional: a theme states the two it cares about and lets the
/// rest fall out of its palette.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatColors {
    pub author_fg: Option<Rgb>,
    pub time_fg: Option<Rgb>,
    pub mention_fg: Option<Rgb>,
    pub mention_bg: Option<Rgb>,
    pub link_fg: Option<Rgb>,
    pub code_fg: Option<Rgb>,
    pub code_bg: Option<Rgb>,
    pub spoiler_bg: Option<Rgb>,
    pub embed_bar: Option<Rgb>,
    pub divider_fg: Option<Rgb>,
    pub unread_fg: Option<Rgb>,
    pub reaction_bg: Option<Rgb>,
    pub reaction_me_bg: Option<Rgb>,
    pub presence_online: Option<Rgb>,
    pub presence_idle: Option<Rgb>,
    pub presence_dnd: Option<Rgb>,
    pub presence_offline: Option<Rgb>,
    pub system_fg: Option<Rgb>,
    pub disconnected_dim: Option<Rgb>,
}

/// The same roles, resolved.
#[derive(Debug, Clone)]
pub struct Chat {
    pub author_fg: Rgb,
    pub time_fg: Rgb,
    pub mention_fg: Rgb,
    pub mention_bg: Rgb,
    pub link_fg: Rgb,
    pub code_fg: Rgb,
    pub code_bg: Rgb,
    pub spoiler_bg: Rgb,
    pub embed_bar: Rgb,
    pub divider_fg: Rgb,
    pub unread_fg: Rgb,
    pub reaction_bg: Rgb,
    pub reaction_me_bg: Rgb,
    pub presence_online: Rgb,
    pub presence_idle: Rgb,
    pub presence_dnd: Rgb,
    pub presence_offline: Rgb,
    pub system_fg: Rgb,
    pub disconnected_dim: Rgb,
}

/// WCAG AA for normal text. Anything carrying words clears this against
/// whatever it is drawn on.
const TEXT_CONTRAST: f64 = 4.5;

/// WCAG AA for a graphical mark: a presence dot, a bar down the side of an
/// embed. They carry no letters, so the text threshold would only make every
/// theme's dots the same shade of loud.
const MARK_CONTRAST: f64 = 3.0;

/// How far a tinted background is pulled from the panel toward its own colour.
///
/// Low. A mention's highlight and a reaction chip are drawn behind text that
/// still has to be read, and every step toward the tint is a step away from
/// the contrast the text had.
const TINT: f64 = 0.20;

/// The same, for the chip marking a reaction this account added. Twice the
/// tint of an ordinary one, which is the whole difference between them.
const ME_TINT: f64 = 0.38;

/// What the file said, or what the palette implies held to a contrast floor.
///
/// The floor is only ever applied to a *derived* colour. A theme that names a
/// role names it: the derivation is a way of not having to write three hundred
/// colours, not a committee sitting over the ones somebody did write.
fn stated_or(stated: Option<Rgb>, derived: Rgb, against: Rgb, target: f64) -> Rgb {
    stated.unwrap_or_else(|| derived.ensure_contrast(against, target))
}

impl Chat {
    fn derive(core: &Core, f: &ChatColors, b16: Option<&starkit::theme::schema::Base16>) -> Self {
        let bg = core.panel_bg;

        // Every role names the base16 slot it comes from, because that is the
        // thing a reader has to be able to check. The spec's own meanings:
        // 08 red, 09 orange, 0A yellow, 0B green, 0C cyan, 0D blue, 0E magenta.
        let author_fg = stated_or(
            f.author_fg,
            pick(None, b16.map(|b| b.base0C), core.accent),
            bg,
            TEXT_CONTRAST,
        );
        let link_fg = stated_or(
            f.link_fg,
            pick(None, b16.map(|b| b.base0D), core.accent),
            bg,
            TEXT_CONTRAST,
        );

        // Backgrounds are mixed from the panel toward the role they belong to,
        // so a highlight reads as the same surface lit differently rather than
        // as a sticker laid on top of it. Each is then held away from the text
        // that will be drawn on it, which is the direction that has to give:
        // `fg` is the theme's body colour and is not this table's to move.
        let mention_base = pick(f.mention_fg, b16.map(|b| b.base0A), core.warn);
        let mention_bg = stated_or(
            f.mention_bg,
            bg.mix(mention_base, TINT),
            mention_base,
            TEXT_CONTRAST,
        );
        let mention_fg = stated_or(f.mention_fg, mention_base, mention_bg, TEXT_CONTRAST);

        let code_bg = stated_or(
            f.code_bg,
            pick(None, b16.map(|b| b.base01), bg.mix(core.fg, 0.10)),
            core.fg,
            TEXT_CONTRAST,
        );
        let code_fg = stated_or(
            f.code_fg,
            pick(None, b16.map(|b| b.base0B), core.ok),
            code_bg,
            TEXT_CONTRAST,
        );

        let reaction_bg = stated_or(
            f.reaction_bg,
            pick(None, b16.map(|b| b.base01), bg.mix(core.fg, 0.12)),
            core.fg,
            TEXT_CONTRAST,
        );
        let reaction_me_bg = stated_or(
            f.reaction_me_bg,
            bg.mix(core.accent, ME_TINT),
            core.fg,
            TEXT_CONTRAST,
        );

        Self {
            author_fg,
            // Timestamps are the quietest text on the screen and still text:
            // `dim` already clears AA, which is why it is reused rather than
            // derived again.
            time_fg: stated_or(f.time_fg, core.dim, bg, TEXT_CONTRAST),
            mention_fg,
            mention_bg,
            link_fg,
            code_fg,
            code_bg,
            spoiler_bg: stated_or(
                f.spoiler_bg,
                pick(None, b16.map(|b| b.base02), bg.mix(core.fg, 0.28)),
                bg,
                MARK_CONTRAST,
            ),
            embed_bar: stated_or(
                f.embed_bar,
                pick(None, b16.map(|b| b.base0E), core.accent),
                bg,
                MARK_CONTRAST,
            ),
            // The panel chrome's own divider, held away from the panel.
            // `divider` is drawn between two rows of a list, where being
            // barely there is the point; a day rule across the message list
            // carries a date on it, and at the 1.46:1 one theme resolves to
            // that date is a rumour.
            divider_fg: stated_or(f.divider_fg, core.divider, bg, MARK_CONTRAST),
            // Unread is the one colour that has to be found without looking
            // for it, so it borrows the palette's orange rather than its
            // accent, which a theme may have spent on something else.
            unread_fg: stated_or(
                f.unread_fg,
                pick(None, b16.map(|b| b.base09), core.warn),
                bg,
                TEXT_CONTRAST,
            ),
            reaction_bg,
            reaction_me_bg,
            presence_online: stated_or(
                f.presence_online,
                pick(None, b16.map(|b| b.base0B), core.ok),
                bg,
                MARK_CONTRAST,
            ),
            presence_idle: stated_or(
                f.presence_idle,
                pick(None, b16.map(|b| b.base0A), core.warn),
                bg,
                MARK_CONTRAST,
            ),
            presence_dnd: stated_or(
                f.presence_dnd,
                pick(None, b16.map(|b| b.base08), core.error),
                bg,
                MARK_CONTRAST,
            ),
            // Offline is the absence of a presence and should read as one.
            presence_offline: stated_or(
                f.presence_offline,
                core.dim.mix(bg, 0.35),
                bg,
                MARK_CONTRAST,
            ),
            system_fg: stated_or(f.system_fg, core.dim, bg, TEXT_CONTRAST),
            // What the message list is drawn in while the connection is down.
            // Still text -- a reader should be able to finish the sentence
            // they were on -- so it clears AA like the rest of it.
            disconnected_dim: stated_or(
                f.disconnected_dim,
                core.fg.mix(bg, 0.45),
                bg,
                TEXT_CONTRAST,
            ),
        }
    }

    /// The roles that carry words, and what each is drawn on. The legibility
    /// test walks this; naming it here is what stops a new role being added
    /// without one.
    #[cfg(test)]
    fn text_roles(&self, panel_bg: Rgb, fg: Rgb) -> Vec<(&'static str, Rgb, Rgb)> {
        vec![
            ("author_fg", self.author_fg, panel_bg),
            ("time_fg", self.time_fg, panel_bg),
            ("link_fg", self.link_fg, panel_bg),
            ("mention_fg", self.mention_fg, self.mention_bg),
            ("code_fg", self.code_fg, self.code_bg),
            ("unread_fg", self.unread_fg, panel_bg),
            ("system_fg", self.system_fg, panel_bg),
            ("disconnected_dim", self.disconnected_dim, panel_bg),
            ("reaction chip", fg, self.reaction_bg),
            ("reaction chip (mine)", fg, self.reaction_me_bg),
        ]
    }

    #[cfg(test)]
    fn mark_roles(&self, panel_bg: Rgb) -> Vec<(&'static str, Rgb, Rgb)> {
        vec![
            ("presence_online", self.presence_online, panel_bg),
            ("presence_idle", self.presence_idle, panel_bg),
            ("presence_dnd", self.presence_dnd, panel_bg),
            ("presence_offline", self.presence_offline, panel_bg),
            ("embed_bar", self.embed_bar, panel_bg),
            ("spoiler_bg", self.spoiler_bg, panel_bg),
            // The day and unread rules. They carry a label, but the label is
            // drawn in the same colour as the rule and is not prose: the mark
            // threshold is the honest one for a line across a panel.
            ("divider_fg", self.divider_fg, panel_bg),
        ]
    }
}

/// The theme STAR/CORD draws with: the shared one, plus `[chat]`.
///
/// `Deref` rather than a hundred delegating accessors, so `theme.accent` and
/// `theme.chat.link_fg` read the same way and every STAR/KIT widget takes
/// `&*theme`.
#[derive(Debug, Clone)]
pub struct Theme {
    core: Core,
    pub chat: Chat,
}

impl Deref for Theme {
    type Target = Core;

    fn deref(&self) -> &Core {
        &self.core
    }
}

impl Resolve for Theme {
    fn resolve(file: &ThemeFile) -> Self {
        let core = Core::resolve(file);
        // A malformed `[chat]` table costs the table, not the theme. The core
        // tables fail loudly because a broken palette is unusable; a broken
        // accent colour on a reaction chip is not worth refusing to start
        // over.
        let stated: ChatColors = file.table("chat").unwrap_or_else(|e| {
            tracing::warn!("{}: the [chat] table was ignored: {e}", core.id);
            ChatColors::default()
        });
        let chat = Chat::derive(&core, &stated, file.base16.as_ref());
        Self { core, chat }
    }

    fn core(&self) -> &Core {
        &self.core
    }
}

/// Where the themes come from.
///
/// The same directories the rest of STAR/CORD's files live under -- this
/// application's own [`crate::paths::PATHS`], spelled here under the name
/// [`Registry`] takes.
pub const THEME_PATHS: starkit::paths::Paths = crate::paths::PATHS;

pub fn registry() -> Registry<Theme> {
    Registry::new(THEME_PATHS)
}

/// A resolved built-in, for the tests in every other module that need
/// something to draw with.
#[cfg(test)]
pub mod tests_support {
    use super::{Resolve as _, Theme, ThemeFile};

    pub fn theme(id: &str) -> Theme {
        let b = starkit::theme::BUILTINS
            .iter()
            .find(|b| b.id == id)
            .unwrap_or_else(|| panic!("no built-in {id}"));
        Theme::resolve(&ThemeFile::parse(b.toml).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::theme;
    use super::*;
    use starkit::theme::BUILTINS;

    /// The test the whole derivation exists to pass.
    ///
    /// Sixteen palettes, nineteen roles, and nobody looking at any of them.
    /// A rule that produces an unreadable colour on one scheme in sixteen is
    /// the normal outcome of writing rules for colours, and this is what
    /// catches it.
    #[test]
    fn every_builtin_chat_role_is_legible() {
        for b in BUILTINS {
            assert_legible(b.id, &theme(b.id));
        }

        // And the desktop's own palette, where there is one. `system` is the
        // one theme nobody here chose: it arrives from Stylix or from COSMIC
        // and is whatever the machine's colours happen to be, which is exactly
        // the case a rule written against sixteen known palettes can fail on.
        // Skipped rather than faked where no desktop theme is set, because a
        // synthesised one would be a seventeenth builtin with a misleading
        // name.
        if let Some((file, _)) = starkit::theme::system::theme() {
            assert_legible("system", &Theme::resolve(&file));
        }
    }

    fn assert_legible(id: &str, t: &Theme) {
        for (role, fg, bg) in t.chat.text_roles(t.panel_bg, t.fg) {
            let c = bg.contrast(fg);
            assert!(
                c >= TEXT_CONTRAST,
                "{id}: {role} is {c:.2}:1 against its background"
            );
        }
        for (role, fg, bg) in t.chat.mark_roles(t.panel_bg) {
            let c = bg.contrast(fg);
            assert!(
                c >= MARK_CONTRAST,
                "{id}: {role} is {c:.2}:1 against its background"
            );
        }
    }

    /// Every built-in resolves, including the three that carry only `[meta]`
    /// and `[base16]`.
    #[test]
    fn every_builtin_resolves() {
        for b in BUILTINS {
            let t = theme(b.id);
            assert_eq!(t.id, b.id, "{} resolved under another id", b.id);
        }
    }

    /// The four presence colours have to be told apart at a glance, or the dot
    /// is decoration. Derived from four different base16 slots, so this is
    /// really a check that no theme collapses them.
    #[test]
    fn the_presence_colours_are_distinguishable() {
        for b in BUILTINS {
            let t = theme(b.id);
            let dots = [
                ("online", t.chat.presence_online),
                ("idle", t.chat.presence_idle),
                ("dnd", t.chat.presence_dnd),
            ];
            for (i, (a_name, a)) in dots.iter().enumerate() {
                for (b_name, other) in &dots[i + 1..] {
                    assert!(
                        a != other,
                        "{}: {a_name} and {b_name} are the same colour",
                        b.id
                    );
                }
            }
        }
    }

    /// A file that states a role gets that role, unchanged, whatever the
    /// derivation would have produced. Themes are allowed to be exact.
    #[test]
    fn a_stated_role_wins_over_the_derivation() {
        let f = ThemeFile::parse(
            r##"
            [meta]
            name = "Stated"
            variant = "dark"
            [app]
            bg = "#000000"
            fg = "#ffffff"
            [chat]
            link_fg = "#123456"
            presence_idle = "#abcdef"
            "##,
        )
        .unwrap();
        let t = Theme::resolve(&f);
        assert_eq!(t.chat.link_fg, Rgb::new(0x12, 0x34, 0x56));
        assert_eq!(t.chat.presence_idle, Rgb::new(0xab, 0xcd, 0xef));
    }

    /// A `[chat]` table that is not a `[chat]` table costs the table and
    /// nothing else. The core tables still fail loudly; this one is decoration
    /// over a working palette.
    #[test]
    fn a_malformed_chat_table_does_not_lose_the_theme() {
        let f = ThemeFile::parse(
            r##"
            [meta]
            name = "Broken"
            variant = "dark"
            [app]
            bg = "#101010"
            fg = "#e0e0e0"
            [chat]
            link_fg = "not a colour"
            "##,
        )
        .unwrap();
        let t = Theme::resolve(&f);
        assert_eq!(t.bg, Rgb::new(0x10, 0x10, 0x10));
        assert!(t.panel_bg.contrast(t.chat.link_fg) >= TEXT_CONTRAST);
    }
}
