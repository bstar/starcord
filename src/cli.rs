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

    /// Make `--send` a reply to this message.
    #[arg(long, value_name = "ID", requires = "send")]
    pub reply_to: Option<u64>,

    /// Ping the person being replied to. Off by default, as the composer's
    /// own default is.
    #[arg(long, requires = "reply_to")]
    pub ping: bool,

    /// Subscribe to member lists with op 14 rather than op 37.
    ///
    /// Which opcode a user-account session is expected to send is one of the
    /// things only a live connection can settle, so both are reachable and the
    /// one that went out is printed.
    #[arg(long)]
    pub legacy_lazy_request: bool,
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
    fn verbose_is_accepted_before_and_after_the_subcommand() {
        assert!(Cli::parse_from(["starcord", "--verbose", "probe"]).verbose);
        assert!(Cli::parse_from(["starcord", "probe", "--verbose"]).verbose);
    }
}
