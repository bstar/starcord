//! Snapshots of whole frames.
//!
//! A layout regression is a diff of a drawn screen. It is the only form in
//! which anybody can actually see one, and it is the one thing a pile of
//! assertions about rectangles cannot show: the tiling test says the panels
//! cover the body, and says nothing at all about a title clipped to `= serv`,
//! a channel row one column wider than its panel, or a header word sitting on
//! a border. Every one of those was found by looking at a frame.
//!
//! Two sizes, because they exercise different code: a hundred by thirty is the
//! layout with everything in it, and sixty by twelve is the floor, where the
//! ladder has taken the member list and the rail away and the columns are at
//! their minimums.
//!
//! Two themes, because the roles are resolved per theme and only a drawn frame
//! shows what they resolve to: `terminal`, which is the sixteen-colour one, and
//! `catppuccin-mocha`, which is the default and has a full base16 palette.
//! Colours are in the snapshots — `Buffer`'s `Debug` carries them — so a
//! derivation change shows up here as well as in the legibility test.

use std::path::PathBuf;

use starkit::graphics::Graphics;
use starkit::ratatui::backend::TestBackend;
use starkit::ratatui::Terminal;

use super::app::App;
use super::fake;
use super::keymap::Action;
use crate::config::Config;

/// A frame as text, one row per line, trailing blanks trimmed.
///
/// The text rather than the `Buffer` debug dump: a snapshot somebody has to
/// read is a snapshot somebody will read, and a wall of `Cell { symbol: "x",
/// fg: Rgb(..) }` is not one. The colours are asserted by the legibility test
/// and by the golden theme dumps; what is asserted here is the shape.
fn render(app: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("a test terminal");
    term.draw(|f| app.draw(f.area(), f.buffer_mut()))
        .expect("drawing");
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| {
            let row: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
            row.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// An app with a core that says nothing, which is the login screen.
///
/// The [`fake::Idle`] comes back with it and has to be held: dropping it closes
/// the channels, and a closed channel is a core that has gone away.
fn app_with(theme: &str) -> (App, fake::Idle) {
    let (core, idle) = fake::silent();
    (
        App::new(
            core,
            config(theme),
            PathBuf::from("/nonexistent/config.toml"),
            None,
            Graphics::disabled(),
        ),
        idle,
    )
}

fn config(theme: &str) -> Config {
    Config {
        ui: crate::config::Ui {
            theme: theme.into(),
            ..Default::default()
        },
        ..Config::default()
    }
}

/// An app past the login screen, with the replay fixture loaded.
///
/// The fixture is applied rather than played: a timeline runs on a clock, and a
/// snapshot taken against one is a snapshot that depends on how busy the
/// machine was. One second in is after the presences and long before the
/// connection drops.
fn loaded(theme: &str) -> (App, fake::Idle) {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/gateway/session.json");
    let session = fake::Session::read(&path).expect("the replay fixture");
    let (core, idle) = fake::loaded(&session, 1_000);

    let mut app = App::new(
        core,
        config(theme),
        PathBuf::from("/nonexistent/config.toml"),
        None,
        Graphics::disabled(),
    );
    assert!(app.login.is_none(), "a ready core needs no login screen");

    // The rail opens on the direct-message home; the snapshots want a server
    // open, which is the first thing anybody does.
    app.tick();
    app.handle(Action::FocusGuilds);
    app.handle(Action::CursorDown);
    app.handle(Action::Activate);
    app.tick();
    (app, idle)
}

#[test]
fn the_login_screen() {
    let (mut app, _idle) = app_with("terminal");
    insta::assert_snapshot!("login-terminal-100x30", render(&mut app, 100, 30));
    insta::assert_snapshot!("login-terminal-60x12", render(&mut app, 60, 12));

    let (mut app, _idle) = app_with("catppuccin-mocha");
    insta::assert_snapshot!("login-mocha-100x30", render(&mut app, 100, 30));
}

#[test]
fn the_main_layout_with_the_fixture_loaded() {
    let (mut app, _idle) = loaded("terminal");
    insta::assert_snapshot!("dock-terminal-100x30", render(&mut app, 100, 30));
    // The floor: the ladder has taken the member list and the rail, the left
    // column is at its minimum, and the DM list has folded into the channels.
    insta::assert_snapshot!("dock-terminal-60x12", render(&mut app, 60, 12));
    // And one below it, which draws one line and nothing else.
    insta::assert_snapshot!("dock-terminal-59x30", render(&mut app, 59, 30));

    let (mut app, _idle) = loaded("catppuccin-mocha");
    insta::assert_snapshot!("dock-mocha-100x30", render(&mut app, 100, 30));
}

#[test]
fn the_help_overlay() {
    let (mut app, _idle) = loaded("terminal");
    app.handle(Action::Help);
    insta::assert_snapshot!("help-terminal-100x30", render(&mut app, 100, 30));

    let (mut app, _idle) = loaded("catppuccin-mocha");
    app.handle(Action::Help);
    insta::assert_snapshot!("help-mocha-100x30", render(&mut app, 100, 30));
}

/// The folded layout, where the DM list has no panel of its own and the
/// channel panel is carrying both lists behind a word.
#[test]
fn the_folded_message_list() {
    let (mut app, _idle) = loaded("terminal");
    // Drawn once first, because a key acts on the layout that is on screen:
    // whether the DM list is folded or closed is a fact about the last frame,
    // and at a hundred columns it is neither.
    render(&mut app, 60, 12);
    // `alt+d` then swaps what the channel panel is showing rather than closing
    // a panel that is not there.
    app.handle(Action::ToggleDms);
    insta::assert_snapshot!("fold-terminal-60x12", render(&mut app, 60, 12));
}
