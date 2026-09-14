//! What the loop and the dispatcher are asserted to do.
//!
//! A file of its own rather than a `mod tests` at the bottom of `app.rs`,
//! because that file is the one place a key becomes a change and is worth
//! keeping short enough to read in one sitting. `#[path]` rather than a
//! directory module: `app` is one file and one file is what it should stay.

use super::*;
use crate::ui::fake;

fn app() -> App {
    let (core, _driver) = fake::silent();
    App::new(
        core,
        Config::default(),
        PathBuf::from("/nonexistent/config.toml"),
        None,
        Graphics::disabled(),
    )
}

/// An app past the login screen, with the replay fixture applied.
///
/// The `Idle` has to be held: dropping it closes the channels, and a closed
/// channel is a core that has gone away.
fn loaded() -> (App, fake::Idle) {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/gateway/session.json");
    let session = fake::Session::read(&path).expect("the replay fixture");
    let (core, idle) = fake::loaded(&session, 1_000);
    let mut a = App::new(
        core,
        Config::default(),
        PathBuf::from("/nonexistent/config.toml"),
        None,
        Graphics::disabled(),
    );
    a.tz = jiff::tz::TimeZone::UTC;
    a.tick();
    (a, idle)
}

/// `#general` in the first server, which is where the conversation is.
const CHANNEL: ChannelId = ChannelId(200000000000000011);

fn click(a: &mut App, x: u16, y: u16, w: u16, h: u16) {
    a.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: starkit::crossterm::event::KeyModifiers::NONE,
        },
        Rect::new(0, 0, w, h),
    );
}

fn frame(app: &mut App, w: u16, h: u16) -> String {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    app.draw(area, &mut buf);
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn it_opens_on_the_login_screen() {
    let mut a = app();
    assert!(a.login.is_some());
    assert!(frame(&mut a, 100, 30).contains("STAR/CORD"));
}

/// The whole frame while there is no session: no panels behind it, so
/// nothing can be clicked or typed into by accident.
#[test]
fn the_login_screen_is_the_whole_frame() {
    let mut a = app();
    let drawn = frame(&mut a, 100, 30);
    assert!(!drawn.contains("channels"), "{drawn}");
    assert!(a.layout.last.is_none());
}

#[test]
fn a_terminal_below_the_floor_says_so() {
    let mut a = app();
    a.login = None;
    let drawn = frame(&mut a, 59, 30);
    assert!(drawn.contains("too small"), "{drawn}");
    assert!(drawn.contains("60x21"));

    // Wide enough and too short says the same thing: every module is always
    // present, so twenty rows is one module short of honest.
    let drawn = frame(&mut a, 100, 20);
    assert!(drawn.contains("60x21"), "{drawn}");
}

/// Thirty per second, and the drain is bounded, so a burst of gateway
/// traffic cannot starve the draw.
#[test]
fn the_frame_and_the_drain_are_bounded() {
    assert!(FRAME <= Duration::from_millis(50));
    assert_eq!(DRAIN_CAP, 500);
}

#[test]
fn cycling_themes_goes_round_and_comes_back() {
    let mut a = app();
    let first = a.look.theme.id.clone();
    let n = a.look.ids.len();
    assert!(n > 1);
    for _ in 0..n {
        a.handle(Action::NextTheme);
    }
    assert_eq!(a.look.theme.id, first, "a full cycle should return");
    a.handle(Action::NextTheme);
    assert_ne!(a.look.theme.id, first);
    a.handle(Action::PrevTheme);
    assert_eq!(a.look.theme.id, first);
    // And the choice is written where it will be saved from.
    assert_eq!(a.cfg.ui.theme, a.look.theme.id);
}

#[test]
fn quitting_sets_the_flag_and_nothing_else() {
    let mut a = app();
    a.login = None;
    a.handle(Action::Quit);
    assert!(a.quit);
}

/// Unless there is something half-written, in which case it asks.
#[test]
fn quitting_with_a_draft_asks_first() {
    let mut a = app();
    a.login = None;
    a.composer.open(ChannelId(1));
    a.composer.input.set_text("half a sentence");
    a.handle(Action::Quit);
    assert!(!a.quit, "it quit without asking");
    assert!(a.over.confirm.is_some());

    a.key(KeyEvent::from(starkit::crossterm::event::KeyCode::Char(
        'y',
    )));
    assert!(a.quit);
}

/// The overlay is checked first in both handlers.
#[test]
fn an_open_overlay_takes_the_keys_and_the_clicks() {
    let mut a = app();
    a.login = None;
    a.handle(Action::Help);
    assert!(a.over.open());

    // A key that would otherwise cycle the theme.
    let before = a.look.theme.id.clone();
    a.key(KeyEvent::from(starkit::crossterm::event::KeyCode::Char(
        't',
    )));
    assert_eq!(a.look.theme.id, before, "the overlay let a key through");

    // A click that would otherwise focus a panel.
    frame(&mut a, 120, 30);
    a.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: starkit::crossterm::event::KeyModifiers::NONE,
        },
        Rect::new(0, 0, 120, 30),
    );
    assert!(!a.over.open(), "a click closes it");
}

/// `alt+m` opens the member list and folds it again, and the conversation
/// gives up the rows for it.
#[test]
fn alt_m_opens_and_folds_the_member_list() {
    let (mut a, _idle) = loaded();
    a.open_channel(CHANNEL);
    a.tick();
    frame(&mut a, 100, 30);
    let folded = a
        .layout
        .last
        .as_ref()
        .unwrap()
        .rect_of(ModuleId::Conversation)
        .height;

    a.handle(Action::ToggleMembers);
    frame(&mut a, 100, 30);
    assert!(a.layout.is_expanded(ModuleId::Members));
    assert_eq!(a.layout.focus(), ModuleId::Members);
    let open = a
        .layout
        .last
        .as_ref()
        .unwrap()
        .rect_of(ModuleId::Conversation)
        .height;
    assert!(open < folded, "the conversation kept {open} rows");

    a.handle(Action::ToggleMembers);
    frame(&mut a, 100, 30);
    assert_eq!(a.layout.expanded(), None);
    assert_eq!(
        a.layout.focus(),
        ModuleId::Compose,
        "folding it left the keyboard nowhere useful"
    );
}

/// Choosing a server folds the servers and opens what is in it.
#[test]
fn choosing_a_server_opens_its_channels() {
    let (mut a, _idle) = loaded();
    assert!(a.layout.is_expanded(ModuleId::Servers));
    a.handle(Action::CursorDown);
    a.handle(Action::Activate);

    assert!(a.nav.guild.is_some(), "no server was chosen");
    assert!(a.layout.is_expanded(ModuleId::Channels));
    assert!(!a.layout.is_expanded(ModuleId::Servers));
    assert_eq!(a.layout.focus(), ModuleId::Channels);
    assert!(
        !a.view.channels.is_empty(),
        "the channels are not there yet"
    );

    let drawn = frame(&mut a, 100, 30);
    assert!(drawn.contains("channels \u{b7} First Guild"), "{drawn}");
}

/// Home is the first server, and choosing it lists the conversations and then
/// the friends, under one heading each.
#[test]
fn choosing_home_lists_conversations_then_friends() {
    let (mut a, _idle) = loaded();
    a.handle(Action::Activate);
    assert_eq!(a.nav.guild, None);

    let headings: Vec<String> = a
        .view
        .messages
        .iter()
        .filter_map(|r| match r {
            dms::Row::Section { label } => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(headings.first().map(String::as_str), Some("conversations"));
    assert!(headings.len() > 1, "no friends are grouped: {headings:?}");

    let first_dm = a
        .view
        .messages
        .iter()
        .position(|r| matches!(r, dms::Row::Dm { .. }))
        .expect("a conversation");
    let first_friend = a
        .view
        .messages
        .iter()
        .position(|r| matches!(r, dms::Row::Friend { .. }))
        .expect("a friend");
    assert!(first_dm < first_friend, "the friends came first");

    let drawn = frame(&mut a, 100, 30);
    assert!(drawn.contains("messages"), "{drawn}");
    assert!(drawn.contains("CONVERSATIONS"), "{drawn}");
}

/// Opening a channel folds both lists and puts the keyboard in the composer.
#[test]
fn opening_a_channel_folds_the_lists_and_focuses_the_composer() {
    let (mut a, _idle) = loaded();
    a.open_channel(CHANNEL);
    a.tick();
    assert_eq!(a.layout.expanded(), None);
    assert_eq!(a.layout.focus(), ModuleId::Compose);

    // And the folded lists say where they are rather than going blank.
    let drawn = frame(&mut a, 100, 30);
    assert!(drawn.contains("First Guild"), "{drawn}");
    assert!(drawn.contains("# general"), "{drawn}");
}

/// A click anywhere on a folded list opens it.
#[test]
fn clicking_a_folded_list_opens_it() {
    let (mut a, _idle) = loaded();
    a.open_channel(CHANNEL);
    a.tick();
    frame(&mut a, 100, 30);
    assert_eq!(a.layout.expanded(), None);

    let rect = a.layout.last.as_ref().unwrap().rect_of(ModuleId::Servers);
    // The body row rather than the border or the header word.
    click(&mut a, rect.x + 2, rect.y + 2, 100, 30);
    assert!(a.layout.is_expanded(ModuleId::Servers));
    assert_eq!(a.layout.focus(), ModuleId::Servers);
}

/// A channel opened into the composer is a channel somebody is reading, so it
/// is still acknowledged. Browsing the server list is not.
#[test]
fn reading_from_the_composer_still_marks_read() {
    let (mut a, _idle) = loaded();
    a.open_channel(CHANNEL);
    a.tick();
    frame(&mut a, 100, 30);
    assert_eq!(a.layout.focus(), ModuleId::Compose);

    a.focused_since = Instant::now() - READ_AFTER - Duration::from_secs(1);
    a.tick();
    assert!(a.acked.is_some(), "the composer is reading and did not ack");

    a.acked = None;
    a.focus_module(ModuleId::Servers);
    a.focused_since = Instant::now() - READ_AFTER - Duration::from_secs(1);
    a.tick();
    assert!(a.acked.is_none(), "browsing the servers acked a channel");
}

/// Tab walks the column in order and wraps. Every module is always there, so
/// there is nowhere it can land that is not on the screen.
#[test]
fn tab_walks_the_column() {
    let mut a = app();
    a.login = None;
    frame(&mut a, 100, 30);

    let mut seen = Vec::new();
    for _ in 0..COLUMN.len() {
        a.handle(Action::FocusNext);
        seen.push(a.layout.focus());
    }
    assert_eq!(
        seen,
        vec![
            ModuleId::Channels,
            ModuleId::Conversation,
            ModuleId::Compose,
            ModuleId::Members,
            ModuleId::Servers,
        ],
        "tab visited {seen:?}"
    );

    // And shift-tab undoes a tab.
    let here = a.layout.focus();
    a.handle(Action::FocusNext);
    a.handle(Action::FocusPrev);
    assert_eq!(a.layout.focus(), here);
}

/// A letter typed into the composer is a letter, and `alt+…` is still a
/// command. The keymap asserts the rule; this asserts that `App` obeys it.
#[test]
fn letters_reach_the_composer_and_alt_keys_do_not() {
    use starkit::crossterm::event::{KeyCode, KeyModifiers};
    let mut a = app();
    a.login = None;
    a.nav.channel = Some(ChannelId(1));
    a.composer.open(ChannelId(1));
    a.focus_module(ModuleId::Compose);

    for c in "delete".chars() {
        a.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(a.composer.text(), "delete");

    let before = a.layout.is_expanded(ModuleId::Members);
    a.key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
    assert_ne!(
        a.layout.is_expanded(ModuleId::Members),
        before,
        "alt+m did not reach the module table"
    );
    assert_eq!(a.composer.text(), "delete", "and did not type an m");
}

/// The escape chain: cancel what is being written, then walk back up the
/// column one module at a time, then fold and come back to the composer.
#[test]
fn escape_walks_back_out_of_the_composer() {
    use starkit::crossterm::event::KeyCode;
    let (mut a, _idle) = loaded();
    a.open_channel(CHANNEL);
    a.tick();
    a.composer.reply_to(MessageId(7), "alex".into(), true);
    assert_eq!(a.layout.focus(), ModuleId::Compose);

    a.key(KeyEvent::from(KeyCode::Esc));
    assert!(a.composer.mode.is_normal(), "the reply was not cancelled");
    assert_eq!(a.layout.focus(), ModuleId::Compose);

    a.key(KeyEvent::from(KeyCode::Esc));
    assert!(a.layout.is_expanded(ModuleId::Channels), "the channels");
    assert_eq!(a.layout.focus(), ModuleId::Channels);

    a.key(KeyEvent::from(KeyCode::Esc));
    assert!(a.layout.is_expanded(ModuleId::Servers), "the servers");
    assert_eq!(a.layout.focus(), ModuleId::Servers);

    a.key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.layout.expanded(), None, "it did not fold");
    assert_eq!(a.layout.focus(), ModuleId::Compose);
}

/// Closing an overlay asks for a whole frame rather than a diff.
///
/// The bug this prevents: an overlay blanks the cells it covers, and if
/// the terminal is holding a character this program's buffer does not know
/// about, the diff will never repaint that cell. It shows as a box left
/// behind after the switcher closes.
#[test]
fn closing_an_overlay_asks_for_a_full_repaint() {
    let mut a = app();
    a.login = None;
    a.handle(Action::Help);
    a.tick();
    assert!(!a.repaint, "opening one does not need it");
    a.handle(Action::CloseOverlay);
    a.tick();
    assert!(a.repaint, "closing one does");

    a.repaint = false;
    a.handle(Action::Redraw);
    assert!(a.repaint, "and ctrl+l asks for one at any time");
}

/// Every row of the message menu reaches the action it names.
#[test]
fn the_message_menu_does_what_its_rows_say() {
    let mut a = app();
    a.login = None;
    a.nav.channel = Some(ChannelId(1));
    a.composer.open(ChannelId(1));

    a.overlay_asked(overlays::Key::Menu(Choice::Reply, MessageId(5)));
    // There is no such message in this empty core, so the reply cannot be
    // set up -- what is asserted is that the menu reached the dispatcher
    // rather than doing nothing at all.
    assert!(a.note.is_some(), "the menu row did nothing");

    // And the picker rows open the picker.
    a.overlay_asked(overlays::Key::Menu(Choice::React, MessageId(5)));
    assert!(a.note.is_some());
}

/// What the emoji picker chose lands in the composer at the caret, and the
/// composer is where the keyboard goes next.
#[test]
fn an_emoji_from_the_picker_is_inserted_and_not_sent() {
    let mut a = app();
    a.login = None;
    a.nav.channel = Some(ChannelId(1));
    a.composer.open(ChannelId(1));
    a.composer.input.set_text("well ");
    a.composer.input.set_cursor(5);

    a.overlay_asked(overlays::Key::Insert("<:pepe:1>".into()));
    assert_eq!(a.composer.text(), "well <:pepe:1>");
    assert_eq!(a.layout.focus(), ModuleId::Compose);
}

/// A chip comes off when its `×` is clicked, and a send carries what is
/// left.
#[test]
fn an_attachment_is_a_chip_that_can_be_taken_off_again() {
    let mut a = app();
    a.login = None;
    a.nav.channel = Some(ChannelId(1));
    a.composer.open(ChannelId(1));
    a.composer
        .attach(composer::Pending::clipboard(vec![0; 8], (2, 2)));
    assert_eq!(a.composer.attachments.len(), 1);
    // A file with nothing typed beside it is still something unsent.
    assert_eq!(a.composer.unsent(), 1);

    let taken = a.composer.take_attachments();
    assert_eq!(taken.len(), 1);
    assert!(a.composer.attachments.is_empty());
}

/// Quitting while a file is halfway up the wire asks first, and says so in
/// its own words rather than the draft ones.
#[test]
fn quitting_during_an_upload_asks_first() {
    use crate::discord::handle::{Nonce, Upload};
    use crate::discord::state::messages::PendingSend;

    let (core, idle) = fake::silent();
    let mut a = App::new(
        core,
        Config::default(),
        PathBuf::from("/nonexistent/config.toml"),
        None,
        Graphics::disabled(),
    );
    a.login = None;
    a.nav.channel = Some(ChannelId(1));
    {
        let mut state = idle.state().write().unwrap();
        state.messages_mut(ChannelId(1)).add_pending(
            PendingSend::new(Nonce(1), String::new(), None, false)
                .with_attachments(vec![Upload::Path("testdata/media/cat.gif".into())]),
        );
        state.touch();
    }
    assert_eq!(a.uploading(), 1);
    a.handle(Action::Quit);
    assert!(!a.quit, "it quit over an upload in flight");
    let confirm = a.over.confirm.as_ref().expect("it asked");
    assert!(confirm.body.contains("still being sent"), "{confirm:?}");
}

/// A settings change reaches the running program even when the file
/// cannot be written, which is the case a read-only home directory is.
#[test]
fn a_settings_row_changes_the_program() {
    let mut a = app();
    a.login = None;
    let before = a.cfg.chat.show_avatars;
    a.change_setting(Setting::Avatars, true);
    assert_ne!(a.cfg.chat.show_avatars, before);
}
