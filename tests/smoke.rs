//! The one test that needs a real account.
//!
//! Gated on `STARCORD_TEST_TOKEN` and skips cleanly without it, so
//! `cargo test` works on a machine that has never logged in — which is every
//! CI runner and most development machines.
//!
//! It drives the built binary rather than the core directly. `starcord` is a
//! binary crate, so an integration test cannot reach `Handle` at all; and
//! running `probe` is a better test anyway, because it exercises the thing a
//! person would actually run when something is wrong.
//!
//! ```sh
//! STARCORD_TEST_TOKEN="$(cat token.txt)" cargo test --test smoke -- --nocapture
//! ```

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Long enough for a large account's READY on a slow connection, short enough
/// that a wedged run fails rather than hanging a suite.
const BUDGET: Duration = Duration::from_secs(60);

#[test]
fn a_real_token_reaches_ready_and_disconnects_cleanly() {
    let Ok(token) = std::env::var("STARCORD_TEST_TOKEN") else {
        eprintln!("STARCORD_TEST_TOKEN is not set; skipping the live smoke test");
        return;
    };

    // Somewhere of its own, so the test cannot write over a real session's
    // credentials or read a token it was not given.
    let home = tempfile::tempdir().expect("a temporary directory");

    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_starcord"))
        .args([
            "probe",
            "--token-from-stdin",
            "--no-store",
            "--timeout",
            "45",
        ])
        .env("STARCORD_DIR", home.path())
        .env("STARCORD_CONFIG_DIR", home.path())
        .env_remove("STARCORD_RECORD_GATEWAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the probe binary started");

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(token.as_bytes())
        .expect("the token was written");

    let output = loop {
        match child.try_wait().expect("waiting on the probe") {
            Some(_) => break child.wait_with_output().expect("the probe's output"),
            None if started.elapsed() > BUDGET => {
                let _ = child.kill();
                panic!("the probe did not finish within {BUDGET:?}");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "probe exited with {}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains("READY as "),
        "probe did not report a READY:\n{stdout}"
    );

    // The token is the thing this test was handed. It must not come back out.
    assert!(
        !stdout.contains(&token) && !stderr.contains(&token),
        "the probe printed the token"
    );

    // `--no-store` means exactly that.
    assert!(
        !home.path().join("credentials.toml").exists(),
        "--no-store wrote a credentials file"
    );

    // And nothing may have been written to the log either.
    let log = home.path().join("cache").join("starcord.log");
    if let Ok(text) = std::fs::read_to_string(&log) {
        assert!(
            !text.contains(&token),
            "the token reached {}",
            log.display()
        );
    }
}
