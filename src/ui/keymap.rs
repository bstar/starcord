//! One table describing every action, its keys and its help text.
//!
//! The table is the only place a key is written down. Dispatch reads it, the
//! help overlay reads it, and `docs/keys-and-mouse.md` is generated from it, so
//! the three cannot disagree — which is the failure this arrangement exists to
//! prevent, and the reason the module compiles with no reference to
//! [`App`](super::app::App) at all.
//!
//! ## Three layers
//!
//! A key is offered to the focused panel first and to the global table second.
//! Both come out of [`BINDINGS`]: a binding's `group` says which, through
//! [`GROUPS`]. A panel only ever *adds* meaning — it never swallows a key it
//! has no use for — so every global binding keeps working from everywhere,
//! which is what makes `q` and `?` reliable.
//!
//! Above both sits the composer, which eats raw keys while it has focus
//! because it is a text field and `d` in a sentence is a letter. The line it
//! draws is [`composer_eats`], and the rule is that **every `alt+…` in the
//! global table falls through it**. That is what keeps `alt+m` closing the
//! member list while somebody is halfway through a word, and it is asserted by
//! a test rather than left as an intention.
//!
//! ## Invariants
//!
//! Carried over from STAR/AMP, where they were learned the expensive way, and
//! each one is a test at the bottom of this file:
//!
//! - a bare arrow moves one, a shifted one moves ten;
//! - every key in the table can be *spelled* in the table: the key column
//!   separates alternatives on `/`, so `alt+/` has no spelling here and the
//!   server-wide search is `alt+f` beside the `ctrl+f` that searches one
//!   channel;
//! - `hjkl` navigates and never adjusts a value;
//! - `esc` never quits;
//! - a label is at most 19 characters, or the help overlay wraps it;
//! - every group appears in one run of the table, or its heading prints twice.

use std::sync::LazyLock;

use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::keymap::{Binding as KitBinding, Keymap, MouseHelp};

pub type Binding = KitBinding<Action>;

/// Everything the UI can be asked to do.
///
/// One flat enum rather than one per panel: the dispatcher is a single `match`
/// in `app.rs`, and an action that two panels can both produce — `Activate`,
/// `Back`, every cursor move — should be one variant or the `match` grows two
/// arms that do the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    // Moving about.
    CursorUp,
    CursorDown,
    CursorUpBig,
    CursorDownBig,
    PageUp,
    PageDown,
    Home,
    End,
    Activate,
    Back,

    // Focus.
    FocusNext,
    FocusPrev,
    FocusGuilds,
    FocusChannels,
    FocusDms,
    FocusChat,
    FocusComposer,
    FocusMembers,

    // Getting somewhere.
    NextGuild,
    PrevGuild,
    NextUnread,
    PrevUnread,
    QuickSwitch,
    Search,
    SearchGuild,
    ToggleCollapse,
    JumpToReply,

    // A message.
    Reply,
    ReplyNoPing,
    Edit,
    Delete,
    React,
    Yank,
    YankLink,
    OpenExternal,
    OpenMedia,
    MarkRead,
    RevealSpoiler,
    TogglePin,
    LoadOlder,
    ToBottom,
    CopyMessageLink,

    // Writing one.
    Send,
    Newline,
    EmojiPicker,
    GifPicker,
    Attach,
    PasteImage,
    CancelCompose,
    ClearComposer,
    EditLast,

    // The dock.
    ToggleGuilds,
    ToggleChannels,
    ToggleDms,
    ToggleMembers,
    ToggleZen,
    OpenPanelSettings,
    ClosePanel,

    // The media viewer.
    MediaNext,
    MediaPrev,
    MediaSave,
    MediaZoom,

    // The application.
    Quit,
    Help,
    NextTheme,
    PrevTheme,
    ToggleTimestamps,
    ToggleAvatars,
    CycleAnimate,
    Reconnect,
    Redraw,
    CloseOverlay,
}

/// Which panel a key is offered to first.
///
/// Mirrors [`PanelId`](super::panels::PanelId) and is its own type so that this
/// module does not depend on the panels, which depend on it. `Media` and
/// `Picker` are overlays rather than panels and have no `PanelId`, which is the
/// other half of why the two enums are separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Module {
    Guilds,
    Channels,
    Dms,
    Chat,
    Composer,
    Members,
    Media,
    Picker,
}

/// Where a group of bindings applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Reachable from anywhere, once the focused panel has declined the key.
    Global,
    /// Only while one of these has focus.
    Modules(&'static [Module]),
}

/// The list panels, which share their fold and unfold keys.
const LISTS: &[Module] = &[
    Module::Guilds,
    Module::Channels,
    Module::Dms,
    Module::Members,
];

/// Every group in [`BINDINGS`], and where it applies.
///
/// The scope is a property of the group rather than of the binding so that the
/// help overlay's headings and the dispatcher's layers are the same division.
/// A group in the table and not in here is a compile-time-invisible mistake,
/// so there is a test.
pub const GROUPS: &[(&str, Scope)] = &[
    ("navigation", Scope::Global),
    ("lists", Scope::Modules(LISTS)),
    ("chat", Scope::Modules(&[Module::Chat])),
    ("composer", Scope::Modules(&[Module::Composer])),
    ("pickers", Scope::Modules(&[Module::Picker])),
    ("media viewer", Scope::Modules(&[Module::Media])),
    ("panels", Scope::Global),
    ("appearance", Scope::Global),
    ("application", Scope::Global),
];

/// Every key, in the order the help overlay prints them.
pub const BINDINGS: &[Binding] = &[
    // -- navigation --------------------------------------------------------
    Binding {
        action: Action::FocusNext,
        keys: "tab",
        label: "next panel",
        group: "navigation",
    },
    Binding {
        action: Action::FocusPrev,
        keys: "shift+tab",
        label: "previous panel",
        group: "navigation",
    },
    Binding {
        action: Action::FocusGuilds,
        keys: "alt+1",
        label: "focus servers",
        group: "navigation",
    },
    Binding {
        action: Action::FocusChannels,
        keys: "alt+2",
        label: "focus channels",
        group: "navigation",
    },
    Binding {
        action: Action::FocusDms,
        keys: "alt+3",
        label: "focus messages",
        group: "navigation",
    },
    Binding {
        action: Action::FocusChat,
        keys: "alt+4",
        label: "focus chat",
        group: "navigation",
    },
    Binding {
        action: Action::FocusComposer,
        keys: "alt+5 / i",
        label: "write a message",
        group: "navigation",
    },
    Binding {
        action: Action::FocusMembers,
        keys: "alt+6",
        label: "focus members",
        group: "navigation",
    },
    Binding {
        action: Action::CursorUp,
        keys: "up / k",
        label: "up one",
        group: "navigation",
    },
    Binding {
        action: Action::CursorDown,
        keys: "down / j",
        label: "down one",
        group: "navigation",
    },
    Binding {
        action: Action::CursorUpBig,
        keys: "shift+up/K",
        label: "up ten",
        group: "navigation",
    },
    Binding {
        action: Action::CursorDownBig,
        keys: "shift+down/J",
        label: "down ten",
        group: "navigation",
    },
    Binding {
        action: Action::PageUp,
        keys: "pgup",
        label: "page up",
        group: "navigation",
    },
    Binding {
        action: Action::PageDown,
        keys: "pgdn",
        label: "page down",
        group: "navigation",
    },
    Binding {
        action: Action::Home,
        keys: "home / gg",
        label: "to the top",
        group: "navigation",
    },
    Binding {
        action: Action::End,
        keys: "end / G",
        label: "to the bottom",
        group: "navigation",
    },
    Binding {
        action: Action::Activate,
        keys: "enter",
        label: "open",
        group: "navigation",
    },
    Binding {
        action: Action::Back,
        keys: "esc",
        label: "back, or mark read",
        group: "navigation",
    },
    Binding {
        action: Action::PrevGuild,
        keys: "[",
        label: "previous server",
        group: "navigation",
    },
    Binding {
        action: Action::NextGuild,
        keys: "]",
        label: "next server",
        group: "navigation",
    },
    Binding {
        action: Action::PrevUnread,
        keys: "alt+up",
        label: "previous unread",
        group: "navigation",
    },
    Binding {
        action: Action::NextUnread,
        keys: "alt+down",
        label: "next unread",
        group: "navigation",
    },
    Binding {
        action: Action::QuickSwitch,
        keys: "ctrl+k",
        label: "jump to anything",
        group: "navigation",
    },
    Binding {
        action: Action::Search,
        keys: "ctrl+f / /",
        label: "search here",
        group: "navigation",
    },
    Binding {
        action: Action::SearchGuild,
        keys: "alt+f",
        label: "search the server",
        group: "navigation",
    },
    // -- lists -------------------------------------------------------------
    Binding {
        action: Action::ToggleCollapse,
        keys: "h / l",
        label: "fold, unfold",
        group: "lists",
    },
    // -- chat --------------------------------------------------------------
    Binding {
        action: Action::Reply,
        keys: "r",
        label: "reply",
        group: "chat",
    },
    Binding {
        action: Action::ReplyNoPing,
        keys: "R",
        label: "reply without ping",
        group: "chat",
    },
    Binding {
        action: Action::Edit,
        keys: "e",
        label: "edit mine",
        group: "chat",
    },
    Binding {
        action: Action::Delete,
        keys: "d",
        label: "delete mine",
        group: "chat",
    },
    Binding {
        action: Action::React,
        keys: "+",
        label: "react",
        group: "chat",
    },
    Binding {
        action: Action::Yank,
        keys: "y",
        label: "copy the text",
        group: "chat",
    },
    Binding {
        action: Action::YankLink,
        keys: "Y",
        label: "copy the link",
        group: "chat",
    },
    Binding {
        action: Action::CopyMessageLink,
        keys: "ctrl+y",
        label: "copy a jump link",
        group: "chat",
    },
    Binding {
        action: Action::OpenExternal,
        keys: "o",
        label: "open elsewhere",
        group: "chat",
    },
    Binding {
        action: Action::OpenMedia,
        keys: "enter",
        label: "view attachment",
        group: "chat",
    },
    Binding {
        action: Action::RevealSpoiler,
        keys: "space",
        label: "reveal, or a thread",
        group: "chat",
    },
    Binding {
        action: Action::JumpToReply,
        keys: "u",
        label: "go to the quoted",
        group: "chat",
    },
    Binding {
        action: Action::TogglePin,
        keys: "p",
        label: "pin, unpin",
        group: "chat",
    },
    Binding {
        action: Action::MarkRead,
        keys: "m",
        label: "mark read",
        group: "chat",
    },
    Binding {
        action: Action::LoadOlder,
        keys: "ctrl+u",
        label: "load older",
        group: "chat",
    },
    Binding {
        action: Action::ToBottom,
        keys: "ctrl+e",
        label: "to the newest",
        group: "chat",
    },
    // -- composer ----------------------------------------------------------
    Binding {
        action: Action::Send,
        keys: "enter",
        label: "send",
        group: "composer",
    },
    Binding {
        action: Action::Newline,
        keys: "shift+enter",
        label: "new line",
        group: "composer",
    },
    Binding {
        action: Action::Newline,
        keys: "alt+enter",
        label: "new line as well",
        group: "composer",
    },
    Binding {
        action: Action::EditLast,
        keys: "up",
        label: "edit the last",
        group: "composer",
    },
    Binding {
        action: Action::ClearComposer,
        keys: "ctrl+u",
        label: "clear it",
        group: "composer",
    },
    Binding {
        action: Action::EmojiPicker,
        keys: "ctrl+e",
        label: "emoji",
        group: "composer",
    },
    Binding {
        action: Action::GifPicker,
        keys: "ctrl+g",
        label: "a GIF",
        group: "composer",
    },
    Binding {
        action: Action::Attach,
        keys: "alt+a",
        label: "attach a file",
        group: "composer",
    },
    Binding {
        action: Action::PasteImage,
        keys: "ctrl+v",
        label: "paste a picture",
        group: "composer",
    },
    Binding {
        action: Action::CancelCompose,
        keys: "esc",
        label: "cancel",
        group: "composer",
    },
    // -- pickers -----------------------------------------------------------
    Binding {
        action: Action::Activate,
        keys: "enter",
        label: "use this one",
        group: "pickers",
    },
    Binding {
        action: Action::GifPicker,
        keys: "ctrl+g",
        label: "emoji to GIFs",
        group: "pickers",
    },
    Binding {
        action: Action::CloseOverlay,
        keys: "esc",
        label: "close",
        group: "pickers",
    },
    // -- media viewer ------------------------------------------------------
    Binding {
        action: Action::MediaPrev,
        keys: "h",
        label: "previous",
        group: "media viewer",
    },
    Binding {
        action: Action::MediaNext,
        keys: "l",
        label: "next",
        group: "media viewer",
    },
    Binding {
        action: Action::MediaSave,
        keys: "s",
        label: "save it",
        group: "media viewer",
    },
    Binding {
        action: Action::MediaZoom,
        keys: "z",
        label: "fit, actual size",
        group: "media viewer",
    },
    Binding {
        action: Action::OpenExternal,
        keys: "o",
        label: "open elsewhere",
        group: "media viewer",
    },
    Binding {
        action: Action::Yank,
        keys: "y",
        label: "copy its link",
        group: "media viewer",
    },
    Binding {
        action: Action::CloseOverlay,
        keys: "esc",
        label: "close",
        group: "media viewer",
    },
    // -- panels ------------------------------------------------------------
    Binding {
        action: Action::ToggleGuilds,
        keys: "alt+g",
        label: "servers",
        group: "panels",
    },
    Binding {
        action: Action::ToggleChannels,
        keys: "alt+c",
        label: "channels",
        group: "panels",
    },
    Binding {
        action: Action::ToggleDms,
        keys: "alt+d",
        label: "direct messages",
        group: "panels",
    },
    Binding {
        action: Action::ToggleMembers,
        keys: "alt+m",
        label: "members",
        group: "panels",
    },
    Binding {
        action: Action::ToggleZen,
        keys: "alt+z",
        label: "chat only",
        group: "panels",
    },
    Binding {
        action: Action::OpenPanelSettings,
        keys: "alt+s",
        label: "panel settings",
        group: "panels",
    },
    Binding {
        action: Action::ClosePanel,
        keys: "alt+x",
        label: "close this panel",
        group: "panels",
    },
    // -- appearance --------------------------------------------------------
    Binding {
        action: Action::NextTheme,
        keys: "t",
        label: "next theme",
        group: "appearance",
    },
    Binding {
        action: Action::PrevTheme,
        keys: "T",
        label: "previous theme",
        group: "appearance",
    },
    Binding {
        action: Action::ToggleTimestamps,
        keys: "alt+t",
        label: "timestamps",
        group: "appearance",
    },
    Binding {
        action: Action::ToggleAvatars,
        keys: "alt+v",
        label: "avatars",
        group: "appearance",
    },
    Binding {
        action: Action::CycleAnimate,
        keys: "alt+n",
        label: "animate GIFs",
        group: "appearance",
    },
    // -- application -------------------------------------------------------
    Binding {
        action: Action::Help,
        keys: "? / F1",
        label: "this list",
        group: "application",
    },
    Binding {
        action: Action::Reconnect,
        keys: "ctrl+r",
        label: "reconnect now",
        group: "application",
    },
    Binding {
        action: Action::Redraw,
        keys: "ctrl+l",
        label: "redraw the screen",
        group: "application",
    },
    Binding {
        action: Action::Quit,
        keys: "q / ctrl+c",
        label: "quit",
        group: "application",
    },
];

/// What the mouse does. Same one-table rule: a gesture that is implemented and
/// not listed here is a gesture nobody will find.
pub const MOUSE: &[MouseHelp] = &[
    MouseHelp {
        gesture: "wheel",
        label: "scroll three rows",
        group: "chat",
    },
    MouseHelp {
        gesture: "click",
        label: "select a message",
        group: "chat",
    },
    MouseHelp {
        gesture: "double-click",
        label: "open the attachment",
        group: "chat",
    },
    MouseHelp {
        gesture: "click a reaction",
        label: "add or remove yours",
        group: "chat",
    },
    MouseHelp {
        gesture: "click a link",
        label: "open it",
        group: "chat",
    },
    MouseHelp {
        gesture: "click the ↩ line",
        label: "go to the quoted",
        group: "chat",
    },
    MouseHelp {
        gesture: "click ↓ n new",
        label: "jump to the newest",
        group: "chat",
    },
    MouseHelp {
        gesture: "right-click",
        label: "what can be done",
        group: "chat",
    },
    MouseHelp {
        gesture: "drag the scrollbar",
        label: "scroll to anywhere",
        group: "chat",
    },
    MouseHelp {
        gesture: "click, wheel",
        label: "choose one",
        group: "lists",
    },
    MouseHelp {
        gesture: "double-click",
        label: "open a channel",
        group: "lists",
    },
    MouseHelp {
        gesture: "click a category",
        label: "fold or unfold it",
        group: "lists",
    },
    MouseHelp {
        gesture: "click",
        label: "place the caret",
        group: "composer",
    },
    MouseHelp {
        gesture: "click a chip's ×",
        label: "drop the attachment",
        group: "composer",
    },
    MouseHelp {
        gesture: "drag a seam",
        label: "resize two panels",
        group: "panels",
    },
    MouseHelp {
        gesture: "click a header word",
        label: "what the word says",
        group: "panels",
    },
    MouseHelp {
        gesture: "click ? help",
        label: "open this list",
        group: "status",
    },
    MouseHelp {
        gesture: "click the channel",
        label: "jump to anything",
        group: "status",
    },
    MouseHelp {
        gesture: "click the state",
        label: "reconnect now",
        group: "status",
    },
];

/// The global half of the table, keyed.
static GLOBAL: LazyLock<Keymap<Action>> = LazyLock::new(|| Keymap::from_table(&global_bindings()));

/// A panel's own half, one map each, built once.
static MODULES: LazyLock<Vec<(Module, Keymap<Action>)>> = LazyLock::new(|| {
    ALL_MODULES
        .iter()
        .map(|&m| (m, Keymap::from_table(&module_bindings(m))))
        .collect()
});

const ALL_MODULES: &[Module] = &[
    Module::Guilds,
    Module::Channels,
    Module::Dms,
    Module::Chat,
    Module::Composer,
    Module::Members,
    Module::Media,
    Module::Picker,
];

fn scope_of(group: &str) -> Scope {
    GROUPS
        .iter()
        .find(|(name, _)| *name == group)
        .map(|(_, scope)| *scope)
        // A group nobody declared is global, which is the safe end to fail
        // toward: the key works from everywhere rather than from nowhere. The
        // test below is what actually catches it.
        .unwrap_or(Scope::Global)
}

fn global_bindings() -> Vec<Binding> {
    BINDINGS
        .iter()
        .filter(|b| scope_of(b.group) == Scope::Global)
        .map(copy_binding)
        .collect()
}

fn module_bindings(m: Module) -> Vec<Binding> {
    BINDINGS
        .iter()
        .filter(|b| match scope_of(b.group) {
            Scope::Global => false,
            Scope::Modules(list) => list.contains(&m),
        })
        .map(copy_binding)
        .collect()
}

/// `Binding` is not `Copy` -- `A` is, but the struct is not declared so -- and
/// every field in it is `'static`, so this is a move of four words.
fn copy_binding(b: &Binding) -> Binding {
    Binding {
        action: b.action,
        keys: b.keys,
        label: b.label,
        group: b.group,
    }
}

/// The focused panel's own bindings, tried first.
///
/// `None` means the panel does not want this key and the global table should
/// have it. A panel only ever adds meaning; it never swallows a key it has no
/// use for, or the global bindings would stop working panel by panel.
pub fn module(m: Module, k: KeyEvent) -> Option<Action> {
    MODULES
        .iter()
        .find(|(id, _)| *id == m)
        .and_then(|(_, map)| map.resolve(k))
}

/// The global table, tried after the focused panel has declined.
pub fn resolve(k: KeyEvent) -> Option<Action> {
    GLOBAL.resolve(k)
}

/// Where a two-key sequence stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixKey {
    /// `g` was pressed; the next key completes it.
    Waiting,
    /// The sequence completed.
    Action(Action),
    /// Not part of a sequence.
    None,
}

/// `gg` to the top, the way every vim-shaped list does it.
///
/// Kept here rather than in the dispatcher because it is part of the key
/// scheme: `g` alone must resolve to nothing in the global table or the first
/// half of the sequence would do something on its own.
pub fn g_prefix(pending: &mut bool, k: KeyEvent) -> PrefixKey {
    let plain = !k
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    if *pending {
        *pending = false;
        if plain && k.code == KeyCode::Char('g') {
            return PrefixKey::Action(Action::Home);
        }
        // Anything else cancels the sequence and is then handled normally, so
        // `g` followed by a mistake does not eat the key after it.
        return PrefixKey::None;
    }
    if plain && k.code == KeyCode::Char('g') {
        *pending = true;
        return PrefixKey::Waiting;
    }
    PrefixKey::None
}

/// Whether the composer, having focus, takes this key as typing.
///
/// The line between "text" and "command" while a text field has focus. Two
/// rules decide it, and both are asserted below:
///
/// - **every `alt+…` falls through**, so the panel and appearance keys work
///   mid-sentence. The composer's own `alt+a` and `alt+enter` are the two
///   exceptions, and they are named;
/// - **no plain letter is needed to leave**, so nothing a person types can
///   strand them: `tab`, `esc` and `alt+…` are the ways out and none of them
///   is a letter.
pub fn composer_eats(k: KeyEvent) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);

    if alt {
        // The composer's own, and nothing else. `alt+enter` is a newline and
        // `alt+a` attaches a file; every other alt key belongs to the panels
        // and the appearance table and has to survive a half-typed word.
        return matches!(k.code, KeyCode::Enter | KeyCode::Char('a'));
    }
    if ctrl {
        // Line editing, the pickers, the clipboard, and nothing else.
        //
        // The list is exactly what the composer acts on, which is a test next
        // door rather than an intention: a key eaten here and handled by
        // nobody is a key that does nothing and cannot be rebound to anything
        // that would. `ctrl+c` is deliberately absent -- quitting works from
        // inside the composer as it does from everywhere else -- and so are
        // the readline motions the text field has no implementation for.
        return matches!(
            k.code,
            KeyCode::Char('a')
                | KeyCode::Char('e')
                | KeyCode::Char('g')
                | KeyCode::Char('k')
                | KeyCode::Char('u')
                | KeyCode::Char('v')
                | KeyCode::Char('w')
        );
    }
    matches!(
        k.code,
        KeyCode::Char(_)
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Enter
    )
}

/// What `docs/keys-and-mouse.md` says before the tables.
const HEADER: &str = "\
# Keys and the mouse

Every key STAR/CORD knows, in the order the `?` overlay prints them. This file
is generated from the table in `src/ui/keymap.rs`, and a test fails if the two
disagree.

A key is offered to the focused panel first and to the global table second, so
a binding under a panel heading works while that panel has focus and the global
ones work from everywhere. The composer is the exception: while it has focus it
takes raw keys, because `d` in a sentence is a letter. Every `alt+\u{2026}`
falls through it, which is what keeps the panel keys working mid-word, and no
plain letter is needed to leave it, so nothing you type can strand you.
";

/// The key table as `docs/keys-and-mouse.md`.
///
/// Generated rather than written, because a document that repeats a table is a
/// document that drifts from it. The test below compares this to the committed
/// file; `STARCORD_UPDATE_DOCS=1 cargo test` rewrites it.
pub fn document() -> String {
    let mut out = String::new();
    out.push_str(HEADER);

    let mut group = "";
    for b in BINDINGS {
        if b.group != group {
            group = b.group;
            let scope = match scope_of(group) {
                Scope::Global => "everywhere".to_string(),
                Scope::Modules(list) => {
                    let names: Vec<&str> = list.iter().map(|m| module_name(*m)).collect();
                    format!("in {}", names.join(", "))
                }
            };
            out.push_str(&format!("\n## {group}\n\n_{scope}_\n\n"));
            out.push_str("| key | what it does |\n|---|---|\n");
        }
        // Padded to the same column the `?` overlay uses, so the file reads as
        // a table in an editor as well as in a browser, and so there is one
        // number rather than two.
        let keys = format!("`{}`", b.keys);
        out.push_str(&format!(
            "| {keys:<width$} | {} |\n",
            b.label,
            width = starkit::keymap::KEYS_COLUMN
        ));
    }

    out.push_str("\n## the mouse\n\n| where | gesture | what it does |\n|---|---|---|\n");
    for m in MOUSE {
        out.push_str(&format!(
            "| {:<8} | {:<width$} | {} |\n",
            m.group,
            m.gesture,
            m.label,
            width = starkit::keymap::GESTURE_COLUMN
        ));
    }
    out
}

/// What a module is called in prose.
fn module_name(m: Module) -> &'static str {
    match m {
        Module::Guilds => "the server rail",
        Module::Channels => "the channel list",
        Module::Dms => "the message list",
        Module::Chat => "the conversation",
        Module::Composer => "the composer",
        Module::Members => "the member list",
        Module::Media => "the media viewer",
        Module::Picker => "a picker",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starkit::keymap::KeySpec;

    fn plain(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn with(c: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(c, m)
    }

    /// Every key string in the table, parsed.
    fn specs(b: &Binding) -> Vec<KeySpec> {
        starkit::keymap::alternatives(b.keys)
            .filter_map(KeySpec::parse)
            .collect()
    }

    #[test]
    fn every_group_in_the_table_declares_its_scope() {
        for b in BINDINGS {
            assert!(
                GROUPS.iter().any(|(name, _)| *name == b.group),
                "the group {:?} is in the table and not in GROUPS, so its keys \
                 silently became global",
                b.group
            );
        }
    }

    /// The help overlay writes a heading whenever the group changes, so a
    /// group split across the table prints its heading twice.
    #[test]
    fn every_binding_group_is_listed_once() {
        let mut seen: Vec<&str> = Vec::new();
        let mut last = "";
        for b in BINDINGS {
            if b.group != last {
                assert!(
                    !seen.contains(&b.group),
                    "{} appears in two places in the table",
                    b.group
                );
                seen.push(b.group);
                last = b.group;
            }
        }
    }

    /// Nothing in the table is wide enough to wrap in the help overlay.
    ///
    /// It draws two columns over at most eighty: keys padded to fourteen with
    /// their labels beside them, gestures padded to twenty-one with theirs. A
    /// key string of fourteen leaves no gap before its label and the two run
    /// together; a label past the column's width wraps, pushes everything
    /// below it down, and the end of the list is silently lost off the bottom.
    /// Both were visible on the first screenshot of it, which is why the
    /// numbers are asserted rather than remembered.
    #[test]
    fn nothing_in_the_table_overruns_its_column() {
        for b in BINDINGS {
            let keys = b.keys.chars().count();
            assert!(
                keys <= 13,
                "{:?} is {keys} wide and leaves no gap before {:?}",
                b.keys,
                b.label
            );
            assert!(
                b.label.chars().count() <= 19,
                "{:?} is {} characters and would wrap",
                b.label,
                b.label.chars().count()
            );
        }
        for m in MOUSE {
            assert!(
                m.gesture.chars().count() <= 19,
                "{:?} is {} characters",
                m.gesture,
                m.gesture.chars().count()
            );
            assert!(
                m.label.chars().count() <= 19,
                "{:?} is {} characters and would wrap",
                m.label,
                m.label.chars().count()
            );
        }
    }

    /// The point of one table: a binding the help prints and the dispatcher
    /// has never heard of is worse than no binding at all.
    ///
    /// `gg` and `1..9` are sequences rather than keys and cannot be parsed as
    /// one, which is why this asks for *an* alternative that works rather than
    /// for all of them.
    #[test]
    fn every_binding_reaches_its_action() {
        for b in BINDINGS {
            let parsed = specs(b);
            assert!(
                !parsed.is_empty(),
                "{:?} has no key anything could press",
                b.keys
            );
            let reachable = parsed.iter().any(|s| {
                let k = KeyEvent::new(s.code, s.mods);
                match scope_of(b.group) {
                    Scope::Global => resolve(k) == Some(b.action),
                    Scope::Modules(list) => list.iter().any(|&m| module(m, k) == Some(b.action)),
                }
            });
            assert!(
                reachable,
                "{:?} ({:?}) is in the help and dispatches to nothing",
                b.keys, b.action
            );
        }
    }

    /// A bare arrow moves one and a shifted one moves ten. A convention rather
    /// than an opinion: it is what the rest of the key scheme is built on, and
    /// a module that breaks it makes the whole thing unguessable.
    /// `/` is the search key every other client has, and it is also the
    /// character the key column separates alternatives on. It is written in
    /// the column rather than handled beside the table, which is what keeps
    /// the help overlay and the dispatcher reading from one place.
    #[test]
    fn the_slash_is_in_the_key_column_and_reaches_search() {
        assert_eq!(resolve(plain('/')), Some(Action::Search));
        assert_eq!(
            resolve(with(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            Some(Action::Search),
            "and so does the other spelling"
        );
        let search = BINDINGS
            .iter()
            .find(|b| b.action == Action::Search)
            .expect("search is in the table");
        let spellings: Vec<&str> = starkit::keymap::alternatives(search.keys).collect();
        assert_eq!(
            spellings,
            vec!["ctrl+f", "/"],
            "the help prints the keys it dispatches on"
        );
    }

    /// Both spellings of shift and tab reach the same place.
    #[test]
    fn shift_tab_arrives_either_way() {
        assert_eq!(resolve(code(KeyCode::BackTab)), Some(Action::FocusPrev));
        assert_eq!(
            resolve(with(KeyCode::Tab, KeyModifiers::SHIFT)),
            Some(Action::FocusPrev)
        );
    }

    #[test]
    fn shift_is_the_big_step() {
        assert_eq!(resolve(code(KeyCode::Up)), Some(Action::CursorUp));
        assert_eq!(resolve(code(KeyCode::Down)), Some(Action::CursorDown));
        assert_eq!(
            resolve(with(KeyCode::Up, KeyModifiers::SHIFT)),
            Some(Action::CursorUpBig)
        );
        assert_eq!(
            resolve(with(KeyCode::Down, KeyModifiers::SHIFT)),
            Some(Action::CursorDownBig)
        );
    }

    /// `hjkl` navigates and never adjusts a value.
    ///
    /// `j` and `k` reach the cursor through the global table and no module
    /// binds them; `h` and `l` fold a list or step through a viewer's items,
    /// which is movement too. Written as a test because reaching for `j`/`k`
    /// as "down/up on a number" is the obvious thing to do and the wrong one.
    #[test]
    fn hjkl_only_ever_moves() {
        for &m in ALL_MODULES {
            for c in ['j', 'k'] {
                assert_eq!(
                    module(m, plain(c)),
                    None,
                    "{m:?} claims {c:?}; the global cursor is what carries it"
                );
            }
            for c in ['h', 'l'] {
                let got = module(m, plain(c));
                assert!(
                    matches!(
                        got,
                        None | Some(Action::ToggleCollapse)
                            | Some(Action::MediaPrev)
                            | Some(Action::MediaNext)
                    ),
                    "{m:?} binds {c:?} to {got:?}, which is not a movement"
                );
            }
        }
        assert_eq!(resolve(plain('j')), Some(Action::CursorDown));
        assert_eq!(resolve(plain('k')), Some(Action::CursorUp));
    }

    /// `esc` never quits. It closes, cancels and steps back, and a key that
    /// sometimes ends the session is a key nobody presses.
    #[test]
    fn esc_never_quits() {
        assert_ne!(resolve(code(KeyCode::Esc)), Some(Action::Quit));
        for &m in ALL_MODULES {
            assert_ne!(module(m, code(KeyCode::Esc)), Some(Action::Quit), "{m:?}");
        }
        for b in BINDINGS {
            if b.action == Action::Quit {
                assert!(!b.keys.contains("esc"), "{:?}", b.keys);
            }
        }
    }

    /// A module adds meaning; it never takes a key it has no use for, or the
    /// global bindings would stop working one panel at a time.
    #[test]
    fn a_module_declines_what_it_does_not_want() {
        for &m in ALL_MODULES {
            for c in ['q', 't', 'T', '[', ']', '/'] {
                assert_eq!(
                    module(m, plain(c)),
                    None,
                    "{m:?} swallowed {c:?}, which is global"
                );
            }
        }
    }

    /// Every global `alt+…` reaches the dispatcher while somebody is typing.
    ///
    /// This is the one that makes the composer usable: closing the member list
    /// halfway through a sentence has to work, and the alternative -- a
    /// modifier that means "text" in one panel and "command" in the rest -- is
    /// not something anyone would learn.
    #[test]
    fn every_alt_binding_survives_the_composer() {
        for b in BINDINGS {
            if scope_of(b.group) != Scope::Global {
                continue;
            }
            for s in specs(b) {
                if !s.mods.contains(KeyModifiers::ALT) {
                    continue;
                }
                let k = KeyEvent::new(s.code, s.mods);
                assert!(
                    !composer_eats(k),
                    "the composer eats {:?}, so {:?} cannot be reached while writing",
                    b.keys,
                    b.action
                );
                assert_eq!(resolve(k), Some(b.action));
            }
        }
    }

    /// The composer's own alt keys, and no others. The complement of the test
    /// above: if a new alt binding is added to the composer, one of the two
    /// fails.
    #[test]
    fn the_composer_claims_only_its_own_alt_keys() {
        let claimed: Vec<KeySpec> = BINDINGS
            .iter()
            .filter(|b| scope_of(b.group) == Scope::Modules(&[Module::Composer]))
            .flat_map(specs)
            .filter(|s| s.mods.contains(KeyModifiers::ALT))
            .collect();
        for s in &claimed {
            assert!(
                composer_eats(KeyEvent::new(s.code, s.mods)),
                "{s:?} is a composer binding the composer does not take"
            );
        }
        for c in 'a'..='z' {
            let k = with(KeyCode::Char(c), KeyModifiers::ALT);
            let is_its_own = claimed
                .iter()
                .any(|s| s.code == k.code && s.mods == k.modifiers);
            assert_eq!(
                composer_eats(k),
                is_its_own,
                "alt+{c} is taken by the composer without being one of its keys"
            );
        }
    }

    /// Nothing a person types can strand them in the composer.
    ///
    /// Every way out is a key that is not a letter, so a sentence cannot
    /// accidentally contain the exit and the exit cannot accidentally be
    /// typed.
    #[test]
    fn plain_letters_are_never_required_to_leave_the_composer() {
        let exits = [
            code(KeyCode::Tab),
            code(KeyCode::Esc),
            with(KeyCode::Char('4'), KeyModifiers::ALT),
            with(KeyCode::Char('z'), KeyModifiers::ALT),
        ];
        for k in exits {
            assert!(
                !composer_eats(k),
                "{k:?} is a way out and the composer takes it"
            );
        }
        // And the converse: every plain letter is typing, never an escape.
        for c in ('a'..='z').chain('A'..='Z').chain('0'..='9') {
            assert!(
                composer_eats(plain(c)),
                "{c:?} escapes the composer, so it cannot be typed"
            );
        }
    }

    /// `enter` in the chat panel opens what the cursor is on. It is not send:
    /// send belongs to the composer, and a chat list that sent on `enter`
    /// would send whatever the composer happened to be holding.
    #[test]
    fn enter_in_chat_is_not_send() {
        assert_eq!(
            module(Module::Chat, code(KeyCode::Enter)),
            Some(Action::OpenMedia)
        );
        assert_ne!(
            module(Module::Chat, code(KeyCode::Enter)),
            Some(Action::Send)
        );
        assert_eq!(resolve(code(KeyCode::Enter)), Some(Action::Activate));
        assert_eq!(
            module(Module::Composer, code(KeyCode::Enter)),
            Some(Action::Send),
            "send is the composer's, and only the composer's"
        );
    }

    /// `g` on its own must resolve to nothing, or the first half of `gg` would
    /// do something before the second half arrived.
    #[test]
    fn the_g_prefix_owns_plain_g() {
        assert_eq!(resolve(plain('g')), None);
        for &m in ALL_MODULES {
            assert_eq!(module(m, plain('g')), None, "{m:?}");
        }

        let mut pending = false;
        assert_eq!(g_prefix(&mut pending, plain('g')), PrefixKey::Waiting);
        assert!(pending);
        assert_eq!(
            g_prefix(&mut pending, plain('g')),
            PrefixKey::Action(Action::Home)
        );
        assert!(!pending);

        // A mistake cancels the sequence and hands the key back rather than
        // eating it.
        assert_eq!(g_prefix(&mut pending, plain('g')), PrefixKey::Waiting);
        assert_eq!(g_prefix(&mut pending, plain('x')), PrefixKey::None);
        assert!(!pending);

        // ctrl+g is the GIF picker and must not start a sequence.
        assert_eq!(
            g_prefix(
                &mut pending,
                with(KeyCode::Char('g'), KeyModifiers::CONTROL)
            ),
            PrefixKey::None
        );
        assert!(!pending);
    }

    /// Two panels wanting the same key is fine; two *groups* claiming it for
    /// the same panel is a table that dispatches by accident.
    #[test]
    fn no_module_has_a_key_twice() {
        for &m in ALL_MODULES {
            let bindings = module_bindings(m);
            let mut seen: Vec<(KeySpec, Action)> = Vec::new();
            for b in &bindings {
                for s in specs(b) {
                    if let Some((_, other)) = seen.iter().find(|(k, _)| *k == s) {
                        assert_eq!(
                            *other, b.action,
                            "{m:?} binds {s:?} to two different actions"
                        );
                    }
                    seen.push((s, b.action));
                }
            }
        }
    }

    /// The global half likewise.
    #[test]
    fn no_global_key_means_two_things() {
        let mut seen: Vec<(KeySpec, Action)> = Vec::new();
        for b in global_bindings() {
            for s in specs(&b) {
                if let Some((_, other)) = seen.iter().find(|(k, _)| *k == s) {
                    panic!("{s:?} is both {other:?} and {:?}", b.action);
                }
                seen.push((s, b.action));
            }
        }
    }

    #[test]
    fn quitting_and_help_work_from_every_panel() {
        for &m in ALL_MODULES {
            for (k, want) in [
                (plain('q'), Action::Quit),
                (plain('?'), Action::Help),
                (
                    with(KeyCode::Char('c'), KeyModifiers::CONTROL),
                    Action::Quit,
                ),
            ] {
                let got = module(m, k).or_else(|| resolve(k));
                assert_eq!(got, Some(want), "{m:?} + {k:?}");
            }
        }
    }
}

#[cfg(test)]
mod doc_tests {
    use super::*;

    fn path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/keys-and-mouse.md")
    }

    /// The document and the table say the same thing.
    ///
    /// The failure this prevents is the ordinary one: a key is changed, the
    /// program is right, and the documentation quietly describes the version
    /// before it. Run with `STARCORD_UPDATE_DOCS=1` to rewrite the file.
    #[test]
    fn the_document_is_the_table() {
        let want = document();
        let path = path();
        if std::env::var_os("STARCORD_UPDATE_DOCS").is_some() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).expect("the docs directory");
            }
            std::fs::write(&path, &want).expect("writing the document");
            return;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            have, want,
            "docs/keys-and-mouse.md is out of step with the key table; \
             run STARCORD_UPDATE_DOCS=1 cargo test to rewrite it"
        );
    }

    /// And the weaker property the stronger one implies, stated on its own so
    /// that a change to the document's shape does not lose it: every key and
    /// every gesture appears somewhere in the file.
    #[test]
    fn every_key_and_gesture_is_in_the_document() {
        let text = std::fs::read_to_string(path()).unwrap_or_default();
        for b in BINDINGS {
            assert!(text.contains(b.keys), "{:?} is not in the document", b.keys);
            assert!(
                text.contains(b.label),
                "{:?} is not in the document",
                b.label
            );
        }
        for m in MOUSE {
            assert!(
                text.contains(m.gesture),
                "{:?} is not in the document",
                m.gesture
            );
            assert!(
                text.contains(m.label),
                "{:?} is not in the document",
                m.label
            );
        }
    }
}
