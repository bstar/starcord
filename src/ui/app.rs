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
//! login screen → overlay → composer → `g` prefix → focused module → global
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
//! ## Where the module logic is not
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
use starkit::chrome::frame::{self, Badge, Tone};
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
use super::login::{LoginScreen, Outcome};
use super::overlays::attach::Attach;
use super::overlays::confirm::{Confirm, Pending};
use super::overlays::media::Viewer;
use super::overlays::menu::{Choice, Menu};
use super::overlays::picker::{Kind, Picker};
use super::overlays::quick::{self, Target};
use super::overlays::search::Search;
use super::overlays::settings::Setting;
use super::overlays::{self, Overlays};
use super::panels::chat::{ChatState, Hit};
use super::panels::composer::{self, Composer, Sources};
use super::panels::{self, channels, chat, dms, guilds, members, rgb, ModuleId, COLUMN};
use super::status;
use super::theme::Theme;
use super::{clipboard, core_ext, layout, unread};
use crate::config::Config;
use crate::discord::handle::{
    AuthEvent, Connection, EmojiRef, Event, ExternalKind, MessagesChange, Note, NoteLevel,
    SearchScope,
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
    pub collapsed: HashSet<ChannelId>,
    /// Where you have been: the conversations opened before this one, most
    /// recent last, and the ones stepped back out of, for `›`. A fresh move
    /// forgets the forward list, the way a browser's does.
    pub back: Vec<ChannelId>,
    pub forward: Vec<ChannelId>,
}

/// How much of where you have been is kept.
const HISTORY: usize = 100;

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
    /// What the second module shows at home: the conversations, then the
    /// friends who have not started one, each under a heading.
    pub messages: Vec<dms::Row>,
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
    /// Whether a READY has been seen this run; the first restores the session,
    /// the rest are reconnects.
    ready_seen: bool,
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
    pub(super) note: Option<(String, NoteLevel, Instant)>,
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
    /// Where the caret goes, if anything on screen has one.
    caret: Option<(u16, u16)>,
    /// Draw every cell next frame rather than only what changed.
    ///
    /// The terminal is a shared surface. A capability reply that arrived late,
    /// a stray byte echoed by something else, a multiplexer redrawing a pane:
    /// any of them leaves the screen holding a character this program's buffer
    /// does not know about, and the diff will never repaint that cell because
    /// as far as it is concerned nothing there changed. Closing an overlay is
    /// where that shows, because the overlay blanked the region and the panels
    /// underneath are about to claim it back. So the frame after an overlay
    /// closes is a full one, and `ctrl+l` asks for one at any time.
    repaint: bool,
    /// A message the view should land on once its channel's window arrives.
    pending_jump: Option<(ChannelId, MessageId)>,
    /// A picture waiting for its undecoded bytes so it can be written out.
    pending_save: Option<super::overlays::media::Item>,
    /// Whether anything was modal on the previous frame, so that the frame
    /// after one closes is a whole one.
    overlay_was_open: bool,
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
            layout: LayoutState::new(cfg.ui.list_rows),
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
            caret: None,
            repaint: false,
            pending_jump: None,
            pending_save: None,
            overlay_was_open: false,
            quit: false,
            core,
            cfg,
            cfg_path,
            session_path,
            session_channel: None,
            ready_seen: false,
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
        let result = app.event_loop(&mut term);
        term::restore()?;
        app.core.send(Command::SaveSession);
        app.core.send(Command::Shutdown);
        result
    }

    fn event_loop(&mut self, term: &mut term::Tui) -> Result<()> {
        while !self.quit {
            self.last_frame = Instant::now();

            self.tick();

            if std::mem::take(&mut self.repaint) {
                // Throw away what the diff believes is on the screen, so the
                // next draw writes every cell.
                term.clear()?;
            }
            term.draw(|f| {
                self.draw(f.area(), f.buffer_mut());
            })?;

            // A frame is the ceiling on how long a keystroke waits; an
            // animation that is due sooner than that is the floor. Without
            // this a hundred-millisecond GIF frame arrives up to a frame late
            // every time, which is visible as a limp.
            let wait = self
                .chat
                .anim
                .next_due(Instant::now())
                .map(|due| due.min(FRAME))
                .unwrap_or(FRAME);
            if event::poll(wait)? {
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
        self.maybe_mark_read();
        self.refresh_qr();

        // An overlay that has gone away took the cells it blanked with it, and
        // the panels underneath have to claim them back. Decided here rather
        // than wherever it closed, because it closes from five places and the
        // one that was forgotten is the one somebody sees.
        let open = self.over.open();
        if self.overlay_was_open && !open {
            self.repaint = true;
        }
        self.overlay_was_open = open;

        let now = Instant::now();
        self.over.tick(now);
        for command in self.over.take_commands() {
            self.core.send(command);
        }

        // The animation clock, at the top of the frame and before anything is
        // measured: what moves is decided from what the last frame drew.
        self.chat.anim.set_policy(self.cfg.media.animate);
        let focused = self.terminal_focused && (self.reading() || self.over.open());
        let media = &self.chat.media;
        let moved = self.chat.anim.tick(now, focused, |key| {
            media.decoded(key).and_then(|d| chat::anim::delays_of(&d))
        });
        if moved {
            self.view.stale = true;
        }
    }

    /// A code nobody scanned in time is replaced without being asked.
    ///
    /// The countdown is redrawn thirty times a second and the request is not:
    /// `start_qr` puts the screen back to waiting, which is what stops this
    /// asking again on the next frame.
    fn refresh_qr(&mut self) {
        if !self.login.as_ref().is_some_and(LoginScreen::expired) {
            return;
        }
        if let Some(screen) = &mut self.login {
            screen.start_qr();
        }
        self.core.send(Command::StartRemoteAuth);
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
                    // Straight to the code. There is no token and no password
                    // to type, so the menu would be one keystroke in front of
                    // the only thing that can happen next; `2` is still there
                    // for somebody who has a token in a password manager.
                    let mut screen = LoginScreen::new();
                    screen.start_qr();
                    self.login = Some(screen);
                    self.core.send(Command::StartRemoteAuth);
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
                    self.login
                        .get_or_insert_with(LoginScreen::new)
                        .qr_ready(url, expires_in, matrix);
                }
                AuthEvent::QrScanned { username, .. } => {
                    if let Some(screen) = &mut self.login {
                        screen.scanned(username.clone());
                    }
                    self.note(format!("scanned by {username}"));
                }
                AuthEvent::StoreRefused(why) => {
                    self.login
                        .get_or_insert_with(LoginScreen::new)
                        .notice(format!("the keyring refused the stored token ({why})"));
                }
                AuthEvent::CaptchaNeeded { url } => {
                    self.login.get_or_insert_with(LoginScreen::new).captcha(url);
                    self.note("Discord wants a captcha; it is open in your browser");
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
                // The first READY of a run puts the reader back where they
                // were. Every later one is a reconnect -- the socket dropped
                // and came back with a fresh identify -- and the reader has
                // not moved: the channel they are in stays open and the panel
                // they are typing in keeps the cursor. What a reconnect does
                // need is the channel asked for again, because the core's
                // subscriptions and history went with the old socket.
                if self.ready_seen {
                    if let Some(channel) = self.nav.channel {
                        self.core.send(Command::OpenChannel(channel));
                        self.core.send(Command::SetFocus {
                            channel: Some(channel),
                            terminal_focused: self.terminal_focused,
                        });
                    }
                } else {
                    self.ready_seen = true;
                    self.restore_session();
                }
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
                // Kept whether it arrived or not: a failure is an answer, and
                // one that is not recorded is one that is asked for again on
                // every frame for as long as the message is on screen.
                let ok = result.is_ok();
                self.chat.media.arrived(key.clone(), result);
                if let Some(item) = self.pending_save.clone() {
                    if item.key == key {
                        if let Some(decoded) = self.chat.media.decoded(&key) {
                            if let crate::discord::media::Decoded::Bytes(bytes) = &*decoded {
                                let bytes = bytes.clone();
                                self.pending_save = None;
                                self.write_saved(&item.filename, &bytes);
                            }
                        }
                    }
                }
                if ok {
                    // Only the messages that draw this picture are measured
                    // again; their generation is what makes their cache keys
                    // miss, and nobody else's.
                    self.chat.media_arrived(&key);
                }
                self.view.stale = true;
            }
            Event::Mention { channel, message } => self.mentioned(channel, message),
            Event::UploadProgress { nonce, sent, total } => {
                self.chat.upload_progress(nonce, sent, total);
                self.view.stale = true;
            }
            Event::Gifs { id, result } => {
                if self.over.gifs_arrived(id, result.map(|page| page.results)) {
                    self.view.stale = true;
                }
            }
            Event::Search { id, result } => {
                let state = self.core.state();
                let tz = self.tz.clone();
                let name_of = |msg: &crate::discord::model::Message| {
                    let guild = state.channel(msg.channel_id).and_then(|c| c.guild_id);
                    (
                        state.display_name(guild, msg.author.id),
                        msg.timestamp
                            .map(|t| {
                                t.to_zoned(tz.clone())
                                    .strftime("%Y-%m-%d %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_default(),
                    )
                };
                let used = self.over.search_arrived(id, result, name_of);
                drop(state);
                if used {
                    self.view.stale = true;
                }
            }
        }
    }

    /// Somebody said this account's name somewhere.
    ///
    /// The desktop notification is the core's — it has the configuration, the
    /// collapse rule and the bus — and this is the terminal's own half: the
    /// bell and a line in the status bar. Doing both here would notify twice;
    /// doing neither would leave the client silent on a machine with no
    /// notification daemon, which is most of the ones it will run on.
    fn mentioned(&mut self, channel: ChannelId, message: MessageId) {
        let (muted, line) = core_ext::mention_line(&self.core.state(), channel, message);
        let decision = unread::interrupt(
            unread::Where {
                channel,
                open: self.nav.channel,
                terminal_focused: self.terminal_focused,
            },
            self.cfg.notify.enabled,
            self.cfg.notify.bell,
            muted,
        );
        if decision.bell {
            // The terminal's own bell: the one notification that needs nothing
            // installed and reaches a machine over ssh. Written and flushed
            // here rather than left in a buffer the next frame would scribble
            // over.
            use std::io::Write as _;
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }
        if decision.note {
            self.note(line);
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
            MessagesChange::Removed(id) | MessagesChange::Reactions(id) => {
                self.chat.cache.forget(id);
            }
            // An edit can change more than the message it happened to: a
            // reply's preview is a copy of what it answers. Forgetting the
            // whole cache is a few hundred measurements on a key nobody
            // presses often, and the alternative is a reverse index from
            // every message to everything that quotes it.
            MessagesChange::Updated(_) => self.chat.cache.clear(),
            MessagesChange::Replaced => {
                // Two windows arrive for one jump: the channel's own latest
                // page, and then the page around the message. Only the one
                // that actually holds it counts, or the jump lands on the
                // bottom of the channel and looks like nothing happened.
                if let Some((wanted, message)) = self.pending_jump {
                    if wanted == channel {
                        self.refresh_now();
                        if self.chat.holds(message) {
                            self.pending_jump = None;
                            self.chat.select(message);
                            self.note("jumped \u{b7} G for the newest");
                        }
                    }
                }
            }
            MessagesChange::Appended(_)
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

        // Home first, then the servers in READY's order. Home is an entry in
        // this list like any other, which is what lets the module under it
        // carry conversations where it otherwise carries channels.
        let mut rail = vec![guilds::Row {
            id: None,
            name: "Home".into(),
            icon: None,
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
                icon: guild.icon.clone(),
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

        // One list under two headings rather than two lists behind a tab: at
        // home, the question is who to talk to, and whether a conversation
        // already exists is an answer to it rather than a different question.
        let mut messages = vec![dms::Row::Section {
            label: "conversations".into(),
        }];
        messages.extend(state.dms_ordered().iter().map(|c| {
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
        }));
        messages.extend(dms::group_friends(core_ext::friends(&state)));
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
            messages,
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
        let dms = self.view.messages.len();
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
        // A move made on purpose is one to come back from. The forward list
        // is what was stepped out of, and a new move makes it moot.
        if let Some(previous) = self.nav.channel {
            if previous != channel {
                self.nav.back.push(previous);
                if self.nav.back.len() > HISTORY {
                    self.nav.back.remove(0);
                }
                self.nav.forward.clear();
            }
        }
        self.go_to(channel);
    }

    /// `‹`: the conversation before this one.
    fn history_back(&mut self) {
        let Some(previous) = self.nav.back.pop() else {
            self.note("nowhere to go back to");
            return;
        };
        if self.core.state().channel(previous).is_none() {
            // Gone since it was visited: a channel deleted, a server left.
            // Skipped rather than announced; the one before it is the answer.
            self.history_back();
            return;
        }
        if let Some(current) = self.nav.channel {
            self.nav.forward.push(current);
        }
        self.go_to(previous);
    }

    /// `›`: the conversation this one was stepped back out of.
    fn history_forward(&mut self) {
        let Some(next) = self.nav.forward.pop() else {
            self.note("nowhere to go forward to");
            return;
        };
        if self.core.state().channel(next).is_none() {
            self.history_forward();
            return;
        }
        if let Some(current) = self.nav.channel {
            self.nav.back.push(current);
        }
        self.go_to(next);
    }

    /// Open a conversation. What every move, deliberate or a step through the
    /// history, ends in; the history itself is the callers' business.
    fn go_to(&mut self, channel: ChannelId) {
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
        // The lists have done their job: fold whichever is open, and put the
        // keyboard where somebody who has just opened a conversation wants it.
        self.layout.collapse(ModuleId::Compose);
        self.focus_module(ModuleId::Compose);
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

    pub(super) fn set_terminal_focus(&mut self, focused: bool) {
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
        // Reading is the conversation or the composer under it. Browsing the
        // server list is not reading, and a channel that scrolled past while
        // somebody was looking for another one should not lose its mark.
        if !self.reading() {
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

    /// Choose a server, without opening anything.
    ///
    /// The lists are rebuilt here rather than on the next frame: what the
    /// module under this one is showing, and where its cursor can land, are
    /// both facts about the server that was just chosen, and a cursor placed
    /// against the previous server's channels is a cursor on the wrong row for
    /// one frame.
    fn select_guild(&mut self, index: usize) {
        let Some(row) = self.view.guilds.get(index) else {
            return;
        };
        self.nav.guild_cursor = index;
        self.nav.guild = row.id;
        self.nav.channel_cursor = 0;
        self.nav.channel_scroll = 0;
        self.nav.dm_cursor = 0;
        self.nav.dm_scroll = 0;
        self.refresh_now();
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
        if focus == ModuleId::Conversation {
            self.chat.move_cursor(delta);
            return;
        }
        let height = usize::from(self.body_height(focus));
        // At home the second module is the conversations rather than the
        // channels, so it moves the other cursor: the module is the list it is
        // drawing, whatever its name says.
        let home = self.nav.guild.is_none();
        let len = match focus {
            ModuleId::Servers => self.view.guilds.len(),
            ModuleId::Channels if home => self.view.messages.len(),
            ModuleId::Channels => self.view.channels.len(),
            ModuleId::Members => self.view.members.as_ref().map(Vec::len).unwrap_or(0),
            _ => return,
        };
        if len == 0 {
            return;
        }
        let (cursor, scroll) = match focus {
            ModuleId::Servers => (&mut self.nav.guild_cursor, &mut self.nav.guild_scroll),
            ModuleId::Channels if home => (&mut self.nav.dm_cursor, &mut self.nav.dm_scroll),
            ModuleId::Channels => (&mut self.nav.channel_cursor, &mut self.nav.channel_scroll),
            ModuleId::Members => (&mut self.nav.member_cursor, &mut self.nav.member_scroll),
            _ => return,
        };
        let next = (*cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        *cursor = next;
        *scroll = clamp_scroll(next, *scroll, height);
    }

    fn body_height(&self, id: ModuleId) -> u16 {
        self.layout
            .last
            .as_ref()
            .map(|r| frame::body(r.rect_of(id), &panels::words(id)).height)
            .unwrap_or(0)
    }

    /// Open whatever the cursor is on in the focused module.
    fn activate(&mut self) {
        match self.layout.focus() {
            ModuleId::Servers => {
                let index = self.nav.guild_cursor;
                self.select_guild(index);
                self.focus_module(ModuleId::Channels);
            }
            // At home the second module is the conversations and the friends.
            ModuleId::Channels if self.nav.guild.is_none() => {
                match self.view.messages.get(self.nav.dm_cursor).cloned() {
                    Some(dms::Row::Dm { id, .. }) => self.open_channel(id),
                    Some(dms::Row::Friend { id, .. }) => {
                        // The core refuses a DM with anybody who is not a
                        // friend, and everybody in this part of the list is
                        // one.
                        self.core.send(Command::OpenDm(id));
                        self.note("opening a conversation");
                    }
                    _ => {}
                }
            }
            ModuleId::Channels => match self.view.channels.get(self.nav.channel_cursor).cloned() {
                Some(channels::Row::Channel { id, .. }) => self.open_channel(id),
                Some(channels::Row::Category { id, .. }) => self.toggle_category(id),
                None => {}
            },
            ModuleId::Members => self.note("opening a conversation with a member is not here yet"),
            _ => {}
        }
    }

    fn toggle_category(&mut self, id: ChannelId) {
        if !self.nav.collapsed.remove(&id) {
            self.nav.collapsed.insert(id);
        }
        self.view.stale = true;
    }

    /// Whether a picture can be put on the screen at all.
    ///
    /// Not the same question as [`Graphics::pictures_available`], which asks
    /// what the terminal can do and ignores the setting. `off` means chips and
    /// nothing else; `halfblocks` means pictures on any terminal at all, drawn
    /// two pixels to a cell; `auto` means whatever was detected. The message
    /// list reserves rows on this answer, so it has to be the whole of it.
    pub fn pictures(&self) -> bool {
        match self.look.graphics.mode() {
            Mode::Off => false,
            Mode::Blocks => true,
            _ => self.look.graphics.pictures_available(),
        }
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
        // Every measured message carried the old theme in its cache key, so
        // the measurements go. The built protocols do not: an avatar is the
        // same pixels in every theme, and anything this program rasterises for
        // itself names its colours in its `ImageId`, so a theme change gives it
        // a new identity and the sweep at the end of the next frame drops the
        // old one. Throwing them all away here would re-encode every picture on
        // the screen for a change none of them can see.
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
                Outcome::CancelQr => self.core.send(Command::CancelRemoteAuth),
                Outcome::Submit(text) => match crate::discord::auth::Token::new(&text) {
                    Ok(token) => self.core.send(Command::LoginWithToken(token)),
                    Err(e) => screen.failed(e.to_string()),
                },
                Outcome::OpenUrl(url) => self.core.send(Command::OpenExternal {
                    url,
                    kind: ExternalKind::Link,
                }),
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
            other => {
                self.overlay_asked(other);
                return;
            }
        }

        // Then the composer, which is a text field and takes raw keys.
        if self.layout.focus() == ModuleId::Compose && keymap::composer_eats(k) {
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
                    self.focus_module(ModuleId::Conversation);
                    return;
                }
                composer::Outcome::EditLast => {
                    self.edit_last();
                    return;
                }
                composer::Outcome::Wants(what) => {
                    self.handle(match what {
                        composer::Action::EmojiPicker => Action::EmojiPicker,
                        composer::Action::GifPicker => Action::GifPicker,
                        composer::Action::Attach => Action::Attach,
                        composer::Action::PasteImage => Action::PasteImage,
                    });
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
        if self.layout.focus() == ModuleId::Compose {
            if self.paste_media(text) {
                return;
            }
            let sources = std::mem::take(&mut self.view.sources);
            self.composer.paste(text, &sources);
            self.view.sources = sources;
            self.draft_changed();
        }
    }

    /// A terminal paste of something that was copied but is not text.
    ///
    /// `cmd+v` reaches a terminal as the clipboard's *text* flavour, and a
    /// copied picture or file has one -- on macOS the file's name with the
    /// extension taken off -- which is what used to land in the composer as
    /// words. The clipboard itself says what was really copied, so it is
    /// asked: a list of files is attached, and a picture whose caption is
    /// exactly what arrived is attached as a picture. Anything else is text
    /// and is pasted as text.
    fn paste_media(&mut self, text: &str) -> bool {
        if self.nav.channel.is_none() {
            return false;
        }
        // What a terminal sends for a copied picture is one short line. A
        // paste with a body in it is text, whatever else is on the clipboard.
        if text.contains('\n') || text.chars().count() > 255 {
            return false;
        }

        let files: Vec<std::path::PathBuf> = clipboard::files()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.is_file())
            .collect();
        if !files.is_empty() {
            let limit = self
                .cfg
                .media
                .max_attachment_mib
                .saturating_mul(1024 * 1024);
            for path in files {
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if limit > 0 && bytes > limit {
                    let name = super::overlays::attach::name_of(&path);
                    self.note_at(
                        format!(
                            "{name} is {}, over the limit",
                            super::overlays::attach::human(bytes)
                        ),
                        NoteLevel::Warning,
                    );
                    continue;
                }
                self.attach_file(path);
            }
            return true;
        }

        // A picture, when the paste is the clipboard's own caption for it
        // rather than words typed somewhere: the same clipboard's text.
        let own = match clipboard::text() {
            Ok(caption) => caption.trim() == text.trim(),
            Err(_) => text.trim().is_empty(),
        };
        if own {
            if let Ok((bytes, dims)) = clipboard::image() {
                self.attach_clipboard_image(bytes, dims);
                return true;
            }
        }
        false
    }

    /// One action. The single place a key turns into a change.
    pub fn handle(&mut self, action: Action) {
        match action {
            Action::Quit => self.ask_to_quit(),
            Action::Help => self.over.toggle_help(),
            Action::CloseOverlay => self.over.close(),
            Action::Redraw => {
                self.repaint = true;
                self.chat.cache.clear();
                self.note("redrawn");
            }

            Action::CursorUp => self.move_cursor(-1),
            Action::CursorDown => self.move_cursor(1),
            Action::CursorUpBig => self.move_cursor(-10),
            Action::CursorDownBig => self.move_cursor(10),
            Action::PageUp => self.page(-1),
            Action::PageDown => self.page(1),
            Action::Home => {
                if self.layout.focus() == ModuleId::Conversation {
                    self.chat.to_top();
                } else {
                    self.move_cursor(isize::MIN / 2);
                }
            }
            Action::End | Action::ToBottom => {
                if self.layout.focus() == ModuleId::Conversation || action == Action::ToBottom {
                    self.back_to_the_present();
                } else {
                    self.move_cursor(isize::MAX / 2);
                }
            }
            Action::Activate => self.activate(),
            Action::Back => self.back(),
            Action::HistoryBack => self.history_back(),
            Action::HistoryForward => self.history_forward(),

            Action::FocusNext => self.step_focus(1),
            Action::FocusPrev => self.step_focus(-1),
            Action::FocusServers => self.focus_module(ModuleId::Servers),
            Action::FocusChannels => self.focus_module(ModuleId::Channels),
            Action::FocusConversation => self.focus_module(ModuleId::Conversation),
            Action::FocusCompose => self.focus_module(ModuleId::Compose),
            Action::FocusMembers => self.focus_module(ModuleId::Members),

            Action::NextGuild => self.step_guild(1),
            Action::PrevGuild => self.step_guild(-1),
            Action::ToggleCollapse => {
                if self.layout.focus() == ModuleId::Channels {
                    if let Some(channels::Row::Category { id, .. }) =
                        self.view.channels.get(self.nav.channel_cursor).cloned()
                    {
                        self.toggle_category(id);
                    }
                }
            }

            Action::ToggleMembers => {
                if self.layout.is_expanded(ModuleId::Members) {
                    let land = self.landing();
                    self.layout.collapse(land);
                } else {
                    self.focus_module(ModuleId::Members);
                }
            }
            Action::OpenModuleSettings => {
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
            Action::OpenExternal => self.open_external(),
            Action::OpenMedia => self.open_viewer(),
            // `space` is one key with two jobs, and which one it does is a
            // fact about the message rather than a mode: a message that
            // started a thread opens it, and one that did not uncovers
            // whatever it is hiding.
            Action::RevealSpoiler => {
                if let Some(thread) = self.chat.selected_thread() {
                    self.open_channel(thread);
                } else if !self.chat.reveal() {
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
            // `esc` in the composer is the same key it is everywhere else:
            // cancel what is half-written, and if there is nothing to cancel,
            // walk back up the column. Two different meanings for one key,
            // depending on which module has it, is the thing this avoids.
            Action::CancelCompose => self.back(),
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

            Action::NextUnread => self.hop_unread(true),
            Action::PrevUnread => self.hop_unread(false),
            Action::Search => self.open_search(false),
            Action::SearchGuild => self.open_search(true),
            Action::React => self.open_react(),
            Action::EmojiPicker => self.open_picker(Kind::Emoji),
            Action::GifPicker => self.open_picker(Kind::Gif),
            Action::Attach => self.open_attach(),
            Action::PasteImage => self.paste_image(),

            // The viewer takes its own keys while it is open, so these only
            // arrive when it is not. Delegated rather than ignored so that the
            // key table and what happens stay the same thing.
            Action::MediaNext | Action::MediaPrev | Action::MediaZoom | Action::MediaSave => {
                self.note("nothing open to look at")
            }

            // Pinning needs a route the core does not have: `Command` has no
            // pin, deliberately, and adding one is a milestone of its own.
            Action::TogglePin => self.note("pinning is not here yet"),
        }
    }

    // -- the pickers, the viewer and the search ----------------------------

    /// `ctrl+e` and `ctrl+g`: the emoji grid and the GIF grid.
    fn open_picker(&mut self, kind: Kind) {
        let emoji = core_ext::custom_emoji(&self.core.state());
        let aspect = self.look.graphics.cell_aspect().unwrap_or(2.0);
        self.over
            .open_picker(Picker::new(kind, self.nav.channel, emoji, aspect));
    }

    /// `+`: react to the message under the cursor.
    fn open_react(&mut self) {
        let (Some(channel), Some(msg)) = (self.nav.channel, self.chat.selected()) else {
            self.note("no message chosen");
            return;
        };
        // Which reactions are already this account's, so that choosing one of
        // them takes it off rather than trying to add it twice.
        let mine: Vec<String> = msg
            .reactions
            .iter()
            .filter(|r| r.me)
            .map(|r| r.emoji.reaction_key())
            .collect();
        let emoji = core_ext::custom_emoji(&self.core.state());
        let aspect = self.look.graphics.cell_aspect().unwrap_or(2.0);
        self.over.open_picker(
            Picker::new(Kind::Reaction(msg.id), Some(channel), emoji, aspect).with_mine(mine),
        );
    }

    /// `alt+a`: attach a file by typing where it is.
    fn open_attach(&mut self) {
        if self.nav.channel.is_none() {
            self.note("no channel open");
            return;
        }
        self.over
            .open_attach(Attach::new(self.cfg.media.max_attachment_mib));
    }

    /// `ctrl+v`: the picture on the clipboard becomes a chip.
    fn paste_image(&mut self) {
        if self.nav.channel.is_none() {
            self.note("no channel open");
            return;
        }
        match clipboard::image() {
            Ok((bytes, dims)) => self.attach_clipboard_image(bytes, dims),
            Err(e) => {
                tracing::debug!("no picture on the clipboard: {e}");
                self.note_at("no picture on the clipboard", NoteLevel::Warning);
            }
        }
    }

    /// A picture that came off the clipboard, as a pending attachment.
    fn attach_clipboard_image(&mut self, bytes: Vec<u8>, dims: (u32, u32)) {
        let limit = self
            .cfg
            .media
            .max_attachment_mib
            .saturating_mul(1024 * 1024);
        if limit > 0 && bytes.len() as u64 > limit {
            self.note_at(
                format!(
                    "that picture is {}, over the limit",
                    super::overlays::attach::human(bytes.len() as u64)
                ),
                NoteLevel::Warning,
            );
            return;
        }
        let pending = composer::Pending::clipboard(bytes, dims);
        let name = pending.name.clone();
        if self.composer.attach(pending) {
            self.note(format!("attached {name}"));
            self.focus_module(ModuleId::Compose);
        }
    }

    /// `/` searches the channel, `alt+f` the whole server.
    fn open_search(&mut self, guild_wide: bool) {
        let scope = if guild_wide {
            match self.nav.guild {
                Some(guild) => SearchScope::Guild(guild),
                None => {
                    self.note("direct messages are searched one at a time");
                    return;
                }
            }
        } else {
            match self.nav.channel {
                Some(channel) => SearchScope::Channel(channel),
                None => {
                    self.note("no channel open");
                    return;
                }
            }
        };
        let where_ = core_ext::scope_name(&self.core.state(), scope);
        self.over.open_search(Search::new(scope, where_));
    }

    /// `enter` on a message with something worth looking at.
    fn open_viewer(&mut self) {
        let Some(channel) = self.nav.channel else {
            self.note("no channel open");
            return;
        };
        let items = core_ext::viewer_items(&self.core.state(), channel);
        let on = self.chat.selected().map(|m| m.id);
        let aspect = self.look.graphics.cell_aspect().unwrap_or(2.0);
        match Viewer::new(items, on, aspect) {
            Some(viewer) => self.over.open_viewer(viewer),
            None => self.note("no pictures in this channel"),
        }
    }

    /// `alt+up` and `alt+down`.
    fn hop_unread(&mut self, forward: bool) {
        let stops = core_ext::unread_stops(&self.core.state(), self.nav.guild);
        match unread::hop(&stops, self.nav.channel, forward) {
            Some(channel) => self.open_channel(channel),
            None => self.note("nothing unread here"),
        }
    }

    /// What an overlay asked the application to do.
    fn overlay_asked(&mut self, what: overlays::Key) {
        match what {
            overlays::Key::Insert(text) => {
                self.composer.input.insert_str(&text);
                self.focus_module(ModuleId::Compose);
                self.draft_changed();
            }
            overlays::Key::JumpToMessage { channel, message } => {
                if self.nav.channel != Some(channel) {
                    self.open_channel(channel);
                }
                self.pending_jump = Some((channel, message));
                self.core.send(Command::JumpTo { channel, message });
            }
            overlays::Key::Menu(choice, message) => {
                self.chat.select(message);
                match choice {
                    Choice::Reply => self.handle(Action::Reply),
                    Choice::ReplyNoPing => self.handle(Action::ReplyNoPing),
                    Choice::React => self.handle(Action::React),
                    Choice::Edit => self.handle(Action::Edit),
                    Choice::Delete => self.handle(Action::Delete),
                    Choice::Yank => self.handle(Action::Yank),
                    Choice::Open => self.handle(Action::OpenExternal),
                    Choice::CopyLink => self.handle(Action::CopyMessageLink),
                }
            }
            overlays::Key::Attach(path) => self.attach_file(path),
            overlays::Key::Open(url) => {
                self.core.send(Command::OpenExternal {
                    url,
                    kind: ExternalKind::Image,
                });
                self.note("opening");
            }
            overlays::Key::Copy(text) => self.copy(&text, "link copied"),
            overlays::Key::SaveMedia => self.save_media(),
            _ => {}
        }
    }

    /// A path the attach box accepted.
    fn attach_file(&mut self, path: std::path::PathBuf) {
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        // The size on the chip is what will be uploaded, and the dimensions
        // come from the header alone: reading four hundred bytes to say
        // `640x480` is worth it, and decoding the whole picture to say it is
        // not.
        let dims = image::ImageReader::open(&path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.into_dimensions().ok());
        let pending = composer::Pending::file(path, bytes, dims);
        let name = pending.name.clone();
        if self.composer.attach(pending) {
            self.note(format!("attached {name}"));
            self.focus_module(ModuleId::Compose);
        } else {
            self.note(format!("{name} is already attached"));
        }
    }

    /// `s` in the media viewer: write the file to `[media] save_dir`.
    ///
    /// What the store holds has been resized to fit a rectangle, so the bytes
    /// are asked for again when that is all there is. The save then happens on
    /// the frame they arrive, which is why this is two paths rather than one.
    fn save_media(&mut self) {
        let Some(viewer) = &self.over.viewer else {
            return;
        };
        let item = viewer.current().clone();
        match self.chat.media.decoded(&item.key) {
            Some(decoded) => match &*decoded {
                crate::discord::media::Decoded::Bytes(bytes) => {
                    let bytes = bytes.clone();
                    self.write_saved(&item.filename, &bytes);
                }
                _ => {
                    self.chat.media.want_bytes(&item.key);
                    self.note("fetching the whole picture\u{2026}");
                    self.pending_save = Some(item);
                }
            },
            None => {
                self.chat.media.want_bytes(&item.key);
                self.note("fetching the whole picture\u{2026}");
                self.pending_save = Some(item);
            }
        }
    }

    fn write_saved(&mut self, filename: &str, bytes: &[u8]) {
        let dir = self.cfg.save_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.note_at(
                format!("could not make {}: {e}", dir.display()),
                NoteLevel::Error,
            );
            return;
        }
        // Never over something already there: a second `cat.png` is
        // `cat-1.png`, because a save that silently replaced a file would be
        // the one destructive thing in the program.
        let path = super::overlays::media::free_name(&dir, filename);
        match std::fs::write(&path, bytes) {
            Ok(()) => self.note(format!("saved {}", path.display())),
            Err(e) => self.note_at(format!("could not save: {e}"), NoteLevel::Error),
        }
    }

    /// `G`: the end of the conversation, and the present if the view has been
    /// carried somewhere else.
    ///
    /// A jump leaves the store holding a page from the middle of a channel,
    /// and the bottom of *that* is not the newest message. Re-opening the
    /// channel is what asks for the latest page again, which is the way back
    /// from a search result.
    fn back_to_the_present(&mut self) {
        self.chat.to_bottom();
        let Some(channel) = self.nav.channel else {
            return;
        };
        let at_latest = self
            .core
            .state()
            .messages(channel)
            .map(|s| s.at_latest())
            .unwrap_or(true);
        if !at_latest {
            self.pending_jump = None;
            self.core.send(Command::OpenChannel(channel));
            self.note("back to the newest");
        }
    }

    /// Whether the open channel's window still ends at the newest message.
    /// For the test next door, which is about exactly that.
    #[cfg(test)]
    pub fn core_state_at_latest(&self) -> Option<bool> {
        let channel = self.nav.channel?;
        self.core.state().messages(channel).map(|s| s.at_latest())
    }

    fn page(&mut self, direction: isize) {
        if self.layout.focus() == ModuleId::Conversation {
            let h = i32::from(self.body_height(ModuleId::Conversation)).max(1);
            self.chat.scroll(h * direction as i32);
            return;
        }
        let h = self.body_height(self.layout.focus()) as isize;
        self.move_cursor(h.max(1) * direction);
    }

    /// `esc`: close, cancel, mark read, then walk back up the column.
    ///
    /// Up rather than out: the modules are the sequence of questions that led
    /// here, so going back is opening the one before this. From a conversation
    /// that is the channel list, from the channel list the server list, and
    /// from the server list there is nowhere further up -- so it folds, and
    /// the keyboard lands back where the writing happens.
    fn back(&mut self) {
        if self.over.open() {
            self.over.close();
            return;
        }
        if self.layout.focus() == ModuleId::Compose && self.composer.cancel() {
            return;
        }
        self.mark_read();
        match self.layout.expanded() {
            None => self.focus_module(ModuleId::Channels),
            Some(ModuleId::Channels) => self.focus_module(ModuleId::Servers),
            _ => {
                let land = self.landing();
                self.layout.collapse(land);
            }
        }
    }

    fn ask_to_quit(&mut self) {
        // A file that is halfway up the wire is not a draft: leaving now loses
        // the upload and leaves a message nobody receives, so it is asked
        // about first and in its own words.
        if self.uploading() > 0 {
            self.over
                .ask(Confirm::quit_while_uploading(self.uploading()));
            return;
        }
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

    /// How many messages are still handing their files over.
    fn uploading(&self) -> usize {
        use crate::discord::state::messages::PendingState;
        let state = self.core.state();
        let mut channels: Vec<ChannelId> = state
            .guilds_ordered()
            .iter()
            .flat_map(|g| state.channels_ordered(g.id))
            .chain(state.dms_ordered())
            .map(|c| c.id)
            .collect();
        // And the one that is open, which is where an upload started and which
        // is not always in a list: a thread the channel list has not been told
        // about is still somewhere a file can be sent.
        if let Some(open) = self.nav.channel {
            if !channels.contains(&open) {
                channels.push(open);
            }
        }
        channels
            .into_iter()
            .filter_map(|id| state.messages(id))
            .flat_map(|store| store.pending())
            .filter(|p| matches!(p.state, PendingState::Uploading { .. }))
            .count()
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
                self.focus_module(ModuleId::Channels);
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
            Setting::ShowVoice => {
                self.cfg.channels.show_voice = !self.cfg.channels.show_voice;
                self.view.stale = true;
            }
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
            Setting::Avatars | Setting::ShowVoice => {
                value.replace("on", "true").replace("off", "false")
            }
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
        self.focus_module(ModuleId::Compose);
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
        self.focus_module(ModuleId::Compose);
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
                self.focus_module(ModuleId::Compose);
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
        if !self.composer.has_something_to_send() {
            return;
        }
        let attachments = self.composer.take_attachments();
        let content = self.composer.take();
        self.core.send(Command::SendMessage {
            channel,
            content,
            reply_to,
            mention_author,
            attachments,
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
        self.focus_module(ModuleId::Conversation);
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
    /// Failure is a note rather than an error: a terminal with no clipboard at
    /// the other end -- over ssh, in a bare tty -- is a perfectly ordinary
    /// place to be running a chat client.
    fn copy(&mut self, text: &str, done: &str) {
        match clipboard::copy(text) {
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
                MouseEventKind::Down(MouseButton::Left) => {
                    match self.over.click(full, m.column, m.row) {
                        overlays::Key::Quit => self.quit = true,
                        overlays::Key::Confirmed(pending) => self.confirmed(pending),
                        overlays::Key::Jump(target) => self.jump(target),
                        overlays::Key::Setting(setting, forward) => {
                            self.change_setting(setting, forward)
                        }
                        overlays::Key::Taken | overlays::Key::Ignored => {}
                        other => self.overlay_asked(other),
                    }
                }
                _ => {}
            }
            return;
        }
        let Some(regions) = self.layout.last.clone() else {
            return;
        };
        let (x, y) = (m.column, m.row);

        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(what) = self.status_hit(&regions, x, y) {
                    self.status_click(what);
                    return;
                }
                let double = self.clicks.click(x, y);
                if let Some(module) = regions.hit(x, y) {
                    self.module_click(&regions, module, x, y, double);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if regions.hit(x, y) == Some(ModuleId::Conversation) {
                    self.open_menu(x, y);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(x, y),
            MouseEventKind::Up(MouseButton::Left) => self.layout.drag = None,
            MouseEventKind::ScrollDown => self.scroll_module(&regions, x, y, 3),
            MouseEventKind::ScrollUp => self.scroll_module(&regions, x, y, -3),
            _ => {}
        }
    }

    fn drag_to(&mut self, _x: u16, y: u16) {
        if self.layout.drag == Some(Drag::Scrollbar) {
            self.drag_scrollbar(y);
        }
    }

    /// The pointer on the scrollbar's track: that fraction of the way down.
    fn drag_scrollbar(&mut self, y: u16) {
        let Some(track) = self.chat.scrollbar_track() else {
            return;
        };
        if track.height == 0 {
            return;
        }
        let within = y.clamp(track.y, track.y + track.height - 1) - track.y;
        let fraction = f32::from(within) / f32::from(track.height.saturating_sub(1).max(1));
        self.chat.scroll_to_fraction(fraction);
    }

    /// Right-click: what can be done to the message under the pointer.
    fn open_menu(&mut self, x: u16, y: u16) {
        let Some(Hit::Message(id)) = self
            .chat
            .hit(x, y)
            .or_else(|| self.chat.selected().map(|m| Hit::Message(m.id)))
        else {
            return;
        };
        self.chat.select(id);
        let mine = self.chat.selected().is_some_and(|m| self.is_mine(&m));
        self.over.open_menu(Menu::new(id, mine));
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
            status::Hit::NewBelow => self.chat.to_bottom(),
        }
    }

    fn module_click(&mut self, regions: &Regions, module: ModuleId, x: u16, y: u16, double: bool) {
        let rect = regions.rect_of(module);
        // A header word first: it is drawn over the module's own first row, and
        // the hit box comes from the same function the renderer used.
        let words = panels::words(module);
        if let Some(word) = starkit::chrome::header::hit(rect, &words, x, y) {
            self.word_click(module, word);
            return;
        }
        let body = frame::body(rect, &words);

        // A folded list is one row that says where you are. Clicking anywhere
        // in it means "show me the rest", which is the same thing focusing it
        // does, so it is the same call.
        if module.is_list() && !self.layout.is_expanded(module) {
            self.focus_module(module);
            return;
        }
        self.focus_module(module);

        match module {
            ModuleId::Servers => {
                let v = self.guilds_view(false);
                if let Some(index) = guilds::row_at(body, &v, y) {
                    self.select_guild(index);
                    self.focus_module(ModuleId::Channels);
                }
            }
            ModuleId::Channels if self.nav.guild.is_none() => {
                let v = self.dms_view(false);
                if let Some(index) = dms::row_at(body, &v, y) {
                    if self
                        .view
                        .messages
                        .get(index)
                        .is_some_and(dms::Row::selectable)
                    {
                        self.nav.dm_cursor = index;
                        if double {
                            self.activate();
                        }
                    }
                }
            }
            ModuleId::Channels => {
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
            ModuleId::Members => {
                let v = self.members_view(false);
                if let Some(index) = members::row_at(body, &v, y) {
                    if v.rows.get(index).is_some_and(members::Row::selectable) {
                        self.nav.member_cursor = index;
                    }
                }
            }
            ModuleId::Conversation => self.chat_click(x, y, double),
            ModuleId::Compose => {
                if let Some(index) = self.composer.chip_at(x, y) {
                    self.composer.drop_attachment(index);
                }
            }
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
                // The chip says whether this account is already on it, and a
                // click on one is a toggle: adding a reaction that is already
                // there is a request Discord answers with nothing at all, so
                // the chip would have looked stuck.
                let mine = self
                    .chat
                    .selected_reaction_is_mine(message, &emoji)
                    .unwrap_or(false);
                self.core.send(if mine {
                    Command::RemoveReaction {
                        channel,
                        message,
                        emoji,
                    }
                } else {
                    Command::AddReaction {
                        channel,
                        message,
                        emoji,
                    }
                });
            }
            Hit::Attachment(id, url, kind) => {
                self.chat.select(id);
                // A photograph opens on the click that lands on it, in the
                // viewer the desktop opens pictures with. A file or a video is
                // chosen on one click and opened on two, as before.
                if kind == ExternalKind::Image || double {
                    self.core.send(Command::OpenExternal { url, kind });
                    self.note("opening");
                }
            }
            Hit::LoadOlder => {
                if let Some(channel) = self.nav.channel {
                    self.core.send(Command::LoadOlder(channel));
                }
            }
            Hit::Scrollbar => {
                self.layout.drag = Some(Drag::Scrollbar);
                self.drag_scrollbar(y);
            }
        }
    }

    fn word_click(&mut self, module: ModuleId, word: panels::Word) {
        match word {
            panels::Word::Back => self.handle(Action::HistoryBack),
            panels::Word::Forward => self.handle(Action::HistoryForward),
            panels::Word::Settings => self.over.open_settings(module),
            panels::Word::Search => self.handle(Action::Search),
            panels::Word::Pins => self.handle(Action::TogglePin),
            panels::Word::Attach => self.handle(Action::Attach),
            panels::Word::Emoji => self.handle(Action::EmojiPicker),
            panels::Word::Gif => self.handle(Action::GifPicker),
        }
    }

    fn scroll_module(&mut self, regions: &Regions, x: u16, y: u16, delta: isize) {
        let Some(module) = regions.hit(x, y) else {
            return;
        };
        match module {
            ModuleId::Conversation => self.chat.scroll(delta as i32),
            ModuleId::Compose => {}
            // A folded server list steps the server: there is one row to
            // scroll and changing server is what somebody with a pointer over
            // it wants. Every other folded module ignores the wheel rather
            // than changing something nobody can see.
            m if !self.layout.is_expanded(m) => {
                if m == ModuleId::Servers {
                    self.step_guild(delta.signum());
                }
            }
            m => {
                let was = self.layout.focus();
                self.layout.focus_set(m);
                self.move_cursor(delta);
                self.layout.focus_set(was);
            }
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
        if self.layout.focus() != ModuleId::Compose {
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
            pictures: self.pictures(),
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

    fn dms_view(&self, focused: bool) -> dms::View<'_> {
        dms::View {
            theme: &self.look.theme,
            rows: &self.view.messages,
            cursor: self.nav.dm_cursor,
            scroll: self.nav.dm_scroll,
            focused,
            open: self.nav.channel,
        }
    }

    // -- the column --------------------------------------------------------

    /// Focus a module, and put its cursor on whatever is current in it.
    ///
    /// Sticky rather than reset: a cursor that has been moved and not acted on
    /// stays where it was left, and one that has nothing to point at -- a
    /// channel list for a server whose channel is not open -- stays where it
    /// was too. What this prevents is the other thing: coming back to a list
    /// and finding the cursor on a row nobody chose.
    fn focus_module(&mut self, m: ModuleId) {
        match m {
            ModuleId::Servers => {
                if let Some(index) = self.view.guilds.iter().position(|r| r.id == self.nav.guild) {
                    self.nav.guild_cursor = index;
                }
            }
            ModuleId::Channels => {
                if let Some(channel) = self.nav.channel {
                    if self.nav.guild.is_some() {
                        if let Some(index) = self
                            .view
                            .channels
                            .iter()
                            .position(|r| r.channel_id() == Some(channel))
                        {
                            self.nav.channel_cursor = index;
                        }
                    } else if let Some(index) = self
                        .view
                        .messages
                        .iter()
                        .position(|r| matches!(r, dms::Row::Dm { id, .. } if *id == channel))
                    {
                        self.nav.dm_cursor = index;
                    }
                }
            }
            _ => {}
        }
        self.layout.focus_set(m);
    }

    /// Where focus lands when the lists fold: the composer if there is
    /// somewhere to write, and the conversation if there is not.
    fn landing(&self) -> ModuleId {
        if self.nav.channel.is_some() {
            ModuleId::Compose
        } else {
            ModuleId::Conversation
        }
    }

    /// Whether the reader is looking at the conversation rather than choosing
    /// in a list. The composer counts: a channel opened straight into it is
    /// one somebody is reading.
    fn reading(&self) -> bool {
        matches!(
            self.layout.focus(),
            ModuleId::Conversation | ModuleId::Compose
        )
    }

    /// `tab` and `shift+tab`, over the column and round.
    fn step_focus(&mut self, delta: isize) {
        let at = self.layout.focus().index() as isize;
        let next = (at + delta).rem_euclid(COLUMN.len() as isize) as usize;
        self.focus_module(COLUMN[next]);
    }

    /// Body rows the open list would like, which is how many rows it has.
    fn wanted_rows(&self) -> u16 {
        let rows = match self.layout.expanded() {
            Some(ModuleId::Servers) => {
                let step = u32::from(guilds::row_rows(self.pictures()));
                return (self.view.guilds.len() as u32 * step).min(u32::from(u16::MAX)) as u16;
            }
            Some(ModuleId::Channels) if self.nav.guild.is_none() => self.view.messages.len(),
            Some(ModuleId::Channels) => self.view.channels.len(),
            Some(ModuleId::Members) => self.view.members.as_ref().map(Vec::len).unwrap_or(0),
            _ => 0,
        };
        rows.min(usize::from(u16::MAX)) as u16
    }

    /// The one line a folded list draws, and how to draw it.
    ///
    /// What is currently chosen rather than where the cursor is: a folded
    /// module is an answer to "where am I", and the cursor is a question
    /// somebody stopped asking when they folded it.
    fn summary(&self, m: ModuleId) -> (String, Style) {
        let t = &self.look.theme;
        let dim = Style::default().fg(rgb(t.empty_fg));
        match m {
            ModuleId::Servers => match self.view.guilds.iter().find(|r| r.id == self.nav.guild) {
                Some(row) => {
                    let fg = if row.unavailable {
                        t.dim
                    } else if row.mentions > 0 {
                        t.chat.mention_fg
                    } else if row.unread {
                        t.chat.unread_fg
                    } else {
                        t.row_fg
                    };
                    let mut style = Style::default().fg(rgb(fg));
                    if row.unread && !row.unavailable {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    (guilds::summary(row), style)
                }
                None => ("no servers".into(), dim),
            },
            ModuleId::Channels if self.nav.guild.is_none() => {
                let row = self.nav.channel.and_then(|channel| {
                    self.view
                        .messages
                        .iter()
                        .find(|r| matches!(r, dms::Row::Dm { id, .. } if *id == channel))
                });
                match row {
                    Some(row) => {
                        let (unread, mentions, muted) = match row {
                            dms::Row::Dm {
                                unread,
                                mentions,
                                muted,
                                ..
                            } => (*unread, *mentions, *muted),
                            _ => (false, 0, false),
                        };
                        (dms::summary(row), self.row_style(unread, mentions, muted))
                    }
                    None => ("choose a conversation".into(), dim),
                }
            }
            ModuleId::Channels => {
                let row = self.nav.channel.and_then(|channel| {
                    self.view
                        .channels
                        .iter()
                        .find(|r| r.channel_id() == Some(channel))
                });
                match row {
                    Some(row) => {
                        let (unread, mentions, muted) = match row {
                            channels::Row::Channel {
                                unread,
                                mentions,
                                muted,
                                ..
                            } => (*unread, *mentions, *muted),
                            _ => (false, 0, false),
                        };
                        (
                            channels::summary(row),
                            self.row_style(unread, mentions, muted),
                        )
                    }
                    None => ("choose a channel".into(), dim),
                }
            }
            ModuleId::Members => match &self.view.members {
                Some(rows) => (
                    members::summary(rows),
                    Style::default().fg(rgb(t.row_meta_fg)),
                ),
                None => ("not loaded".into(), dim),
            },
            _ => (String::new(), dim),
        }
    }

    /// The colour a summary takes from the row it stands for: the open thing
    /// is the open thing, whether it is a row or a folded module's only line.
    fn row_style(&self, unread: bool, mentions: u32, muted: bool) -> Style {
        let t = &self.look.theme;
        let fg = if mentions > 0 {
            t.chat.mention_fg
        } else if muted {
            t.dim
        } else if unread {
            t.chat.unread_fg
        } else {
            t.row_playing_fg
        };
        let mut style = Style::default().fg(rgb(fg));
        if unread && !muted {
            style = style.add_modifier(Modifier::BOLD);
        }
        style
    }

    pub fn draw(&mut self, area: Rect, buf: &mut Buffer) {
        self.caret = None;
        let bg = Style::default()
            .bg(rgb(self.look.theme.bg))
            .fg(rgb(self.look.theme.fg));
        buf.set_style(area, bg);

        if let Some(screen) = &self.login {
            screen.render(area, buf, &self.look.theme, &mut self.look.graphics);
            return;
        }

        let mut drawn: std::collections::HashSet<starkit::graphics::ImageId> =
            std::collections::HashSet::new();
        // The composer says how tall it wants to be before anything has been
        // laid out, so it is measured against the width every module has
        // rather than against a width from the last frame.
        let composer_rows = self.composer.rows(
            &self.cfg.compose,
            layout::content_width(area, self.look.padding),
        );
        let wanted = self.wanted_rows();
        let Some(regions) = self
            .layout
            .regions(area, composer_rows, self.look.padding, wanted)
            .cloned()
        else {
            too_small(area, buf, &self.look.theme);
            return;
        };

        let focus = self.layout.focus();
        // Everything asked for after this counts as on screen; what is not
        // asked for again before `end_frame` has scrolled away.
        self.chat.media.begin_frame();
        let mut icons: Vec<(guilds::Icon, Rect)> = Vec::new();
        for module in COLUMN {
            let rect = regions.rect_of(module);
            let focused = module == focus;
            let (name, detail) = self.module_title(module);
            let words = panels::words(module);
            // The top of the column says what the window is, as the top of
            // STAR/AMP's does: its own name moves to a badge on the right of
            // the same border, where the heading otherwise sits.
            let heading = module == ModuleId::Servers;
            // The core theme type -- a struct literal is not a coercion site,
            // so the deref from this crate's own `Theme` is spelled out here.
            let core: &starkit::theme::Theme = &self.look.theme;
            let body = frame::frame(
                rect,
                buf,
                &frame::Frame {
                    theme: core,
                    focused,
                    title: if heading { panels::HEADING } else { &name },
                    detail: detail.as_deref(),
                    heading,
                    badge: heading.then(|| Badge {
                        text: &name,
                        tone: Tone::Dim,
                    }),
                    footer: None,
                    words: &words,
                },
            );
            // A folded list is its summary and nothing else.
            if module.is_list() && !self.layout.is_expanded(module) {
                let (text, style) = self.summary(module);
                panels::summary_row(body, buf, &text, style);
                continue;
            }
            match module {
                ModuleId::Servers => {
                    let placed = guilds::render(body, buf, &self.guilds_view(focused));
                    icons.extend(placed.into_iter().map(|icon| (icon, body)));
                }
                ModuleId::Channels if self.nav.guild.is_none() => {
                    dms::render(body, buf, &self.dms_view(focused))
                }
                ModuleId::Channels => channels::render(body, buf, &self.channels_view(focused)),
                ModuleId::Members => members::render(body, buf, &self.members_view(focused)),
                ModuleId::Conversation => {
                    let params = chat::Params {
                        theme: &self.look.theme,
                        cfg: &self.cfg,
                        focused,
                        pictures: self.pictures(),
                        aspect: self.look.graphics.cell_aspect().unwrap_or(2.0),
                        me: self.core.state().me().map(|u| u.id),
                        tz: self.tz.clone(),
                    };
                    self.chat.render(rect, body, buf, &params);
                }
                ModuleId::Compose => {
                    let view = super::panels::composer::View {
                        theme: &self.look.theme,
                        focused,
                        channel: &self.view.location,
                    };
                    self.caret = self.composer.render(body, buf, &view);
                }
            }
        }

        // The pictures, all of them, in one pass over what every module placed.
        // After the text and before the overlays: an overlay clears the cells
        // it covers, and a protocol image whose cell has been cleared is one
        // the terminal is never told about.
        drawn.extend(self.paint_pictures(icons, buf));

        // The message list asks for older history once it is looking at the
        // top of what it has, which is only knowable after it has been laid
        // out.
        if self.chat.wants_older() {
            if let Some(channel) = self.nav.channel {
                self.core.send(Command::LoadOlder(channel));
            }
        }

        status::render(regions.status, buf, &self.status_view());
        // The overlay's own pictures go after its chrome, and in a second
        // pass: a `Clear` wipes the cells a protocol image lives in, so the
        // panels' pictures have to be down before this and the overlay's after
        // it.
        let over = self
            .over
            .render(area, buf, &self.look.theme, &self.cfg, &self.chat.media);
        self.paint_overlay(over, buf, &mut drawn);
        self.look.graphics.forget_unused(&drawn);
        self.finish_pictures();
        self.draw_caret(area, buf);
    }

    /// Draw every picture the frame placed, and tell the core what is missing.
    ///
    /// One pass for the whole screen, because the set of what is on it is what
    /// decides which built protocols are still worth the terminal's memory.
    fn paint_pictures(
        &mut self,
        icons: Vec<(guilds::Icon, Rect)>,
        buf: &mut Buffer,
    ) -> std::collections::HashSet<starkit::graphics::ImageId> {
        let mut places = self.chat.take_slots();
        places.extend(
            icons
                .into_iter()
                .map(|(icon, clip)| chat::media::Placement {
                    rect: icon.rect,
                    clip,
                    clipped: false,
                    key: icon.key,
                    shape: chat::media::Shape::Icon {
                        initials: icon.initials,
                        colour: self.look.theme.row_fg,
                    },
                    alt: String::new(),
                }),
        );

        let painted = chat::media::paint(
            &places,
            &mut self.look.graphics,
            &mut self.chat.media,
            &self.chat.anim,
            &self.look.theme,
            buf,
        );
        // What moved this frame is what may move next frame: the clock is fed
        // from the drawing pass and from nothing else, which is what keeps an
        // animation that has scrolled away from costing anything.
        self.chat.anim.set_visible(painted.animated);
        painted.drawn
    }

    /// The open overlay's pictures, drawn over its own chrome.
    fn paint_overlay(
        &mut self,
        places: Vec<chat::media::Placement>,
        buf: &mut Buffer,
        drawn: &mut std::collections::HashSet<starkit::graphics::ImageId>,
    ) {
        if places.is_empty() {
            return;
        }
        // An overlay's picture is far larger than the slot the message list
        // fetched it for, so it is asked for again at the size it is about to
        // be drawn at. Bounded: the size only ever grows.
        for place in &places {
            self.chat
                .media
                .want_bigger(&place.key, place.rect.width, place.rect.height);
        }
        let painted = chat::media::paint(
            &places,
            &mut self.look.graphics,
            &mut self.chat.media,
            &self.chat.anim,
            &self.look.theme,
            buf,
        );
        drawn.extend(painted.drawn);
        // An overlay's animation is the only one that matters while it is up:
        // everything behind it is covered.
        let mut moving = painted.animated;
        moving.extend(self.chat.anim.visible().iter().cloned());
        self.chat.anim.set_visible(moving);
    }

    /// Whatever is not on the screen is not worth the terminal's memory, and
    /// whatever is missing is worth asking for. Both are answers only the end
    /// of the frame has.
    fn finish_pictures(&mut self) {
        self.chat.media.end_frame();
        for request in self.chat.media.take_requests() {
            self.core.send(Command::FetchMedia(request));
        }
        for key in self.chat.media.take_cancels() {
            self.core.send(Command::CancelMedia(key));
        }
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

    /// What a module's border says: its name, and what the name is about.
    ///
    /// The second module names the server it is listing, because the folded
    /// row above it is one line and `channels` on its own is not an answer to
    /// "which server's".
    fn module_title(&self, module: ModuleId) -> (String, Option<String>) {
        match module {
            ModuleId::Channels if self.nav.guild.is_none() => ("messages".into(), None),
            ModuleId::Channels => match self.nav.guild {
                Some(g) => match self.core.state().guild(g) {
                    Some(guild) => (module.title().to_string(), Some(guild.name.clone())),
                    None => (module.title().to_string(), None),
                },
                None => (module.title().to_string(), None),
            },
            ModuleId::Conversation => {
                let location = chat::title(&self.view.location);
                (module.title().to_string(), Some(location))
            }
            _ => (module.title().to_string(), None),
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
#[path = "app_tests.rs"]
mod tests;
