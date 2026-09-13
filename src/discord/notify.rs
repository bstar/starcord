//! Deciding whether something deserves to interrupt somebody.
//!
//! The decision is [`decide`], and it is separated from the delivery on purpose:
//! deciding is pure arithmetic over the state and the configuration, so the
//! whole of it can be asserted in a table, while delivery is a synchronous call
//! into another process over a bus.
//!
//! Five suppressions, each of which exists because the alternative is worse
//! than no notifications at all:
//!
//! - **A muted channel is muted.** Muting is the one instruction a person gives
//!   a chat client about what may interrupt them, and a client that pops up
//!   anyway will be uninstalled rather than reconfigured.
//! - **Nothing this account wrote.** Sending a message from a phone must not
//!   ring the desktop.
//! - **Nothing that is not addressed to the reader.** `State::mentions_me` is
//!   the whole rule and it lives there rather than here, because the bell, the
//!   badge and the popup have to agree about what a mention is.
//! - **Not the channel that is already on screen**, when the terminal has
//!   focus. Telling somebody about the message they are watching arrive is
//!   noise. `only_when_unfocused` turns this off for people who want the
//!   history in their notification centre.
//! - **Not twice in two seconds for one channel.** A conversation that is going
//!   quickly becomes one notification that counts, rather than eleven.
//!
//! The body is the message with its markup removed and its spoilers left
//! hidden: a spoiler is the one piece of a message somebody deliberately
//! concealed, and a popup that reveals it has defeated the point of writing it
//! that way.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::discord::markdown;
use crate::discord::model::Message;
use crate::discord::snowflake::ChannelId;
use crate::discord::state::State;

/// How long a channel stays collapsed after one notification.
pub const COLLAPSE: Duration = Duration::from_secs(2);

/// The longest body a notification carries.
///
/// Two hundred characters. Every desktop notification daemon truncates
/// somewhere and none of them says where, so it is done here, visibly, with an
/// ellipsis that says something was cut.
pub const MAX_BODY: usize = 200;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyConfig {
    pub enabled: bool,
    /// Say nothing about the channel already on screen while the terminal has
    /// focus.
    pub only_when_unfocused: bool,
    /// Only DMs, never a server mention.
    pub dms_only: bool,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            only_when_unfocused: true,
            dms_only: false,
        }
    }
}

/// Where the reader is looking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Focus {
    pub channel: Option<ChannelId>,
    pub terminal_focused: bool,
}

/// What to put on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub summary: String,
    pub body: String,
}

/// What has been said about each channel lately, for the collapse rule.
#[derive(Debug, Default)]
pub struct Recent {
    channels: HashMap<ChannelId, (Instant, u32)>,
}

impl Recent {
    /// Count this notification and say how many are in the current run.
    ///
    /// One means it stands on its own; more means the run is still going and
    /// the notification is the collapsed one.
    fn count(&mut self, channel: ChannelId, now: Instant) -> u32 {
        let entry = self.channels.entry(channel).or_insert((now, 0));
        if now.duration_since(entry.0) > COLLAPSE {
            *entry = (now, 1);
        } else {
            entry.0 = now;
            entry.1 = entry.1.saturating_add(1);
        }
        entry.1
    }

    pub fn forget(&mut self, channel: ChannelId) {
        self.channels.remove(&channel);
    }
}

/// Whether this message should interrupt somebody, and with what.
///
/// Pure: everything it reads is an argument, and `now` is one of them, so the
/// whole table below runs without a clock.
pub fn decide(
    state: &State,
    message: &Message,
    focus: Focus,
    config: &NotifyConfig,
    recent: &mut Recent,
    now: Instant,
) -> Option<Notification> {
    if !config.enabled {
        return None;
    }

    // The one rule about what counts as addressed to the reader, so the bell,
    // the badge and the popup cannot disagree. It also covers this account's
    // own messages and the flag an author sets to post without pinging.
    if !state.mentions_me(message) {
        return None;
    }

    let channel = message.channel_id;
    let is_dm = state.channel(channel).is_some_and(|c| c.kind.is_private());

    if config.dms_only && !is_dm {
        return None;
    }
    if state.unread(channel).muted {
        return None;
    }
    if config.only_when_unfocused && focus.terminal_focused && focus.channel == Some(channel) {
        return None;
    }

    let where_ = place(state, channel, is_dm);
    let run = recent.count(channel, now);
    if run > 1 {
        // The conversation is going quickly. One notification that counts beats
        // one per message.
        return Some(Notification {
            summary: format!("{run} new mentions in {where_}"),
            body: String::new(),
        });
    }

    let guild = message
        .guild_id
        .or_else(|| state.channel(channel).and_then(|c| c.guild_id));
    let author = state.display_name(guild, message.author.id);
    let summary = if is_dm {
        author
    } else {
        format!("{author} in {where_}")
    };

    Some(Notification {
        summary,
        body: body_of(message),
    })
}

/// What to call the channel a message arrived in.
fn place(state: &State, channel: ChannelId, is_dm: bool) -> String {
    let Some(held) = state.channel(channel) else {
        return channel.to_string();
    };
    match held.name() {
        Some(name) if is_dm => name.to_string(),
        Some(name) => format!("#{name}"),
        None if is_dm => state.dm_title(channel),
        None => channel.to_string(),
    }
}

/// The message, with its markup gone, its spoilers still hidden, and a length.
fn body_of(message: &Message) -> String {
    let mut text = markdown::plain_text_hiding_spoilers(&message.content);

    // A message with nothing but a picture in it still deserves a body saying
    // so; an empty popup looks like a broken one.
    if text.trim().is_empty() {
        if let Some(attachment) = message.attachments.first() {
            text = format!("[{}]", attachment.filename);
        } else if !message.embeds.is_empty() {
            text = "[a link]".to_string();
        } else if !message.sticker_items.is_empty() {
            text = "[a sticker]".to_string();
        }
    }

    truncate(&text, MAX_BODY)
}

/// Cut to a character count, with an ellipsis that says something was cut.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The decision and the delivery together, with the state the decision needs.
///
/// Cheap to clone. The core holds one, the gateway's bridge holds a clone, and
/// `Command::SetFocus` moves the focus in it.
#[derive(Clone)]
pub struct Notifier {
    config: Arc<NotifyConfig>,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    focus: Focus,
    recent: Recent,
    /// Whether the failure to reach a notification daemon has been logged. It
    /// is logged once: a machine with no daemon has no daemon for every message
    /// that arrives, and a warning per message is worse than none.
    complained: bool,
}

impl Notifier {
    pub fn new(config: NotifyConfig) -> Self {
        Self {
            config: Arc::new(config),
            inner: Arc::new(Mutex::new(Inner {
                focus: Focus {
                    channel: None,
                    terminal_focused: true,
                },
                ..Default::default()
            })),
        }
    }

    pub fn config(&self) -> &NotifyConfig {
        &self.config
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_focus(&self, channel: Option<ChannelId>, terminal_focused: bool) {
        let mut inner = self.inner();
        inner.focus = Focus {
            channel,
            terminal_focused,
        };
        // Looking at a channel ends whatever run of notifications it was in:
        // the next one after that is news again.
        if let Some(channel) = channel {
            inner.recent.forget(channel);
        }
    }

    /// Decide about a message that `apply` has already said is a mention.
    ///
    /// The caller holds the read lock; nothing here takes one.
    pub fn consider(&self, state: &State, message: &Message) -> Option<Notification> {
        let mut inner = self.inner();
        let focus = inner.focus;
        decide(
            state,
            message,
            focus,
            &self.config,
            &mut inner.recent,
            Instant::now(),
        )
    }

    /// Put it on screen.
    ///
    /// On a blocking thread: `notify-rust` is synchronous and talks to another
    /// process over a bus, and a runtime worker parked on that is a gateway not
    /// reading its socket. Nothing waits for the result — a notification that
    /// fails to appear must not be able to delay a message that has.
    pub fn deliver(&self, notification: Notification) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // No runtime: a test, or the decision half being used on its own.
            return;
        };
        let inner = Arc::clone(&self.inner);
        runtime.spawn_blocking(move || {
            let sent = notify_rust::Notification::new()
                .appname("STAR/CORD")
                .summary(&notification.summary)
                .body(&notification.body)
                .show();

            if let Err(e) = sent {
                let mut inner = inner.lock().unwrap_or_else(|e| e.into_inner());
                if !inner.complained {
                    inner.complained = true;
                    // Once. A machine with no notification daemon has no daemon
                    // for every message that ever arrives.
                    tracing::warn!("desktop notifications are not working: {e}");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state with this account, a friend, a guild channel and a DM.
    fn state() -> State {
        let mut state = State::new();
        state.set_me(serde_json::from_str(r#"{"id":"1","username":"sam"}"#).unwrap());
        state.upsert_user(
            serde_json::from_str(r#"{"id":"2","username":"alex","global_name":"Alex"}"#).unwrap(),
        );
        state.upsert_guild(
            serde_json::from_str(
                r#"{"id":"9","name":"A Server","channels":[
                     {"id":"7","type":0,"name":"general","position":0}]}"#,
            )
            .unwrap(),
        );
        state.upsert_channel(
            serde_json::from_str(r#"{"id":"400","type":1,"recipient_ids":["1","2"]}"#).unwrap(),
        );
        state
    }

    fn message(channel: u64, content: &str, extra: &str) -> Message {
        serde_json::from_str(&format!(
            r#"{{"id":"500","channel_id":"{channel}","content":{},
                 "author":{{"id":"2","username":"alex","global_name":"Alex"}}{extra}}}"#,
            serde_json::to_string(content).unwrap()
        ))
        .unwrap()
    }

    /// A message in #general that names this account.
    fn mention() -> Message {
        message(
            7,
            "are you about?",
            r#","guild_id":"9","mentions":[{"id":"1"}]"#,
        )
    }

    fn dm() -> Message {
        message(400, "are you about?", "")
    }

    fn unfocused() -> Focus {
        Focus {
            channel: None,
            terminal_focused: false,
        }
    }

    fn once(
        state: &State,
        message: &Message,
        focus: Focus,
        config: &NotifyConfig,
    ) -> Option<Notification> {
        decide(
            state,
            message,
            focus,
            config,
            &mut Recent::default(),
            Instant::now(),
        )
    }

    #[test]
    fn a_mention_in_a_server_names_who_and_where() {
        let notification = once(&state(), &mention(), unfocused(), &NotifyConfig::default())
            .expect("a mention should interrupt");
        assert_eq!(notification.summary, "Alex in #general");
        assert_eq!(notification.body, "are you about?");
    }

    #[test]
    fn a_dm_is_just_who() {
        let notification =
            once(&state(), &dm(), unfocused(), &NotifyConfig::default()).expect("a dm");
        assert_eq!(notification.summary, "Alex");
    }

    /// The table. Each row is a reason not to interrupt somebody.
    #[test]
    fn every_suppression_suppresses() {
        let state = state();
        let default = NotifyConfig::default();

        // Turned off outright.
        assert_eq!(
            once(
                &state,
                &mention(),
                unfocused(),
                &NotifyConfig {
                    enabled: false,
                    ..default.clone()
                }
            ),
            None
        );

        // This account's own message, sent from somewhere else.
        let mine = message(7, "typing from my phone", r#","guild_id":"9""#);
        let mut mine = mine;
        mine.author = serde_json::from_str(r#"{"id":"1","username":"sam"}"#).unwrap();
        assert_eq!(once(&state, &mine, unfocused(), &default), None);

        // Not addressed to anybody in particular.
        let chatter = message(7, "morning all", r#","guild_id":"9""#);
        assert_eq!(once(&state, &chatter, unfocused(), &default), None);

        // A server mention when only DMs were asked for.
        assert_eq!(
            once(
                &state,
                &mention(),
                unfocused(),
                &NotifyConfig {
                    dms_only: true,
                    ..default.clone()
                }
            ),
            None
        );
        // ...and the DM still gets through.
        assert!(once(
            &state,
            &dm(),
            unfocused(),
            &NotifyConfig {
                dms_only: true,
                ..default.clone()
            }
        )
        .is_some());

        // The channel already on screen, with the terminal in front.
        let watching = Focus {
            channel: Some(ChannelId(7)),
            terminal_focused: true,
        };
        assert_eq!(once(&state, &mention(), watching, &default), None);

        // The same channel with the terminal in the background is news.
        let behind = Focus {
            channel: Some(ChannelId(7)),
            terminal_focused: false,
        };
        assert!(once(&state, &mention(), behind, &default).is_some());

        // And with the rule turned off, even watching it notifies.
        assert!(once(
            &state,
            &mention(),
            watching,
            &NotifyConfig {
                only_when_unfocused: false,
                ..default
            }
        )
        .is_some());
    }

    #[test]
    fn a_muted_channel_stays_quiet() {
        let mut state = state();
        state.set_settings(
            serde_json::from_str(
                r#"{"guild_id":"9","muted":false,
                    "channel_overrides":[{"channel_id":"7","muted":true}]}"#,
            )
            .unwrap(),
        );
        assert_eq!(
            once(&state, &mention(), unfocused(), &NotifyConfig::default()),
            None,
            "muting is the one instruction a person gives about interruptions"
        );
    }

    /// A conversation going quickly is one notification that counts, not
    /// eleven.
    #[test]
    fn a_run_of_mentions_collapses_and_then_starts_again() {
        let state = state();
        let config = NotifyConfig::default();
        let mut recent = Recent::default();
        let start = Instant::now();

        let first = decide(&state, &mention(), unfocused(), &config, &mut recent, start).unwrap();
        assert_eq!(first.summary, "Alex in #general");

        let second = decide(
            &state,
            &mention(),
            unfocused(),
            &config,
            &mut recent,
            start + Duration::from_millis(200),
        )
        .unwrap();
        assert_eq!(second.summary, "2 new mentions in #general");
        assert!(second.body.is_empty());

        let third = decide(
            &state,
            &mention(),
            unfocused(),
            &config,
            &mut recent,
            start + Duration::from_millis(400),
        )
        .unwrap();
        assert_eq!(third.summary, "3 new mentions in #general");

        // Past the window, it is news again.
        let later = decide(
            &state,
            &mention(),
            unfocused(),
            &config,
            &mut recent,
            start + Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(later.summary, "Alex in #general");
    }

    /// Two channels are counted separately: one busy conversation must not
    /// silence another.
    #[test]
    fn one_busy_channel_does_not_collapse_another() {
        let state = state();
        let config = NotifyConfig::default();
        let mut recent = Recent::default();
        let now = Instant::now();

        decide(&state, &mention(), unfocused(), &config, &mut recent, now);
        decide(&state, &mention(), unfocused(), &config, &mut recent, now);

        let elsewhere = decide(&state, &dm(), unfocused(), &config, &mut recent, now).unwrap();
        assert_eq!(elsewhere.summary, "Alex");
    }

    /// The one piece of a message somebody deliberately hid.
    #[test]
    fn a_spoiler_stays_a_spoiler() {
        let state = state();
        let spoilered = message(
            7,
            "the butler ||did it|| apparently",
            r#","guild_id":"9","mentions":[{"id":"1"}]"#,
        );
        let notification = once(&state, &spoilered, unfocused(), &NotifyConfig::default()).unwrap();
        assert_eq!(notification.body, "the butler [spoiler] apparently");
    }

    #[test]
    fn a_long_message_is_cut_where_this_client_can_see_it() {
        let state = state();
        let long = message(
            7,
            &"かたつむり".repeat(200),
            r#","guild_id":"9","mentions":[{"id":"1"}]"#,
        );
        let notification = once(&state, &long, unfocused(), &NotifyConfig::default()).unwrap();
        assert_eq!(notification.body.chars().count(), MAX_BODY);
        assert!(notification.body.ends_with('…'));
    }

    /// An empty popup looks like a broken one.
    #[test]
    fn a_message_that_is_only_a_picture_says_so() {
        let state = state();
        let picture = message(
            400,
            "",
            r#","attachments":[{"id":"1","filename":"cat.png","url":"https://x/1","size":10}]"#,
        );
        let notification = once(&state, &picture, unfocused(), &NotifyConfig::default()).unwrap();
        assert_eq!(notification.body, "[cat.png]");
    }

    /// Looking at a channel ends the run it was in, so the next message after
    /// that is news rather than the fourth of something.
    #[test]
    fn opening_a_channel_ends_its_collapse() {
        let notifier = Notifier::new(NotifyConfig::default());
        notifier.set_focus(None, false);
        let state = state();

        assert!(notifier.consider(&state, &mention()).is_some());
        let second = notifier.consider(&state, &mention()).unwrap();
        assert!(second.summary.starts_with("2 new"), "{second:?}");

        notifier.set_focus(Some(ChannelId(7)), false);
        let after = notifier.consider(&state, &mention()).unwrap();
        assert_eq!(after.summary, "Alex in #general");
    }

    /// Delivery with no runtime under it is a no-op rather than a panic, which
    /// is what makes the decision half testable on its own.
    #[test]
    fn delivering_outside_a_runtime_does_nothing() {
        let notifier = Notifier::new(NotifyConfig::default());
        notifier.deliver(Notification {
            summary: "x".into(),
            body: "y".into(),
        });
    }
}
