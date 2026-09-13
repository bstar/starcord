//! The command line.
//!
//! `starcord` with no arguments is the client. `starcord probe` is a headless
//! one: it connects, prints what happened, and exits. It exists because the
//! whole Discord core is written before there is a terminal UI to drive it, and
//! it is meant to stay afterwards — a defect that reproduces without a terminal
//! is a defect with a much shorter report.
//!
//! A token is never an argument. `argv` is readable by every process on the
//! machine through `/proc`, and it lands in shell history besides; `probe`
//! reads it from standard input.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "starcord",
    version,
    about = "STAR/CORD — a Winamp-feel terminal Discord client",
    long_about = None,
)]
pub struct Cli {
    /// Log at debug level. To the log file, never to the terminal.
    #[arg(long, short, global = true)]
    pub verbose: bool,

    /// Run against a recorded session instead of Discord.
    ///
    /// `--replay testdata/gateway/session.json` plays a timeline through the
    /// whole interface with no network and no account: a READY, presences, a
    /// connection that drops and comes back. It is how the client is developed
    /// and how a layout is looked at without arranging for somebody to send a
    /// message.
    #[arg(long, value_name = "FILE")]
    pub replay: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Connect, report what the gateway said, and exit.
    Probe(Probe),
}

#[derive(Debug, clap::Args)]
pub struct Probe {
    /// Read the token from standard input rather than from the keyring.
    ///
    /// There is deliberately no `--token`: an argument is visible to every
    /// other process on the machine.
    #[arg(long)]
    pub token_from_stdin: bool,

    /// Do not save the token. Use it for this run and forget it.
    #[arg(long)]
    pub no_store: bool,

    /// How long to wait for READY before giving up.
    #[arg(long, value_name = "SECONDS", default_value = "45")]
    pub timeout: u64,

    /// Stay connected after READY, printing events as they arrive, until
    /// interrupted or the timeout passes.
    #[arg(long)]
    pub follow: bool,

    /// Use the pinned build number instead of looking up the current one.
    #[arg(long)]
    pub offline: bool,

    /// Open this channel after READY and print its history.
    ///
    /// With `--follow` it then prints messages, edits, deletions and typing as
    /// they arrive. A channel id is not a secret; a message is, which is why
    /// nothing below DEBUG in the log ever carries one and why this prints to
    /// stdout instead.
    #[arg(long, value_name = "ID")]
    pub channel: Option<u64>,

    /// Send one message to `--channel` and wait for the gateway to echo it.
    #[arg(long, value_name = "TEXT", requires = "channel")]
    pub send: Option<String>,

    /// Attach a file to the message sent to `--channel`.
    ///
    /// May be given more than once. Works with or without `--send`: a message
    /// with a file and no text is an ordinary message with a file in it.
    #[arg(long, value_name = "PATH", requires = "channel")]
    pub send_file: Vec<std::path::PathBuf>,

    /// Make `--send` a reply to this message.
    #[arg(long, value_name = "ID", requires = "send")]
    pub reply_to: Option<u64>,

    /// Ping the person being replied to. Off by default, as the composer's
    /// own default is.
    #[arg(long, requires = "reply_to")]
    pub ping: bool,

    /// Search for messages and print what came back.
    ///
    /// Searches the whole server `--channel` is in, or just that channel with
    /// `--search-here`.
    #[arg(long, value_name = "TEXT", requires = "channel")]
    pub search: Option<String>,

    /// Make `--search` look only in `--channel` rather than the whole server.
    #[arg(long, requires = "search")]
    pub search_here: bool,

    /// React to a message in `--channel`.
    ///
    /// Takes a message id and an emoji: the character itself for a unicode one,
    /// `name:id` for a custom one. `--unreact` takes this account's reaction
    /// off again instead of putting it on.
    #[arg(long, value_names = ["MESSAGE_ID", "EMOJI"], num_args = 2, requires = "channel")]
    pub react: Option<Vec<String>>,

    /// Make `--react` remove the reaction rather than add it.
    #[arg(long, requires = "react")]
    pub unreact: bool,

    /// Sign in by scanning a code with the Discord phone app.
    ///
    /// Prints the login URL, draws the code, and waits. The password is never
    /// typed and the token never appears on screen; it goes straight into the
    /// keyring, or into `credentials.toml` when there is none.
    #[arg(long, conflicts_with = "token_from_stdin")]
    pub qr: bool,

    /// Draw the code for a light terminal background.
    ///
    /// The default assumes a dark one, where the light modules of the code have
    /// to be the bright cells. On a light background that is inside out, and a
    /// camera will not read it.
    #[arg(long, requires = "qr")]
    pub qr_invert: bool,

    /// Fetch one picture, decode it, and print what it is.
    ///
    /// No account and no gateway: media comes off a CDN and carries no token,
    /// which is exactly why it is worth being able to check on its own. The URL
    /// is fetched under the embed cap, cached like any other picture, and the
    /// result is reported as a size, a format and a frame count.
    #[arg(long, value_name = "URL")]
    pub media: Option<String>,

    /// Search the GIF picker and print what came back.
    ///
    /// An empty string asks for what is trending. Prints a title and the link
    /// that would be posted for each result, which is the whole of what the
    /// picker sends: posting a GIF is an ordinary message whose content is that
    /// link.
    #[arg(long, value_name = "QUERY")]
    pub gifs: Option<String>,

    /// Subscribe to member lists with op 14 rather than op 37.
    ///
    /// Which opcode a user-account session is expected to send is one of the
    /// things only a live connection can settle, so both are reachable and the
    /// one that went out is printed.
    #[arg(long)]
    pub legacy_lazy_request: bool,
}

/// Draw a QR matrix with half-block characters.
///
/// Two cells per module across and one half-block down, because a terminal cell
/// is about twice as tall as it is wide and a camera will not read a code that
/// is twice as tall as it is square.
///
/// The polarity is the part worth explaining. A scanner needs the *dark*
/// modules dark, and in a terminal the dark thing is the background, so the
/// light modules are what gets painted. That is inside out on a light
/// background, which is what `invert` is for. The four-module quiet zone is
/// part of the code rather than decoration: without it a scanner has nothing to
/// find the edges against.
pub fn render_qr(matrix: &[Vec<bool>], invert: bool) -> String {
    const QUIET: usize = 4;

    if matrix.is_empty() {
        return String::new();
    }
    let size = matrix.len();
    let span = size + QUIET * 2;

    let dark = |x: usize, y: usize| -> bool {
        if x < QUIET || y < QUIET || x >= QUIET + size || y >= QUIET + size {
            // The quiet zone is light, everywhere outside the code.
            return false;
        }
        matrix[y - QUIET].get(x - QUIET).copied().unwrap_or(false)
    };
    let lit = |x: usize, y: usize| -> bool {
        if invert {
            dark(x, y)
        } else {
            !dark(x, y)
        }
    };

    let mut out = String::new();
    let mut y = 0;
    while y < span {
        for x in 0..span {
            let glyph = match (lit(x, y), lit(x, y + 1)) {
                (true, true) => '\u{2588}',
                (true, false) => '\u{2580}',
                (false, true) => '\u{2584}',
                (false, false) => ' ',
            };
            out.push(glyph);
            out.push(glyph);
        }
        out.push('\n');
        y += 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn the_definition_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_arguments_is_the_client() {
        let cli = Cli::parse_from(["starcord"]);
        assert!(cli.command.is_none());
        assert!(!cli.verbose);
        assert!(cli.replay.is_none());
    }

    #[test]
    fn a_replay_names_a_file() {
        let cli = Cli::parse_from(["starcord", "--replay", "testdata/gateway/session.json"]);
        assert!(cli.command.is_none());
        assert_eq!(
            cli.replay.as_deref(),
            Some(std::path::Path::new("testdata/gateway/session.json"))
        );
    }

    #[test]
    fn probe_reads_its_token_from_stdin() {
        let cli = Cli::parse_from(["starcord", "probe", "--token-from-stdin"]);
        match cli.command {
            Some(Command::Probe(probe)) => {
                assert!(probe.token_from_stdin);
                assert!(!probe.no_store);
                assert_eq!(probe.timeout, 45);
            }
            other => panic!("{other:?}"),
        }
    }

    /// The rule, asserted rather than remembered.
    #[test]
    fn there_is_no_way_to_pass_a_token_as_an_argument() {
        let rendered = format!("{:?}", Cli::command());
        assert!(
            !rendered.contains("\"token\""),
            "a --token argument would put a session token in /proc and in shell history"
        );
        assert!(Cli::try_parse_from(["starcord", "probe", "--token", "x"]).is_err());
    }

    #[test]
    fn tailing_a_channel_needs_an_id_and_sending_needs_a_channel() {
        let cli = Cli::parse_from(["starcord", "probe", "--channel", "123", "--follow"]);
        match cli.command {
            Some(Command::Probe(probe)) => {
                assert_eq!(probe.channel, Some(123));
                assert!(probe.follow);
                assert!(probe.send.is_none());
            }
            other => panic!("{other:?}"),
        }

        assert!(
            Cli::try_parse_from(["starcord", "probe", "--send", "hello"]).is_err(),
            "there is nowhere to send that"
        );
        assert!(
            Cli::try_parse_from(["starcord", "probe", "--reply-to", "1"]).is_err(),
            "there is nothing to reply with"
        );
    }

    #[test]
    fn the_member_list_opcode_can_be_switched_from_the_command_line() {
        let cli = Cli::parse_from(["starcord", "probe", "--legacy-lazy-request"]);
        match cli.command {
            Some(Command::Probe(probe)) => assert!(probe.legacy_lazy_request),
            other => panic!("{other:?}"),
        }
        let default = Cli::parse_from(["starcord", "probe"]);
        match default.command {
            Some(Command::Probe(probe)) => assert!(!probe.legacy_lazy_request),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_file_needs_somewhere_to_go_and_does_not_need_text() {
        let cli = Cli::parse_from([
            "starcord",
            "probe",
            "--channel",
            "1",
            "--send-file",
            "a.png",
            "--send-file",
            "b.png",
        ]);
        match cli.command {
            Some(Command::Probe(probe)) => {
                assert_eq!(probe.send_file.len(), 2);
                assert!(
                    probe.send.is_none(),
                    "a message with a file and no text is an ordinary message"
                );
            }
            other => panic!("{other:?}"),
        }

        assert!(
            Cli::try_parse_from(["starcord", "probe", "--send-file", "a.png"]).is_err(),
            "there is nowhere to send that"
        );
    }

    #[test]
    fn a_reply_says_whether_it_pings() {
        let cli = Cli::parse_from([
            "starcord",
            "probe",
            "--channel",
            "1",
            "--send",
            "hi",
            "--reply-to",
            "2",
            "--ping",
        ]);
        match cli.command {
            Some(Command::Probe(probe)) => {
                assert_eq!(probe.reply_to, Some(2));
                assert!(probe.ping);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_search_needs_somewhere_to_look() {
        let cli = Cli::parse_from([
            "starcord",
            "probe",
            "--channel",
            "1",
            "--search",
            "kettle",
            "--search-here",
        ]);
        match cli.command {
            Some(Command::Probe(probe)) => {
                assert_eq!(probe.search.as_deref(), Some("kettle"));
                assert!(probe.search_here);
            }
            other => panic!("{other:?}"),
        }

        assert!(
            Cli::try_parse_from(["starcord", "probe", "--search", "kettle"]).is_err(),
            "the scope comes from the channel, so there has to be one"
        );
    }

    #[test]
    fn a_reaction_takes_a_message_and_an_emoji() {
        let cli = Cli::parse_from([
            "starcord",
            "probe",
            "--channel",
            "1",
            "--react",
            "500",
            "\u{1f44d}",
        ]);
        match cli.command {
            Some(Command::Probe(probe)) => {
                assert_eq!(
                    probe.react.as_deref(),
                    Some(["500".to_string(), "\u{1f44d}".to_string()].as_slice())
                );
                assert!(!probe.unreact);
            }
            other => panic!("{other:?}"),
        }

        assert!(
            Cli::try_parse_from(["starcord", "probe", "--channel", "1", "--react", "500"]).is_err(),
            "a message id on its own says nothing about which reaction"
        );
        assert!(
            Cli::try_parse_from(["starcord", "probe", "--unreact"]).is_err(),
            "there is nothing to take off"
        );
    }

    #[test]
    fn a_scanned_login_and_a_pasted_one_are_not_both() {
        let cli = Cli::parse_from(["starcord", "probe", "--qr"]);
        match cli.command {
            Some(Command::Probe(probe)) => {
                assert!(probe.qr);
                assert!(!probe.qr_invert);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            Cli::try_parse_from(["starcord", "probe", "--qr", "--token-from-stdin"]).is_err(),
            "there is only one login per run"
        );
        assert!(
            Cli::try_parse_from(["starcord", "probe", "--qr-invert"]).is_err(),
            "there is nothing to invert"
        );
    }

    /// A square code, drawn twice as wide per module so that it comes out
    /// square on screen, with the quiet zone a scanner needs.
    #[test]
    fn a_code_is_drawn_square_with_its_quiet_zone() {
        // A five-by-five code with one dark module in the middle.
        let mut matrix = vec![vec![false; 5]; 5];
        matrix[2][2] = true;

        let drawn = render_qr(&matrix, false);
        let lines: Vec<&str> = drawn.lines().collect();

        // Five modules plus four of quiet on each side is thirteen, which is
        // seven half-block rows and twenty-six cells across.
        assert_eq!(lines.len(), 7, "{drawn}");
        for line in &lines {
            assert_eq!(line.chars().count(), 26, "{line:?}");
        }

        // The quiet zone is light, and light is what gets painted.
        assert!(lines[0].chars().all(|c| c == '\u{2588}'), "{:?}", lines[0]);
        // The one dark module is somewhere in the middle row.
        assert!(
            drawn.contains('\u{2580}') || drawn.contains('\u{2584}') || drawn.contains(' '),
            "the dark module was not drawn"
        );
    }

    /// On a light background the whole thing is inside out, which no camera
    /// will read.
    #[test]
    fn inverting_swaps_what_is_painted() {
        let mut matrix = vec![vec![false; 5]; 5];
        matrix[2][2] = true;

        let dark_terminal = render_qr(&matrix, false);
        let light_terminal = render_qr(&matrix, true);
        assert_ne!(dark_terminal, light_terminal);
        assert_eq!(
            dark_terminal.lines().count(),
            light_terminal.lines().count()
        );

        // With a light background the quiet zone is drawn as nothing at all.
        assert!(light_terminal.lines().next().unwrap().trim().is_empty());
    }

    #[test]
    fn an_empty_matrix_draws_nothing_rather_than_panicking() {
        assert_eq!(render_qr(&[], false), "");
        assert_eq!(render_qr(&[], true), "");
    }

    #[test]
    fn verbose_is_accepted_before_and_after_the_subcommand() {
        assert!(Cli::parse_from(["starcord", "--verbose", "probe"]).verbose);
        assert!(Cli::parse_from(["starcord", "probe", "--verbose"]).verbose);
    }
}
