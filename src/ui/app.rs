//! The loop, and everything that holds state between frames.
//!
//! Synchronous, like STAR/AMP's: draw, poll for a frame's worth of time, act,
//! repeat. The Discord core is on its own thread with its own runtime and this
//! never sees a future; it drains events once a frame and reads the truth out
//! of `State` behind a read lock it drops before drawing.
//!
//! ## The order of a frame
//!
//! 1. drain up to five hundred events and apply them;
//! 2. rebuild the view lists if `State::version` moved;
//! 3. `regions()` once, kept in `layout.last`;
//! 4. draw;
//! 5. poll, and dispatch whatever arrived.
//!
//! The bound on the drain is not a performance guard. It is what stops a burst
//! of gateway traffic from starving the draw: events carry no data, so
//! whatever is left in the channel is still true next frame.
//!
//! ## Key dispatch, outermost first
//!
//! login screen → overlay → composer → `g` prefix → focused panel → global
//! table. Modality is checked in `handle` **and** in `handle_mouse`, because an
//! overlay that swallows keys and not clicks is a dialogue you can click
//! through.
//!
//! ## Graphics before the terminal
//!
//! `Graphics::probe` writes a capability query and reads the answer off stdin.
//! Once raw mode is on, that answer arrives interleaved with whatever is being
//! typed. So the probe happens in [`run`], before `term::init`, and STAR/KIT
//! asserts it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use starkit::crossterm::event::{
    self, Event as TermEvent, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use starkit::graphics::{Graphics, Mode};
use starkit::list::clamp_scroll;
use starkit::mouse::ClickTracker;
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::Style;
use starkit::term;

use super::keymap::{self, Action, PrefixKey};
use super::layout::{Drag, LayoutState, Regions};
use super::login::{LoginScreen, Outcome, Stage};
use super::overlays::{self, Overlays};
use super::panels::{self, channels, dms, guilds, rgb, DmTab, PanelId};
use super::status;
use super::theme::Theme;
use super::{core_ext, layout};
use crate::config::Config;
use crate::discord::handle::{AuthEvent, Connection, Event, MessagesChange, Note, NoteLevel};
use crate::discord::snowflake::{ChannelId, GuildId};
use crate::discord::{Command, Handle};

/// One frame. Thirty per second is plenty for text and is what STAR/AMP
/// settled on; it is also the ceiling on how long a keystroke waits.
const FRAME: Duration = Duration::from_millis(33);

/// How many events one frame will take before it draws anyway.
const DRAIN_CAP: usize = 500;

/// How the whole thing looks.
pub struct Look {
    pub theme: Theme,
    /// Every theme that can be cycled to, and where in it we are.
    pub ids: Vec<String>,
    pub index: usize,
    pub graphics: Graphics,
    pub padding: (u16, u16),
}

/// Where the user is, and where each list's cursor is.
#[derive(Debug, Default)]
pub struct Nav {
    /// `None` is the direct-message home.
    pub guild: Option<GuildId>,
    pub channel: Option<ChannelId>,
    pub guild_cursor: usize,
    pub guild_scroll: usize,
    pub channel_cursor: usize,
    pub channel_scroll: usize,
    pub dm_cursor: usize,
    pub dm_scroll: usize,
    pub dm_tab: DmTab,
    pub collapsed: HashSet<ChannelId>,
}

/// What the panels draw, copied out of `State` under the lock and kept until
/// the core says something changed.
///
/// The copy is the point. A read guard held across a draw stalls the gateway,
/// which is applying dispatches under the write lock; a few hundred short
/// strings once a change is cheaper than the alternative by a wide margin.
#[derive(Debug, Default)]
pub struct ViewData {
    version: u64,
    stale: bool,
    pub guilds: Vec<guilds::Row>,
    pub channels: Vec<channels::Row>,
    pub dms: Vec<dms::Row>,
    pub friends: Vec<dms::Row>,
    /// `#general · Some Guild`.
    pub location: String,
    pub unread: u32,
    pub mentions: u32,
}

pub struct App {
    core: Handle,
    cfg: Config,
    cfg_path: PathBuf,
    session_path: Option<PathBuf>,
    pub look: Look,
    pub layout: LayoutState,
    pub nav: Nav,
    pub view: ViewData,
    pub over: Overlays,
    conn: Arc<Connection>,
    pub login: Option<LoginScreen>,
    clicks: ClickTracker,
    note: Option<(String, NoteLevel, Instant)>,
    g_prefix: bool,
    last_frame: Instant,
    quit: bool,
}

impl App {
    pub fn new(
        core: Handle,
        cfg: Config,
        cfg_path: PathBuf,
        session_path: Option<PathBuf>,
        graphics: Graphics,
    ) -> Self {
        let registry = super::theme::registry();
        let (theme, _reason) = registry.resolve_named(&cfg.ui.theme);
        let ids = registry.selectable();
        let index = ids.iter().position(|id| *id == theme.id).unwrap_or(0);

        let conn = core.status();
        let login = (!conn.is_ready()).then(LoginScreen::new);

        Self {
            layout: LayoutState::new(&cfg.layout),
            look: Look {
                theme,
                ids,
                index,
                graphics,
                padding: (cfg.ui.padding_x, cfg.ui.padding_y),
            },
            nav: Nav::default(),
            view: ViewData {
                stale: true,
                ..ViewData::default()
            },
            over: Overlays::default(),
            conn,
            login,
            clicks: ClickTracker::new(),
            note: None,
            g_prefix: false,
            last_frame: Instant::now(),
            quit: false,
            core,
            cfg,
            cfg_path,
            session_path,
        }
    }

    /// Take over the terminal and run until something says to stop.
    ///
    /// The probe is before `term::init` and the restore is before the result
    /// is returned, so a failure inside the loop still leaves a usable
    /// terminal behind.
    pub fn run(
        core: Handle,
        cfg: Config,
        cfg_path: PathBuf,
        session_path: Option<PathBuf>,
    ) -> Result<()> {
        let graphics = Graphics::probe(Mode::parse(&cfg.ui.graphics));
        graphics.log_capabilities();
        let mut app = App::new(core, cfg, cfg_path, session_path, graphics);

        let mut term = term::init()?;
        let result = app.event_loop(&mut term);
        term::restore()?;
        app.core.send(Command::Shutdown);
        result
    }

    fn event_loop(&mut self, term: &mut term::Tui) -> Result<()> {
        while !self.quit {
            self.last_frame = Instant::now();

            // Collected before they are applied: `drain` borrows the handle
            // and `apply` takes the whole app. Bounded, so that a burst of
            // gateway traffic cannot starve the draw -- whatever is left is
            // still true next frame, because an event carries no data.
            let batch: Vec<Event> = self.core.drain().take(DRAIN_CAP).collect();
            for ev in batch {
                self.apply(ev);
            }
            self.refresh();

            term.draw(|f| self.draw(f.area(), f.buffer_mut()))?;

            if event::poll(FRAME)? {
                match event::read()? {
                    TermEvent::Key(k) if k.kind == KeyEventKind::Press => self.key(k),
                    TermEvent::Mouse(m) => {
                        self.g_prefix = false;
                        let size = term.size()?;
                        self.handle_mouse(m, Rect::new(0, 0, size.width, size.height));
                    }
                    // A font zoom arrives as a resize, and it changes the cell
                    // size anything already drawn as a picture was built for.
                    TermEvent::Resize(..) => self.look.graphics.remeasure(),
                    TermEvent::Paste(text) => self.paste(&text),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    // -- events from the core ---------------------------------------------

    /// One notification from the core.
    ///
    /// Almost every arm marks the view stale rather than changing anything:
    /// events say *that* something changed, not what, and the truth is read
    /// out of `State` on the next refresh. That is what makes a dropped event
    /// survivable and `Event::Refresh` enough on its own.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Status(status) => {
                let was_ready = self.conn.is_ready();
                self.conn = status;
                match &*self.conn {
                    Connection::Ready { resumed, .. } => {
                        if !was_ready {
                            self.note(if *resumed { "resumed" } else { "connected" });
                        }
                        if let Some(screen) = &mut self.login {
                            screen.connecting();
                        }
                    }
                    Connection::AuthFailed(reason) => {
                        let reason = reason.clone();
                        self.login
                            .get_or_insert_with(LoginScreen::new)
                            .failed(reason);
                    }
                    _ => {}
                }
                self.view.stale = true;
            }
            Event::Auth(auth) => match auth {
                AuthEvent::NeedsLogin | AuthEvent::LoggedOut => {
                    self.login = Some(LoginScreen::new());
                }
                AuthEvent::Failed(reason) => {
                    self.login
                        .get_or_insert_with(LoginScreen::new)
                        .failed(reason);
                }
                AuthEvent::LoggedIn { .. } => {
                    if let Some(screen) = &mut self.login {
                        screen.connecting();
                    }
                }
                AuthEvent::QrReady {
                    url, expires_in, ..
                } => {
                    if let Some(screen) = &mut self.login {
                        screen.stage = Stage::Qr {
                            url,
                            expires: Instant::now() + expires_in,
                            matrix: Vec::new(),
                        };
                    }
                }
                AuthEvent::QrScanned { username, .. } => {
                    self.note(format!("scanned by {username}"));
                }
            },
            Event::Ready => {
                self.login = None;
                self.view.stale = true;
                self.restore_session();
            }
            Event::Note(Note { level, text, .. }) => {
                self.note_at(text, level);
            }
            // Every one of these is "re-read the truth". Naming them rather
            // than writing a catch-all is deliberate: a new variant should
            // make this match fail to compile so somebody decides.
            Event::Guilds
            | Event::Channels(_)
            | Event::Relationships
            | Event::ReadState(_)
            | Event::Presence(_)
            | Event::Members(_)
            | Event::Typing(_)
            | Event::Refresh => {
                self.view.stale = true;
            }
            Event::Messages(_, MessagesChange::Appended(_)) => self.view.stale = true,
            Event::Messages(..)
            | Event::SendResult { .. }
            | Event::UploadProgress { .. }
            | Event::Mention { .. }
            | Event::Media { .. }
            | Event::Gifs { .. }
            | Event::Search { .. } => {}
        }
    }

    /// Copy what the panels draw out of `State`, if anything has changed.
    fn refresh(&mut self) {
        let state = self.core.state();
        if !self.view.stale && state.version() == self.view.version {
            return;
        }

        let unread_of = |id: ChannelId| state.unread(id);

        // The rail: the DM home, then the servers in READY's order.
        let mut rail = vec![guilds::Row {
            id: None,
            name: "direct messages".into(),
            unread: state
                .dms_ordered()
                .iter()
                .any(|c| unread_of(c.id).notable()),
            mentions: state
                .dms_ordered()
                .iter()
                .map(|c| unread_of(c.id).mentions)
                .sum(),
            unavailable: false,
        }];
        for guild in state.guilds_ordered() {
            let marks = state
                .channels_ordered(guild.id)
                .iter()
                .map(|c| unread_of(c.id))
                .fold((false, 0u32), |(u, m), r| {
                    (u || r.notable(), m + r.mentions)
                });
            rail.push(guilds::Row {
                id: Some(guild.id),
                name: guild.name.clone(),
                unread: marks.0,
                mentions: marks.1,
                unavailable: guild.unavailable,
            });
        }

        let channels = match self.nav.guild {
            Some(guild) => channels::rows(
                &state.channels_ordered(guild),
                &self.nav.collapsed,
                self.cfg.channels.show_voice,
                unread_of,
            ),
            None => Vec::new(),
        };

        let dms = state
            .dms_ordered()
            .iter()
            .map(|c| {
                let (title, presence, members) = core_ext::dm_row(&state, c.id);
                let unread = unread_of(c.id);
                dms::Row::Dm {
                    id: c.id,
                    title,
                    presence,
                    unread: unread.unread,
                    mentions: unread.mentions,
                    muted: unread.muted,
                    members,
                }
            })
            .collect();
        let friends = dms::group_friends(core_ext::friends(&state));

        let location = match self.nav.channel {
            Some(id) => {
                let channel = state.channel(id);
                let name = channel
                    .as_ref()
                    .and_then(|c| c.name().map(str::to_string))
                    .unwrap_or_else(|| state.dm_title(id));
                match channel.as_ref().and_then(|c| c.guild_id) {
                    Some(g) => {
                        let guild = state
                            .guild(g)
                            .map(|g| g.name.clone())
                            .unwrap_or_else(|| "—".into());
                        format!("#{name} · {guild}")
                    }
                    None => name,
                }
            }
            None => String::new(),
        };

        let (unread, mentions) = state
            .guilds_ordered()
            .iter()
            .flat_map(|g| state.channels_ordered(g.id))
            .chain(state.dms_ordered())
            .map(|c| unread_of(c.id))
            .fold((0u32, 0u32), |(u, m), r| {
                (u + u32::from(r.notable()), m + r.mentions)
            });

        self.view = ViewData {
            version: state.version(),
            stale: false,
            guilds: rail,
            channels,
            dms,
            friends,
            location,
            unread,
            mentions,
        };
        drop(state);
        self.clamp_cursors();
    }

    fn clamp_cursors(&mut self) {
        let dms = self.dm_rows().len();
        let (guilds, channels) = (self.view.guilds.len(), self.view.channels.len());
        let cap = |cursor: &mut usize, len: usize| {
            *cursor = (*cursor).min(len.saturating_sub(1));
        };
        cap(&mut self.nav.guild_cursor, guilds);
        cap(&mut self.nav.channel_cursor, channels);
        cap(&mut self.nav.dm_cursor, dms);
    }

    fn dm_rows(&self) -> &[dms::Row] {
        match self.nav.dm_tab {
            DmTab::Dms => &self.view.dms,
            DmTab::Friends => &self.view.friends,
        }
    }

    /// Reopen the channel the last session was in.
    ///
    /// Best effort and quiet: a session that cannot be restored is a client
    /// that opens on nothing, which is the state it opens on for a new
    /// account anyway.
    fn restore_session(&mut self) {
        let Some(path) = self.session_path.clone() else {
            return;
        };
        let Some(channel) = core_ext::last_channel(&path) else {
            return;
        };
        if self.core.state().channel(channel).is_some() {
            self.open_channel(channel);
        }
    }

    // -- acting ------------------------------------------------------------

    /// A transient line in the middle of the status bar.
    pub fn note(&mut self, text: impl Into<String>) {
        self.note_at(text.into(), NoteLevel::Info);
    }

    fn note_at(&mut self, text: impl Into<String>, level: NoteLevel) {
        self.note = Some((text.into(), level, Instant::now()));
    }

    pub fn open_channel(&mut self, channel: ChannelId) {
        self.nav.channel = Some(channel);
        if let Some(c) = self.core.state().channel(channel) {
            self.nav.guild = c.guild_id;
        }
        self.core.send(Command::OpenChannel(channel));
        self.core.send(Command::SetFocus {
            channel: Some(channel),
            terminal_focused: true,
        });
        self.layout.focus_set(PanelId::Chat);
        self.view.stale = true;
    }

    fn select_guild(&mut self, index: usize) {
        let Some(row) = self.view.guilds.get(index) else {
            return;
        };
        self.nav.guild_cursor = index;
        self.nav.guild = row.id;
        self.nav.channel_cursor = 0;
        self.nav.channel_scroll = 0;
        self.view.stale = true;
    }

    fn step_guild(&mut self, delta: isize) {
        if self.view.guilds.is_empty() {
            return;
        }
        let n = self.view.guilds.len() as isize;
        let next = (self.nav.guild_cursor as isize + delta).rem_euclid(n) as usize;
        self.select_guild(next);
    }

    /// Move the focused list's cursor, and keep its scroll under it.
    ///
    /// The lengths and the height are read before the cursor is borrowed: they
    /// all come off `self`, and the alternative is four copies of the same
    /// three lines, one per panel.
    fn move_cursor(&mut self, delta: isize) {
        let focus = self.layout.focus();
        let height = usize::from(self.body_height(focus));
        let len = match focus {
            PanelId::Guilds => self.view.guilds.len(),
            PanelId::Channels => self.view.channels.len(),
            PanelId::Dms => self.dm_rows().len(),
            _ => return,
        };
        if len == 0 {
            return;
        }
        let (cursor, scroll) = match focus {
            PanelId::Guilds => (&mut self.nav.guild_cursor, &mut self.nav.guild_scroll),
            PanelId::Channels => (&mut self.nav.channel_cursor, &mut self.nav.channel_scroll),
            PanelId::Dms => (&mut self.nav.dm_cursor, &mut self.nav.dm_scroll),
            _ => return,
        };
        let next = (*cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        *cursor = next;
        *scroll = clamp_scroll(next, *scroll, height);
    }

    fn body_height(&self, id: PanelId) -> u16 {
        self.layout
            .last
            .as_ref()
            .and_then(|r| r.rect_of(id))
            .map(|rect| starkit::chrome::header::body(rect).height)
            .unwrap_or(0)
    }

    /// Open whatever the cursor is on in the focused panel.
    fn activate(&mut self) {
        match self.layout.focus() {
            PanelId::Guilds => {
                let index = self.nav.guild_cursor;
                self.select_guild(index);
                self.layout.focus_set(PanelId::Channels);
            }
            PanelId::Channels => match self.view.channels.get(self.nav.channel_cursor).cloned() {
                Some(channels::Row::Channel { id, .. }) => self.open_channel(id),
                Some(channels::Row::Category { id, .. }) => self.toggle_category(id),
                None => {}
            },
            PanelId::Dms => {
                if let Some(dms::Row::Dm { id, .. }) = self.dm_rows().get(self.nav.dm_cursor) {
                    let id = *id;
                    self.open_channel(id);
                }
            }
            _ => {}
        }
    }

    fn toggle_category(&mut self, id: ChannelId) {
        if !self.nav.collapsed.remove(&id) {
            self.nav.collapsed.insert(id);
        }
        self.view.stale = true;
    }

    fn cycle_theme(&mut self, forward: bool) {
        if self.look.ids.is_empty() {
            return;
        }
        let n = self.look.ids.len();
        self.look.index = if forward {
            (self.look.index + 1) % n
        } else {
            (self.look.index + n - 1) % n
        };
        let id = self.look.ids[self.look.index].clone();
        let registry = super::theme::registry();
        let (theme, _) = registry.resolve_named(&id);
        let name = theme.name.clone();
        self.look.theme = theme;
        self.cfg.ui.theme = id;
        // Every protocol image was encoded for the old colours.
        self.look.graphics.forget_all();
        self.note(name);
    }

    // -- keys --------------------------------------------------------------

    fn key(&mut self, k: KeyEvent) {
        // The login screen is the whole frame, so it is the whole keyboard.
        if let Some(screen) = &mut self.login {
            match screen.handle(k) {
                Outcome::Quit => self.quit = true,
                Outcome::Consumed => {}
                Outcome::StartQr => self.core.send(Command::StartRemoteAuth),
                Outcome::Submit(text) => match crate::discord::auth::Token::new(&text) {
                    Ok(token) => self.core.send(Command::LoginWithToken(token)),
                    Err(e) => screen.failed(e.to_string()),
                },
                Outcome::Nothing => {}
            }
            return;
        }

        // Then anything modal.
        match self.over.handle(k) {
            overlays::Key::Quit => {
                self.quit = true;
                return;
            }
            overlays::Key::Taken => return,
            overlays::Key::Ignored => {}
        }

        // Then the composer, which is a text field and takes raw keys.
        if self.layout.focus() == PanelId::Composer && keymap::composer_eats(k) {
            // Composing lands with the milestone that can send; until then the
            // keys are swallowed here so that the panel behaves like the text
            // field it is about to be rather than like a list.
            return;
        }

        let action = match keymap::g_prefix(&mut self.g_prefix, k) {
            PrefixKey::Waiting => return,
            PrefixKey::Action(action) => Some(action),
            PrefixKey::None => {
                keymap::module(self.layout.focus().module(), k).or_else(|| keymap::resolve(k))
            }
        };
        if let Some(action) = action {
            self.handle(action);
        }
    }

    fn paste(&mut self, text: &str) {
        if let Some(screen) = &mut self.login {
            screen.paste(text);
        }
    }

    /// One action. The single place a key turns into a change.
    pub fn handle(&mut self, action: Action) {
        match action {
            Action::Quit => self.quit = true,
            Action::Help => self.over.toggle_help(),
            Action::CloseOverlay => self.over.close(),

            Action::CursorUp => self.move_cursor(-1),
            Action::CursorDown => self.move_cursor(1),
            Action::CursorUpBig => self.move_cursor(-10),
            Action::CursorDownBig => self.move_cursor(10),
            Action::PageUp => {
                let h = self.body_height(self.layout.focus()) as isize;
                self.move_cursor(-h.max(1));
            }
            Action::PageDown => {
                let h = self.body_height(self.layout.focus()) as isize;
                self.move_cursor(h.max(1));
            }
            Action::Home => self.move_cursor(isize::MIN / 2),
            Action::End => self.move_cursor(isize::MAX / 2),
            Action::Activate => self.activate(),
            Action::Back => {
                self.over.close();
                self.layout.focus_set(PanelId::Chat);
            }

            Action::FocusNext => self.layout.focus_step(true),
            Action::FocusPrev => self.layout.focus_step(false),
            Action::FocusGuilds => self.layout.focus_set(PanelId::Guilds),
            Action::FocusChannels => self.layout.focus_set(PanelId::Channels),
            Action::FocusDms => self.layout.focus_set(PanelId::Dms),
            Action::FocusChat => self.layout.focus_set(PanelId::Chat),
            Action::FocusComposer => self.layout.focus_set(PanelId::Composer),
            Action::FocusMembers => self.layout.focus_set(PanelId::Members),

            Action::NextGuild => self.step_guild(1),
            Action::PrevGuild => self.step_guild(-1),
            Action::ToggleCollapse => {
                if self.layout.focus() == PanelId::Channels {
                    if let Some(channels::Row::Category { id, .. }) =
                        self.view.channels.get(self.nav.channel_cursor).cloned()
                    {
                        self.toggle_category(id);
                    }
                }
            }

            Action::ToggleGuilds => self.layout.toggle(PanelId::Guilds),
            Action::ToggleChannels => self.layout.toggle(PanelId::Channels),
            Action::ToggleDms => {
                if self.layout.is_open(PanelId::Dms) && self.layout.dms_folded() {
                    // Folded into the channel panel: the key swaps the tab
                    // rather than closing a list that is not on screen.
                    self.swap_dm_tab();
                } else {
                    self.layout.toggle(PanelId::Dms);
                }
            }
            Action::ToggleMembers => self.layout.toggle(PanelId::Members),
            Action::ToggleZen => {
                let zen = !self.layout.zen;
                self.layout.set_zen(zen);
            }
            Action::ClosePanel => {
                let focus = self.layout.focus();
                if focus.closable() {
                    self.layout.toggle(focus);
                }
            }

            Action::NextTheme => self.cycle_theme(true),
            Action::PrevTheme => self.cycle_theme(false),
            Action::ToggleTimestamps => {
                self.cfg.chat.timestamps = self.cfg.chat.timestamps.next();
                let name = self.cfg.chat.timestamps.name();
                self.note(format!("timestamps {name}"));
            }
            Action::ToggleAvatars => {
                self.cfg.chat.show_avatars = !self.cfg.chat.show_avatars;
                let on = if self.cfg.chat.show_avatars {
                    "on"
                } else {
                    "off"
                };
                self.note(format!("avatars {on}"));
            }
            Action::CycleAnimate => {
                self.cfg.media.animate = self.cfg.media.animate.next();
                let name = self.cfg.media.animate.name();
                self.note(format!("animate {name}"));
            }
            Action::Reconnect => {
                self.core.send(Command::Connect);
                self.note("reconnecting");
            }

            // Everything the later milestones own. Named rather than left to a
            // catch-all so that adding an action forces a decision here.
            Action::NextUnread
            | Action::PrevUnread
            | Action::QuickSwitch
            | Action::Search
            | Action::JumpToReply
            | Action::Reply
            | Action::ReplyNoPing
            | Action::Edit
            | Action::Delete
            | Action::React
            | Action::Yank
            | Action::YankLink
            | Action::OpenExternal
            | Action::OpenMedia
            | Action::MarkRead
            | Action::RevealSpoiler
            | Action::TogglePin
            | Action::LoadOlder
            | Action::ToBottom
            | Action::CopyMessageLink
            | Action::Send
            | Action::Newline
            | Action::EmojiPicker
            | Action::GifPicker
            | Action::Attach
            | Action::PasteImage
            | Action::CancelCompose
            | Action::ClearComposer
            | Action::EditLast
            | Action::OpenPanelSettings
            | Action::MediaNext
            | Action::MediaPrev
            | Action::MediaSave
            | Action::MediaZoom => self.note("not yet"),
        }
    }

    fn swap_dm_tab(&mut self) {
        self.nav.dm_tab = match self.nav.dm_tab {
            DmTab::Dms => DmTab::Friends,
            DmTab::Friends => DmTab::Dms,
        };
        self.nav.dm_cursor = 0;
        self.nav.dm_scroll = 0;
    }

    // -- the mouse ---------------------------------------------------------

    pub fn handle_mouse(&mut self, m: MouseEvent, full: Rect) {
        // The same modality, in the same order. An overlay that takes keys and
        // not clicks is a dialogue you can click through.
        if self.login.is_some() {
            return;
        }
        if self.over.open() {
            match m.kind {
                MouseEventKind::ScrollDown => self.over.scroll(3),
                MouseEventKind::ScrollUp => self.over.scroll(-3),
                MouseEventKind::Down(MouseButton::Left) => self.over.close(),
                _ => {}
            }
            return;
        }
        let Some(regions) = self.layout.last.clone() else {
            return;
        };
        let _ = full;
        let (x, y) = (m.column, m.row);

        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(seam) = self.layout.seam_at(x, y) {
                    self.layout.drag = Some(Drag::Seam(seam));
                    return;
                }
                if let Some(what) = self.status_hit(&regions, x, y) {
                    self.status_click(what);
                    return;
                }
                let double = self.clicks.click(x, y);
                if let Some(panel) = regions.hit(x, y) {
                    self.panel_click(&regions, panel, x, y, double);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.layout.drag = None,
            MouseEventKind::ScrollDown => self.scroll_panel(&regions, x, y, 3),
            MouseEventKind::ScrollUp => self.scroll_panel(&regions, x, y, -3),
            _ => {}
        }
    }

    fn status_hit(&self, regions: &Regions, x: u16, y: u16) -> Option<status::Hit> {
        let view = self.status_view();
        status::hit(regions.status, &view, x, y)
    }

    fn status_click(&mut self, what: status::Hit) {
        match what {
            status::Hit::Help => self.over.toggle_help(),
            status::Hit::Connection => self.handle(Action::Reconnect),
            status::Hit::Location => self.handle(Action::QuickSwitch),
        }
    }

    fn panel_click(&mut self, regions: &Regions, panel: PanelId, x: u16, y: u16, double: bool) {
        let Some(rect) = regions.rect_of(panel) else {
            return;
        };
        // A header word first: it is drawn over the panel's own first row, and
        // the hit box comes from the same function the renderer used.
        let words = panels::words(panel, self.nav.dm_tab);
        if let Some(word) = starkit::chrome::header::hit(rect, &words, x, y) {
            self.word_click(panel, word);
            return;
        }
        let body = starkit::chrome::header::body(rect);
        self.layout.focus_set(panel);

        match panel {
            PanelId::Guilds => {
                let v = self.guilds_view(false);
                if let Some(index) = guilds::row_at(body, &v, y) {
                    self.select_guild(index);
                }
            }
            PanelId::Channels => {
                let v = self.channels_view(false);
                if let Some(index) = channels::row_at(body, &v, y) {
                    self.nav.channel_cursor = index;
                    if double {
                        self.activate();
                    } else if let Some(channels::Row::Category { id, .. }) =
                        self.view.channels.get(index).cloned()
                    {
                        self.toggle_category(id);
                    }
                }
            }
            PanelId::Dms => {
                let v = self.dms_view(false);
                if let Some(index) = dms::row_at(body, &v, y) {
                    if self.dm_rows().get(index).is_some_and(dms::Row::selectable) {
                        self.nav.dm_cursor = index;
                        if double {
                            self.activate();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn word_click(&mut self, panel: PanelId, word: panels::Word) {
        match word {
            panels::Word::Close => self.layout.toggle(panel),
            panels::Word::ShowFriends | panels::Word::ShowDms => self.swap_dm_tab(),
            panels::Word::Zen => self.handle(Action::ToggleZen),
            panels::Word::Settings => self.handle(Action::OpenPanelSettings),
            panels::Word::Search => self.handle(Action::Search),
            panels::Word::Pins => self.handle(Action::TogglePin),
            panels::Word::Attach => self.handle(Action::Attach),
            panels::Word::Emoji => self.handle(Action::EmojiPicker),
            panels::Word::Gif => self.handle(Action::GifPicker),
        }
    }

    fn scroll_panel(&mut self, regions: &Regions, x: u16, y: u16, delta: isize) {
        let Some(panel) = regions.hit(x, y) else {
            return;
        };
        match panel {
            // The rail cycles servers rather than scrolling, because it is
            // rarely longer than the screen and changing server is what
            // somebody with a pointer over it wants.
            PanelId::Guilds => self.step_guild(delta.signum()),
            PanelId::Channels | PanelId::Dms => {
                let was = self.layout.focus();
                self.layout.focus_set(panel);
                self.move_cursor(delta);
                self.layout.focus_set(was);
            }
            _ => {}
        }
    }

    // -- drawing -----------------------------------------------------------

    fn status_view(&self) -> status::View<'_> {
        status::View {
            theme: &self.look.theme,
            connection: &self.conn,
            location: &self.view.location,
            note: self.note.as_ref(),
            unread: self.view.unread,
            mentions: self.view.mentions,
            graphics: self.look.graphics.name(),
            now: self.last_frame,
        }
    }

    fn guilds_view(&self, focused: bool) -> guilds::View<'_> {
        guilds::View {
            theme: &self.look.theme,
            rows: &self.view.guilds,
            cursor: self.nav.guild_cursor,
            scroll: self.nav.guild_scroll,
            style: self.cfg.layout.guilds,
            focused,
        }
    }

    fn channels_view(&self, focused: bool) -> channels::View<'_> {
        channels::View {
            theme: &self.look.theme,
            rows: &self.view.channels,
            cursor: self.nav.channel_cursor,
            scroll: self.nav.channel_scroll,
            focused,
            open: self.nav.channel,
            folded_tab: self.layout.dms_folded().then_some(self.nav.dm_tab),
        }
    }

    fn dms_view(&self, focused: bool) -> dms::View<'_> {
        dms::View {
            theme: &self.look.theme,
            rows: self.dm_rows(),
            cursor: self.nav.dm_cursor,
            scroll: self.nav.dm_scroll,
            focused,
            tab: self.nav.dm_tab,
            open: self.nav.channel,
        }
    }

    pub fn draw(&mut self, area: Rect, buf: &mut Buffer) {
        let t = &self.look.theme;
        buf.set_style(area, Style::default().bg(rgb(t.bg)).fg(rgb(t.fg)));

        if let Some(screen) = &self.login {
            screen.render(area, buf, t);
            return;
        }

        let composer_rows = super::panels::composer::ROWS;
        let Some(regions) = self
            .layout
            .regions(area, composer_rows, self.look.padding)
            .cloned()
        else {
            too_small(area, buf, t);
            return;
        };

        let focus = self.layout.focus();
        for panel in regions.visible() {
            let Some(rect) = regions.rect_of(panel) else {
                continue;
            };
            let focused = panel == focus;
            let title = self.panel_title(panel);
            let words = panels::words(panel, self.nav.dm_tab);
            let body = panels::frame(
                rect,
                buf,
                &panels::Frame {
                    theme: t,
                    focused,
                    title: &title,
                    words: &words,
                },
            );
            if regions.too_small.contains(&panel) {
                panels::empty(body, buf, t, "too narrow");
                continue;
            }
            match panel {
                PanelId::Guilds => guilds::render(body, buf, &self.guilds_view(focused)),
                PanelId::Channels => channels::render(body, buf, &self.channels_view(focused)),
                PanelId::Dms => dms::render(body, buf, &self.dms_view(focused)),
                PanelId::Chat => super::panels::chat::render(
                    body,
                    buf,
                    &super::panels::chat::View {
                        theme: t,
                        title: &self.view.location,
                        focused,
                    },
                ),
                PanelId::Composer => super::panels::composer::render(
                    body,
                    buf,
                    &super::panels::composer::View {
                        theme: t,
                        focused,
                        channel: &self.view.location,
                    },
                ),
                PanelId::Members => super::panels::members::render(
                    body,
                    buf,
                    &super::panels::members::View { theme: t, focused },
                ),
            }
        }

        status::render(regions.status, buf, &self.status_view());
        self.over.render(area, buf, t);
    }

    fn panel_title(&self, panel: PanelId) -> String {
        match panel {
            // The channel list says which server it is listing, because at
            // twenty-six columns the rail's two letters are not an answer.
            PanelId::Channels => match self.nav.guild {
                Some(g) => self
                    .core
                    .state()
                    .guild(g)
                    .map(|g| g.name.clone())
                    .unwrap_or_else(|| panel.title().to_string()),
                None => panel.title().to_string(),
            },
            PanelId::Dms if self.nav.dm_tab == DmTab::Friends => "friends".into(),
            _ => panel.title().to_string(),
        }
    }
}

/// The one line a terminal below the floor gets.
fn too_small(area: Rect, buf: &mut Buffer, theme: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let text = format!(
        "terminal too small — {}x{} minimum",
        layout::MIN_COLS,
        layout::MIN_ROWS
    );
    let text: String = text.chars().take(usize::from(area.width)).collect();
    let x = area.x + (area.width.saturating_sub(text.chars().count() as u16)) / 2;
    let y = area.y + area.height / 2;
    buf.set_string(x, y, text, Style::default().fg(rgb(theme.error)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fake;

    fn app() -> App {
        let (core, _driver) = fake::silent();
        App::new(
            core,
            Config::default(),
            PathBuf::from("/nonexistent/config.toml"),
            None,
            Graphics::disabled(),
        )
    }

    fn frame(app: &mut App, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        app.draw(area, &mut buf);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn it_opens_on_the_login_screen() {
        let mut a = app();
        assert!(a.login.is_some());
        assert!(frame(&mut a, 100, 30).contains("STAR/CORD"));
    }

    /// The whole frame while there is no session: no panels behind it, so
    /// nothing can be clicked or typed into by accident.
    #[test]
    fn the_login_screen_is_the_whole_frame() {
        let mut a = app();
        let drawn = frame(&mut a, 100, 30);
        assert!(!drawn.contains("channels"), "{drawn}");
        assert!(a.layout.last.is_none());
    }

    #[test]
    fn a_terminal_below_the_floor_says_so() {
        let mut a = app();
        a.login = None;
        let drawn = frame(&mut a, 59, 30);
        assert!(drawn.contains("too small"), "{drawn}");
        assert!(drawn.contains("60x12"));
    }

    /// Thirty per second, and the drain is bounded, so a burst of gateway
    /// traffic cannot starve the draw.
    #[test]
    fn the_frame_and_the_drain_are_bounded() {
        assert!(FRAME <= Duration::from_millis(50));
        assert_eq!(DRAIN_CAP, 500);
    }

    #[test]
    fn cycling_themes_goes_round_and_comes_back() {
        let mut a = app();
        let first = a.look.theme.id.clone();
        let n = a.look.ids.len();
        assert!(n > 1);
        for _ in 0..n {
            a.handle(Action::NextTheme);
        }
        assert_eq!(a.look.theme.id, first, "a full cycle should return");
        a.handle(Action::NextTheme);
        assert_ne!(a.look.theme.id, first);
        a.handle(Action::PrevTheme);
        assert_eq!(a.look.theme.id, first);
        // And the choice is written where it will be saved from.
        assert_eq!(a.cfg.ui.theme, a.look.theme.id);
    }

    #[test]
    fn quitting_sets_the_flag_and_nothing_else() {
        let mut a = app();
        a.login = None;
        a.handle(Action::Quit);
        assert!(a.quit);
    }

    /// The overlay is checked first in both handlers.
    #[test]
    fn an_open_overlay_takes_the_keys_and_the_clicks() {
        let mut a = app();
        a.login = None;
        a.handle(Action::Help);
        assert!(a.over.open());

        // A key that would otherwise cycle the theme.
        let before = a.look.theme.id.clone();
        a.key(KeyEvent::from(starkit::crossterm::event::KeyCode::Char(
            't',
        )));
        assert_eq!(a.look.theme.id, before, "the overlay let a key through");

        // A click that would otherwise focus a panel.
        frame(&mut a, 120, 30);
        a.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 2,
                modifiers: starkit::crossterm::event::KeyModifiers::NONE,
            },
            Rect::new(0, 0, 120, 30),
        );
        assert!(!a.over.open(), "a click closes it");
    }

    /// Panels toggle and zen collapses to the conversation.
    #[test]
    fn the_panel_keys_open_and_close_panels() {
        let mut a = app();
        a.login = None;
        frame(&mut a, 140, 30);
        assert!(a.layout.last.as_ref().unwrap().visible().len() == 6);

        a.handle(Action::ToggleMembers);
        frame(&mut a, 140, 30);
        assert!(!a
            .layout
            .last
            .as_ref()
            .unwrap()
            .panels
            .contains_key(&PanelId::Members));

        a.handle(Action::ToggleZen);
        frame(&mut a, 140, 30);
        assert_eq!(
            a.layout.last.as_ref().unwrap().visible(),
            vec![PanelId::Chat, PanelId::Composer]
        );
    }

    /// Tab walks the panels that are drawn, and never lands on one that is
    /// not.
    #[test]
    fn tab_walks_the_visible_panels() {
        let mut a = app();
        a.login = None;
        frame(&mut a, 140, 30);
        let visible = a.layout.last.as_ref().unwrap().visible();
        assert_eq!(visible.len(), 6);

        let mut seen = Vec::new();
        for _ in 0..visible.len() {
            a.handle(Action::FocusNext);
            seen.push(a.layout.focus());
        }
        // One step per panel comes back to where it started, and every stop
        // was somewhere that was actually drawn.
        assert_eq!(seen.last().copied(), Some(a.layout.focus()));
        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), visible.len(), "tab visited {seen:?}");
        for p in &seen {
            assert!(visible.contains(p), "{p:?} is not on screen");
        }

        // And shift-tab undoes a tab.
        let here = a.layout.focus();
        a.handle(Action::FocusNext);
        a.handle(Action::FocusPrev);
        assert_eq!(a.layout.focus(), here);
    }
}
