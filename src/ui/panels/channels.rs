//! The channel list.
//!
//! Categories fold, channels sit under them, and the whole thing is one flat
//! vector of rows rather than a tree: folding is then a filter rather than a
//! traversal, the cursor is an index, and the mouse is arithmetic. Rebuilding
//! it costs one pass over a few dozen channels and is done when the list
//! changes, not every frame.

use std::collections::HashSet;

use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use super::{empty, rgb, DmTab};
use crate::discord::model::ChannelKind;
use crate::discord::snowflake::ChannelId;
use crate::ui::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Category {
        id: ChannelId,
        name: String,
        collapsed: bool,
    },
    Channel {
        id: ChannelId,
        name: String,
        kind: ChannelKind,
        unread: bool,
        mentions: u32,
        muted: bool,
        /// Whether it hangs under a category, which is the only depth there is
        /// until threads arrive.
        nested: bool,
    },
}

impl Row {
    pub fn channel_id(&self) -> Option<ChannelId> {
        match self {
            Row::Channel { id, .. } => Some(*id),
            Row::Category { .. } => None,
        }
    }

    pub fn id(&self) -> ChannelId {
        match self {
            Row::Category { id, .. } | Row::Channel { id, .. } => *id,
        }
    }
}

/// The glyph in front of a channel's name.
///
/// One character each, and the same ones Discord's own client uses, because
/// somebody arriving from it should not have to learn a second alphabet.
fn sigil(kind: ChannelKind) -> char {
    match kind {
        ChannelKind::GuildAnnouncement => '\u{1f4e2}',
        ChannelKind::GuildForum | ChannelKind::GuildMedia => '\u{2637}',
        ChannelKind::GuildVoice | ChannelKind::GuildStageVoice => '\u{1f50a}',
        k if k.is_thread() => '\u{21b3}',
        _ => '#',
    }
}

pub struct View<'a> {
    pub theme: &'a Theme,
    pub rows: &'a [Row],
    pub cursor: usize,
    pub scroll: usize,
    pub focused: bool,
    /// The channel currently open, which is marked whether or not the cursor
    /// is on it.
    pub open: Option<ChannelId>,
    /// Set when the DM list has folded in here and this panel is carrying both
    /// lists behind a tab.
    pub folded_tab: Option<DmTab>,
}

/// Build the flat row list for one guild.
///
/// Channels with no category come first, in position order, exactly as
/// Discord's own client puts them; everything else hangs under its category.
/// `State::channels_ordered` has already sorted them, so this only groups.
pub fn rows(
    ordered: &[std::sync::Arc<crate::discord::model::Channel>],
    collapsed: &HashSet<ChannelId>,
    show_voice: bool,
    unread_of: impl Fn(ChannelId) -> crate::discord::state::Unread,
) -> Vec<Row> {
    let mut out = Vec::with_capacity(ordered.len());
    let mut hidden_parent: Option<ChannelId> = None;

    for channel in ordered {
        if channel.kind == ChannelKind::GuildCategory {
            let is_collapsed = collapsed.contains(&channel.id);
            hidden_parent = is_collapsed.then_some(channel.id);
            out.push(Row::Category {
                id: channel.id,
                name: channel.name().unwrap_or("—").to_string(),
                collapsed: is_collapsed,
            });
            continue;
        }
        if channel.kind.is_voice() && !show_voice {
            continue;
        }
        if !channel.kind.is_text() && !channel.kind.is_voice() {
            continue;
        }
        // A folded category takes its children with it. `hidden_parent` is the
        // last category seen, which is why this works on a list that is
        // already in display order and would not on one that is not.
        if channel.parent_id.is_some() && channel.parent_id == hidden_parent {
            continue;
        }
        let unread = unread_of(channel.id);
        out.push(Row::Channel {
            id: channel.id,
            name: channel.name().unwrap_or("—").to_string(),
            kind: channel.kind,
            unread: unread.unread,
            mentions: unread.mentions,
            muted: unread.muted,
            nested: channel.parent_id.is_some(),
        });
    }
    out
}

pub fn row_at(body: Rect, v: &View<'_>, y: u16) -> Option<usize> {
    if y < body.y || y >= body.y + body.height {
        return None;
    }
    let index = v.scroll + usize::from(y - body.y);
    (index < v.rows.len()).then_some(index)
}

pub fn render(body: Rect, buf: &mut Buffer, v: &View<'_>) {
    let t = v.theme;
    if v.rows.is_empty() {
        empty(body, buf, t, "no channels");
        return;
    }
    let width = usize::from(body.width);

    for (index, row) in v
        .rows
        .iter()
        .enumerate()
        .skip(v.scroll)
        .take(usize::from(body.height))
    {
        let y = body.y + (index - v.scroll) as u16;
        let selected = index == v.cursor;
        let (text, mut style) = match row {
            Row::Category {
                name, collapsed, ..
            } => {
                let arrow = if *collapsed { '\u{25b8}' } else { '\u{25be}' };
                (
                    format!("{arrow} {}", name.to_uppercase()),
                    Style::default().fg(rgb(t.row_meta_fg)),
                )
            }
            Row::Channel {
                id,
                name,
                kind,
                unread,
                mentions,
                muted,
                nested,
            } => {
                let indent = if *nested { "  " } else { "" };
                let badge = if *mentions > 0 {
                    format!(" ({mentions})")
                } else {
                    String::new()
                };
                let open = v.open == Some(*id);
                let fg = if *mentions > 0 {
                    t.chat.mention_fg
                } else if *muted {
                    t.dim
                } else if *unread {
                    t.chat.unread_fg
                } else if open {
                    t.row_playing_fg
                } else {
                    t.row_fg
                };
                let mut style = Style::default().fg(rgb(fg));
                if *unread && !*muted {
                    style = style.add_modifier(Modifier::BOLD);
                }
                (format!("{indent}{} {name}{badge}", sigil(*kind)), style)
            }
        };
        if selected {
            style = style
                .fg(rgb(if v.focused {
                    t.row_selected_fg
                } else {
                    t.row_cursor_fg
                }))
                .bg(rgb(if v.focused {
                    t.row_selected_bg
                } else {
                    t.row_cursor_bg
                }));
        }
        let text: String = text.chars().take(width).collect();
        buf.set_string(body.x, y, format!("{text:width$}"), style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::state::Unread;
    use std::sync::Arc;

    fn channel(
        id: u64,
        name: &str,
        kind: ChannelKind,
        parent: Option<u64>,
    ) -> Arc<crate::discord::model::Channel> {
        Arc::new(crate::discord::model::Channel {
            id: ChannelId(id),
            kind,
            guild_id: None,
            name: Some(name.into()),
            position: 0,
            parent_id: parent.map(ChannelId),
            topic: None,
            nsfw: false,
            last_message_id: None,
            recipients: Vec::new(),
            recipient_ids: Vec::new(),
            owner_id: None,
            icon: None,
            flags: 0,
        })
    }

    fn quiet(_: ChannelId) -> Unread {
        Unread::default()
    }

    fn sample() -> Vec<Arc<crate::discord::model::Channel>> {
        vec![
            channel(1, "rules", ChannelKind::GuildText, None),
            channel(2, "Text Channels", ChannelKind::GuildCategory, None),
            channel(3, "general", ChannelKind::GuildText, Some(2)),
            channel(4, "random", ChannelKind::GuildText, Some(2)),
            channel(5, "General Voice", ChannelKind::GuildVoice, Some(2)),
        ]
    }

    #[test]
    fn voice_channels_are_out_unless_asked_for() {
        let without = rows(&sample(), &HashSet::new(), false, quiet);
        assert!(without.iter().all(|r| !matches!(
            r,
            Row::Channel {
                kind: ChannelKind::GuildVoice,
                ..
            }
        )));
        let with = rows(&sample(), &HashSet::new(), true, quiet);
        assert_eq!(with.len(), without.len() + 1);
    }

    /// Folding a category takes its channels with it and leaves the ones above
    /// it alone. The uncategorised channel at the top is the case that used to
    /// disappear along with everything else.
    #[test]
    fn folding_a_category_hides_only_its_own_children() {
        let mut collapsed = HashSet::new();
        collapsed.insert(ChannelId(2));
        let built = rows(&sample(), &collapsed, true, quiet);
        let names: Vec<ChannelId> = built.iter().map(|r| r.id()).collect();
        assert_eq!(names, vec![ChannelId(1), ChannelId(2)]);
        assert!(matches!(
            built[1],
            Row::Category {
                collapsed: true,
                ..
            }
        ));
    }

    #[test]
    fn a_category_is_not_a_channel_anything_can_open() {
        let built = rows(&sample(), &HashSet::new(), false, quiet);
        assert_eq!(built[1].channel_id(), None);
        assert_eq!(built[2].channel_id(), Some(ChannelId(3)));
    }

    #[test]
    fn every_kind_gets_a_mark_of_its_own() {
        assert_eq!(sigil(ChannelKind::GuildText), '#');
        assert_ne!(sigil(ChannelKind::GuildAnnouncement), '#');
        assert_ne!(sigil(ChannelKind::GuildForum), '#');
        assert_ne!(sigil(ChannelKind::GuildVoice), '#');
        assert_ne!(sigil(ChannelKind::PublicThread), '#');
    }

    #[test]
    fn row_at_answers_only_for_rows_that_were_drawn() {
        let built = rows(&sample(), &HashSet::new(), false, quiet);
        let theme = crate::ui::theme::tests_support::theme("terminal");
        let v = View {
            theme: &theme,
            rows: &built,
            cursor: 0,
            scroll: 1,
            focused: true,
            open: None,
            folded_tab: None,
        };
        let body = Rect::new(0, 2, 20, 2);
        assert_eq!(row_at(body, &v, 1), None);
        assert_eq!(row_at(body, &v, 2), Some(1));
        assert_eq!(row_at(body, &v, 3), Some(2));
        assert_eq!(row_at(body, &v, 4), None);
    }
}
