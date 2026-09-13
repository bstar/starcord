//! File-based logging.
//!
//! **Temporary.** This is `starkit::logging` with one app's name baked in, and
//! it exists only until that crate lands. The signature is the one STAR/KIT will
//! publish, so the swap is deleting this file and changing an import.
//!
//! Never stdout. A TUI that scribbles on its own alternate screen when
//! something goes wrong is worse than one that says nothing, and `probe` writes
//! a report to stdout that a script may be reading.
//!
//! Nothing logged here may contain a token or a message body. `Token`'s `Debug`
//! is redacted so that is difficult to do by accident, but the filter default
//! is `info` for the same reason: the noisy levels are where payloads end up.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

use crate::paths::Paths;

/// Returns a guard that must be held for the process lifetime — dropping it
/// stops the writer thread and silently loses buffered lines.
pub fn init(paths: &Paths, verbose: bool) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let dir = paths.log_dir()?;
    crate::paths::own_dir(&dir)?;

    let file = format!("{}.log", paths.app());
    let appender = tracing_appender::rolling::never(&dir, &file);
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let default = if verbose {
        format!("{}=debug", paths.app())
    } else {
        format!("{}=info", paths.app())
    };
    let env = format!("{}_LOG", paths.app().to_uppercase());
    let filter = EnvFilter::try_from_env(&env).unwrap_or_else(|_| EnvFilter::new(default));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();

    Ok(guard)
}
