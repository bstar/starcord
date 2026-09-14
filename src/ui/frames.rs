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
//!
//! Everything here is pinned to UTC. A message header carries a local time,
//! and a snapshot taken in the machine's own zone is a snapshot that fails in
//! another country.

use std::path::PathBuf;

use starkit::graphics::{Graphics, Mode};
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
    app.tz = jiff::tz::TimeZone::UTC;
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

/// The same, at a moment in the timeline rather than at the start of it.
fn loaded_at(theme: &str, upto_ms: u64) -> (App, fake::Idle) {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/gateway/session.json");
    let session = fake::Session::read(&path).expect("the replay fixture");
    let (core, idle) = fake::loaded(&session, upto_ms);
    let mut app = App::new(
        core,
        config(theme),
        PathBuf::from("/nonexistent/config.toml"),
        None,
        Graphics::disabled(),
    );
    app.tz = jiff::tz::TimeZone::UTC;
    app.tick();
    app.open_channel(CHANNEL);
    app.tick();
    (app, idle)
}

/// The same, with the fixture channel open and the conversation in it.
///
/// `#general` in the first server is the one `messages_basic.json` fills: a
/// group of three, a system line, a code block, a reply with reactions, a
/// spoiler, a mention, an image, a link card and a gifv, across two days.
fn in_general(theme: &str) -> (App, fake::Idle) {
    let (mut app, idle) = loaded(theme);
    app.open_channel(CHANNEL);
    app.tick();
    (app, idle)
}

/// `#general` in the first server, which is where the conversation is.
const CHANNEL: crate::discord::snowflake::ChannelId =
    crate::discord::snowflake::ChannelId(200000000000000011);

/// `#random`, which is where the pictures are.
const PICTURES: crate::discord::snowflake::ChannelId =
    crate::discord::snowflake::ChannelId(200000000000000012);

/// The picture channel, on a terminal drawing half blocks.
///
/// Half blocks rather than a protocol because they are the one way of drawing
/// a picture that lands in the buffer as cells: a snapshot can see them, and
/// every terminal has them. What this pins is the part that is the same
/// either way -- how many rows a picture was given, where the avatars and the
/// emoji went, and that the rail grew to hold an icon.
fn in_pictures(theme: &str) -> (App, fake::Idle) {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/gateway/session.json");
    let session = fake::Session::read(&path).expect("the replay fixture");
    let (core, mut idle) = fake::loaded(&session, 1_000);

    let mut graphics = Graphics::disabled();
    graphics.set_mode(Mode::Blocks);
    let mut cfg = config(theme);
    // Six rows rather than the default twelve, which is also what makes this a
    // test of `[chat] max_image_rows`: a sixty-four by forty-eight picture
    // across thirty-five columns works out at thirteen rows, and the whole
    // conversation would be one photograph.
    cfg.chat.max_image_rows = 6;
    let mut app = App::new(
        core,
        cfg,
        PathBuf::from("/nonexistent/config.toml"),
        None,
        graphics,
    );
    app.tz = jiff::tz::TimeZone::UTC;
    app.tick();
    app.open_channel(PICTURES);
    app.tick();

    // One frame places the pictures, which is what asks for them; the replay
    // answers, and the frame after that has them. Twice over, because an
    // avatar that arrives is a message to measure again, and measuring it is
    // what places the next picture down.
    let mut asked = 0;
    for _ in 0..3 {
        render(&mut app, 100, 30);
        asked += idle.pump();
        app.tick();
    }
    assert!(asked > 0, "nothing was asked for");
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

/// The conversation itself: every construct the renderer has, at once.
///
/// This is the snapshot the whole of `panels::chat` is for. A grouping change,
/// a wrap change, a divider that moved, a chip that lost its brackets: all of
/// them are a diff here, and none of them is visible in an assertion about a
/// row count.
#[test]
fn the_fixture_conversation() {
    let (mut app, _idle) = in_general("terminal");
    insta::assert_snapshot!("chat-terminal-100x30", render(&mut app, 100, 30));

    let (mut app, _idle) = in_general("catppuccin-mocha");
    insta::assert_snapshot!("chat-mocha-100x30", render(&mut app, 100, 30));
}

/// The top of the same conversation, where the day divider, the group of
/// three, the system line and the code block are.
#[test]
fn the_top_of_the_conversation() {
    let (mut app, _idle) = in_general("terminal");
    app.handle(Action::FocusChat);
    app.handle(Action::Home);
    insta::assert_snapshot!("chat-top-terminal-100x30", render(&mut app, 100, 30));
}

/// A spoiler covered, and the same spoiler uncovered. Only that message
/// changes: the two snapshots differ in one run of cells and nowhere else,
/// which is the property the cache key exists to keep.
#[test]
fn a_spoiler_hidden_and_revealed() {
    let (mut app, _idle) = in_general("terminal");
    app.handle(Action::FocusChat);
    // Put the cursor on the message carrying the spoiler.
    app.chat
        .select(crate::discord::snowflake::MessageId(500000000000000107));
    let hidden = render(&mut app, 100, 30);
    insta::assert_snapshot!("chat-spoiler-hidden-100x30", hidden.clone());

    app.handle(Action::RevealSpoiler);
    let shown = render(&mut app, 100, 30);
    insta::assert_snapshot!("chat-spoiler-shown-100x30", shown.clone());
    assert_ne!(hidden, shown, "revealing changed nothing");
}

/// The composer in reply mode, with its banner and a half-typed message.
#[test]
fn the_composer_replying() {
    let (mut app, _idle) = in_general("terminal");
    app.handle(Action::FocusChat);
    app.chat
        .select(crate::discord::snowflake::MessageId(500000000000000105));
    app.handle(Action::Reply);
    for c in "and a reply to it".chars() {
        app.key(starkit::crossterm::event::KeyEvent::from(
            starkit::crossterm::event::KeyCode::Char(c),
        ));
    }
    insta::assert_snapshot!("composer-reply-terminal-100x30", render(&mut app, 100, 30));
}

/// The composer's `@` popup, which is the part of it that has geometry.
#[test]
fn the_composer_completing_a_name() {
    let (mut app, _idle) = in_general("terminal");
    app.handle(Action::FocusComposer);
    for c in "hello @al".chars() {
        app.key(starkit::crossterm::event::KeyEvent::from(
            starkit::crossterm::event::KeyCode::Char(c),
        ));
    }
    insta::assert_snapshot!(
        "composer-complete-terminal-100x30",
        render(&mut app, 100, 30)
    );
}

#[test]
fn the_quick_switcher() {
    let (mut app, _idle) = in_general("terminal");
    app.handle(Action::QuickSwitch);
    for c in "gen".chars() {
        app.key(starkit::crossterm::event::KeyEvent::from(
            starkit::crossterm::event::KeyCode::Char(c),
        ));
    }
    insta::assert_snapshot!("quick-terminal-100x30", render(&mut app, 100, 30));
}

#[test]
fn the_confirm_overlay() {
    let (mut app, _idle) = in_general("terminal");
    app.handle(Action::FocusChat);
    app.over.ask(crate::ui::overlays::confirm::Confirm::delete(
        CHANNEL,
        crate::discord::snowflake::MessageId(500000000000000105),
        "here is the thing I meant",
    ));
    insta::assert_snapshot!("confirm-terminal-100x30", render(&mut app, 100, 30));
}

#[test]
fn the_settings_overlay() {
    let (mut app, _idle) = in_general("terminal");
    app.handle(Action::FocusChat);
    app.handle(Action::OpenPanelSettings);
    insta::assert_snapshot!("settings-terminal-100x30", render(&mut app, 100, 30));
}

/// The typing line, which is the one row that is neither a message nor a
/// divider and is always last.
#[test]
fn the_typing_row() {
    // Nine seconds in, which is after the TYPING_START in `#general` and long
    // before the connection drops.
    let (mut app, _idle) = loaded_at("terminal", 9_000);
    insta::assert_snapshot!("chat-typing-terminal-100x30", render(&mut app, 100, 30));
}

/// The picture channel, drawn.
///
/// Everything M4 added at once: an inline image given rows from its declared
/// size, an animated one showing its first frame, a custom emoji inline and on
/// a reaction, a card with its thumbnail against the right edge, avatars in
/// the gutter and icons in the rail.
#[test]
fn the_pictures() {
    let (mut app, _idle) = in_pictures("terminal");
    insta::assert_snapshot!("pictures-halfblocks-100x30", render(&mut app, 100, 30));
}

/// The same channel with no pictures at all, which is the chip it was before.
/// The two snapshots side by side are the whole of what `[ui] graphics` does.
#[test]
fn the_pictures_as_chips() {
    let (mut app, _idle) = loaded("terminal");
    app.open_channel(PICTURES);
    app.tick();
    insta::assert_snapshot!("pictures-chips-100x30", render(&mut app, 100, 30));
}

/// The emoji grid, filtered to the server's own.
#[test]
fn the_emoji_picker() {
    let (mut app, _idle) = in_general("terminal");
    app.handle(Action::FocusComposer);
    app.handle(Action::EmojiPicker);
    for c in "pe".chars() {
        app.key(starkit::crossterm::event::KeyEvent::from(
            starkit::crossterm::event::KeyCode::Char(c),
        ));
    }
    insta::assert_snapshot!("picker-emoji-terminal-100x30", render(&mut app, 100, 30));
}

/// The GIF grid, with the tiles still `░` because nothing has been fetched.
///
/// Which is the state it is in for the first frame after it opens, every time:
/// the tiles are asked for by being drawn, so the frame that places them is
/// the frame before the one that has them.
#[test]
fn the_gif_picker() {
    let (mut app, mut idle) = in_general("terminal");
    app.handle(Action::FocusComposer);
    app.handle(Action::GifPicker);
    // The picker asks for what is trending on the tick after it opens; the
    // replay answers, and the frame after that has titles to draw.
    app.tick();
    idle.pump();
    app.tick();
    insta::assert_snapshot!("picker-gif-terminal-100x30", render(&mut app, 100, 30));
}

/// The media viewer over the picture channel, drawing half blocks.
#[test]
fn the_media_viewer() {
    let (mut app, _idle) = in_pictures("terminal");
    app.handle(Action::FocusChat);
    app.chat
        .select(crate::discord::snowflake::MessageId(500000000000000201));
    app.handle(Action::OpenMedia);
    assert!(app.over.viewer.is_some(), "the viewer did not open");
    insta::assert_snapshot!("viewer-halfblocks-100x30", render(&mut app, 100, 30));
}

/// The search overlay with a page of hits in it.
#[test]
fn the_search_overlay() {
    let (mut app, mut idle) = in_general("terminal");
    app.handle(Action::FocusChat);
    app.handle(Action::Search);
    for c in "the".chars() {
        app.key(starkit::crossterm::event::KeyEvent::from(
            starkit::crossterm::event::KeyCode::Char(c),
        ));
    }
    app.key(starkit::crossterm::event::KeyEvent::from(
        starkit::crossterm::event::KeyCode::Enter,
    ));
    app.tick();
    idle.pump();
    app.tick();
    insta::assert_snapshot!("search-terminal-100x30", render(&mut app, 100, 30));
}

/// A message on its way out, with two files still going up.
#[test]
fn a_message_being_uploaded() {
    use crate::discord::handle::{Nonce, Upload};
    use crate::discord::state::messages::PendingSend;

    let (mut app, idle) = in_general("terminal");
    {
        let mut state = idle.state().write().unwrap();
        state.messages_mut(CHANNEL).add_pending(
            PendingSend::new(Nonce(77), "here they are".into(), None, false).with_attachments(
                vec![
                    Upload::Path("testdata/media/harbour.png".into()),
                    Upload::Path("testdata/media/cat.gif".into()),
                ],
            ),
        );
        state.touch();
    }
    app.apply(crate::discord::Event::UploadProgress {
        nonce: Nonce(77),
        sent: 3,
        total: 8,
    });
    app.tick();
    app.chat.to_bottom();
    insta::assert_snapshot!(
        "composer-uploading-terminal-100x30",
        render(&mut app, 100, 30)
    );
}

/// The right-click menu over a message that is not this account's.
#[test]
fn the_message_menu() {
    let (mut app, _idle) = in_general("terminal");
    app.handle(Action::FocusChat);
    app.chat
        .select(crate::discord::snowflake::MessageId(500000000000000105));
    app.over.open_menu(crate::ui::overlays::menu::Menu::new(
        crate::discord::snowflake::MessageId(500000000000000105),
        false,
    ));
    insta::assert_snapshot!("menu-terminal-100x30", render(&mut app, 100, 30));
}

/// A search result opens the message it names, and `G` comes back to the
/// present.
///
/// End to end through the replay, because the interesting part is the order of
/// the two windows that arrive: the channel's own latest page, and then the
/// page around the message. Landing on the first of them is landing at the
/// bottom of the channel, which looks exactly like nothing happening.
#[test]
fn a_search_result_is_jumped_to_and_g_comes_back() {
    let (mut app, mut idle) = in_general("terminal");
    render(&mut app, 100, 30);
    app.handle(Action::FocusChat);
    app.handle(Action::Search);
    for c in "older".chars() {
        app.key(starkit::crossterm::event::KeyEvent::from(
            starkit::crossterm::event::KeyCode::Char(c),
        ));
    }
    // Run it, and let the replay answer.
    app.key(starkit::crossterm::event::KeyEvent::from(
        starkit::crossterm::event::KeyCode::Enter,
    ));
    app.tick();
    idle.pump();
    app.tick();
    let hits = app
        .over
        .search
        .as_ref()
        .expect("the box is open")
        .hits
        .len();
    assert!(hits > 0, "the search found nothing to jump to");

    // And open the one the cursor is on.
    app.key(starkit::crossterm::event::KeyEvent::from(
        starkit::crossterm::event::KeyCode::Enter,
    ));
    assert!(app.over.search.is_none(), "it stayed open");
    for _ in 0..4 {
        app.tick();
        idle.pump();
        render(&mut app, 100, 30);
    }
    let on = app.chat.selected().expect("a message is selected");
    assert!(
        app.chat.text_at_cursor().contains("older"),
        "the cursor landed on {on:?} rather than the hit"
    );
    // The window it landed in is a page out of the middle of the channel, so
    // the store knows it is not showing the newest message any more. That is
    // what makes `G` a way back rather than a key that does nothing.
    let carried = app.core_state_at_latest().expect("the channel has a store");
    assert!(
        !carried,
        "the jump left the window at the end of the channel"
    );

    // `G` asks for the present back.
    app.handle(Action::End);
    for _ in 0..3 {
        app.tick();
        idle.pump();
        render(&mut app, 100, 30);
    }
    assert!(app.chat.at_bottom());
    assert!(
        app.core_state_at_latest().unwrap(),
        "it is the present again"
    );
}

/// Somebody says this account's name in a channel that is not open: the status
/// bar says so, and says where.
#[test]
fn a_mention_elsewhere_writes_a_line() {
    // Thirteen seconds in, which is after the fixture's MESSAGE_CREATE
    // carrying the mention and before the connection drops.
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/gateway/session.json");
    let session = fake::Session::read(&path).expect("the replay fixture");
    let mention = session
        .steps
        .iter()
        .find(|s| s.dispatch.as_deref() == Some("MESSAGE_CREATE"))
        .and_then(|s| s.data.clone())
        .expect("the fixture carries a mention");

    // Thirteen seconds in, so the message the event names is in the store.
    let (core, _idle) = fake::loaded(&session, 13_000);
    let mut app = App::new(
        core,
        config("terminal"),
        PathBuf::from("/nonexistent/config.toml"),
        None,
        Graphics::disabled(),
    );
    app.tz = jiff::tz::TimeZone::UTC;
    app.tick();
    app.open_channel(CHANNEL);
    app.tick();

    // The channel the mention lands in is `#random`, which is not the one on
    // screen, so it is worth saying something about.
    let channel = crate::discord::snowflake::ChannelId(
        mention["channel_id"].as_str().unwrap().parse().unwrap(),
    );
    let message =
        crate::discord::snowflake::MessageId(mention["id"].as_str().unwrap().parse().unwrap());
    assert_ne!(channel, CHANNEL);
    app.apply(crate::discord::Event::Mention { channel, message });
    let drawn = render(&mut app, 100, 30);
    // A channel nobody has opened has no window for the message to be in --
    // the store refuses to append below a gap it cannot show -- so the line
    // names the place and says somebody, which is what the reader needs.
    assert!(drawn.contains("mentioned you in #announcements"), "{drawn}");

    // With the message held, it says who and what.
    app.note = None;
    let held = crate::discord::snowflake::MessageId(500000000000000105);
    app.apply(crate::discord::Event::Mention {
        channel: CHANNEL,
        message: held,
    });
    app.set_terminal_focus(false);
    app.note = None;
    app.apply(crate::discord::Event::Mention {
        channel: CHANNEL,
        message: held,
    });
    let drawn = render(&mut app, 100, 30);
    assert!(drawn.contains("@Jo in #general:"), "{drawn}");
    app.set_terminal_focus(true);

    // And nothing at all for the channel already on screen while the window
    // is in front.
    app.note = None;
    app.apply(crate::discord::Event::Mention {
        channel: CHANNEL,
        message: held,
    });
    assert!(app.note.is_none(), "it interrupted about what is on screen");
}

/// `space` on a message somebody started a thread from opens the thread, and
/// on any other message it uncovers whatever is hidden.
///
/// One key with two jobs, and which one it does is a fact about the message
/// rather than a mode.
#[test]
fn space_opens_the_thread_a_message_started() {
    let (mut app, _idle) = in_general("terminal");
    render(&mut app, 100, 30);
    app.handle(Action::FocusChat);

    // The fixture hangs `about the deploy` off this message; a thread has the
    // id of the message it was started from, which is the only link between
    // the two.
    let started_from = crate::discord::snowflake::MessageId(500000000000000105);
    app.chat.select(started_from);
    render(&mut app, 100, 30);
    assert_eq!(
        app.chat.selected_thread(),
        Some(crate::discord::snowflake::ChannelId(500000000000000105))
    );
    app.handle(Action::RevealSpoiler);
    app.tick();
    assert_eq!(
        app.chat.channel(),
        Some(crate::discord::snowflake::ChannelId(500000000000000105)),
        "space did not open the thread"
    );
}
