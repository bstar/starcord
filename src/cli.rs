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
    fn verbose_is_accepted_before_and_after_the_subcommand() {
        assert!(Cli::parse_from(["starcord", "--verbose", "probe"]).verbose);
        assert!(Cli::parse_from(["starcord", "probe", "--verbose"]).verbose);
    }
}
