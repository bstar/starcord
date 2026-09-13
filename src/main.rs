//! STAR/CORD — a Winamp-feel terminal Discord client.

mod cli;
mod discord;
mod logging;
mod paths;
mod session;

use std::io::Read as _;
use std::io::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser as _;

use discord::auth::{StorePreference, Token};
use discord::handle::{AuthEvent, Connection, DiscordConfig, Event, Handle, MessagesChange, Nonce};
use discord::markdown;
use discord::model::Message;
use discord::snowflake::{ChannelId, MessageId};
use paths::PATHS;

/// The environment variable that turns a probe into a fixture recorder.
const RECORD_ENV: &str = "STARCORD_RECORD_GATEWAY";

fn main() -> Result<()> {
    let cli = cli::Cli::parse();

    PATHS.init_private_dirs();
    // The guard must outlive everything that logs; dropping it early loses
    // whatever the writer thread had buffered.
    let _log = logging::init(&PATHS, cli.verbose)?;

    match cli.command {
        Some(cli::Command::Probe(probe)) => run_probe(probe),
        None => {
            println!("TUI not built yet; use `starcord probe`");
            Ok(())
        }
    }
}

fn run_probe(options: cli::Probe) -> Result<()> {
    let record_gateway = std::env::var_os(RECORD_ENV).map(std::path::PathBuf::from);
    if let Some(dir) = record_gateway.as_deref() {
        eprintln!(
            "recording every dispatch to {} — it will contain real names and \
             message text until it is scrubbed",
            dir.display()
        );
    }

    let config = DiscordConfig {
        store: if options.no_store {
            StorePreference::None
        } else {
            StorePreference::default()
        },
        // A pasted token is logged in explicitly below, so that the failure is
        // reported against the token that was given rather than against a
        // stored one that happened to be there.
        auto_connect: !options.token_from_stdin,
        discover_build: !options.offline,
        legacy_lazy_request: options.legacy_lazy_request,
        record_gateway,
        ..Default::default()
    };

    let token = if options.token_from_stdin {
        Some(read_token_from_stdin()?)
    } else {
        None
    };

    let handle = Handle::spawn(config, PATHS).context("starting the Discord core")?;
    if let Some(token) = token {
        handle.send(discord::Command::LoginWithToken(token));
    }

    let started = Instant::now();
    let deadline = started + Duration::from_secs(options.timeout);
    let mut ready = false;
    let mut failure: Option<String> = None;

    while Instant::now() < deadline {
        for event in handle.drain() {
            match event {
                Event::Status(status) => report_status(started, &status),
                Event::Auth(auth) => {
                    if let Some(message) = report_auth(started, &auth) {
                        failure = Some(message);
                    }
                }
                Event::Note(note) => {
                    stamp(started);
                    println!("{:?}: {}", note.level, note.text);
                }
                Event::Ready => ready = true,
                _ => {}
            }
        }

        if ready || failure.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if let Some(message) = failure {
        anyhow::bail!("{message}");
    }
    if !ready {
        anyhow::bail!(
            "no READY within {}s; the last status was {}",
            options.timeout,
            handle.status().word()
        );
    }

    report_ready(&handle);

    if let Some(channel) = options.channel.map(ChannelId) {
        open_channel(&handle, started, channel, deadline)?;

        if let Some(text) = options.send.clone() {
            send_one(
                &handle,
                started,
                channel,
                text,
                options.reply_to.map(MessageId),
                options.ping,
            );
        }
    }

    if options.follow {
        println!();
        match options.channel.map(ChannelId) {
            Some(channel) => println!("following {channel}; press ctrl-c to stop"),
            None => println!("following; press ctrl-c to stop"),
        }
        follow(&handle, started, options.channel.map(ChannelId), deadline);
    }

    if let Some(channel) = options.channel.map(ChannelId) {
        handle.send(discord::Command::CloseChannel(channel));
    }
    handle.send(discord::Command::Disconnect);
    let dropped = handle.events_dropped();
    drop(handle);
    if dropped > 0 {
        println!("{dropped} events were dropped on the way to this report");
    }
    Ok(())
}

/// Open a channel, wait for its history, and print it.
///
/// The wait is on `Event::Messages(channel, Replaced)`, which is what an open
/// produces whether the window came off the wire or out of memory.
fn open_channel(
    handle: &Handle,
    started: Instant,
    channel: ChannelId,
    deadline: Instant,
) -> Result<()> {
    stamp(started);
    println!("opening {channel}");

    // What the client does when somebody clicks a channel: focus first, then
    // open. Nothing here ever sends MarkRead, so nothing here marks anything
    // read -- a debugging tool must not change what the account has seen.
    handle.send(discord::Command::SetFocus {
        channel: Some(channel),
        terminal_focused: true,
    });
    handle.send(discord::Command::OpenChannel(channel));

    let mut loaded = false;
    while Instant::now() < deadline && !loaded {
        for event in handle.drain() {
            match event {
                Event::Messages(c, MessagesChange::Replaced) if c == channel => loaded = true,
                Event::Note(note) => {
                    stamp(started);
                    println!("{:?}: {}", note.level, note.text);
                }
                Event::Status(status) => report_status(started, &status),
                _ => {}
            }
        }
        if !loaded {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if !loaded {
        anyhow::bail!("{channel} did not load; see the log for why");
    }

    let messages = handle.state().recent(channel, 50);
    println!();
    println!("{} messages, oldest first:", messages.len());
    for message in &messages {
        print_message(handle, message, "");
    }
    Ok(())
}

/// Send one message and wait for the gateway to echo it back.
///
/// The echo is the point: it is what proves the nonce round-tripped and that
/// the optimistic row on screen became the real message rather than a second
/// copy of it.
fn send_one(
    handle: &Handle,
    started: Instant,
    channel: ChannelId,
    text: String,
    reply_to: Option<MessageId>,
    ping: bool,
) {
    stamp(started);
    println!("sending {} characters to {channel}", text.chars().count());

    handle.send(discord::Command::SendMessage {
        channel,
        content: text,
        reply_to,
        mention_author: ping,
        attachments: Vec::new(),
    });

    // Long enough to cover the send's own ten-second echo fallback.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut nonce: Option<Nonce> = None;
    let mut sent: Option<MessageId> = None;

    while Instant::now() < deadline {
        for event in handle.drain() {
            match event {
                Event::Messages(c, MessagesChange::Pending(n)) if c == channel => {
                    if nonce.is_none() {
                        nonce = Some(n);
                        stamp(started);
                        println!("pending as nonce {n}");
                    }
                }
                Event::SendResult { nonce: n, result } => {
                    stamp(started);
                    match result {
                        Ok(id) => {
                            println!("accepted as {id}");
                            sent = Some(id);
                        }
                        Err(e) => println!("refused ({n}): {e}"),
                    }
                }
                Event::Messages(c, MessagesChange::Appended(id)) if c == channel => {
                    if Some(id) == sent {
                        stamp(started);
                        println!("echoed back by the gateway");
                        // Bound before the `if let`, so the read guard is gone
                        // before `print_message` takes one of its own: a second
                        // read on the same thread deadlocks against a writer
                        // that queued between them.
                        let message = handle.state().message(channel, id);
                        if let Some(message) = message {
                            print_message(handle, &message, "  ");
                        }
                        return;
                    }
                }
                Event::Note(note) => {
                    stamp(started);
                    println!("{:?}: {}", note.level, note.text);
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("no echo arrived within fifteen seconds");
}

/// Print everything that happens until the timeout.
fn follow(handle: &Handle, started: Instant, channel: Option<ChannelId>, deadline: Instant) {
    while Instant::now() < deadline {
        for event in handle.drain() {
            match event {
                Event::Status(status) => report_status(started, &status),
                Event::Note(note) => {
                    stamp(started);
                    println!("{:?}: {}", note.level, note.text);
                }
                Event::Messages(c, change) if Some(c) == channel => {
                    report_change(handle, started, c, change)
                }
                Event::Typing(c) if Some(c) == channel => {
                    let state = handle.state();
                    let who: Vec<String> = state
                        .typing(c)
                        .into_iter()
                        .map(|user| state.display_name(None, user))
                        .collect();
                    drop(state);
                    stamp(started);
                    match who.as_slice() {
                        [] => println!("nobody is typing"),
                        names => println!("typing: {}", names.join(", ")),
                    }
                }
                Event::Mention {
                    channel: c,
                    message,
                } => {
                    stamp(started);
                    println!("mentioned in {c} by {message}");
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn report_change(handle: &Handle, started: Instant, channel: ChannelId, change: MessagesChange) {
    match change {
        MessagesChange::Appended(id) | MessagesChange::Updated(id) => {
            let verb = if matches!(change, MessagesChange::Updated(_)) {
                "edited"
            } else {
                "new"
            };
            let Some(message) = handle.state().message(channel, id) else {
                return;
            };
            stamp(started);
            println!("{verb}:");
            print_message(handle, &message, "  ");
        }
        MessagesChange::Removed(id) => {
            stamp(started);
            println!("deleted {id}");
        }
        MessagesChange::Reactions(id) => {
            let Some(message) = handle.state().message(channel, id) else {
                return;
            };
            let chips: Vec<String> = message
                .reactions
                .iter()
                .map(|r| {
                    let name = r.emoji.name.clone().unwrap_or_else(|| "?".into());
                    let me = if r.me { "*" } else { "" };
                    format!("{name} {}{me}", r.count)
                })
                .collect();
            stamp(started);
            println!("reactions on {id}: {}", chips.join("  "));
        }
        MessagesChange::Prepended(n) => {
            stamp(started);
            println!("{n} older messages");
        }
        MessagesChange::Replaced => {
            stamp(started);
            println!("the whole window changed");
        }
        MessagesChange::Pending(nonce) => {
            stamp(started);
            println!("pending {nonce}");
        }
        MessagesChange::Loading(on) => {
            stamp(started);
            println!("{}", if on { "loading…" } else { "loaded" });
        }
    }
}

/// One message, as a line.
///
/// The content goes through the markdown parser's `plain_text`, which is the
/// only rendering the core can do and the thing `probe` exists to exercise.
/// This prints message text on purpose; nothing else in the program does.
fn print_message(handle: &Handle, message: &Message, indent: &str) {
    let time = local_time(message.created_at());
    let author = message.author_name();
    let edited = if message.is_edited() { " (edited)" } else { "" };

    let mut body = markdown::plain_text(&message.content);
    if let Some(replied) = message.reply_target() {
        let to = handle
            .state()
            .message(message.channel_id, replied)
            .map(|m| m.author_name().to_string())
            .unwrap_or_else(|| replied.to_string());
        body = format!("↩ {to} | {body}");
    }
    for attachment in &message.attachments {
        body.push_str(&format!(" [{}]", attachment.filename));
    }
    for embed in &message.embeds {
        body.push_str(&format!(" <{}>", embed.kind.as_str()));
    }

    for (n, line) in body.lines().enumerate() {
        if n == 0 {
            println!("{indent}{time}  {author}{edited}: {line}");
        } else {
            println!("{indent}         {line}");
        }
    }
    if body.is_empty() {
        println!("{indent}{time}  {author}{edited}:");
    }
}

/// A wall-clock time in the reader's own zone, which is the only form a
/// timestamp is any use in.
fn local_time(at: jiff::Timestamp) -> String {
    let zoned = at.to_zoned(jiff::tz::TimeZone::system());
    format!("{:02}:{:02}", zoned.hour(), zoned.minute())
}

/// What READY contained, which is the whole point of the exercise.
fn report_ready(handle: &Handle) {
    let state = handle.state();
    let me = state.me();
    let name = me
        .as_ref()
        .map(|u| u.display_name().to_string())
        .unwrap_or_else(|| "unknown".into());

    println!();
    println!(
        "READY as {name}, {} guilds, {} DMs",
        state.guild_count(),
        state.dm_count()
    );

    println!();
    println!("guilds, in the order READY sent them:");
    for guild in state.guilds_ordered() {
        let channels = state.channels_ordered(guild.id).len();
        let unavailable = if guild.unavailable {
            " (unavailable)"
        } else {
            ""
        };
        println!(
            "  {:<40} {channels:>3} channels{unavailable}",
            truncate(&guild.name, 40)
        );
    }

    println!();
    println!("DMs, newest conversation first:");
    for dm in state.dms_ordered() {
        let unread = state.unread(dm.id);
        let marks = match (unread.unread, unread.mentions, unread.muted) {
            (_, mentions, _) if mentions > 0 => format!(" @{mentions}"),
            (true, _, false) => " •".to_string(),
            _ => String::new(),
        };
        println!("  {}{marks}", truncate(&state.dm_title(dm.id), 60));
    }
}

fn report_status(started: Instant, status: &Connection) {
    stamp(started);
    match status {
        Connection::Reconnecting {
            attempt,
            next_in,
            reason,
        } => println!(
            "reconnecting in {:.1}s (attempt {attempt}): {reason}",
            next_in.as_secs_f32()
        ),
        Connection::Ready { resumed, .. } => {
            println!("online{}", if *resumed { " (resumed)" } else { "" })
        }
        Connection::AuthFailed(reason) => println!("rejected: {reason}"),
        other => println!("{}", other.word()),
    }
}

/// Returns a message when the login failed for good.
fn report_auth(started: Instant, auth: &AuthEvent) -> Option<String> {
    stamp(started);
    match auth {
        AuthEvent::NeedsLogin => {
            println!("no stored token");
            None
        }
        AuthEvent::LoggedIn { user, stored_in } => {
            println!(
                "signed in as {} ({}), token in {}",
                user.display_name(),
                user.tag(),
                stored_in.describe()
            );
            None
        }
        AuthEvent::Failed(reason) => {
            println!("login failed: {reason}");
            Some(reason.clone())
        }
        AuthEvent::LoggedOut => {
            println!("logged out");
            None
        }
        AuthEvent::QrReady { url, .. } => {
            println!("scan {url}");
            None
        }
        AuthEvent::QrScanned { username, .. } => {
            println!("scanned by {username}");
            None
        }
    }
}

/// Seconds since the probe started, so a report shows how long each step took.
fn stamp(started: Instant) {
    print!("[{:>6.2}s] ", started.elapsed().as_secs_f32());
    let _ = std::io::stdout().flush();
}

/// Read a token from standard input.
///
/// The whole of stdin, not a line: a token pasted into a terminal may or may
/// not have a newline after it, and `Token::new` trims what arrives anyway.
fn read_token_from_stdin() -> Result<Token> {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .context("reading the token from standard input")?;
    let token = Token::new(&raw).context("that does not look like a token")?;
    // `raw` still holds it. Overwriting is not a guarantee — the allocator may
    // have moved it — but it costs nothing and removes the obvious copy.
    raw.clear();
    Ok(token)
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_name_is_cut_with_an_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactlyten", 10), "exactlyten");
        assert_eq!(
            truncate("elevenchars", 10),
            "ninechars…".replace("ninechars", "elevencha")
        );
    }

    /// `truncate` counts characters, not bytes: a guild name is as likely to be
    /// Japanese as English, and slicing one in the middle of a character
    /// panics.
    #[test]
    fn truncation_does_not_split_a_character() {
        let name = "サーバーの名前がとても長い場合";
        let cut = truncate(name, 5);
        assert_eq!(cut.chars().count(), 5);
        assert!(cut.ends_with('…'));
    }
}
