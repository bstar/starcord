//! One message, turned into rows of styled text and a list of what is where.
//!
//! The whole of the chat panel's typography lives here, and nothing else in
//! the program measures a message. That matters more than it sounds: the row
//! count this produces is what the virtual list stacks, what the wrap cache
//! stores and what the scrollbar divides by, so a height computed anywhere
//! else would be a second opinion about where the screen ends.
//!
//! ## Why not `Paragraph::wrap`
//!
//! ratatui can wrap. What it cannot do is say *where* it put anything, and
//! half of what a chat window does is answer that question: which cell a link
//! starts at, so a click opens it; which run of cells a spoiler covers, so
//! `space` uncovers exactly one; which rows a picture was given, so the
//! protocol image lands on them. So the wrapping is done here, cluster by
//! cluster, and every span that somebody can point at comes back out in a
//! slot list beside the lines.
//!
//! ## Slots are recorded before they are drawn
//!
//! Pictures arrive with the milestone after this one, and the slots for them
//! are recorded now regardless. A row reserved from the attachment's declared
//! `width`/`height` is a row whose height does not change when the bytes
//! finally arrive, and a layout that reflows when a picture loads is a layout
//! that loses the reader's place. Until there is a decoder, the reserved rows
//! draw the chip.

use std::collections::HashMap;
use std::collections::HashSet;

use starkit::ratatui::style::{Modifier, Style};
use starkit::ratatui::text::{Line, Span};
use starkit::wrap::{clusters, width_of};

use crate::config::{Spoilers, Timestamps};
use crate::discord::markdown::ast::{Block, Emoji, Inline, ListItem, Mention};
use crate::discord::markdown::parse;
use crate::discord::media::MediaKey;
use crate::discord::model::{Attachment, Embed, EmbedKind, Message, PartialEmoji};
use crate::discord::snowflake::{ChannelId, MessageId, RoleId, UserId};
use crate::ui::panels::rgb;
use crate::ui::theme::Theme;

/// The bar down the left of a code block.
const CODE_BAR: &str = "\u{258f}";
/// The bar down the left of a quote.
const QUOTE_BAR: &str = "\u{258e}";
/// The bar down the left of an embed card.
const EMBED_BAR: &str = "\u{2502}";
/// What a hidden spoiler is covered with.
const SPOILER_FILL: char = '\u{2592}';
/// The corner and arrow of a reply preview.
///
/// Indented by one, because the first column of every row belongs to the
/// cursor bar and a corner drawn under it is a corner nobody sees.
const REPLY_LEAD: &str = " \u{256d} \u{21a9} ";
/// What marks a video or a gifv that this terminal will not play.
const PLAY: &str = " \u{25b6} ";

/// Columns an embed description is wrapped to, whatever the panel's width.
///
/// A card that ran the full width of a wide terminal would be a wall of
/// somebody else's marketing beside two lines of conversation.
const EMBED_COLS: u16 = 60;

/// Columns of quoted text a reply preview carries.
const REPLY_COLS: u16 = 60;

/// Where a message's own content starts when there is no avatar beside it.
const PLAIN_GUTTER: u16 = 2;
/// The same, with a four-column avatar in the margin.
const AVATAR_GUTTER: u16 = 5;

/// Everything the renderer needs that is not the message.
pub struct RenderCtx<'a> {
    pub theme: &'a Theme,
    /// Columns available for the whole message, gutter included.
    pub width: u16,
    pub avatars: bool,
    pub timestamps: Timestamps,
    /// Cell aspect ratio, for turning a picture's pixels into rows.
    pub aspect: f32,
    pub max_image_rows: u16,
    /// Whether this terminal can draw a picture at all.
    pub pictures: bool,
    pub emoji_images: bool,
    pub show_embeds: bool,
    pub spoilers: Spoilers,
    pub me: Option<UserId>,
    /// Spoilers the reader has uncovered, by message and ordinal.
    pub revealed: &'a Revealed,
    pub names: &'a Names,
    /// The zone timestamps are rendered in. Carried rather than asked for, so
    /// a test is not a test of the machine's clock settings.
    pub tz: jiff::tz::TimeZone,
}

impl<'a> RenderCtx<'a> {
    /// A context with everything a plain terminal would have. The caller sets
    /// the fields it cares about; a builder would be eleven methods for a
    /// struct that is written in three places.
    pub fn new(theme: &'a Theme, width: u16, names: &'a Names, revealed: &'a Revealed) -> Self {
        Self {
            theme,
            width,
            avatars: false,
            timestamps: Timestamps::Short,
            aspect: 2.0,
            max_image_rows: 0,
            pictures: false,
            emoji_images: false,
            show_embeds: true,
            spoilers: Spoilers::Hidden,
            me: None,
            revealed,
            names,
            tz: jiff::tz::TimeZone::UTC,
        }
    }
}

/// Which spoilers the reader has uncovered.
pub type Revealed = HashSet<(MessageId, u16)>;

impl RenderCtx<'_> {
    /// Where content starts. The avatar column only exists where a picture can
    /// actually be drawn; reserving five columns for nothing is five columns
    /// of conversation given away.
    pub fn gutter(&self) -> u16 {
        if self.avatars && self.pictures {
            AVATAR_GUTTER
        } else {
            PLAIN_GUTTER
        }
    }
}

/// Who the ids in a message refer to.
///
/// A plain map rather than a trait object. The chat panel copies what it needs
/// out of `State` once per change, exactly as every other panel does, and a
/// renderer holding a lock on the core while it measures text is a renderer
/// that stalls the gateway.
#[derive(Debug, Default, Clone)]
pub struct Names {
    pub users: HashMap<UserId, String>,
    pub channels: HashMap<ChannelId, String>,
    pub roles: HashMap<RoleId, String>,
    /// Who said what, for the messages in the window, so a reply can preview
    /// what it answers even when the payload did not carry it.
    ///
    /// Discord sends `referenced_message` on a reply it fetched and omits it
    /// on the gateway echo of one this client just sent. Looking the target up
    /// here is what stops a reply reading "the original is not loaded" one
    /// second after it was written.
    pub replies: HashMap<MessageId, (String, String)>,
}

impl Names {
    pub fn user(&self, id: UserId) -> String {
        self.users
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "unknown".into())
    }

    pub fn channel(&self, id: ChannelId) -> String {
        self.channels
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "unknown".into())
    }

    pub fn role(&self, id: RoleId) -> String {
        self.roles
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "unknown-role".into())
    }
}

/// What kind of picture a slot is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotKind {
    Still,
    Gif,
    /// An mp4 dressed as a GIF. Never drawn in the terminal; the chip opens it.
    Gifv,
    Avatar,
}

/// Rows and columns reserved for a picture that is not here yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSlot {
    pub row: u16,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
    pub key: MediaKey,
    pub kind: SlotKind,
    /// What to draw in the cells while there are no pixels for them: a
    /// person's initials in an avatar slot, nothing anywhere else. Carried on
    /// the slot because the drawing pass has the rectangle and not the
    /// message.
    pub alt: String,
}

/// Two cells holding a custom emoji.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiSlot {
    pub row: u16,
    pub col: u16,
    pub key: MediaKey,
    /// What it is called, for the two cells to say something while the picture
    /// is on its way.
    pub name: String,
}

/// A run of cells that is a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub row: u16,
    pub col: u16,
    pub width: u16,
    pub url: String,
}

/// A run of cells covering a spoiler, and which spoiler it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoilerSpan {
    pub row: u16,
    pub col: u16,
    pub width: u16,
    pub index: u16,
}

/// A file on a message, and whether it is worth opening in a viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    pub row: u16,
    pub url: String,
    pub filename: String,
    pub viewable: bool,
}

/// One reaction chip and the cells it sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionChip {
    pub row: u16,
    pub col: u16,
    pub width: u16,
    pub emoji: PartialEmoji,
    pub me: bool,
}

/// One message, measured and styled.
#[derive(Debug, Clone, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub images: Vec<ImageSlot>,
    pub emoji: Vec<EmojiSlot>,
    pub links: Vec<LinkSpan>,
    pub spoilers: Vec<SpoilerSpan>,
    pub attachments: Vec<AttachmentRef>,
    pub reactions: Vec<ReactionChip>,
    /// The row carrying `╭ ↩ …`, so a click on it jumps to the quoted message.
    pub reply_row: Option<u16>,
    /// The message as text, for `y`.
    pub plain: String,
    pub height: u16,
}

impl Rendered {
    /// A rough byte cost, for the cache's budget. The lines dominate and their
    /// spans are owned strings; counting them exactly would cost more than the
    /// budget saves.
    pub fn weight(&self) -> usize {
        let text: usize = self
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.len() + 24).sum::<usize>())
            .sum();
        text + self.plain.len() + 96
    }

    /// The first link in the message, for `Y`.
    pub fn first_link(&self) -> Option<&str> {
        self.links
            .first()
            .map(|l| l.url.as_str())
            .or_else(|| self.attachments.first().map(|a| a.url.as_str()))
    }
}

/// Build the rows for one message.
pub fn render(msg: &Message, first_in_group: bool, ctx: &RenderCtx<'_>) -> Rendered {
    let mut w = Writer::new(ctx, msg.id);

    if msg.kind.is_system() {
        w.system(msg);
        return w.finish(msg);
    }

    if first_in_group {
        w.reply_preview(msg);
        w.header(msg);
    }

    let doc = parse(&msg.content);
    w.blocks(&doc.blocks, ctx.gutter());

    w.attachments(msg);
    if ctx.show_embeds {
        w.embeds(msg);
    }
    w.reactions(msg);

    w.finish(msg)
}

/// What a span is, for the slot lists. Carried alongside the style so that the
/// writer records a position for a link without knowing what a link is.
#[derive(Debug, Clone, PartialEq)]
enum Mark {
    None,
    Link(String),
    Spoiler(u16),
}

struct Writer<'a> {
    ctx: &'a RenderCtx<'a>,
    theme: &'a Theme,
    message: MessageId,
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    col: u16,
    indent: u16,
    out: Rendered,
    plain: String,
    spoiler_count: u16,
}

impl<'a> Writer<'a> {
    fn new(ctx: &'a RenderCtx<'a>, message: MessageId) -> Self {
        Self {
            ctx,
            theme: ctx.theme,
            message,
            lines: Vec::new(),
            cur: Vec::new(),
            col: 0,
            indent: 0,
            out: Rendered::default(),
            plain: String::new(),
            spoiler_count: 0,
        }
    }

    fn finish(mut self, msg: &Message) -> Rendered {
        self.flush();
        if self.lines.is_empty() {
            self.lines.push(Line::default());
        }
        self.out.height = self.lines.len().min(usize::from(u16::MAX)) as u16;
        self.out.lines = self.lines;
        self.out.plain = if self.plain.trim().is_empty() {
            msg.content.clone()
        } else {
            self.plain.trim_end().to_string()
        };
        self.out
    }

    /// The row the next character would be written on.
    fn row(&self) -> u16 {
        self.lines.len().min(usize::from(u16::MAX)) as u16
    }

    fn width(&self) -> u16 {
        self.ctx.width.max(1)
    }

    /// End the row being built, whether or not anything is on it.
    fn flush(&mut self) {
        if self.cur.is_empty() && self.col == 0 {
            return;
        }
        self.lines.push(Line::from(std::mem::take(&mut self.cur)));
        self.col = 0;
    }

    /// Start a fresh row indented to the current gutter.
    fn newline(&mut self) {
        self.flush();
        self.pad_to_indent();
    }

    fn pad_to_indent(&mut self) {
        if self.indent > 0 && self.col == 0 {
            self.cur
                .push(Span::raw(" ".repeat(usize::from(self.indent))));
            self.col = self.indent;
        }
    }

    /// Put a whole row down at once, with no wrapping. For dividers and bars,
    /// which are built to fit by construction.
    fn row_of(&mut self, spans: Vec<Span<'static>>) {
        self.flush();
        self.lines.push(Line::from(spans));
    }

    fn push_span(&mut self, text: &str, style: Style, mark: &Mark) {
        if text.is_empty() {
            return;
        }
        let w = width_of(text);
        match mark {
            Mark::Link(url) => self.out.links.push(LinkSpan {
                row: self.row(),
                col: self.col,
                width: w,
                url: url.clone(),
            }),
            Mark::Spoiler(index) => self.out.spoilers.push(SpoilerSpan {
                row: self.row(),
                col: self.col,
                width: w,
                index: *index,
            }),
            Mark::None => {}
        }
        self.cur.push(Span::styled(text.to_string(), style));
        self.col += w;
    }

    /// Write text, wrapping at spaces and hard-breaking anything too long.
    ///
    /// Cluster by cluster rather than character by character: an emoji is two
    /// columns and several code points, and a row measured in characters is a
    /// row that overruns its panel and writes on the border.
    fn text(&mut self, text: &str, style: Style, mark: Mark) {
        self.plain.push_str(text);
        let limit = self.width();
        for token in tokens(text) {
            match token {
                Token::Break => {
                    self.newline();
                }
                Token::Space(s) => {
                    // A space at the start of a wrapped row is a space nobody
                    // asked for.
                    if self.col > self.indent {
                        let w = width_of(s);
                        if self.col + w <= limit {
                            self.push_span(s, style, &Mark::None);
                        }
                    }
                }
                Token::Word(word) => {
                    let w = width_of(word);
                    if self.col > self.indent && self.col + w > limit {
                        self.newline();
                    }
                    if self.col + w <= limit {
                        self.push_span(word, style, &mark);
                        continue;
                    }
                    // Longer than a whole row: break it wherever it reaches
                    // the edge, which is what a URL or a run of CJK does.
                    let mut chunk = String::new();
                    for (_, cluster) in clusters(word) {
                        let cw = width_of(cluster);
                        if self.col + width_of(&chunk) + cw > limit {
                            let done = std::mem::take(&mut chunk);
                            self.push_span(&done, style, &mark);
                            self.newline();
                        }
                        chunk.push_str(cluster);
                    }
                    if !chunk.is_empty() {
                        self.push_span(&chunk, style, &mark);
                    }
                }
            }
        }
    }

    // -- the pieces of a message -------------------------------------------

    /// `╭ ↩ @name: the first sixty columns of what they said`.
    fn reply_preview(&mut self, msg: &Message) {
        if msg.reply_target().is_none() && msg.referenced_message.is_none() {
            return;
        }
        let t = self.theme;
        let room = self
            .width()
            .saturating_sub(width_of(REPLY_LEAD))
            .clamp(4, REPLY_COLS.max(4));
        let (name, text) = match msg.referenced_message.as_deref() {
            Some(target) => (target.author_name().to_string(), target.content.clone()),
            None => ("someone".to_string(), "the original is not loaded".into()),
        };
        self.out.reply_row = Some(self.row());
        let body = summarise(&format!("@{name}: {text}"), room);
        self.row_of(vec![
            Span::styled(REPLY_LEAD, Style::default().fg(rgb(t.chat.divider_fg))),
            Span::styled(body, Style::default().fg(rgb(t.dim))),
        ]);
    }

    /// `name  14:32 (edited)`, with the avatar's cells reserved beside it.
    fn header(&mut self, msg: &Message) {
        let t = self.theme;
        let gutter = self.ctx.gutter();
        let mark = initials(msg.author_name());
        // The avatar's four columns by two rows, taken out of the gutter the
        // header and the first line of content already leave. An account with
        // no avatar hash has no picture to ask for, and the same cells carry
        // its initials instead -- which is why the gutter is five whenever
        // avatars are on and pictures are possible, rather than only for the
        // people who have one.
        let lead = if gutter == AVATAR_GUTTER && msg.author.avatar.is_none() {
            format!("{mark:<width$}", width = usize::from(gutter))
        } else {
            " ".repeat(usize::from(gutter))
        };
        if gutter == AVATAR_GUTTER {
            if let Some(hash) = msg.author.avatar.clone() {
                self.out.images.push(ImageSlot {
                    row: self.row(),
                    col: 0,
                    cols: 4,
                    rows: 2,
                    key: MediaKey::Avatar {
                        user: msg.author.id,
                        hash,
                        size: 32,
                    },
                    kind: SlotKind::Avatar,
                    alt: mark,
                });
            }
        }

        let mut spans = vec![
            Span::styled(
                lead,
                Style::default()
                    .fg(rgb(t.chat.author_fg))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                msg.author_name().to_string(),
                Style::default()
                    .fg(rgb(t.chat.author_fg))
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if let Some(stamp) = self.stamp(msg) {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                stamp,
                Style::default().fg(rgb(t.chat.time_fg)),
            ));
        }
        if msg.is_edited() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                "(edited)",
                Style::default().fg(rgb(t.chat.time_fg)),
            ));
        }
        self.row_of(spans);
    }

    fn stamp(&self, msg: &Message) -> Option<String> {
        let at = msg.timestamp?;
        let zoned = at.to_zoned(self.ctx.tz.clone());
        match self.ctx.timestamps {
            Timestamps::Off => None,
            Timestamps::Short => Some(zoned.strftime("%H:%M").to_string()),
            Timestamps::Full => Some(zoned.strftime("%Y-%m-%d %H:%M").to_string()),
        }
    }

    /// A join, a pin, a boost: one dim line that never groups with anything.
    fn system(&mut self, msg: &Message) {
        let t = self.theme;
        let text = system_text(msg);
        self.plain.push_str(&text);
        let body = cut(&text, self.width().saturating_sub(PLAIN_GUTTER));
        self.row_of(vec![
            Span::raw(" ".repeat(usize::from(PLAIN_GUTTER))),
            Span::styled(body, Style::default().fg(rgb(t.chat.system_fg))),
        ]);
    }

    // -- markdown ----------------------------------------------------------

    fn blocks(&mut self, blocks: &[Block], indent: u16) {
        let was = self.indent;
        self.indent = indent;
        for (n, block) in blocks.iter().enumerate() {
            if n > 0 {
                self.flush();
            }
            self.block(block);
        }
        self.flush();
        self.indent = was;
    }

    fn block(&mut self, block: &Block) {
        let t = self.theme;
        match block {
            Block::Paragraph(inlines) => {
                self.pad_to_indent();
                self.inlines(inlines, Style::default().fg(rgb(t.fg)), &Mark::None);
            }
            Block::Heading { content, .. } => {
                self.pad_to_indent();
                self.inlines(
                    content,
                    Style::default().fg(rgb(t.fg)).add_modifier(Modifier::BOLD),
                    &Mark::None,
                );
            }
            Block::Subtext(content) => {
                self.pad_to_indent();
                self.inlines(content, Style::default().fg(rgb(t.dim)), &Mark::None);
            }
            Block::Quote(blocks) => {
                let inner = self.indent + 2;
                let bar_col = self.indent;
                let from = self.lines.len();
                self.blocks(blocks, inner);
                // The bar is painted down the rows the quote turned out to
                // take, which is the only moment the count is known.
                for row in from..self.lines.len() {
                    paint_bar(
                        &mut self.lines[row],
                        bar_col,
                        QUOTE_BAR,
                        Style::default().fg(rgb(t.chat.divider_fg)),
                    );
                }
            }
            Block::CodeBlock { lang, text } => self.code_block(lang.as_deref(), text),
            Block::List { ordered, items } => self.list(*ordered, items),
        }
    }

    fn list(&mut self, ordered: bool, items: &[ListItem]) {
        let t = self.theme;
        for (n, item) in items.iter().enumerate() {
            self.flush();
            let lead = match (ordered, item.number) {
                (true, Some(number)) => format!("{number}. "),
                (true, None) => format!("{}. ", n + 1),
                (false, _) => "\u{2022} ".into(),
            };
            let indent = self.indent + u16::from(item.indent).saturating_mul(2);
            self.indent = indent;
            self.pad_to_indent();
            self.push_span(&lead, Style::default().fg(rgb(t.dim)), &Mark::None);
            let body = indent + width_of(&lead);
            let was = self.indent;
            self.indent = body;
            for block in &item.blocks {
                self.block(block);
            }
            self.indent = was;
        }
        self.flush();
    }

    fn code_block(&mut self, lang: Option<&str>, text: &str) {
        let t = self.theme;
        let style = Style::default()
            .fg(rgb(t.chat.code_fg))
            .bg(rgb(t.chat.code_bg));
        let bar = Style::default().fg(rgb(t.chat.embed_bar));
        let indent = self.indent;
        let room = self
            .width()
            .saturating_sub(indent + width_of(CODE_BAR) + 1)
            .max(1);

        self.flush();
        if let Some(lang) = lang.filter(|l| !l.is_empty()) {
            self.plain.push_str(lang);
            self.plain.push('\n');
            self.row_of(vec![
                Span::raw(" ".repeat(usize::from(indent))),
                Span::styled(CODE_BAR, bar),
                Span::styled(
                    format!(" {}", cut(lang, room)),
                    Style::default().fg(rgb(t.dim)),
                ),
            ]);
        }
        self.plain.push_str(text);
        for line in text.split('\n') {
            // A long line of code is cut rather than wrapped: code that wraps
            // is code you cannot read, and the chip at the end says so.
            let drawn = cut(line, room);
            self.row_of(vec![
                Span::raw(" ".repeat(usize::from(indent))),
                Span::styled(CODE_BAR, bar),
                Span::styled(format!(" {drawn}"), style),
            ]);
        }
    }

    fn inlines(&mut self, inlines: &[Inline], style: Style, mark: &Mark) {
        for inline in inlines {
            self.inline(inline, style, mark);
        }
    }

    fn inline(&mut self, inline: &Inline, style: Style, mark: &Mark) {
        let t = self.theme;
        match inline {
            Inline::Text(text) => self.text(text, style, mark.clone()),
            Inline::LineBreak => self.newline(),
            Inline::Bold(inner) => self.inlines(inner, style.add_modifier(Modifier::BOLD), mark),
            Inline::Italic(inner) => {
                self.inlines(inner, style.add_modifier(Modifier::ITALIC), mark)
            }
            Inline::Underline(inner) => {
                self.inlines(inner, style.add_modifier(Modifier::UNDERLINED), mark)
            }
            Inline::Strike(inner) => {
                self.inlines(inner, style.add_modifier(Modifier::CROSSED_OUT), mark)
            }
            Inline::Spoiler(inner) => self.spoiler(inner, style, mark),
            Inline::Code(text) => self.text(
                text,
                Style::default()
                    .fg(rgb(t.chat.code_fg))
                    .bg(rgb(t.chat.code_bg)),
                mark.clone(),
            ),
            Inline::Link { text, url, .. } => {
                let style = style
                    .fg(rgb(t.chat.link_fg))
                    .add_modifier(Modifier::UNDERLINED);
                let mark = Mark::Link(url.clone());
                if text.is_empty() {
                    self.text(url, style, mark);
                } else {
                    self.inlines(text, style, &mark);
                }
            }
            Inline::Mention(mention) => self.mention(mention, style, mark),
            Inline::Timestamp { unix, style: kind } => {
                let text = timestamp(*unix, *kind, &self.ctx.tz);
                self.text(&text, style.fg(rgb(t.chat.time_fg)), mark.clone());
            }
            Inline::Emoji(emoji) => self.emoji(emoji, style, mark),
        }
    }

    fn spoiler(&mut self, inner: &[Inline], style: Style, _mark: &Mark) {
        let index = self.spoiler_count;
        self.spoiler_count += 1;
        let revealed = self.ctx.spoilers == Spoilers::Shown
            || self.ctx.revealed.contains(&(self.message, index));
        if revealed {
            self.inlines(
                inner,
                style.bg(rgb(self.theme.chat.spoiler_bg)),
                &Mark::Spoiler(index),
            );
            return;
        }
        // Covered: as many blocks as the text was wide, so uncovering it does
        // not move the rest of the line.
        let mut plain = String::new();
        for i in inner {
            write_plain(i, &mut plain);
        }
        let w = width_of(&plain).clamp(1, self.width().saturating_sub(self.indent).max(1));
        let fill: String = std::iter::repeat_n(SPOILER_FILL, usize::from(w)).collect();
        self.text(
            &fill,
            Style::default()
                .fg(rgb(self.theme.chat.spoiler_bg))
                .bg(rgb(self.theme.chat.spoiler_bg)),
            Mark::Spoiler(index),
        );
    }

    fn mention(&mut self, mention: &Mention, style: Style, mark: &Mark) {
        let t = self.theme;
        let names = self.ctx.names;
        let (text, is_me) = match mention {
            Mention::User(id) => (format!("@{}", names.user(*id)), Some(*id) == self.ctx.me),
            Mention::Channel(id) => (format!("#{}", names.channel(*id)), false),
            Mention::Role(id) => (format!("@{}", names.role(*id)), false),
            Mention::Everyone => ("@everyone".into(), true),
            Mention::Here => ("@here".into(), true),
            Mention::Command { name, .. } => (format!("/{name}"), false),
        };
        let style = if is_me {
            style.fg(rgb(t.warn)).bg(rgb(t.chat.mention_bg))
        } else {
            style.fg(rgb(t.chat.mention_fg)).bg(rgb(t.chat.mention_bg))
        };
        self.text(&text, style, mark.clone());
    }

    fn emoji(&mut self, emoji: &Emoji, style: Style, mark: &Mark) {
        match emoji {
            Emoji::Unicode(text) => self.text(text, style, mark.clone()),
            Emoji::Custom { name, id, animated } => {
                if self.ctx.pictures && self.ctx.emoji_images {
                    // Two cells, which is what an emoji is worth on a line of
                    // text, and one slot saying which picture goes in them.
                    let limit = self.width();
                    if self.col + 2 > limit {
                        self.newline();
                    }
                    self.out.emoji.push(EmojiSlot {
                        row: self.row(),
                        col: self.col,
                        key: MediaKey::Emoji {
                            id: *id,
                            animated: *animated,
                            size: 48,
                        },
                        name: name.clone(),
                    });
                    self.plain.push_str(&format!(":{name}:"));
                    self.push_span("  ", style, &Mark::None);
                } else {
                    self.text(&format!(":{name}:"), style, mark.clone());
                }
            }
        }
    }

    // -- what hangs off a message ------------------------------------------

    fn attachments(&mut self, msg: &Message) {
        let gutter = self.ctx.gutter();
        for attachment in &msg.attachments {
            self.flush();
            let row = self.row();
            self.out.attachments.push(AttachmentRef {
                row,
                url: attachment.url.clone(),
                filename: attachment.filename.clone(),
                viewable: attachment.is_image(),
            });

            let chip = attachment_chip(attachment);
            let (cols, rows) = self.picture_box(attachment);
            if rows > 0 {
                self.out.images.push(ImageSlot {
                    row,
                    col: gutter,
                    cols,
                    rows,
                    key: MediaKey::Attachment {
                        message: msg.id,
                        id: attachment.id.0,
                        url: attachment.url.clone(),
                    },
                    kind: if is_animated(attachment) {
                        SlotKind::Gif
                    } else {
                        SlotKind::Still
                    },
                    alt: chip,
                });
                for _ in 0..rows {
                    self.placeholder_row(gutter, cols);
                }
                // No chip under it. The rows are the picture; a caption
                // repeating its file name under every photograph is a caption
                // nobody reads, and the one case it is needed -- the fetch
                // failed -- draws it in the reserved rows instead.
                continue;
            }
            self.chip(gutter, &chip);
        }
    }

    /// The cells to reserve for a picture, from what the attachment says it is.
    ///
    /// From the declared size rather than from the bytes, so the height is
    /// settled before anything is downloaded and the list does not reflow
    /// under the reader when it arrives.
    ///
    /// The rows come first, because they are what the setting caps and what
    /// the scrolling depends on; the columns follow from them, so the box has
    /// the picture's own shape. A box the width of the panel would letterbox
    /// every photograph into its top-left corner and reserve a screenful of
    /// blank beside it, which is what this looked like before the box was
    /// measured in both directions.
    fn picture_box(&self, attachment: &Attachment) -> (u16, u16) {
        if !self.ctx.pictures || self.ctx.max_image_rows == 0 || !attachment.is_image() {
            return (0, 0);
        }
        let (Some(w), Some(h)) = (attachment.width, attachment.height) else {
            return (0, 0);
        };
        if w == 0 || h == 0 {
            return (0, 0);
        }
        let room = self.width().saturating_sub(self.ctx.gutter()).max(1);
        let aspect = if self.ctx.aspect > 0.0 {
            self.ctx.aspect
        } else {
            2.0
        };
        // A picture wider than the panel is cut to the panel; one narrower
        // keeps its own width, which is what stops a postage stamp being
        // blown up across a conversation.
        let wide = f32::from(room.min(w.min(u32::from(u16::MAX)) as u16));
        let rows = ((wide * (h as f32 / w as f32) / aspect).ceil().max(1.0) as u16)
            .min(self.ctx.max_image_rows);
        let cols = ((f32::from(rows) * aspect * (w as f32 / h as f32))
            .ceil()
            .max(1.0) as u16)
            .min(room);
        (cols, rows)
    }

    fn placeholder_row(&mut self, gutter: u16, cols: u16) {
        let room = cols.min(self.width().saturating_sub(gutter)).max(1);
        self.row_of(vec![
            Span::raw(" ".repeat(usize::from(gutter))),
            Span::styled(
                "\u{2591}".repeat(usize::from(room)),
                Style::default().fg(rgb(self.theme.chat.spoiler_bg)),
            ),
        ]);
    }

    fn chip(&mut self, gutter: u16, text: &str) {
        let room = self.width().saturating_sub(gutter).max(1);
        self.row_of(vec![
            Span::raw(" ".repeat(usize::from(gutter))),
            Span::styled(
                cut(text, room),
                Style::default().fg(rgb(self.theme.chat.system_fg)),
            ),
        ]);
    }

    fn embeds(&mut self, msg: &Message) {
        if msg.embeds_suppressed() {
            return;
        }
        for embed in &msg.embeds {
            self.embed(embed);
        }
    }

    fn embed(&mut self, embed: &Embed) {
        let t = self.theme;
        let gutter = self.ctx.gutter();
        let bar = Style::default().fg(rgb(t.chat.embed_bar));

        // A video or an animated-GIF embed is a file this terminal will not
        // play, so it is a chip that opens somewhere else rather than a card
        // pretending otherwise.
        if embed.is_playable() {
            let what = if embed.kind == EmbedKind::Gifv {
                "gif"
            } else {
                "video"
            };
            let title = embed
                .title
                .clone()
                .or_else(|| embed.url.clone())
                .unwrap_or_else(|| what.into());
            if let Some(url) = embed.url.clone() {
                self.out.attachments.push(AttachmentRef {
                    row: self.row(),
                    url,
                    filename: title.clone(),
                    viewable: false,
                });
            }
            // The still is drawn with a play marker over it; the file itself
            // is an mp4 and opens in whatever `[media] player` names.
            if let Some(url) = embed
                .still()
                .or(embed.video.as_ref())
                .and_then(thumbnail_url)
            {
                self.thumbnail(url, SlotKind::Gifv);
            }
            self.chip(gutter, &format!("[{what}{PLAY}{title}]"));
            return;
        }

        let room = self
            .width()
            .saturating_sub(gutter + width_of(EMBED_BAR) + 1)
            .clamp(1, EMBED_COLS);

        let card = |w: &mut Self, spans: Vec<Span<'static>>| {
            let mut row = vec![
                Span::raw(" ".repeat(usize::from(gutter))),
                Span::styled(EMBED_BAR, bar),
                Span::raw(" "),
            ];
            row.extend(spans);
            w.row_of(row);
        };

        if let Some(author) = &embed.author {
            card(
                self,
                vec![Span::styled(
                    cut(&author.name, room),
                    Style::default().fg(rgb(t.dim)),
                )],
            );
        }
        if let Some(title) = &embed.title {
            let style = Style::default()
                .fg(rgb(t.chat.link_fg))
                .add_modifier(Modifier::UNDERLINED);
            if let Some(url) = &embed.url {
                self.flush();
                self.out.links.push(LinkSpan {
                    row: self.row(),
                    col: gutter + width_of(EMBED_BAR) + 1,
                    width: width_of(&cut(title, room)),
                    url: url.clone(),
                });
            }
            card(self, vec![Span::styled(cut(title, room), style)]);
        }
        if let Some(description) = &embed.description {
            for row in starkit::wrap::wrap(description, room) {
                card(
                    self,
                    vec![Span::styled(
                        row.drawn(description).to_string(),
                        Style::default().fg(rgb(t.fg)),
                    )],
                );
            }
        }
        if let Some(url) = embed.still().and_then(thumbnail_url) {
            self.thumbnail(url, SlotKind::Still);
        }
    }

    /// Eight columns by four rows against the right edge, for the picture on a
    /// card.
    ///
    /// Right-aligned because a card is read left to right: the title and the
    /// description are the part somebody asked for, and a thumbnail in front
    /// of them pushes the words into a column half the width of the panel.
    /// The rows are reserved whether or not the picture arrives, so the card
    /// keeps its height.
    fn thumbnail(&mut self, url: String, kind: SlotKind) {
        const COLS: u16 = 8;
        const ROWS: u16 = 4;
        if !self.ctx.pictures || self.ctx.max_image_rows == 0 {
            return;
        }
        let width = self.width();
        if width < COLS + self.ctx.gutter() {
            return;
        }
        let row = self.row();
        self.out.images.push(ImageSlot {
            row,
            col: width - COLS,
            cols: COLS,
            rows: ROWS,
            key: MediaKey::EmbedImage { url },
            kind,
            alt: String::new(),
        });
        for _ in 0..ROWS {
            self.placeholder_row(width - COLS, COLS);
        }
    }

    fn reactions(&mut self, msg: &Message) {
        if msg.reactions.is_empty() {
            return;
        }
        let t = self.theme;
        let gutter = self.ctx.gutter();
        self.flush();
        let row = self.row();
        let mut spans = vec![Span::raw(" ".repeat(usize::from(gutter)))];
        let mut col = gutter;
        let pictures = self.ctx.pictures && self.ctx.emoji_images;
        for reaction in &msg.reactions {
            // A custom emoji with pictures is two blank cells and a slot; the
            // chip is measured from what is written, so the hit box is the
            // same either way.
            let custom = reaction.emoji.id.filter(|_| pictures);
            let name = match (&reaction.emoji.id, &reaction.emoji.name) {
                _ if custom.is_some() => "  ".to_string(),
                (Some(_), Some(name)) => format!(":{name}:"),
                (_, Some(name)) => name.clone(),
                _ => "?".into(),
            };
            let chip = format!("[{} {}] ", name, reaction.count);
            let w = width_of(&chip);
            if col + w > self.width() {
                break;
            }
            if let Some(id) = custom {
                self.out.emoji.push(EmojiSlot {
                    row,
                    col: col + 1,
                    key: MediaKey::Emoji {
                        id,
                        animated: reaction.emoji.animated,
                        size: 48,
                    },
                    name: reaction.emoji.name.clone().unwrap_or_default(),
                });
            }
            let style = if reaction.me {
                Style::default()
                    .fg(rgb(t.fg))
                    .bg(rgb(t.chat.reaction_me_bg))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(rgb(t.fg)).bg(rgb(t.chat.reaction_bg))
            };
            self.out.reactions.push(ReactionChip {
                row,
                col,
                width: w.saturating_sub(1),
                emoji: reaction.emoji.clone(),
                me: reaction.me,
            });
            spans.push(Span::styled(chip, style));
            col += w;
        }
        self.row_of(spans);
    }
}

/// Split text into words, runs of spaces and line breaks.
///
/// Runs rather than single characters so that the writer can decide once per
/// word whether it fits, and so that a run of spaces at a wrap point is
/// dropped rather than indenting the row after it.
enum Token<'a> {
    Word(&'a str),
    Space(&'a str),
    Break,
}

fn tokens(text: &str) -> Vec<Token<'_>> {
    fn push<'a>(out: &mut Vec<Token<'a>>, slice: &'a str, space: Option<bool>) {
        if slice.is_empty() {
            return;
        }
        out.push(if space == Some(true) {
            Token::Space(slice)
        } else {
            Token::Word(slice)
        });
    }

    let mut out = Vec::new();
    let mut start = 0usize;
    let mut run: Option<bool> = None;

    for (i, c) in text.char_indices() {
        if c == '\n' {
            push(&mut out, &text[start..i], run);
            out.push(Token::Break);
            start = i + c.len_utf8();
            run = None;
            continue;
        }
        let space = c == ' ' || c == '\t';
        match run {
            Some(was) if was == space => {}
            Some(was) => {
                push(&mut out, &text[start..i], Some(was));
                start = i;
                run = Some(space);
            }
            None => {
                start = i;
                run = Some(space);
            }
        }
    }
    push(&mut out, &text[start..], run);
    out
}

/// Put a bar into the leading padding of a row that has already been built.
///
/// A quote's height is only known once its blocks have been laid out, so the
/// bar is painted down the rows afterwards rather than written as each row is
/// started. Rows that do not have blank padding at that column are left alone.
fn paint_bar(line: &mut Line<'static>, col: u16, bar: &str, style: Style) {
    let mut at = 0u16;
    for index in 0..line.spans.len() {
        let w = width_of(&line.spans[index].content);
        if at <= col && col < at + w && line.spans[index].content.trim().is_empty() {
            let before = usize::from(col - at);
            let after = usize::from(w - (col - at) - 1);
            line.spans[index] = Span::raw(" ".repeat(before));
            line.spans
                .insert(index + 1, Span::styled(bar.to_string(), style));
            line.spans.insert(index + 2, Span::raw(" ".repeat(after)));
            return;
        }
        at += w;
    }
}

/// Cut a string to `width` columns, on cluster boundaries.
pub fn cut(text: &str, width: u16) -> String {
    let mut out = String::new();
    let mut used = 0u16;
    for (_, cluster) in clusters(text) {
        let w = width_of(cluster);
        if used + w > width {
            break;
        }
        out.push_str(cluster);
        used += w;
    }
    out
}

/// The first line of a message, cut to `width`, for a reply preview.
fn summarise(text: &str, width: u16) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return "(no text)".into();
    }
    if width_of(first) <= width {
        return first.to_string();
    }
    format!("{}\u{2026}", cut(first, width.saturating_sub(1)))
}

fn write_plain(inline: &Inline, out: &mut String) {
    match inline {
        Inline::Text(text) => out.push_str(text),
        Inline::Code(text) => out.push_str(text),
        Inline::LineBreak => out.push(' '),
        Inline::Bold(inner)
        | Inline::Italic(inner)
        | Inline::Underline(inner)
        | Inline::Strike(inner)
        | Inline::Spoiler(inner) => {
            for i in inner {
                write_plain(i, out);
            }
        }
        Inline::Link { url, .. } => out.push_str(url),
        Inline::Mention(_) => out.push_str("@someone"),
        Inline::Timestamp { .. } => out.push_str("a time"),
        Inline::Emoji(Emoji::Unicode(text)) => out.push_str(text),
        Inline::Emoji(Emoji::Custom { name, .. }) => {
            out.push(':');
            out.push_str(name);
            out.push(':');
        }
    }
}

/// `<t:1700000000:R>` in the reader's own zone.
fn timestamp(unix: i64, style: Option<char>, tz: &jiff::tz::TimeZone) -> String {
    let Ok(at) = jiff::Timestamp::from_second(unix) else {
        return "(a time)".into();
    };
    let zoned = at.to_zoned(tz.clone());
    match style {
        Some('t') => zoned.strftime("%H:%M").to_string(),
        Some('T') => zoned.strftime("%H:%M:%S").to_string(),
        Some('d') => zoned.strftime("%Y-%m-%d").to_string(),
        Some('D') => zoned.strftime("%-d %B %Y").to_string(),
        Some('F') => zoned.strftime("%A, %-d %B %Y %H:%M").to_string(),
        Some('R') => relative(at),
        _ => zoned.strftime("%-d %b %Y %H:%M").to_string(),
    }
}

fn relative(at: jiff::Timestamp) -> String {
    let now = jiff::Timestamp::now();
    let secs = (at.as_second() - now.as_second()).abs();
    let (n, unit) = match secs {
        s if s < 60 => (s, "second"),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3600, "hour"),
        s => (s / 86_400, "day"),
    };
    let plural = if n == 1 { "" } else { "s" };
    if at.as_second() < now.as_second() {
        format!("{n} {unit}{plural} ago")
    } else {
        format!("in {n} {unit}{plural}")
    }
}

/// `[image 1024x768 cat.png]`, `[file 1.2 MB notes.pdf]`.
/// Two characters standing in for somebody's face.
///
/// The initials of the first two words, or the first two characters of one.
/// The same rule the server rail uses, so a name is abbreviated the same way
/// wherever it is too wide to write out.
fn initials(name: &str) -> String {
    let mut words = name
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric) || w.chars().count() == 1);
    match (words.next(), words.next()) {
        (Some(a), Some(b)) => {
            let mut s = String::new();
            s.extend(a.chars().next());
            s.extend(b.chars().next());
            s
        }
        (Some(a), None) => a.chars().take(2).collect(),
        _ => "\u{b7}\u{b7}".into(),
    }
}

/// Whether a file is a moving picture, which decides whether the slot is one
/// an animation pass will ever be asked to advance.
fn is_animated(attachment: &Attachment) -> bool {
    matches!(attachment.content_type.as_deref(), Some("image/gif"))
        || attachment.filename.to_ascii_lowercase().ends_with(".gif")
}

/// Where a card's picture is fetched from.
///
/// The proxy first. Discord serves somebody else's image through its own media
/// proxy, and the proxy is the copy that is resized, cached and served over a
/// connection this client already has; the original is a request to a host the
/// message merely named.
fn thumbnail_url(media: &crate::discord::model::EmbedMedia) -> Option<String> {
    media
        .proxy_url
        .clone()
        .filter(|u| !u.is_empty())
        .or_else(|| media.url.clone().filter(|u| !u.is_empty()))
}

fn attachment_chip(attachment: &Attachment) -> String {
    let what = if attachment.is_image() {
        "image"
    } else if attachment.is_video() {
        "video"
    } else {
        "file"
    };
    match (attachment.width, attachment.height) {
        (Some(w), Some(h)) => format!("[{what} {w}x{h} {}]", attachment.filename),
        _ => format!(
            "[{what} {} {}]",
            human_size(attachment.size),
            attachment.filename
        ),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// What a system message says, in this client's words rather than Discord's
/// templated ones.
fn system_text(msg: &Message) -> String {
    use crate::discord::model::MessageKind;
    let who = msg.author_name();
    match msg.kind {
        MessageKind::RecipientAdd => format!("{who} added somebody to the group"),
        MessageKind::RecipientRemove => format!("{who} removed somebody from the group"),
        MessageKind::Call => format!("{who} started a call"),
        MessageKind::ChannelNameChange => format!("{who} renamed the channel"),
        MessageKind::ChannelIconChange => format!("{who} changed the icon"),
        MessageKind::ChannelPinnedMessage => format!("{who} pinned a message"),
        MessageKind::UserJoin => format!("{who} joined"),
        MessageKind::GuildBoost
        | MessageKind::GuildBoostTier1
        | MessageKind::GuildBoostTier2
        | MessageKind::GuildBoostTier3 => format!("{who} boosted the server"),
        MessageKind::ThreadCreated => format!("{who} started a thread"),
        MessageKind::ChannelFollowAdd => format!("{who} followed a channel"),
        _ if !msg.content.is_empty() => msg.content.clone(),
        other => format!("{who}: {other:?}"),
    }
}

/// Everything the cache key is built from.
///
/// One struct rather than a tuple because it is the thing that decides whether
/// a message is redrawn, and a tuple of ten fields is a thing nobody can read
/// a miss out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub message: MessageId,
    /// When it was last edited, so an edit misses.
    pub edited: i64,
    pub width: u16,
    pub theme: u64,
    pub reactions: u64,
    pub first_in_group: bool,
    pub revealed: u64,
    /// Bumped when a picture this message owns arrives.
    pub media_gen: u64,
    pub avatars: bool,
    pub timestamps: Timestamps,
}

/// The renderer's cache: `Rendered` keyed by everything that would change it.
pub struct Cache {
    entries: HashMap<Key, std::sync::Arc<Rendered>>,
    order: Vec<Key>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    hits: u64,
    misses: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new(2000, 8 * 1024 * 1024)
    }
}

impl Cache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            bytes: 0,
            max_entries,
            max_bytes,
            hits: 0,
            misses: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn misses(&self) -> u64 {
        self.misses
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    /// Forget everything about one message: an edit, a delete, a reaction.
    pub fn forget(&mut self, message: MessageId) {
        let gone: Vec<Key> = self
            .entries
            .keys()
            .filter(|k| k.message == message)
            .copied()
            .collect();
        for key in gone {
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.weight());
            }
            self.order.retain(|k| *k != key);
        }
    }

    pub fn get_or_insert(
        &mut self,
        key: Key,
        build: impl FnOnce() -> Rendered,
    ) -> std::sync::Arc<Rendered> {
        if let Some(hit) = self.entries.get(&key) {
            self.hits += 1;
            return std::sync::Arc::clone(hit);
        }
        self.misses += 1;
        let built = std::sync::Arc::new(build());
        self.bytes += built.weight();
        self.entries.insert(key, std::sync::Arc::clone(&built));
        self.order.push(key);
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            let Some(oldest) = self.order.first().copied() else {
                break;
            };
            self.order.remove(0);
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(entry.weight());
            }
        }
        built
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::model::{Reaction, User};
    use crate::discord::snowflake::{ChannelId, EmojiId};
    use crate::ui::theme::tests_support::theme;

    fn msg(content: &str) -> Message {
        Message {
            id: MessageId(500),
            channel_id: ChannelId(1),
            author: User {
                id: UserId(10),
                username: "alex".into(),
                ..User::default()
            },
            content: content.into(),
            timestamp: jiff::Timestamp::from_second(1_757_764_800).ok(),
            ..Message::default()
        }
    }

    /// One message, as rows of plain text.
    fn rows(msg: &Message, width: u16) -> Vec<String> {
        let t = theme("terminal");
        let names = Names::default();
        let revealed = Revealed::default();
        let ctx = RenderCtx::new(&t, width, &names, &revealed);
        text_of(&render(msg, true, &ctx))
    }

    fn text_of(r: &Rendered) -> Vec<String> {
        r.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// Every row fits the width it was measured for.
    ///
    /// The property the whole writer exists to keep: a row one column wider
    /// than the panel writes over the border, and the border stays broken
    /// until something else redraws it.
    fn fits(rows: &[String], width: u16) {
        for row in rows {
            assert!(
                width_of(row) <= width,
                "{:?} is {} columns in a {width}-column panel",
                row,
                width_of(row)
            );
        }
    }

    #[test]
    fn a_plain_message_gets_a_header_and_its_text() {
        let rows = rows(&msg("hello there"), 40);
        assert!(rows[0].contains("alex"), "{rows:?}");
        assert!(rows[0].contains(':'), "a timestamp: {rows:?}");
        assert_eq!(rows[1].trim(), "hello there");
    }

    /// CJK is two columns a character, and the wrapper has to agree with the
    /// terminal about that or every line after it is offset.
    #[test]
    fn cjk_is_measured_in_columns_not_characters() {
        let text = "二".repeat(30);
        let rows = rows(&msg(&text), 20);
        fits(&rows, 20);
        // Twenty columns, two of gutter, so nine characters a row.
        assert!(rows.len() >= 4, "{rows:?}");
    }

    /// A family emoji is one grapheme of several code points and two columns.
    /// Counting its code points would make the line four times too long.
    #[test]
    fn a_zero_width_joiner_sequence_counts_once() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let rows = rows(&msg(&format!("{family} {family}")), 30);
        fits(&rows, 30);
        assert_eq!(rows.len(), 2, "it should not have wrapped: {rows:?}");
    }

    /// A word longer than the panel is broken at the edge rather than
    /// overflowing it, which is what a URL does.
    #[test]
    fn a_word_longer_than_the_row_is_broken() {
        let long = format!("https://example.invalid/{}", "a".repeat(80));
        let rows = rows(&msg(&long), 30);
        fits(&rows, 30);
        assert!(rows.len() > 3, "{rows:?}");
        let joined: String = rows[1..].iter().map(|r| r.trim()).collect();
        assert!(joined.contains("example.invalid"), "{rows:?}");
    }

    /// A spoiler is covered by as many blocks as the text was wide, so
    /// revealing it does not move anything else on the line.
    #[test]
    fn a_spoiler_is_covered_and_uncovers_in_place() {
        let t = theme("terminal");
        let names = Names::default();
        let message = msg("the answer is ||forty two|| you know");

        let hidden = Revealed::default();
        let ctx = RenderCtx::new(&t, 60, &names, &hidden);
        let covered = render(&message, true, &ctx);
        let covered_rows = text_of(&covered);
        assert!(covered_rows[1].contains('\u{2592}'), "{covered_rows:?}");
        assert!(!covered_rows[1].contains("forty"), "{covered_rows:?}");
        assert_eq!(covered.spoilers.len(), 1);
        assert_eq!(covered.spoilers[0].index, 0);

        let mut shown = Revealed::default();
        shown.insert((message.id, 0));
        let ctx = RenderCtx::new(&t, 60, &names, &shown);
        let uncovered = render(&message, true, &ctx);
        let uncovered_rows = text_of(&uncovered);
        assert!(
            uncovered_rows[1].contains("forty two"),
            "{uncovered_rows:?}"
        );
        assert_eq!(
            covered.height, uncovered.height,
            "uncovering changed the height"
        );
        assert!(
            covered_rows[1].len() >= uncovered_rows[1].len(),
            "the cover is at least as wide as what it covers"
        );
    }

    /// Revealing one message's spoiler says nothing about another's. The
    /// revealed set is keyed by message and ordinal for exactly this.
    #[test]
    fn revealing_one_spoiler_leaves_the_others_covered() {
        let t = theme("terminal");
        let names = Names::default();
        let mut one = msg("||first||");
        one.id = MessageId(1);
        let mut two = msg("||second||");
        two.id = MessageId(2);

        let mut shown = Revealed::default();
        shown.insert((MessageId(1), 0));
        let ctx = RenderCtx::new(&t, 60, &names, &shown);
        assert!(text_of(&render(&one, true, &ctx))[1].contains("first"));
        assert!(text_of(&render(&two, true, &ctx))[1].contains('\u{2592}'));
    }

    /// A picture nothing can draw is a chip that says what it was.
    #[test]
    fn an_image_is_a_chip_when_there_are_no_pictures() {
        let mut message = msg("look");
        message.attachments.push(Attachment {
            id: crate::discord::snowflake::AttachmentId(600),
            filename: "harbour.png".into(),
            content_type: Some("image/png".into()),
            size: 1234,
            url: "https://cdn.invalid/harbour.png".into(),
            width: Some(1024),
            height: Some(768),
            ..Attachment::default()
        });

        let t = theme("terminal");
        let names = Names::default();
        let revealed = Revealed::default();

        let ctx = RenderCtx::new(&t, 60, &names, &revealed);
        let flat = render(&message, true, &ctx);
        let rows = text_of(&flat);
        assert!(
            rows.iter()
                .any(|r| r.contains("[image 1024x768 harbour.png]")),
            "{rows:?}"
        );
        assert!(flat.images.is_empty(), "no slot without a picture protocol");
        assert_eq!(flat.attachments.len(), 1);

        // With pictures, the rows are reserved as well as the chip drawn.
        let mut ctx = RenderCtx::new(&t, 60, &names, &revealed);
        ctx.pictures = true;
        ctx.max_image_rows = 12;
        let with = render(&message, true, &ctx);
        assert_eq!(with.images.len(), 1, "{:?}", with.images);
        assert!(with.height > flat.height, "no rows were reserved");
        assert!(
            with.images[0].rows <= 12,
            "[chat] max_image_rows was not honoured"
        );

        // And none at all when the setting says zero.
        ctx.max_image_rows = 0;
        let none = render(&message, true, &ctx);
        assert!(none.images.is_empty());
        assert_eq!(none.height, flat.height);
    }

    /// The rows a picture is given come from what it says it is and from the
    /// shape of a cell, and are then held to the setting.
    ///
    /// The cell aspect is the half of this nobody thinks about: a terminal
    /// cell is about twice as tall as it is wide, so a square picture across
    /// twenty columns is ten rows and not twenty. Getting it wrong is how a
    /// photograph ends up letterboxed inside the space reserved for it.
    #[test]
    fn the_reserved_rows_follow_the_picture_and_the_cell() {
        let mut message = msg("look");
        message.attachments.push(Attachment {
            id: crate::discord::snowflake::AttachmentId(600),
            filename: "square.png".into(),
            content_type: Some("image/png".into()),
            url: "https://cdn.invalid/square.png".into(),
            width: Some(400),
            height: Some(400),
            ..Attachment::default()
        });
        let t = theme("terminal");
        let names = Names::default();
        let revealed = Revealed::default();

        let rows_at = |aspect: f32, cap: u16, width: u16| {
            let mut ctx = RenderCtx::new(&t, width, &names, &revealed);
            ctx.pictures = true;
            ctx.max_image_rows = cap;
            ctx.aspect = aspect;
            render(&message, true, &ctx).images[0].rows
        };

        // Twenty-two columns wide, two of them the gutter: a square across
        // twenty columns is ten rows at an aspect of two.
        assert_eq!(rows_at(2.0, 40, 22), 10);
        // A taller cell is fewer rows for the same picture.
        assert_eq!(rows_at(2.5, 40, 22), 8);
        // And the setting is a ceiling, whatever the arithmetic says.
        assert_eq!(rows_at(2.0, 4, 22), 4);
    }

    /// Somebody with no avatar hash still gets the gutter, with their initials
    /// in it: the five columns are a fact about the setting, not about who
    /// happens to have uploaded a picture.
    #[test]
    fn a_head_without_an_avatar_carries_initials() {
        let t = theme("terminal");
        let names = Names::default();
        let revealed = Revealed::default();
        let mut ctx = RenderCtx::new(&t, 60, &names, &revealed);
        ctx.pictures = true;
        ctx.avatars = true;

        let mut message = msg("hello");
        message.author.username = "alex".into();
        message.author.global_name = Some("Alex Green".into());
        let bare = render(&message, true, &ctx);
        assert!(bare.images.is_empty(), "nothing to fetch without a hash");
        assert!(text_of(&bare)[0].starts_with("AG"), "{:?}", text_of(&bare));

        message.author.avatar = Some("a1b2c3".into());
        let with = render(&message, true, &ctx);
        assert_eq!(with.images.len(), 1);
        assert_eq!(with.images[0].kind, SlotKind::Avatar);
        assert_eq!(with.images[0].rows, 2);
        assert_eq!(with.images[0].cols, 4);
        assert_eq!(
            with.images[0].alt, "AG",
            "no initials to draw while it loads"
        );
        assert!(
            text_of(&with)[0].starts_with("     Alex"),
            "the initials were drawn under the picture: {:?}",
            text_of(&with)
        );
    }

    /// A custom emoji is two cells and a slot when there are pictures, and its
    /// own name when there are not.
    #[test]
    fn a_custom_emoji_is_two_cells_or_its_name() {
        let t = theme("terminal");
        let names = Names::default();
        let revealed = Revealed::default();
        let message = msg("nice <:pepe:900000000000000001> one");

        let flat = render(&message, true, &RenderCtx::new(&t, 60, &names, &revealed));
        let rows = text_of(&flat);
        assert!(rows.iter().any(|r| r.contains(":pepe:")), "{rows:?}");
        assert!(flat.emoji.is_empty());

        let mut ctx = RenderCtx::new(&t, 60, &names, &revealed);
        ctx.pictures = true;
        ctx.emoji_images = true;
        let with = render(&message, true, &ctx);
        assert_eq!(with.emoji.len(), 1);
        assert_eq!(with.emoji[0].name, "pepe");
        assert!(
            !text_of(&with).iter().any(|r| r.contains(":pepe:")),
            "the name was written under the picture"
        );
        // And `y` still yields the text somebody typed, not two blanks.
        assert!(with.plain.contains(":pepe:"));
    }

    /// A reaction carrying a custom emoji reserves the same two cells, and the
    /// chip stays a chip: the hit box is measured from what was written.
    #[test]
    fn a_custom_reaction_reserves_its_two_cells() {
        let t = theme("terminal");
        let names = Names::default();
        let revealed = Revealed::default();
        let mut message = msg("hi");
        message.reactions.push(crate::discord::model::Reaction {
            count: 3,
            me: false,
            emoji: PartialEmoji {
                id: Some(crate::discord::snowflake::EmojiId(900)),
                name: Some("pepe".into()),
                animated: false,
            },
            ..Default::default()
        });

        let mut ctx = RenderCtx::new(&t, 60, &names, &revealed);
        ctx.pictures = true;
        ctx.emoji_images = true;
        let with = render(&message, true, &ctx);
        assert_eq!(with.emoji.len(), 1);
        assert_eq!(with.reactions.len(), 1);
        // The slot sits inside the chip's brackets.
        assert!(with.emoji[0].col > with.reactions[0].col);
        assert!(with.emoji[0].col < with.reactions[0].col + with.reactions[0].width);
    }

    /// The code block keeps its bar and its language, and does not wrap.
    #[test]
    fn a_code_block_is_barred_and_labelled() {
        let rows = rows(&msg("see:\n```rust\nfn main() {}\n```"), 40);
        assert!(rows.iter().any(|r| r.contains("rust")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains(CODE_BAR)), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("fn main()")), "{rows:?}");
        fits(&rows, 40);
    }

    /// A quote gets a bar down however many rows it turned out to take.
    #[test]
    fn a_quote_is_barred_down_its_whole_height() {
        let rows = rows(
            &msg("> a quotation long enough to wrap over two rows of a narrow panel"),
            30,
        );
        let barred = rows.iter().filter(|r| r.contains(QUOTE_BAR)).count();
        assert!(barred >= 2, "{rows:?}");
        fits(&rows, 30);
    }

    /// A mention of the reader is drawn in the warning colour rather than the
    /// ordinary mention one, because it is the one that has to be found.
    #[test]
    fn a_mention_of_me_stands_out() {
        let t = theme("terminal");
        let mut names = Names::default();
        names.users.insert(UserId(1), "sam".into());
        let revealed = Revealed::default();

        let mut ctx = RenderCtx::new(&t, 60, &names, &revealed);
        ctx.me = Some(UserId(1));
        let mine = render(&msg("hello <@1>"), true, &ctx);
        let me_style = mine.lines[1]
            .spans
            .iter()
            .find(|s| s.content.contains("@sam"))
            .expect("the mention")
            .style;
        assert_eq!(me_style.fg, Some(rgb(t.warn)));

        ctx.me = Some(UserId(2));
        let theirs = render(&msg("hello <@1>"), true, &ctx);
        let other = theirs.lines[1]
            .spans
            .iter()
            .find(|s| s.content.contains("@sam"))
            .expect("the mention")
            .style;
        assert_eq!(other.fg, Some(rgb(t.chat.mention_fg)));
    }

    /// A link is underlined and recorded, so a click and a `Y` both find it.
    #[test]
    fn a_link_is_underlined_and_recorded() {
        let t = theme("terminal");
        let names = Names::default();
        let revealed = Revealed::default();
        let ctx = RenderCtx::new(&t, 60, &names, &revealed);
        let out = render(&msg("see https://example.invalid/page for it"), true, &ctx);
        assert_eq!(out.links.len(), 1, "{:?}", out.links);
        assert_eq!(out.links[0].url, "https://example.invalid/page");
        assert_eq!(out.first_link(), Some("https://example.invalid/page"));
        let styled = out.lines[1]
            .spans
            .iter()
            .find(|s| s.content.contains("example.invalid"))
            .expect("the link");
        assert!(styled.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    /// Reaction chips carry a hit box each, and the account's own is marked.
    #[test]
    fn reactions_are_chips_with_hit_boxes() {
        let mut message = msg("well then");
        message.reactions = vec![
            Reaction {
                count: 3,
                me: true,
                emoji: PartialEmoji {
                    id: None,
                    name: Some("\u{1f44d}".into()),
                    animated: false,
                },
                ..Reaction::default()
            },
            Reaction {
                count: 1,
                me: false,
                emoji: PartialEmoji {
                    id: Some(EmojiId(7)),
                    name: Some("pepe".into()),
                    animated: false,
                },
                ..Reaction::default()
            },
        ];
        let t = theme("terminal");
        let names = Names::default();
        let revealed = Revealed::default();
        let ctx = RenderCtx::new(&t, 60, &names, &revealed);
        let out = render(&message, true, &ctx);
        assert_eq!(out.reactions.len(), 2);
        assert!(out.reactions[0].me);
        assert!(!out.reactions[1].me);
        assert!(out.reactions[0].width > 0);
        // The chips do not overlap, or a click lands on whichever was pushed
        // first rather than on the one under the pointer.
        let a = &out.reactions[0];
        let b = &out.reactions[1];
        assert!(a.col + a.width <= b.col, "{a:?} {b:?}");
        let rows = text_of(&out);
        assert!(rows.last().unwrap().contains(":pepe:"), "{rows:?}");
    }

    /// A system message is one dim line and carries no header.
    #[test]
    fn a_system_message_is_one_line() {
        let mut message = msg("");
        message.kind = crate::discord::model::MessageKind::UserJoin;
        let rows = rows(&message, 40);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("joined"), "{rows:?}");
    }

    /// The height reported is the number of rows drawn. Everything about the
    /// scrolling rests on this being true.
    #[test]
    fn the_height_is_the_row_count() {
        for text in [
            "short",
            "a much longer message that will certainly wrap at this width",
            "```\ncode\n```",
            "> quoted\n> twice",
        ] {
            let t = theme("terminal");
            let names = Names::default();
            let revealed = Revealed::default();
            let ctx = RenderCtx::new(&t, 30, &names, &revealed);
            let out = render(&msg(text), true, &ctx);
            assert_eq!(out.height as usize, out.lines.len(), "{text:?}");
        }
    }

    /// The cache's miss-and-hit matrix: every field of the key is a field
    /// because changing it changes the answer, so every one of them must miss.
    #[test]
    fn the_cache_misses_on_everything_in_its_key() {
        let base = Key {
            message: MessageId(1),
            edited: 0,
            width: 40,
            theme: 1,
            reactions: 0,
            first_in_group: true,
            revealed: 0,
            media_gen: 0,
            avatars: true,
            timestamps: Timestamps::Short,
        };

        let mut cache = Cache::default();
        let build = || Rendered {
            height: 1,
            ..Rendered::default()
        };
        cache.get_or_insert(base, build);
        assert_eq!((cache.hits(), cache.misses()), (0, 1));
        cache.get_or_insert(base, build);
        assert_eq!(
            (cache.hits(), cache.misses()),
            (1, 1),
            "the same key missed"
        );

        let variants: Vec<(&str, Key)> = vec![
            (
                "message",
                Key {
                    message: MessageId(2),
                    ..base
                },
            ),
            ("edited", Key { edited: 5, ..base }),
            ("width", Key { width: 41, ..base }),
            ("theme", Key { theme: 2, ..base }),
            (
                "reactions",
                Key {
                    reactions: 9,
                    ..base
                },
            ),
            (
                "first_in_group",
                Key {
                    first_in_group: false,
                    ..base
                },
            ),
            (
                "revealed",
                Key {
                    revealed: 3,
                    ..base
                },
            ),
            (
                "media_gen",
                Key {
                    media_gen: 1,
                    ..base
                },
            ),
            (
                "avatars",
                Key {
                    avatars: false,
                    ..base
                },
            ),
            (
                "timestamps",
                Key {
                    timestamps: Timestamps::Full,
                    ..base
                },
            ),
        ];
        for (what, key) in variants {
            let before = cache.misses();
            cache.get_or_insert(key, build);
            assert_eq!(
                cache.misses(),
                before + 1,
                "a change of {what} came back from the cache"
            );
        }
    }

    /// Forgetting a message forgets every width and theme it was drawn at,
    /// which is what an edit or a new reaction needs.
    #[test]
    fn forgetting_a_message_forgets_all_of_it() {
        let mut cache = Cache::default();
        let build = Rendered::default;
        for width in [30u16, 40, 50] {
            cache.get_or_insert(
                Key {
                    message: MessageId(1),
                    edited: 0,
                    width,
                    theme: 0,
                    reactions: 0,
                    first_in_group: true,
                    revealed: 0,
                    media_gen: 0,
                    avatars: false,
                    timestamps: Timestamps::Off,
                },
                build,
            );
        }
        cache.get_or_insert(
            Key {
                message: MessageId(2),
                edited: 0,
                width: 30,
                theme: 0,
                reactions: 0,
                first_in_group: true,
                revealed: 0,
                media_gen: 0,
                avatars: false,
                timestamps: Timestamps::Off,
            },
            build,
        );
        assert_eq!(cache.len(), 4);
        cache.forget(MessageId(1));
        assert_eq!(cache.len(), 1, "the other widths survived");
        cache.clear();
        assert!(cache.is_empty());
    }

    /// The cache does not grow without bound.
    #[test]
    fn the_cache_evicts_the_oldest() {
        let mut cache = Cache::new(4, 1 << 20);
        for id in 0..10u64 {
            cache.get_or_insert(
                Key {
                    message: MessageId(id),
                    edited: 0,
                    width: 30,
                    theme: 0,
                    reactions: 0,
                    first_in_group: true,
                    revealed: 0,
                    media_gen: 0,
                    avatars: false,
                    timestamps: Timestamps::Off,
                },
                Rendered::default,
            );
        }
        assert!(cache.len() <= 4, "{}", cache.len());
    }

    /// A timestamp is drawn in the zone the context names, not the machine's.
    #[test]
    fn a_timestamp_is_local_to_the_stated_zone() {
        let utc = timestamp(1_757_764_800, Some('t'), &jiff::tz::TimeZone::UTC);
        let elsewhere = timestamp(
            1_757_764_800,
            Some('t'),
            &jiff::tz::TimeZone::fixed(jiff::tz::offset(5)),
        );
        assert_ne!(utc, elsewhere, "the zone made no difference");
        assert!(utc.contains(':'));
    }

    /// The reply preview says who and what, and is cut with an ellipsis rather
    /// than silently.
    #[test]
    fn a_reply_carries_a_preview_of_what_it_answers() {
        let mut message = msg("yes");
        message.referenced_message = Some(Box::new(msg(&"a very long original ".repeat(10))));
        let t = theme("terminal");
        let names = Names::default();
        let revealed = Revealed::default();
        let ctx = RenderCtx::new(&t, 50, &names, &revealed);
        let out = render(&message, true, &ctx);
        let rows = text_of(&out);
        assert_eq!(out.reply_row, Some(0));
        assert!(rows[0].contains("\u{21a9}"), "{rows:?}");
        assert!(rows[0].contains("@alex"), "{rows:?}");
        assert!(rows[0].ends_with('\u{2026}'), "{rows:?}");
        fits(&rows, 50);
    }
}
