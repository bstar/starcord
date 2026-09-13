//! Everything that talks to Discord.
//!
//! The boundary this module draws is the reason the rest of the program is
//! testable: **nothing under `src/discord/` knows the terminal exists.** No
//! terminal or drawing crate, and nothing from the UI module. The test at the
//! bottom of this file greps the module's own sources and fails if any of them
//! appear.
//!
//! It is not a layering preference. It is why `starcord probe` can drive the
//! whole protocol with no TTY attached, why the gateway and the state machine
//! can be exercised from recorded fixtures, and why a change to how a message
//! is drawn cannot break how one is sent.
//!
//! The shape:
//!
//! - [`handle`] is the contract — commands in, notifications out, truth behind
//!   a lock.
//! - [`core`] owns the thread and dispatches commands.
//! - [`gateway`] holds the socket; [`http`] makes requests; [`auth`] holds the
//!   one secret.
//! - [`model`] is the wire; [`state`] is the truth; `state::apply` is the only
//!   thing that writes to it.

pub mod auth;
pub mod core;
pub mod gateway;
pub mod handle;
pub mod http;
pub mod model;
pub mod props;
pub mod snowflake;
pub mod state;

// The UI's whole vocabulary, named here so that it imports from `discord`
// rather than from `discord::handle`. Most of it has no consumer until there is
// a UI.
#[allow(unused_imports)]
pub use handle::{
    AuthEvent, Command, Connection, DiscordConfig, Event, Handle, HandleParts, MessagesChange,
    Note, NoteLevel,
};

#[cfg(test)]
mod tests {
    /// The rule this module exists to keep.
    ///
    /// Written as a grep rather than enforced by the crate graph because
    /// `starcord` is a single binary crate, as STAR/AMP is: there is no
    /// `discord` crate for Cargo to keep `ratatui` out of. A test is the next
    /// best thing, and it fails on the line that broke it.
    #[test]
    fn nothing_in_the_core_knows_about_the_terminal() {
        // Import shapes rather than bare words, so a sentence that mentions a
        // crate is not a failure. Each one carries the exemption marker, which
        // is what keeps this test from failing on its own source.
        const FORBIDDEN: &[&str] = &[
            "use ratatui",   // NO-TERMINAL-HERE
            "ratatui::",     // NO-TERMINAL-HERE
            "use crossterm", // NO-TERMINAL-HERE
            "crossterm::",   // NO-TERMINAL-HERE
            "crate::ui",     // NO-TERMINAL-HERE
        ];

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("discord");
        let mut offences = Vec::new();
        let mut files = 0usize;

        walk(&root, &mut |path| {
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                return;
            }
            files += 1;
            let Ok(text) = std::fs::read_to_string(path) else {
                return;
            };
            for (number, line) in text.lines().enumerate() {
                if line.contains("NO-TERMINAL-HERE") {
                    continue;
                }
                for needle in FORBIDDEN {
                    if line.contains(needle) {
                        offences.push(format!(
                            "{}:{}: {}",
                            path.display(),
                            number + 1,
                            line.trim()
                        ));
                    }
                }
            }
        });

        assert!(
            files > 10,
            "only {files} files were scanned; the walk is wrong"
        );
        assert!(
            offences.is_empty(),
            "the Discord core reached for the terminal:\n{}",
            offences.join("\n")
        );
    }

    fn walk(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, f);
            } else {
                f(&path);
            }
        }
    }
}
