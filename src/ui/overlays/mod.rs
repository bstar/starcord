//! Modal things drawn over everything else.
//!
//! One rule holds them together and is the reason they are gathered in a
//! module rather than scattered: **[`Overlays::open`] is the first check in
//! both `handle` and `handle_mouse`.** An overlay that is drawn over a panel
//! and does not take that panel's keys is a dialogue you can type through,
//! which is the bug this arrangement makes impossible to write.
//!
//! Eight of them: the help, the confirmation, the quick switcher, the panel
//! settings, the emoji and GIF picker, the media viewer, the search, the
//! attach-a-file box and the message menu. Each is a field on this struct and
//! an arm in the functions below.
//!
//! Only one is ever open. Stacking them would mean deciding what `esc` closes,
//! and the answer "the innermost one" is a stack somebody has to keep in their
//! head; the answer "the one that is open" is not.
//!
//! ## Two things flow out of here besides keys
//!
//! An overlay that has to *ask the core something* — the GIF grid, the search
//! box — pushes a [`Command`] into its own queue, and [`Overlays::take_commands`]
//! is drained once a frame. That keeps a request that is made repeatedly, with
//! a debounce and an id to match answers against, inside the overlay that owns
//! it rather than spread across the dispatcher.
//!
//! An overlay that draws a *picture* — the GIF tiles, the media viewer, the
//! custom emoji in the grid — returns [`Placement`]s from `render`, and the
//! application paints them after the chrome is down. It cannot paint them with
//! the panels' pictures: an overlay clears the cells it covers, and a protocol
//! image whose cell has been cleared is one the terminal is never told about.

pub mod attach;
pub mod confirm;
pub mod media;
pub mod menu;
pub mod picker;
pub mod quick;
pub mod search;
pub mod settings;

use std::path::PathBuf;
use std::time::Instant;

use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::keymap::HelpView;
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::widgets::Widget;

use self::attach::Attach;
use self::confirm::{Confirm, Pending};
use self::media::Viewer;
use self::menu::{Choice, Menu};
use self::picker::Picker;
use self::quick::{Quick, Target};
use self::search::Search;
use self::settings::{Setting, Settings};
use crate::config::Config;
use crate::discord::handle::{RequestId, SearchPage};
use crate::discord::model::{GifResult, Message};
use crate::discord::snowflake::{ChannelId, MessageId};
use crate::discord::Command;
use crate::ui::keymap::{BINDINGS, MOUSE};
use crate::ui::panels::chat::media::{MediaStore, Placement};
use crate::ui::panels::ModuleId;
use crate::ui::theme::Theme;

/// What an overlay did with a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// Nothing was open; the key belongs to whatever is underneath.
    Ignored,
    /// The overlay had it, whether or not it did anything with it.
    Taken,
    /// Quitting works from here as it does from everywhere.
    Quit,
    /// The confirmation was answered yes.
    Confirmed(Pending),
    /// The switcher chose somewhere to go.
    Jump(Target),
    /// The search chose a message to go to.
    JumpToMessage {
        channel: ChannelId,
        message: MessageId,
    },
    /// A settings row was changed; `true` steps forward.
    Setting(Setting, bool),
    /// Put this text into the composer at the caret.
    Insert(String),
    /// One of the things the message menu offers, for the message it was
    /// opened on.
    Menu(Choice, MessageId),
    /// A file that exists and is small enough, to be made a chip of.
    Attach(PathBuf),
    /// Hand this to whatever the system opens things with.
    Open(String),
    /// Put this on the clipboard.
    Copy(String),
    /// Write what the media viewer is showing to `[media] save_dir`.
    SaveMedia,
}

/// Everything modal, and whether any of it is up.
#[derive(Debug, Default)]
pub struct Overlays {
    pub help: bool,
    pub help_scroll: u16,
    pub confirm: Option<Confirm>,
    pub quick: Option<Quick>,
    pub settings: Option<Settings>,
    pub picker: Option<Picker>,
    pub viewer: Option<Viewer>,
    pub search: Option<Search>,
    pub attach: Option<Attach>,
    pub menu: Option<Menu>,
    /// Commands from an overlay that has since closed.
    ///
    /// Choosing a GIF sends a message and closes the picker in one keystroke,
    /// and the picker is dropped before the frame's drain gets to it. Without
    /// this the message would go nowhere at all, which is exactly what it did.
    queued: Vec<Command>,
}

impl Overlays {
    /// Whether anything modal is on screen.
    ///
    /// Checked first in both `handle` and `handle_mouse`, so that a key or a
    /// click reaches the overlay rather than the panel under it.
    pub fn open(&self) -> bool {
        self.help
            || self.confirm.is_some()
            || self.quick.is_some()
            || self.settings.is_some()
            || self.picker.is_some()
            || self.viewer.is_some()
            || self.search.is_some()
            || self.attach.is_some()
            || self.menu.is_some()
    }

    /// Whether the open overlay is a text field, so bracketed paste goes to it.
    pub fn takes_paste(&self) -> bool {
        self.quick.is_some()
            || self.picker.is_some()
            || self.search.is_some()
            || self.attach.is_some()
    }

    pub fn close(&mut self) {
        self.help = false;
        self.help_scroll = 0;
        self.confirm = None;
        self.quick = None;
        self.settings = None;
        self.close_picker();
        self.viewer = None;
        self.close_search();
        self.attach = None;
        self.menu = None;
    }

    pub fn toggle_help(&mut self) {
        if self.help {
            self.close();
        } else {
            self.close();
            self.help = true;
            self.help_scroll = 0;
        }
    }

    pub fn ask(&mut self, confirm: Confirm) {
        self.close();
        self.confirm = Some(confirm);
    }

    pub fn open_quick(&mut self, items: Vec<quick::Item>) {
        self.close();
        self.quick = Some(Quick::new(items));
    }

    pub fn open_settings(&mut self, module: ModuleId) {
        self.close();
        self.settings = Some(Settings::new(module));
    }

    pub fn open_picker(&mut self, picker: Picker) {
        self.close();
        self.picker = Some(picker);
    }

    pub fn open_viewer(&mut self, viewer: Viewer) {
        self.close();
        self.viewer = Some(viewer);
    }

    pub fn open_search(&mut self, search: Search) {
        self.close();
        self.search = Some(search);
    }

    pub fn open_attach(&mut self, attach: Attach) {
        self.close();
        self.attach = Some(attach);
    }

    pub fn open_menu(&mut self, menu: Menu) {
        self.close();
        self.menu = Some(menu);
    }

    /// The debounce, and anything else an overlay does on a clock.
    pub fn tick(&mut self, now: Instant) {
        if let Some(picker) = &mut self.picker {
            picker.tick(now);
        }
    }

    /// What the open overlay wants asked of the core.
    pub fn take_commands(&mut self) -> Vec<Command> {
        let mut out = std::mem::take(&mut self.queued);
        if let Some(picker) = &mut self.picker {
            out.extend(picker.take_commands());
        }
        if let Some(search) = &mut self.search {
            out.extend(search.take_commands());
        }
        out
    }

    /// Take an overlay's commands before the overlay goes away.
    fn close_picker(&mut self) {
        if let Some(mut picker) = self.picker.take() {
            self.queued.extend(picker.take_commands());
        }
    }

    fn close_search(&mut self) {
        if let Some(mut search) = self.search.take() {
            self.queued.extend(search.take_commands());
        }
    }

    /// A page of GIF results. True when it was one this overlay asked for.
    pub fn gifs_arrived(&mut self, id: RequestId, result: Result<Vec<GifResult>, String>) -> bool {
        match &mut self.picker {
            Some(picker) => picker.gifs_arrived(id, result),
            None => false,
        }
    }

    /// A page of search results.
    pub fn search_arrived(
        &mut self,
        id: RequestId,
        result: Result<SearchPage, String>,
        name_of: impl Fn(&Message) -> (String, String),
    ) -> bool {
        match &mut self.search {
            Some(search) => search.arrived(id, result, name_of),
            None => false,
        }
    }

    /// Keys, while something is open.
    ///
    /// [`Key::Ignored`] only ever means "nothing is open". Once one is, every
    /// key is taken: a modal overlay that let a key through to the panel it is
    /// drawn over is a dialogue you can type through.
    pub fn handle(&mut self, key: KeyEvent) -> Key {
        if let Some(confirm) = &self.confirm {
            let pending = confirm.on_yes.clone();
            return match confirm::answer(key) {
                confirm::Answer::Yes => {
                    self.confirm = None;
                    Key::Confirmed(pending)
                }
                confirm::Answer::No => {
                    self.confirm = None;
                    Key::Taken
                }
                confirm::Answer::Quit => Key::Quit,
                confirm::Answer::Waiting => Key::Taken,
            };
        }

        if let Some(quick) = &mut self.quick {
            return match quick.handle(key) {
                quick::Action::Taken => Key::Taken,
                quick::Action::Close => {
                    self.quick = None;
                    Key::Taken
                }
                quick::Action::Open(target) => {
                    self.quick = None;
                    Key::Jump(target)
                }
                quick::Action::Quit => Key::Quit,
            };
        }

        if let Some(picker) = &mut self.picker {
            return match picker.handle(key) {
                picker::Action::Taken => Key::Taken,
                picker::Action::Close => {
                    self.close_picker();
                    Key::Taken
                }
                picker::Action::Insert(text) => {
                    self.close_picker();
                    Key::Insert(text)
                }
                picker::Action::ToGif => {
                    picker.switch_to_gifs();
                    Key::Taken
                }
                picker::Action::Quit => Key::Quit,
            };
        }

        if let Some(viewer) = &mut self.viewer {
            return match viewer.handle(key) {
                media::Action::Taken => Key::Taken,
                media::Action::Close => {
                    self.viewer = None;
                    Key::Taken
                }
                media::Action::Open(url) => Key::Open(url),
                media::Action::Copy(url) => Key::Copy(url),
                media::Action::Save => Key::SaveMedia,
                media::Action::Quit => Key::Quit,
            };
        }

        if let Some(search) = &mut self.search {
            return match search.handle(key) {
                search::Action::Taken => Key::Taken,
                search::Action::Close => {
                    self.close_search();
                    Key::Taken
                }
                search::Action::Jump { channel, message } => {
                    self.close_search();
                    Key::JumpToMessage { channel, message }
                }
                search::Action::Quit => Key::Quit,
            };
        }

        if let Some(attach) = &mut self.attach {
            return match attach.handle(key) {
                attach::Action::Taken => Key::Taken,
                attach::Action::Close => {
                    self.attach = None;
                    Key::Taken
                }
                attach::Action::Attach(path) => {
                    self.attach = None;
                    Key::Attach(path)
                }
                attach::Action::Quit => Key::Quit,
            };
        }

        if let Some(menu) = &mut self.menu {
            let message = menu.message;
            return match menu.handle(key) {
                menu::Action::Taken => Key::Taken,
                menu::Action::Close => {
                    self.menu = None;
                    Key::Taken
                }
                menu::Action::Chose(choice) => {
                    self.menu = None;
                    Key::Menu(choice, message)
                }
                menu::Action::Quit => Key::Quit,
            };
        }

        if let Some(settings) = &mut self.settings {
            return match settings.handle(key) {
                settings::Action::Taken => Key::Taken,
                settings::Action::Close => {
                    self.settings = None;
                    Key::Taken
                }
                settings::Action::Change(setting, forward) => Key::Setting(setting, forward),
                settings::Action::Quit => Key::Quit,
            };
        }

        if !self.help {
            return Key::Ignored;
        }
        // Raw keys rather than the table: `j` scrolls the list here and should
        // not also move a cursor in a panel nobody can see.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => return Key::Quit,
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1) | KeyCode::Char('q') => self.close(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.help_scroll = self.help_scroll.saturating_add(1)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.help_scroll = self.help_scroll.saturating_sub(1)
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.help_scroll = self.help_scroll.saturating_add(10)
            }
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
            KeyCode::Home => self.help_scroll = 0,
            _ => {}
        }
        Key::Taken
    }

    /// A bracketed paste, while a text overlay is open.
    pub fn paste(&mut self, text: &str) {
        if let Some(quick) = &mut self.quick {
            quick.paste(text);
        }
        if let Some(picker) = &mut self.picker {
            picker.paste(text);
        }
        if let Some(search) = &mut self.search {
            search.paste(text);
        }
        if let Some(attach) = &mut self.attach {
            attach.paste(text);
        }
    }

    /// A click, while something is open.
    ///
    /// The ones with something to click take the click; everything else
    /// closes, which is what a click outside a dialogue has always meant.
    pub fn click(&mut self, area: Rect, x: u16, y: u16) -> Key {
        if let Some(confirm) = &self.confirm {
            let pending = confirm.on_yes.clone();
            let hit = confirm::layout(area, confirm).and_then(|l| confirm::hit(&l, x, y));
            self.confirm = None;
            return match hit {
                Some(confirm::Answer::Yes) => Key::Confirmed(pending),
                _ => Key::Taken,
            };
        }
        if let Some(settings) = &mut self.settings {
            return match settings.click(area, x, y) {
                settings::Action::Change(setting, forward) => Key::Setting(setting, forward),
                settings::Action::Close => {
                    self.settings = None;
                    Key::Taken
                }
                _ => Key::Taken,
            };
        }
        if let Some(menu) = &mut self.menu {
            let message = menu.message;
            return match menu.click(area, x, y) {
                menu::Action::Chose(choice) => {
                    self.menu = None;
                    Key::Menu(choice, message)
                }
                _ => {
                    self.menu = None;
                    Key::Taken
                }
            };
        }
        if let Some(picker) = &mut self.picker {
            let r = picker::rect(area, picker.is_gif());
            if let Some(index) = picker.hit(r, x, y) {
                picker.cursor = index;
                return match picker.choose() {
                    picker::Action::Insert(text) => {
                        self.close_picker();
                        Key::Insert(text)
                    }
                    picker::Action::Close => {
                        self.close_picker();
                        Key::Taken
                    }
                    _ => Key::Taken,
                };
            }
            self.close_picker();
            return Key::Taken;
        }
        if let Some(viewer) = &mut self.viewer {
            let r = media::rect(area);
            if x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height {
                viewer.click(r, x, y);
                return Key::Taken;
            }
            self.viewer = None;
            return Key::Taken;
        }
        if let Some(search) = &mut self.search {
            if let Some(index) = search.hit_at(area, x, y) {
                search.cursor = index;
                if let Some(hit) = search.selected() {
                    let (channel, message) = (hit.channel, hit.message);
                    self.close_search();
                    return Key::JumpToMessage { channel, message };
                }
                return Key::Taken;
            }
            self.close_search();
            return Key::Taken;
        }
        self.close();
        Key::Taken
    }

    /// The wheel, while something is open.
    pub fn scroll(&mut self, delta: i16) {
        if self.help {
            self.help_scroll = self.help_scroll.saturating_add_signed(delta);
        }
        if let Some(quick) = &mut self.quick {
            quick.scroll(delta);
        }
        if let Some(settings) = &mut self.settings {
            settings.scroll_by(delta);
        }
        if let Some(picker) = &mut self.picker {
            picker.scroll_by(delta);
        }
        if let Some(viewer) = &mut self.viewer {
            viewer.step(delta.signum() as isize);
        }
        if let Some(search) = &mut self.search {
            search.scroll_by(delta);
        }
        if let Some(menu) = &mut self.menu {
            menu.scroll_by(delta);
        }
    }

    /// Draw whatever is open, and hand back the pictures it placed and the
    /// caret it wants shown, if it is a text field.
    ///
    /// Only the four typing boxes -- the quick switcher, search, attach and
    /// the emoji/GIF picker -- ever answer with `Some`; every other overlay
    /// answers `None`, which is what makes the composer's own caret stop
    /// showing through once one of them is open.
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        cfg: &Config,
        store: &MediaStore,
    ) -> (Vec<Placement>, Option<(u16, u16)>) {
        if let Some(confirm) = &self.confirm {
            confirm::render(area, buf, theme, confirm);
            return (Vec::new(), None);
        }
        if let Some(quick) = &mut self.quick {
            let caret = quick::render(area, buf, theme, quick);
            return (Vec::new(), caret);
        }
        if let Some(picker) = &mut self.picker {
            return picker::render(area, buf, theme, picker);
        }
        if let Some(viewer) = &self.viewer {
            return (media::render(area, buf, theme, viewer, store), None);
        }
        if let Some(search) = &mut self.search {
            let caret = search::render(area, buf, theme, search);
            return (Vec::new(), caret);
        }
        if let Some(attach) = &mut self.attach {
            let caret = attach::render(area, buf, theme, attach);
            return (Vec::new(), caret);
        }
        if let Some(menu) = &self.menu {
            menu.render(area, buf, theme);
            return (Vec::new(), None);
        }
        if let Some(settings) = &self.settings {
            settings.render(area, buf, theme, cfg);
            return (Vec::new(), None);
        }
        if !self.help {
            return (Vec::new(), None);
        }
        HelpView {
            theme,
            bindings: BINDINGS,
            mouse: MOUSE,
            scroll: self.help_scroll,
            title: "keys",
        }
        .render(area, buf);
        (Vec::new(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::{ChannelId, MessageId};
    use crate::ui::theme::tests_support::theme;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn every_overlay() -> Vec<fn(&mut Overlays)> {
        vec![
            |o: &mut Overlays| o.toggle_help(),
            |o: &mut Overlays| o.ask(Confirm::quit_with_draft(1)),
            |o: &mut Overlays| o.open_quick(Vec::new()),
            |o: &mut Overlays| o.open_settings(ModuleId::Conversation),
            |o: &mut Overlays| {
                o.open_picker(Picker::new(picker::Kind::Emoji, None, Vec::new(), 2.0))
            },
            |o: &mut Overlays| {
                o.open_viewer(
                    Viewer::new(
                        vec![media::Item {
                            message: MessageId(1),
                            key: crate::discord::media::MediaKey::Gif {
                                url: "https://x.invalid/a.gif".into(),
                            },
                            url: "https://x.invalid/a.gif".into(),
                            filename: "a.gif".into(),
                        }],
                        None,
                        2.0,
                    )
                    .unwrap(),
                )
            },
            |o: &mut Overlays| {
                o.open_search(Search::new(
                    crate::discord::handle::SearchScope::Channel(ChannelId(1)),
                    "#general".into(),
                ))
            },
            |o: &mut Overlays| o.open_attach(Attach::new(25)),
            |o: &mut Overlays| o.open_menu(Menu::new(MessageId(1), true)),
        ]
    }

    #[test]
    fn nothing_is_open_to_begin_with() {
        let mut o = Overlays::default();
        assert!(!o.open());
        assert_eq!(o.handle(key('j')), Key::Ignored);
    }

    #[test]
    fn the_help_opens_and_closes_on_the_same_key() {
        let mut o = Overlays::default();
        o.toggle_help();
        assert!(o.open());
        assert_eq!(o.handle(key('?')), Key::Taken);
        assert!(!o.open());
    }

    /// Escape closes; it does not quit. The rule the key table holds, held
    /// again here because an overlay is the place it is most tempting to
    /// break.
    #[test]
    fn escape_closes_the_overlay_rather_than_the_program() {
        for open in every_overlay() {
            let mut o = Overlays::default();
            open(&mut o);
            let answer = o.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert_ne!(answer, Key::Quit, "escape quit");
            assert!(!o.open(), "escape did not close it");
        }
    }

    /// A modal overlay is modal: while it is up, no key reaches the panel
    /// underneath. This is the test that fails if somebody adds a
    /// fall-through.
    #[test]
    fn no_key_falls_through_an_open_overlay() {
        let mut o = Overlays::default();
        for c in ['t', 'r', 'd', 'x', 'i', 'g'] {
            o.close();
            o.help = true;
            o.help_scroll = 0;
            assert_eq!(
                o.handle(key(c)),
                Key::Taken,
                "{c:?} fell through to the panel below"
            );
            assert!(o.open(), "{c:?} closed it");
        }
    }

    /// The same, for every other overlay. A dialogue you can type through is
    /// the failure this module exists to make impossible, so it is asserted
    /// once per overlay rather than once.
    #[test]
    fn every_overlay_is_modal() {
        for open in every_overlay() {
            let mut o = Overlays::default();
            open(&mut o);
            assert!(o.open());
            assert_ne!(o.handle(key('t')), Key::Ignored, "a key fell through");
        }
    }

    /// Except quitting, which works from everywhere including here.
    #[test]
    fn ctrl_c_still_quits() {
        for open in every_overlay() {
            let mut o = Overlays::default();
            open(&mut o);
            assert_eq!(
                o.handle(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                Key::Quit
            );
        }
    }

    /// Yes carries the thing it was asked about; no carries nothing.
    #[test]
    fn a_confirmation_answers_once() {
        let mut o = Overlays::default();
        let pending = Pending::DeleteMessage {
            channel: ChannelId(1),
            message: MessageId(2),
        };
        o.ask(Confirm::delete(ChannelId(1), MessageId(2), "hello"));
        assert_eq!(o.handle(key('y')), Key::Confirmed(pending));
        assert!(!o.open(), "and it closed itself");

        o.ask(Confirm::delete(ChannelId(1), MessageId(2), "hello"));
        assert_eq!(o.handle(key('n')), Key::Taken);
        assert!(!o.open());
    }

    /// Only one at a time: opening one closes whatever was up.
    #[test]
    fn opening_one_closes_the_others() {
        let mut o = Overlays::default();
        for open in every_overlay() {
            open(&mut o);
            let up = [
                o.help,
                o.confirm.is_some(),
                o.quick.is_some(),
                o.settings.is_some(),
                o.picker.is_some(),
                o.viewer.is_some(),
                o.search.is_some(),
                o.attach.is_some(),
                o.menu.is_some(),
            ]
            .into_iter()
            .filter(|up| *up)
            .count();
            assert_eq!(up, 1, "two overlays at once");
        }
    }

    #[test]
    fn scrolling_never_goes_above_the_top() {
        let mut o = Overlays::default();
        o.toggle_help();
        o.handle(key('k'));
        assert_eq!(o.help_scroll, 0);
        o.handle(key('j'));
        assert_eq!(o.help_scroll, 1);
        o.scroll(-5);
        assert_eq!(o.help_scroll, 0);
    }

    #[test]
    fn a_closed_overlay_draws_nothing() {
        let t = theme("terminal");
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        let before = buf.clone();
        let (places, caret) =
            Overlays::default().render(area, &mut buf, &t, &Config::default(), &MediaStore::new());
        assert_eq!(buf, before);
        assert!(places.is_empty());
        assert_eq!(caret, None);
    }

    fn drawn(o: &mut Overlays) -> String {
        let t = theme("terminal");
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        o.render(area, &mut buf, &t, &Config::default(), &MediaStore::new());
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_open_help_draws_the_key_table() {
        let mut o = Overlays::default();
        o.toggle_help();
        let text = drawn(&mut o);
        assert!(text.contains("navigation"), "{text}");
        assert!(text.contains("next module"), "{text}");
        // The list is longer than any terminal is tall, which is what the
        // scroll is for. The end of it is reachable rather than silently cut.
        assert!(
            !text.contains("quit"),
            "the whole table fitted; nothing to scroll"
        );
    }

    #[test]
    fn scrolling_reaches_the_end_of_the_table() {
        let mut o = Overlays::default();
        o.toggle_help();
        o.scroll(120);
        let text = drawn(&mut o);
        assert!(text.contains("quit"), "{text}");
    }

    /// Every overlay draws something rather than leaving the panels visible
    /// through a hole where a box should be.
    #[test]
    fn every_overlay_draws_a_box() {
        for open in every_overlay() {
            let mut o = Overlays::default();
            open(&mut o);
            let text = drawn(&mut o);
            assert!(
                text.chars().any(|c| c != ' '),
                "an open overlay drew nothing"
            );
        }
    }

    /// A picker that chose something and closed in the same keystroke still
    /// gets its command sent. It did not, once: choosing a GIF closed the grid
    /// and the message went nowhere.
    #[test]
    fn a_command_survives_the_overlay_that_asked_for_it() {
        let mut o = Overlays::default();
        let picker = Picker::new(
            picker::Kind::Reaction(MessageId(5)),
            Some(ChannelId(1)),
            vec![("pepe".into(), crate::discord::snowflake::EmojiId(1), false)],
            2.0,
        );
        o.open_picker(picker);
        for c in "pepe".chars() {
            o.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(
            o.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Key::Taken
        );
        assert!(!o.open(), "it closed");
        assert!(
            matches!(o.take_commands().first(), Some(Command::AddReaction { .. })),
            "the reaction was dropped with the picker"
        );
    }

    /// The picker and the search box ask the core for things; nothing else
    /// does, and a closed overlay asks for nothing at all.
    #[test]
    fn only_the_asking_overlays_ask() {
        let mut o = Overlays::default();
        assert!(o.take_commands().is_empty());
        o.open_picker(Picker::new(
            picker::Kind::Gif,
            Some(ChannelId(1)),
            Vec::new(),
            2.0,
        ));
        assert!(
            matches!(o.take_commands().first(), Some(Command::GifTrending { .. })),
            "opening the GIF grid asks what is trending"
        );
        assert!(o.take_commands().is_empty(), "and only once");
    }
}
