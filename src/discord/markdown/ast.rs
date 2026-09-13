//! What a parsed message is.
//!
//! Two levels, as Discord's own renderer has: blocks stack vertically and
//! inlines flow inside one. Nothing here refers to a terminal — a `Bold` is a
//! `Bold`, not a `Modifier::BOLD` — because the whole point of the split is
//! that the chat panel decides what any of it looks like.
//!
//! [`Document::plain_text`] is the other consumer, and it has a contract worth
//! stating: **every alphanumeric character of the source appears in it, in
//! order.** That is what a property test asserts, and it is why a mention comes
//! back out as `<@1234>` rather than as a name — the core has no name lookup,
//! the id is the only thing it knows, and throwing the digits away would make
//! the plain text a lossy summary rather than a faithful one. Notifications and
//! `starcord probe` both read it; both want the truth over the prettier
//! version.

use std::fmt::Write as _;

use crate::discord::snowflake::{ChannelId, EmojiId, RoleId, UserId};

/// A whole message.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// The source was longer than the cap and was cut. Nothing anywhere sends
    /// a message this long, so it means something hostile or something broken.
    pub truncated: bool,
}

/// What to do with `||spoilers||` when flattening to plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spoilers {
    /// Write what is inside them, which is what keeps every alphanumeric
    /// character of the source in the output.
    Reveal,
    /// Write `[spoiler]` instead. The one caller that wants this is the desktop
    /// notification: a spoiler is the one piece of text somebody deliberately
    /// hid, and a popup that shows it anyway has defeated the point of it.
    Hide,
}

/// What [`Spoilers::Hide`] writes.
pub const HIDDEN: &str = "[spoiler]";

impl Document {
    /// The message with every marker removed.
    pub fn plain_text(&self) -> String {
        self.plain_text_with(Spoilers::Reveal)
    }

    /// The same, saying what to do about spoilers.
    pub fn plain_text_with(&self, spoilers: Spoilers) -> String {
        let mut out = String::new();
        for (n, block) in self.blocks.iter().enumerate() {
            if n > 0 {
                out.push('\n');
            }
            block.write_plain(&mut out, spoilers);
        }
        out
    }

    /// Whether anything at all was parsed.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

// `Block::CodeBlock` reads as a stutter and is the name Discord's own
// documentation uses; renaming it to `Block::Code` would collide with
// `Inline::Code`, which is the inline span and a different thing.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    /// `#`, `##`, `###` — Discord has three and no more.
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    /// `-#`, drawn small and dim.
    Subtext(Vec<Inline>),
    /// `>` or `>>>`. Holds blocks, because a quote can contain a list.
    Quote(Vec<Block>),
    CodeBlock {
        lang: Option<String>,
        text: String,
    },
    List {
        ordered: bool,
        items: Vec<ListItem>,
    },
}

/// One row of a list.
///
/// `number` is kept rather than recomputed from the position: a list written
/// `1.` then `7.` renders with both numbers in Discord, and a renderer that
/// counts for itself would show a number that is not in the message.
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub number: Option<u64>,
    /// Nesting, in levels rather than columns. Kept flat because the chat panel
    /// draws an indent, not a tree.
    pub indent: u8,
    pub blocks: Vec<Block>,
}

impl Block {
    fn write_plain(&self, out: &mut String, spoilers: Spoilers) {
        match self {
            Block::Paragraph(inlines) => write_inlines(inlines, out, spoilers),
            Block::Heading { content, .. } => write_inlines(content, out, spoilers),
            Block::Subtext(content) => write_inlines(content, out, spoilers),
            Block::Quote(blocks) => {
                for (n, block) in blocks.iter().enumerate() {
                    if n > 0 {
                        out.push('\n');
                    }
                    block.write_plain(out, spoilers);
                }
            }
            Block::CodeBlock { lang, text } => {
                // The language tag is part of the source and therefore part of
                // the plain text, or the round-trip property is a lie.
                if let Some(lang) = lang {
                    out.push_str(lang);
                    out.push('\n');
                }
                out.push_str(text);
            }
            Block::List { items, .. } => {
                for (n, item) in items.iter().enumerate() {
                    if n > 0 {
                        out.push('\n');
                    }
                    if let Some(number) = item.number {
                        let _ = write!(out, "{number}. ");
                    }
                    for (m, block) in item.blocks.iter().enumerate() {
                        if m > 0 {
                            out.push('\n');
                        }
                        block.write_plain(out, spoilers);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    /// A newline inside one paragraph.
    LineBreak,
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Underline(Vec<Inline>),
    Strike(Vec<Inline>),
    Spoiler(Vec<Inline>),
    Code(String),
    Link {
        /// `[this](url)`. Empty for a bare or angle-bracketed URL, where the
        /// text is the URL.
        text: Vec<Inline>,
        url: String,
        /// `<url>`: written to stop Discord unfurling it.
        suppressed: bool,
    },
    Mention(Mention),
    /// `<t:1700000000:R>`. The style letter is Discord's, and `None` means the
    /// default.
    Timestamp {
        unix: i64,
        style: Option<char>,
    },
    Emoji(Emoji),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mention {
    User(UserId),
    Channel(ChannelId),
    Role(RoleId),
    Everyone,
    Here,
    /// `</name:id>`, a slash command.
    Command {
        name: String,
        id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emoji {
    /// A real emoji character, as it was written.
    Unicode(String),
    Custom {
        name: String,
        id: EmojiId,
        animated: bool,
    },
}

fn write_inlines(inlines: &[Inline], out: &mut String, spoilers: Spoilers) {
    for inline in inlines {
        inline.write_plain(out, spoilers);
    }
}

impl Inline {
    fn write_plain(&self, out: &mut String, spoilers: Spoilers) {
        match self {
            Inline::Text(text) => out.push_str(text),
            Inline::LineBreak => out.push('\n'),
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Underline(inner)
            | Inline::Strike(inner) => write_inlines(inner, out, spoilers),
            Inline::Spoiler(inner) => match spoilers {
                Spoilers::Reveal => write_inlines(inner, out, spoilers),
                Spoilers::Hide => out.push_str(HIDDEN),
            },
            Inline::Code(text) => out.push_str(text),
            Inline::Link { text, url, .. } => {
                if text.is_empty() {
                    out.push_str(url);
                } else {
                    write_inlines(text, out, spoilers);
                    let _ = write!(out, " ({url})");
                }
            }
            Inline::Mention(mention) => match mention {
                Mention::User(id) => {
                    let _ = write!(out, "<@{id}>");
                }
                Mention::Channel(id) => {
                    let _ = write!(out, "<#{id}>");
                }
                Mention::Role(id) => {
                    let _ = write!(out, "<@&{id}>");
                }
                Mention::Everyone => out.push_str("@everyone"),
                Mention::Here => out.push_str("@here"),
                Mention::Command { name, id } => {
                    let _ = write!(out, "</{name}:{id}>");
                }
            },
            Inline::Timestamp { unix, style } => match style {
                Some(style) => {
                    let _ = write!(out, "<t:{unix}:{style}>");
                }
                None => {
                    let _ = write!(out, "<t:{unix}>");
                }
            },
            Inline::Emoji(emoji) => match emoji {
                Emoji::Unicode(text) => out.push_str(text),
                Emoji::Custom { name, id, animated } => {
                    let prefix = if *animated { "a" } else { "" };
                    let _ = write!(out, "<{prefix}:{name}:{id}>");
                }
            },
        }
    }
}
