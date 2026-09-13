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
//! The probe writes a capability query and reads the answer off stdin. Once raw
//! mode is on, that answer arrives interleaved with whatever is being typed. So
//! it happens in [`App::run`], before `term::init`, and STAR/KIT asserts the
//! ordering.
//!
//! It is `probe_if_tty` rather than `probe`, and not only because output that
//! has been piped somewhere should not be asked questions. `probe_if_tty` also
//! drains stdin afterwards, and a late reply left sitting there is eaten by the
//! backend's own cursor-position query on the way into the alternate screen --
//! which then times out, and the first frame never arrives. That is a hang with
//! no message in it, so it is worth the sentence.
//!
//! ## Where the panel logic is not
//!
//! Almost nowhere here. The message list measures and draws itself in
//! `panels::chat`, the composer owns its drafts and its autocomplete, the
//! overlays own their keys. What is left in this file is the loop, the event
//! translation, and one `match` that turns an [`Action`] into a call — which is
//! the only place a key becomes a change, and the reason it is worth keeping
//! this file small enough to read in one sitting.

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
use starkit::ratatui::style::{Modifier, Style};
use starkit::term;

use super::keymap::{self, Action, PrefixKey};
use super::layout::{Drag, LayoutState, Regions};
use super::login::{LoginScreen, Outcome, Stage};
use super::overlays::confirm::{Confirm, Pending};
use super::overlays::quick::{self, Target};
use super::overlays::settings::Setting;
use super::overlays::{self, Overlays};
use super::panels::chat::{ChatState, Hit};
use super::panels::composer::{self, Composer, Sources};
use super::panels::{self, channels, chat, dms, guilds, members, rgb, DmTab, Fold, PanelId};
use super::status;
use super::theme::Theme;
use super::{core_ext, layout};
use crate::config::Config;
use crate::discord::handle::{
    AuthEvent, Connection, EmojiRef, Event, ExternalKind, MessagesChange, Note, NoteLevel,
};
use crate::discord::snowflake::{ChannelId, GuildId, MessageId};
use crate::discord::{Command, Handle};

/// One frame. Thirty per second is plenty for text and is what STAR/AMP
/// settled on; it is also the ceiling on how long a keystroke waits.
const FRAME: Duration = Duration::from_millis(33);

/// How many events one frame will take before it draws anyway.
const DRAIN_CAP: usize = 500;

/// How long the terminal has to have been focused, at the bottom of a channel,
/// before the channel is acknowledged as read.
///
/// Two seconds. Long enough that tabbing past a window does not clear somebody
/// else's mentions, short enough that reading a message marks it read without
/// having to press anything.
const READ_AFTER: Duration = Duration::from_secs(2);

/// How long after a drag ends before `[layout]` is written.
///
/// The write rewrites a line of `config.toml`; doing it per mouse move would
/// be a file write every few milliseconds for the length of the drag.
const SETTLE: Duration = Duration::from_secs(1);

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
    pub member_cursor: usize,
    pub member_scroll: usize,
    pub dm_tab: DmTab,
    /// Which list the channel panel is showing while it is carrying both.
    pub fold: Fold,
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
    pub members: Option<Vec<members::Row>>,
    /// `#general · Some Guild`.
    pub location: String,
    pub unread: u32,
    pub mentions: u32,
    /// What the composer's `@`, `#` and `:` popups offer.
    pub sources: Sources,
}

pub struct App {
    core: Handle,
    cfg: Config,
    cfg_path: PathBuf,
    session_path: Option<PathBuf>,
    /// What the core read out of `session.toml` before connecting. Preferred
    /// over the file itself, which is only read when no event arrived.
    session_channel: Option<ChannelId>,
    pub look: Look,
    pub layout: LayoutState,
    pub nav: Nav,
    pub view: ViewData,
    pub chat: ChatState,
    pub composer: Composer,
    pub over: Overlays,
    conn: Arc<Connection>,
    pub login: Option<LoginScreen>,
    clicks: ClickTracker,
    note: Option<(String, NoteLevel, Instant)>,
    g_prefix: bool,
    last_frame: Instant,
    /// The zone message times are drawn in. Read once: a client left running
    /// over a daylight-saving change redraws every timestamp on the next
    /// resize, and asking the system for it per message is a syscall per row.
    pub tz: jiff::tz::TimeZone,
    /// Whether the terminal itself has focus, for acks and notifications.
    terminal_focused: bool,
    focused_since: Instant,
    /// The last id acknowledged, so a channel is not acked twice.
    acked: Option<(ChannelId, MessageId)>,
    /// Set when a seam drag changes `[layout]`, cleared when it is written.
    layout_dirty: Option<Instant>,
    /// The width the composer last had, for the height it asks the dock for
    /// before the dock has decided anything.
    composer_width: u16,
    /// Where the caret goes, if anything on screen has one.
    caret: Option<(u16, u16)>,
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
            chat: ChatState::new(),
            composer: Composer::new(),
            over: Overlays::default(),
            conn,
            login,
            clicks: ClickTracker::new(),
            note: None,
            g_prefix: false,
            last_frame: Instant::now(),
            tz: jiff::tz::TimeZone::system(),
            terminal_focused: true,
            focused_since: Instant::now(),
            acked: None,
            layout_dirty: None,
            composer_width: 40,
            caret: None,
            quit: false,
            core,
            cfg,
            cfg_path,
            session_path,
            session_channel: None,
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
        let graphics = Graphics::probe_if_tty(Mode::parse(&cfg.ui.graphics));
        graphics.log_capabilities();
        let mut app = App::new(core, cfg, cfg_path, session_path, graphics);

        let mut term = term::init()?;
        // Focus reporting, which STAR/KIT's `term::init` does not turn on: it
        // is what stops a client in a background window from acknowledging
        // somebody else's mentions. A terminal that does not support it simply
        // never sends the events, and the flag stays true, which is the state
        // this had before the escape was written.
        let _ = starkit::crossterm::execute!(
            std::io::stdout(),
            starkit::crossterm::event::EnableFocusChange
        );
        let result = app.event_loop(&mut term);
        let _ = starkit::crossterm::execute!(
            std::io::stdout(),
            starkit::crossterm::event::DisableFocusChange
        );
        term::restore()?;
        app.core.send(Command::SaveSession);
        app.core.send(Command::Shutdown);
        result
    }

    fn event_loop(&mut self, term: &mut term::Tui) -> Result<()> {
        while !self.quit {
            self.last_frame = Instant::now();

            self.tick();

            term.draw(|f| {
                self.draw(f.area(), f.buffer_mut());
            })?;

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
                    TermEvent::Resize(..) => {
                        self.look.graphics.remeasure();
                        self.chat.cache.clear();
                    }
                    TermEvent::Paste(text) => self.paste(&text),
                    TermEvent::FocusGained => self.set_terminal_focus(true),
                    TermEvent::FocusLost => self.set_terminal_focus(false),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Everything a frame does before it draws: take what the core has said,
    /// and copy out what the panels need if anything has changed.
    ///
    /// Its own function so that a test can advance the program without a
    /// terminal, which is how the snapshots are taken.
    pub fn tick(&mut self) {
        // Collected before they are applied: `drain` borrows the handle and
        // `apply` takes the whole app. Bounded, so that a burst of gateway
        // traffic cannot starve the draw -- whatever is left is still true next
        // frame, because an event carries no data.
        let batch: Vec<Event> = self.core.drain().take(DRAIN_CAP).collect();
        for event in batch {
            self.apply(event);
        }
        self.refresh();
        self.settle_layout();
        self.maybe_mark_read();
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
                            if let Some(channel) = self.nav.channel {
                                self.core.send(Command::OpenChannel(channel));
                            }
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
                    url,
                    expires_in,
                    matrix,
                    ..
                } => {
                    if let Some(screen) = &mut self.login {
                        screen.stage = Stage::Qr {
                            url,
                            expires: Instant::now() + expires_in,
                            matrix,
                        };
                    }
                }
                AuthEvent::QrScanned { username, .. } => {
                    self.note(format!("scanned by {username}"));
                }
            },
            Event::SessionLoaded(session) => {
                self.session_channel = session.last_channel;
                let drafts: Vec<(ChannelId, String)> = session
                    .drafts
                    .iter()
                    .filter_map(|(id, text)| {
                        id.parse::<u64>()
                            .ok()
                            .map(|id| (ChannelId(id), text.clone()))
                    })
                    .collect();
                self.composer.load_drafts(drafts);
            }
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
            Event::Messages(channel, change) => self.messages_changed(channel, change),
            Event::SendResult { result, .. } => {
                if let Err(reason) = result {
                    self.note_at(format!("not sent: {reason}"), NoteLevel::Error);
                }
                self.view.stale = true;
            }
            Event::Media { key, result } => {
                if result.is_ok() {
                    // Every message that owns this picture has to be measured
                    // again; the generation is what makes their cache keys
                    // miss.
                    let _ = key;
                    self.chat.media_gen = self.chat.media_gen.wrapping_add(1);
                    self.view.stale = true;
                }
            }
            Event::Mention { .. } => {
                if self.cfg.notify.bell {
                    // The terminal's own bell, which is the only notification
                    // that needs nothing installed. Desktop notifications land
                    // with the milestone that owns them.
                    print!("\x07");
                }
            }
            Event::UploadProgress { .. } | Event::Gifs { .. } | Event::Search { .. } => {}
        }
    }

    fn messages_changed(&mut self, channel: ChannelId, change: MessagesChange) {
        if Some(channel) != self.chat.channel() {
            // Another channel's traffic still moves the unread counts.
            self.view.stale = true;
            return;
        }
        match change {
            MessagesChange::Prepended(count) => {
                // Re-read first, so the rows the anchor is adjusted against
                // are the ones the page arrived into.
                self.refresh_now();
                self.chat.prepended(count);
            }
            MessagesChange::Updated(id)
            | MessagesChange::Removed(id)
            | MessagesChange::Reactions(id) => {
                self.chat.cache.forget(id);
            }
            MessagesChange::Appended(_)
            | MessagesChange::Replaced
            | MessagesChange::Pending(_)
            | MessagesChange::Loading(_) => {}
        }
        self.view.stale = true;
    }

    /// Copy what the panels draw out of `State`, if anything has changed.
    fn refresh(&mut self) {
        {
            let state = self.core.state();
            if !self.view.stale && state.version() == self.view.version {
                return;
            }
        }
        self.refresh_now();
    }

    fn refresh_now(&mut self) {
        let state = self.core.state();

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
        let member_rows = self
            .nav
            .guild
            .and_then(|guild| core_ext::member_rows(&state, guild));

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

        let sources = self.sources(&state);
        self.chat.refresh(&state, &self.cfg, &self.tz);

        self.view = ViewData {
            version: state.version(),
            stale: false,
            guilds: rail,
            channels,
            dms,
            friends,
            members: member_rows,
            location,
            unread,
            mentions,
            sources,
        };
        drop(state);
        self.clamp_cursors();
    }

    /// What the composer's popups offer: everybody who has said something in
    /// this channel, every channel in the server, and the account's own emoji.
    fn sources(&self, state: &crate::discord::state::State) -> Sources {
        let mut users: Vec<(String, u64)> = Vec::new();
        if let Some(channel) = self.nav.channel {
            for msg in state.recent(channel, 100) {
                let name = msg.author_name().to_string();
                if !users.iter().any(|(n, _)| *n == name) {
                    users.push((name, msg.author.id.0));
                }
            }
            if let Some(c) = state.channel(channel) {
                for id in c.recipient_ids() {
                    let name = state.display_name(None, id);
                    if !users.iter().any(|(n, _)| *n == name) {
                        users.push((name, id.0));
                    }
                }
            }
        }
        let channels = self
            .nav
            .guild
            .map(|guild| {
                state
                    .channels_ordered(guild)
                    .into_iter()
                    .filter(|c| c.kind.is_text())
                    .filter_map(|c| c.name().map(|n| (n.to_string(), c.id.0)))
                    .collect()
            })
            .unwrap_or_default();
        Sources {
            users,
            channels,
            emoji: core_ext::custom_emoji(state),
        }
    }

    fn clamp_cursors(&mut self) {
        let dms = self.dm_rows().len();
        let (guilds, channels) = (self.view.guilds.len(), self.view.channels.len());
        let members = self.view.members.as_ref().map(Vec::len).unwrap_or(0);
        let cap = |cursor: &mut usize, len: usize| {
            *cursor = (*cursor).min(len.saturating_sub(1));
        };
        cap(&mut self.nav.guild_cursor, guilds);
        cap(&mut self.nav.channel_cursor, channels);
        cap(&mut self.nav.dm_cursor, dms);
        cap(&mut self.nav.member_cursor, members);
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
        let channel = match self.session_channel {
            Some(channel) => channel,
            None => {
                let Some(path) = self.session_path.clone() else {
                    return;
                };
                let Some(channel) = core_ext::last_channel(&path) else {
                    return;
                };
                channel
            }
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
        // Anchor and draft belong to where they were written, so they are put
        // away before the move rather than after it.
        if let Some(previous) = self.nav.channel {
            if previous != channel {
                self.save_channel_state(previous);
                self.core.send(Command::CloseChannel(previous));
            }
        }
        self.nav.channel = Some(channel);
        if let Some(c) = self.core.state().channel(channel) {
            self.nav.guild = c.guild_id;
        }
        self.chat.open(channel);
        self.composer.open(channel);
        self.core.send(Command::OpenChannel(channel));
        self.core.send(Command::SetFocus {
            channel: Some(channel),
            terminal_focused: self.terminal_focused,
        });
        // The member list is a subscription against a range, and the range is
        // the top of the list: what the panel can draw without scrolling.
        // Asking for more as it scrolls is the milestone after this one.
        if let Some(guild) = self.nav.guild {
            self.core.send(Command::RequestMembers {
                guild,
                channel,
                ranges: vec![(0, 99)],
            });
        }
        self.focused_since = Instant::now();
        self.layout.focus_set(PanelId::Chat);
        self.view.stale = true;
    }

    fn save_channel_state(&mut self, channel: ChannelId) {
        self.core.send(Command::SetAnchor {
            channel,
            message: self.chat.saved_anchor(channel),
        });
        self.core.send(Command::SetDraft {
            channel,
            text: self.composer.draft(channel),
        });
    }

    fn set_terminal_focus(&mut self, focused: bool) {
        if self.terminal_focused == focused {
            return;
        }
        self.terminal_focused = focused;
        self.focused_since = Instant::now();
        self.core.send(Command::SetFocus {
            channel: self.nav.channel,
            terminal_focused: focused,
        });
    }

    /// Acknowledge a channel once it has been looked at rather than merely
    /// opened: at the bottom, with the terminal in front, for two seconds.
    fn maybe_mark_read(&mut self) {
        if !self.terminal_focused || self.login.is_some() {
            return;
        }
        if self.focused_since.elapsed() < READ_AFTER {
            return;
        }
        if !self.chat.at_bottom() {
            return;
        }
        self.mark_read();
    }

    fn mark_read(&mut self) {
        let (Some(channel), Some(up_to)) = (self.nav.channel, self.chat.newest()) else {
            return;
        };
        if self.acked == Some((channel, up_to)) {
            return;
        }
        self.acked = Some((channel, up_to));
        self.core.send(Command::MarkRead { channel, up_to });
    }

    /// Write `[layout]` a second after the drag that changed it stopped.
    fn settle_layout(&mut self) {
        let Some(at) = self.layout_dirty else { return };
        if at.elapsed() < SETTLE || self.layout.drag.is_some() {
            return;
        }
        self.layout_dirty = None;
        self.cfg.layout = self.layout.cfg.clone();
        self.write_config(&[
            ("layout", "left_cols", self.cfg.layout.left_cols.to_string()),
            (
                "layout",
                "members_cols",
                self.cfg.layout.members_cols.to_string(),
            ),
            ("layout", "dms_share", self.cfg.layout.dms_share.to_string()),
        ]);
    }

    /// Rewrite keys in `config.toml`, one line each, leaving the comments the
    /// template was written to carry exactly where they are.
    fn write_config(&mut self, keys: &[(&str, &str, String)]) {
        for (section, key, value) in keys {
            let value = match value.parse::<i64>() {
                Ok(n) => starkit::config::edit::Value::Int(n),
                Err(_) => match value.as_str() {
                    "true" => starkit::config::edit::Value::Bool(true),
                    "false" => starkit::config::edit::Value::Bool(false),
                    text => starkit::config::edit::Value::Str(text.to_string()),
                },
            };
            if let Err(e) = starkit::config::edit::set(&self.cfg_path, section, key, &value) {
                tracing::warn!("could not write [{section}] {key}: {e}");
                self.note_at(format!("could not save {key}"), NoteLevel::Warning);
                return;
            }
        }
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
        if focus == PanelId::Chat {
            self.chat.move_cursor(delta);
            return;
        }
        let height = usize::from(self.body_height(focus));
        // A folded channel panel showing the DM list moves the DM cursor: the
        // panel is the list it is drawing, whatever its id says.
        let folded = self.dms_in_the_fold();
        let len = match focus {
            PanelId::Guilds => self.view.guilds.len(),
            PanelId::Channels if folded => self.dm_rows().len(),
            PanelId::Channels => self.view.channels.len(),
            PanelId::Dms => self.dm_rows().len(),
            PanelId::Members => self.view.members.as_ref().map(Vec::len).unwrap_or(0),
            _ => return,
        };
        if len == 0 {
            return;
        }
        let (cursor, scroll) = match focus {
            PanelId::Guilds => (&mut self.nav.guild_cursor, &mut self.nav.guild_scroll),
            PanelId::Channels if folded => (&mut self.nav.dm_cursor, &mut self.nav.dm_scroll),
            PanelId::Channels => (&mut self.nav.channel_cursor, &mut self.nav.channel_scroll),
            PanelId::Dms => (&mut self.nav.dm_cursor, &mut self.nav.dm_scroll),
            PanelId::Members => (&mut self.nav.member_cursor, &mut self.nav.member_scroll),
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
        if self.layout.focus() == PanelId::Channels && self.dms_in_the_fold() {
            if let Some(dms::Row::Dm { id, .. }) = self.dm_rows().get(self.nav.dm_cursor) {
                let id = *id;
                self.open_channel(id);
            }
            return;
        }
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
        // Every protocol image was encoded for the old colours, and every
        // measured message carried the old ones in its key.
        self.look.graphics.forget_all();
        self.chat.cache.clear();
        self.note(name);
    }

    // -- keys --------------------------------------------------------------

    pub fn key(&mut self, k: KeyEvent) {
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
            overlays::Key::Confirmed(pending) => {
                self.confirmed(pending);
                return;
            }
            overlays::Key::Jump(target) => {
                self.jump(target);
                return;
            }
            overlays::Key::Setting(setting, forward) => {
                self.change_setting(setting, forward);
                return;
            }
            overlays::Key::Ignored => {}
        }

        // Then the composer, which is a text field and takes raw keys.
        if self.layout.focus() == PanelId::Composer && keymap::composer_eats(k) {
            let outcome = {
                let sources = std::mem::take(&mut self.view.sources);
                let out = self.composer.handle(k, &self.cfg.compose, &sources);
                self.view.sources = sources;
                out
            };
            match outcome {
                composer::Outcome::Taken => return,
                composer::Outcome::Changed => {
                    self.draft_changed();
                    return;
                }
                composer::Outcome::Send => {
                    self.send();
                    return;
                }
                composer::Outcome::SaveEdit(id) => {
                    self.save_edit(id);
                    return;
                }
                composer::Outcome::Leave => {
                    self.layout.focus_set(PanelId::Chat);
                    return;
                }
                composer::Outcome::EditLast => {
                    self.edit_last();
                    return;
                }
                composer::Outcome::Ignored => {}
            }
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
            return;
        }
        if self.over.takes_paste() {
            self.over.paste(text);
            return;
        }
        if self.layout.focus() == PanelId::Composer {
            let sources = std::mem::take(&mut self.view.sources);
            self.composer.paste(text, &sources);
            self.view.sources = sources;
            self.draft_changed();
        }
    }

    /// One action. The single place a key turns into a change.
    pub fn handle(&mut self, action: Action) {
        match action {
            Action::Quit => self.ask_to_quit(),
            Action::Help => self.over.toggle_help(),
            Action::CloseOverlay => self.over.close(),

            Action::CursorUp => self.move_cursor(-1),
            Action::CursorDown => self.move_cursor(1),
            Action::CursorUpBig => self.move_cursor(-10),
            Action::CursorDownBig => self.move_cursor(10),
            Action::PageUp => self.page(-1),
            Action::PageDown => self.page(1),
            Action::Home => {
                if self.layout.focus() == PanelId::Chat {
                    self.chat.to_top();
                } else {
                    self.move_cursor(isize::MIN / 2);
                }
            }
            Action::End | Action::ToBottom => {
                if self.layout.focus() == PanelId::Chat || action == Action::ToBottom {
                    self.chat.to_bottom();
                } else {
                    self.move_cursor(isize::MAX / 2);
                }
            }
            Action::Activate => self.activate(),
            Action::Back => self.back(),

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
                if self.layout.dms_folded() {
                    // Folded into the channel panel: the key swaps what that
                    // panel is showing rather than closing a list that has no
                    // panel to close.
                    self.swap_fold();
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
            Action::OpenPanelSettings => {
                let focus = self.layout.focus();
                self.over.open_settings(focus);
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

            Action::QuickSwitch => self.open_quick(),
            Action::Reply => self.reply(true),
            Action::ReplyNoPing => self.reply(false),
            Action::Edit => self.edit_selected(),
            Action::Delete => self.delete_selected(),
            Action::Yank => self.yank(false),
            Action::YankLink => self.yank(true),
            Action::CopyMessageLink => self.copy_jump_link(),
            Action::OpenExternal | Action::OpenMedia => self.open_external(),
            Action::RevealSpoiler => {
                if !self.chat.reveal() {
                    self.note("nothing hidden here");
                }
            }
            Action::JumpToReply => match self.chat.jump_to_reply() {
                Some(_) => {}
                None => self.note("not a reply"),
            },
            Action::MarkRead => {
                self.mark_read();
                self.note("marked read");
            }
            Action::LoadOlder => {
                if let Some(channel) = self.nav.channel {
                    self.core.send(Command::LoadOlder(channel));
                }
            }

            Action::Send => self.send(),
            Action::EditLast => self.edit_last(),
            Action::CancelCompose => {
                if !self.composer.cancel() {
                    self.layout.focus_set(PanelId::Chat);
                }
            }
            Action::ClearComposer => {
                self.composer.input.clear();
                self.draft_changed();
            }
            Action::Newline => {
                let sources = std::mem::take(&mut self.view.sources);
                self.composer.paste("\n", &sources);
                self.view.sources = sources;
                self.draft_changed();
            }

            // Everything the later milestones own. Named rather than left to a
            // catch-all so that adding an action forces a decision here.
            Action::NextUnread
            | Action::PrevUnread
            | Action::Search
            | Action::React
            | Action::TogglePin
            | Action::EmojiPicker
            | Action::GifPicker
            | Action::Attach
            | Action::PasteImage
            | Action::MediaNext
            | Action::MediaPrev
            | Action::MediaSave
            | Action::MediaZoom => self.note("not yet"),
        }
    }

    fn page(&mut self, direction: isize) {
        if self.layout.focus() == PanelId::Chat {
            let h = i32::from(self.body_height(PanelId::Chat)).max(1);
            self.chat.scroll(h * direction as i32);
            return;
        }
        let h = self.body_height(self.layout.focus()) as isize;
        self.move_cursor(h.max(1) * direction);
    }

    /// `esc`: close, cancel, then mark read and go back to the conversation.
    fn back(&mut self) {
        if self.over.open() {
            self.over.close();
            return;
        }
        if self.layout.focus() == PanelId::Composer && self.composer.cancel() {
            return;
        }
        self.mark_read();
        self.layout.focus_set(PanelId::Chat);
    }

    fn ask_to_quit(&mut self) {
        let unsent = self.composer.unsent();
        if unsent > 0 {
            self.over.ask(Confirm::quit_with_draft(unsent));
            return;
        }
        self.leave();
    }

    fn leave(&mut self) {
        if let Some(channel) = self.nav.channel {
            self.save_channel_state(channel);
        }
        self.quit = true;
    }

    fn confirmed(&mut self, pending: Pending) {
        match pending {
            Pending::Quit => self.leave(),
            Pending::DeleteMessage { channel, message } => {
                self.core.send(Command::DeleteMessage { channel, message });
                self.note("deleted");
            }
        }
    }

    fn jump(&mut self, target: Target) {
        match target {
            Target::Guild(id) => {
                let index = self
                    .view
                    .guilds
                    .iter()
                    .position(|row| row.id == Some(id))
                    .unwrap_or(0);
                self.select_guild(index);
                self.layout.focus_set(PanelId::Channels);
            }
            Target::Channel(id) | Target::Dm(id) => self.open_channel(id),
            Target::Friend(id) => {
                // The core refuses a DM with anybody who is not a friend, so
                // this is only ever asked for people who are.
                self.core.send(Command::OpenDm(id));
                self.note("opening a conversation");
            }
        }
    }

    fn open_quick(&mut self) {
        let state = self.core.state();
        let mut items = Vec::new();
        for channel in state.dms_ordered() {
            items.push(quick::Item {
                target: Target::Dm(channel.id),
                label: format!("@{}", state.dm_title(channel.id)),
                hint: "dm".into(),
            });
        }
        for guild in state.guilds_ordered() {
            items.push(quick::Item {
                target: Target::Guild(guild.id),
                label: guild.name.clone(),
                hint: "server".into(),
            });
            for channel in state.channels_ordered(guild.id) {
                if !channel.kind.is_text() {
                    continue;
                }
                let Some(name) = channel.name() else { continue };
                items.push(quick::Item {
                    target: Target::Channel(channel.id),
                    label: format!("#{name}"),
                    hint: guild.name.clone(),
                });
            }
        }
        for (id, name, _) in core_ext::friends(&state) {
            items.push(quick::Item {
                target: Target::Friend(id),
                label: format!("@{name}"),
                hint: "friend".into(),
            });
        }
        drop(state);
        self.over.open_quick(items);
    }

    fn change_setting(&mut self, setting: Setting, forward: bool) {
        match setting {
            Setting::Theme => self.cycle_theme(forward),
            Setting::Timestamps => self.cfg.chat.timestamps = self.cfg.chat.timestamps.next(),
            Setting::Avatars => self.cfg.chat.show_avatars = !self.cfg.chat.show_avatars,
            Setting::Animate => self.cfg.media.animate = self.cfg.media.animate.next(),
            Setting::SendKey => {
                self.cfg.compose.send_key = match self.cfg.compose.send_key {
                    crate::config::SendKey::Enter => crate::config::SendKey::CtrlEnter,
                    crate::config::SendKey::CtrlEnter => crate::config::SendKey::Enter,
                }
            }
        }
        self.chat.cache.clear();
        let (section, key) = setting.where_written();
        let value = setting.value(&self.cfg);
        // The file says what the running program says, always. A row that
        // changed only one of the two is a setting that reverts on restart.
        let value = match setting {
            Setting::Avatars => value.replace("on", "true").replace("off", "false"),
            Setting::SendKey => value.replace("ctrl+enter", "ctrl-enter"),
            _ => value,
        };
        self.write_config(&[(section, key, value)]);
    }

    // -- a message ---------------------------------------------------------

    fn reply(&mut self, ping: bool) {
        let Some(msg) = self.chat.selected() else {
            self.note("no message chosen");
            return;
        };
        let author = msg.author_name().to_string();
        self.composer.reply_to(msg.id, author, ping);
        self.layout.focus_set(PanelId::Composer);
    }

    fn edit_selected(&mut self) {
        let Some(msg) = self.chat.selected() else {
            self.note("no message chosen");
            return;
        };
        if !self.is_mine(&msg) {
            self.note("only your own");
            return;
        }
        let content = msg.content.clone();
        self.composer.edit(msg.id, content);
        self.layout.focus_set(PanelId::Composer);
    }

    fn edit_last(&mut self) {
        let Some(channel) = self.nav.channel else {
            return;
        };
        let state = self.core.state();
        let me = state.me().map(|u| u.id);
        let last = state
            .recent(channel, 100)
            .into_iter()
            .rev()
            .find(|m| Some(m.author.id) == me && !m.kind.is_system());
        drop(state);
        match last {
            Some(msg) => {
                let content = msg.content.clone();
                self.composer.edit(msg.id, content);
                self.layout.focus_set(PanelId::Composer);
            }
            None => self.note("nothing of yours to edit"),
        }
    }

    fn delete_selected(&mut self) {
        let (Some(channel), Some(msg)) = (self.nav.channel, self.chat.selected()) else {
            self.note("no message chosen");
            return;
        };
        if !self.is_mine(&msg) {
            self.note("only your own");
            return;
        }
        self.over
            .ask(Confirm::delete(channel, msg.id, &msg.content));
    }

    fn is_mine(&self, msg: &crate::discord::model::Message) -> bool {
        self.core.state().me().map(|u| u.id) == Some(msg.author.id)
    }

    fn send(&mut self) {
        let Some(channel) = self.nav.channel else {
            self.note("no channel open");
            return;
        };
        let (reply_to, mention_author) = match &self.composer.mode {
            composer::Mode::Reply { to, ping, .. } => (Some(*to), *ping),
            _ => (None, true),
        };
        let content = self.composer.take();
        if content.trim().is_empty() {
            return;
        }
        self.core.send(Command::SendMessage {
            channel,
            content,
            reply_to,
            mention_author,
            attachments: Vec::new(),
        });
        self.core.send(Command::SetDraft {
            channel,
            text: String::new(),
        });
        self.chat.to_bottom();
        self.view.stale = true;
    }

    fn save_edit(&mut self, message: MessageId) {
        let Some(channel) = self.nav.channel else {
            return;
        };
        let content = self.composer.take();
        if content.trim().is_empty() {
            self.note("an empty edit would be a delete");
            return;
        }
        self.core.send(Command::EditMessage {
            channel,
            message,
            content,
        });
        self.chat.cache.forget(message);
        self.layout.focus_set(PanelId::Chat);
    }

    /// The text changed: remember it, and say somebody is typing.
    ///
    /// `Command::Typing` goes out on every change and the core throttles it to
    /// one request every nine seconds per channel. Throttling here as well
    /// would be the same rule written twice, and the one that matters is the
    /// one next to the request.
    fn draft_changed(&mut self) {
        let Some(channel) = self.nav.channel else {
            return;
        };
        self.core.send(Command::SetDraft {
            channel,
            text: self.composer.draft(channel),
        });
        if self.cfg.compose.typing_indicator && !self.composer.is_empty() {
            self.core.send(Command::Typing(channel));
        }
    }

    fn yank(&mut self, link: bool) {
        let Some(msg) = self.chat.selected() else {
            self.note("no message chosen");
            return;
        };
        let text = if link {
            match first_link(&msg) {
                Some(url) => url,
                None => {
                    self.note("no link in it");
                    return;
                }
            }
        } else {
            msg.content.clone()
        };
        self.copy(&text, if link { "link copied" } else { "copied" });
    }

    fn copy_jump_link(&mut self) {
        let (Some(channel), Some(msg)) = (self.nav.channel, self.chat.selected()) else {
            self.note("no message chosen");
            return;
        };
        let guild = self
            .core
            .state()
            .channel(channel)
            .and_then(|c| c.guild_id)
            .map(|g| g.to_string())
            .unwrap_or_else(|| "@me".into());
        let url = format!("https://discord.com/channels/{guild}/{channel}/{}", msg.id);
        self.copy(&url, "link copied");
    }

    /// Put text on the system clipboard.
    ///
    /// A fresh `Clipboard` each time. Under Wayland the clipboard is owned by
    /// a live connection, and holding one for the life of the program means
    /// holding a socket open for a feature used a few times an hour; under X11
    /// `wayland-data-control` is not in play at all. Failure is a note rather
    /// than an error: a terminal with no clipboard at the other end -- over
    /// ssh, in a bare tty -- is a perfectly ordinary place to be running this.
    fn copy(&mut self, text: &str, done: &str) {
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())) {
            Ok(()) => self.note(done),
            Err(e) => {
                tracing::debug!("clipboard: {e}");
                self.note_at("no clipboard here", NoteLevel::Warning);
            }
        }
    }

    fn open_external(&mut self) {
        let Some(msg) = self.chat.selected() else {
            self.note("no message chosen");
            return;
        };
        let (url, kind) = match msg.attachments.first() {
            Some(attachment) => (
                attachment.url.clone(),
                if attachment.is_video() {
                    ExternalKind::Video
                } else if attachment.is_image() {
                    ExternalKind::Image
                } else {
                    ExternalKind::Link
                },
            ),
            None => match first_link(&msg) {
                Some(url) => (url, ExternalKind::Link),
                None => {
                    self.note("nothing to open");
                    return;
                }
            },
        };
        self.core.send(Command::OpenExternal { url, kind });
        self.note("opening");
    }

    fn swap_fold(&mut self) {
        self.nav.fold = match self.nav.fold {
            Fold::Channels => Fold::Dms,
            Fold::Dms => Fold::Channels,
        };
        self.layout.focus_set(PanelId::Channels);
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
                    self.layout.drag = Some(Drag::Seam { seam, x, y });
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
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(x, y),
            MouseEventKind::Up(MouseButton::Left) => self.layout.drag = None,
            MouseEventKind::ScrollDown => self.scroll_panel(&regions, x, y, 3),
            MouseEventKind::ScrollUp => self.scroll_panel(&regions, x, y, -3),
            _ => {}
        }
    }

    fn drag_to(&mut self, x: u16, y: u16) {
        let Some(Drag::Seam { seam, x: px, y: py }) = self.layout.drag else {
            return;
        };
        let horizontal = matches!(
            self.layout.seam_axis(seam),
            Some(starkit::dock::Axis::Horizontal)
        );
        let delta = if horizontal {
            i32::from(x) - i32::from(px)
        } else {
            i32::from(y) - i32::from(py)
        };
        let delta = delta.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        if self.layout.drag_seam(seam, delta) {
            self.layout_dirty = Some(Instant::now());
            self.chat.cache.clear();
        }
        self.layout.drag = Some(Drag::Seam { seam, x, y });
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
        let words = panels::words(panel, self.nav.dm_tab, self.fold());
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
            PanelId::Channels if self.dms_in_the_fold() => {
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
            PanelId::Members => {
                let v = self.members_view(false);
                if let Some(index) = members::row_at(body, &v, y) {
                    if v.rows.get(index).is_some_and(members::Row::selectable) {
                        self.nav.member_cursor = index;
                    }
                }
            }
            PanelId::Chat => self.chat_click(x, y, double),
            PanelId::Composer => {}
        }
    }

    fn chat_click(&mut self, x: u16, y: u16, double: bool) {
        let Some(hit) = self.chat.hit(x, y) else {
            return;
        };
        match hit {
            Hit::Message(id) => self.chat.select(id),
            Hit::Link(url) => self.core.send(Command::OpenExternal {
                url,
                kind: ExternalKind::Link,
            }),
            Hit::Reply(id) => {
                self.chat.select(id);
                self.handle(Action::JumpToReply);
            }
            Hit::Reaction(message, emoji) => {
                let Some(channel) = self.nav.channel else {
                    return;
                };
                let emoji = match (emoji.id, emoji.name.clone()) {
                    (Some(id), Some(name)) => EmojiRef::Custom {
                        name,
                        id,
                        animated: emoji.animated,
                    },
                    (_, Some(name)) => EmojiRef::Unicode(name),
                    _ => return,
                };
                // The chip says whether this account is on it; toggling is the
                // one gesture, and the core decides which request that is.
                self.core.send(Command::AddReaction {
                    channel,
                    message,
                    emoji,
                });
            }
            Hit::Attachment(id, url) => {
                self.chat.select(id);
                if double {
                    self.core.send(Command::OpenExternal {
                        url,
                        kind: ExternalKind::Image,
                    });
                }
            }
            Hit::LoadOlder => {
                if let Some(channel) = self.nav.channel {
                    self.core.send(Command::LoadOlder(channel));
                }
            }
        }
    }

    fn word_click(&mut self, panel: PanelId, word: panels::Word) {
        match word {
            panels::Word::Close => self.layout.toggle(panel),
            panels::Word::ShowFriends | panels::Word::ShowDms => self.swap_dm_tab(),
            panels::Word::ShowMessages | panels::Word::ShowChannels => self.swap_fold(),
            panels::Word::Zen => self.handle(Action::ToggleZen),
            panels::Word::Settings => self.over.open_settings(panel),
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
            PanelId::Chat => self.chat.scroll(delta as i32),
            PanelId::Channels | PanelId::Dms | PanelId::Members => {
                let was = self.layout.focus();
                self.layout.focus_set(panel);
                self.move_cursor(delta);
                self.layout.focus_set(was);
            }
            PanelId::Composer => {}
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
            mode: self.mode_word(),
            new_below: self.chat.new_below() as u32,
            now: self.last_frame,
        }
    }

    /// What the status line calls what is happening: the right-hand field is
    /// the one somebody glances at, and "am I about to type into a message or
    /// into a list" is the thing they are glancing for.
    fn mode_word(&self) -> &'static str {
        if self.layout.focus() != PanelId::Composer {
            return "chat";
        }
        match self.composer.mode {
            composer::Mode::Normal => "compose",
            composer::Mode::Reply { .. } => "reply",
            composer::Mode::Edit { .. } => "edit",
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
        }
    }

    fn members_view(&self, focused: bool) -> members::View<'_> {
        members::View {
            theme: &self.look.theme,
            focused,
            rows: self.view.members.as_deref().unwrap_or(&[]),
            cursor: self.nav.member_cursor,
            scroll: self.nav.member_scroll,
            loaded: self.view.members.is_some(),
        }
    }

    /// Which list the channel panel is carrying, or `None` when the DM list
    /// has a panel of its own.
    fn fold(&self) -> Option<Fold> {
        self.layout.dms_folded().then_some(self.nav.fold)
    }

    /// Whether the DM list is being drawn inside the channel panel.
    fn dms_in_the_fold(&self) -> bool {
        self.fold() == Some(Fold::Dms)
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
        self.caret = None;
        let bg = Style::default()
            .bg(rgb(self.look.theme.bg))
            .fg(rgb(self.look.theme.fg));
        buf.set_style(area, bg);

        if let Some(screen) = &self.login {
            screen.render(area, buf, &self.look.theme);
            return;
        }

        let composer_rows = self.composer.rows(&self.cfg.compose, self.composer_width);
        let Some(regions) = self
            .layout
            .regions(area, composer_rows, self.look.padding)
            .cloned()
        else {
            too_small(area, buf, &self.look.theme);
            return;
        };
        if let Some(rect) = regions.rect_of(PanelId::Composer) {
            self.composer_width = rect.width;
        }

        let focus = self.layout.focus();
        for panel in regions.visible() {
            let Some(rect) = regions.rect_of(panel) else {
                continue;
            };
            let focused = panel == focus;
            let title = self.panel_title(panel);
            let words = panels::words(panel, self.nav.dm_tab, self.fold());
            let body = panels::frame(
                rect,
                buf,
                &panels::Frame {
                    theme: &self.look.theme,
                    focused,
                    title: &title,
                    words: &words,
                },
            );
            if regions.too_small.contains(&panel) {
                panels::empty(body, buf, &self.look.theme, "too narrow");
                continue;
            }
            match panel {
                PanelId::Guilds => guilds::render(body, buf, &self.guilds_view(focused)),
                PanelId::Channels if self.dms_in_the_fold() => {
                    dms::render(body, buf, &self.dms_view(focused))
                }
                PanelId::Channels => channels::render(body, buf, &self.channels_view(focused)),
                PanelId::Dms => dms::render(body, buf, &self.dms_view(focused)),
                PanelId::Members => members::render(body, buf, &self.members_view(focused)),
                PanelId::Chat => {
                    let params = chat::Params {
                        theme: &self.look.theme,
                        cfg: &self.cfg,
                        focused,
                        pictures: self.look.graphics.pictures_available(),
                        aspect: self.look.graphics.cell_aspect().unwrap_or(2.0),
                        me: self.core.state().me().map(|u| u.id),
                        tz: self.tz.clone(),
                    };
                    self.chat.render(rect, body, buf, &params);
                }
                PanelId::Composer => {
                    let view = super::panels::composer::View {
                        theme: &self.look.theme,
                        focused,
                        channel: &self.view.location,
                    };
                    self.caret = self.composer.render(body, buf, &view);
                }
            }
        }

        // The message list asks for older history once it is looking at the
        // top of what it has, which is only knowable after it has been laid
        // out.
        if self.chat.wants_older() {
            if let Some(channel) = self.nav.channel {
                self.core.send(Command::LoadOlder(channel));
            }
        }

        status::render(regions.status, buf, &self.status_view());
        self.over.render(area, buf, &self.look.theme, &self.cfg);
        self.draw_caret(area, buf);
    }

    /// The caret, drawn rather than placed.
    ///
    /// `term::init` hides the terminal's own cursor, and a drawn caret is also
    /// the only one a snapshot can see. Reversing the cell keeps whatever
    /// colour the text under it had, which is what makes it visible on every
    /// one of the sixteen themes without a role of its own.
    fn draw_caret(&self, area: Rect, buf: &mut Buffer) {
        let Some((x, y)) = self.caret else { return };
        if x < area.x || y < area.y || x >= area.x + area.width || y >= area.y + area.height {
            return;
        }
        buf[(x, y)].modifier |= Modifier::REVERSED;
    }

    fn panel_title(&self, panel: PanelId) -> String {
        match panel {
            // The channel list says which server it is listing, because at
            // twenty-six columns the rail's two letters are not an answer.
            PanelId::Channels if self.dms_in_the_fold() => match self.nav.dm_tab {
                DmTab::Dms => "messages".into(),
                DmTab::Friends => "friends".into(),
            },
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
            PanelId::Chat => chat::title(&self.view.location),
            _ => panel.title().to_string(),
        }
    }
}

/// The first link in a message, from its text or from what is attached.
fn first_link(msg: &crate::discord::model::Message) -> Option<String> {
    for word in msg.content.split_whitespace() {
        let word = word.trim_matches(|c: char| "<>()[],.".contains(c));
        if word.starts_with("https://") || word.starts_with("http://") {
            return Some(word.to_string());
        }
    }
    if let Some(attachment) = msg.attachments.first() {
        return Some(attachment.url.clone());
    }
    msg.embeds.iter().find_map(|e| e.url.clone())
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

    /// Unless there is something half-written, in which case it asks.
    #[test]
    fn quitting_with_a_draft_asks_first() {
        let mut a = app();
        a.login = None;
        a.composer.open(ChannelId(1));
        a.composer.input.set_text("half a sentence");
        a.handle(Action::Quit);
        assert!(!a.quit, "it quit without asking");
        assert!(a.over.confirm.is_some());

        a.key(KeyEvent::from(starkit::crossterm::event::KeyCode::Char(
            'y',
        )));
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

    /// A letter typed into the composer is a letter, and `alt+…` is still a
    /// command. The keymap asserts the rule; this asserts that `App` obeys it.
    #[test]
    fn letters_reach_the_composer_and_alt_keys_do_not() {
        use starkit::crossterm::event::{KeyCode, KeyModifiers};
        let mut a = app();
        a.login = None;
        a.nav.channel = Some(ChannelId(1));
        a.composer.open(ChannelId(1));
        a.layout.focus_set(PanelId::Composer);

        for c in "delete".chars() {
            a.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(a.composer.text(), "delete");

        let before = a.layout.is_open(PanelId::Members);
        a.key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
        assert_ne!(
            a.layout.is_open(PanelId::Members),
            before,
            "alt+m did not reach the panel table"
        );
        assert_eq!(a.composer.text(), "delete", "and did not type an m");
    }

    /// The escape chain, from the composer out.
    #[test]
    fn escape_walks_back_out_of_the_composer() {
        use starkit::crossterm::event::KeyCode;
        let mut a = app();
        a.login = None;
        a.nav.channel = Some(ChannelId(1));
        a.composer.open(ChannelId(1));
        a.composer.reply_to(MessageId(7), "alex".into(), true);
        a.layout.focus_set(PanelId::Composer);

        a.key(KeyEvent::from(KeyCode::Esc));
        assert!(a.composer.mode.is_normal(), "the reply was not cancelled");
        assert_eq!(a.layout.focus(), PanelId::Composer);

        a.key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.layout.focus(), PanelId::Chat);
    }

    /// A settings change reaches the running program even when the file
    /// cannot be written, which is the case a read-only home directory is.
    #[test]
    fn a_settings_row_changes_the_program() {
        let mut a = app();
        a.login = None;
        let before = a.cfg.chat.show_avatars;
        a.change_setting(Setting::Avatars, true);
        assert_ne!(a.cfg.chat.show_avatars, before);
    }
}
