//! STAR/CORD — a Winamp-feel terminal Discord client.

mod cli;
mod discord;
mod logging;
mod paths;

use std::io::Read as _;
use std::io::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser as _;

use discord::auth::{StorePreference, Token};
use discord::handle::{AuthEvent, Connection, DiscordConfig, Event, Handle};
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

        if ready && !options.follow {
            break;
        }
        if failure.is_some() {
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

    if options.follow {
        println!();
        println!("following; press ctrl-c to stop");
        while Instant::now() < deadline {
            for event in handle.drain() {
                if let Event::Status(status) = event {
                    report_status(started, &status);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    handle.send(discord::Command::Disconnect);
    let dropped = handle.events_dropped();
    drop(handle);
    if dropped > 0 {
        println!("{dropped} events were dropped on the way to this report");
    }
    Ok(())
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
