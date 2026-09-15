//! The channel list.
//!
//! Categories fold, channels sit under them, and the whole thing is one flat
//! vector of rows rather than a tree: folding is then a filter rather than a
//! traversal, the cursor is an index, and the mouse is arithmetic. Rebuilding
//! it costs one pass over a few dozen channels and is done when the list
//! changes, not every frame.

use std::collections::HashSet;

use starkit::chrome::scrollbar;
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::{Modifier, Style};

use super::{empty, fit, rgb};
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

/// The mark in front of a channel's name.
///
/// **One column each, and ASCII.** The obvious thing to reach for is the
/// loudspeaker and the speech bubble Discord's own client uses, and they are
/// wrong here for a reason worth writing down: an emoji is two columns wide in
/// a terminal that has the font for it and one in a terminal that does not, so
/// a channel list built out of them is a channel list whose rows are a
/// different width on different machines, and on half of them the last cell
/// lands on the panel border. `#` is what a channel is called anyway.
fn sigil(kind: ChannelKind) -> char {
    match kind {
        ChannelKind::GuildAnnouncement => '!',
        ChannelKind::GuildForum | ChannelKind::GuildMedia => '=',
        ChannelKind::GuildVoice | ChannelKind::GuildStageVoice => '~',
        k if k.is_thread() => '>',
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

/// The one line the folded module draws for the channel that is open.
///
/// The row without its indent and without its selection: the sigil, the name
/// and the mention count, which is what somebody glancing at a folded module
/// needs in order to know where they are.
pub fn summary(row: &Row) -> String {
    match row {
        Row::Category { name, .. } => name.to_uppercase(),
        Row::Channel {
            name,
            kind,
            mentions,
            ..
        } => {
            let badge = if *mentions > 0 {
                format!(" ({mentions})")
            } else {
                String::new()
            };
            format!("{} {name}{badge}", sigil(*kind))
        }
    }
}

pub fn row_at(body: Rect, v: &View<'_>, y: u16) -> Option<usize> {
    if y < body.y || y >= body.y + body.height {
        return None;
    }
    let index = v.scroll + usize::from(y - body.y);
    (index < v.rows.len()).then_some(index)
}

pub fn render(outer: Rect, body: Rect, buf: &mut Buffer, v: &View<'_>) {
    let t = v.theme;
    if v.rows.is_empty() {
        empty(body, buf, t, "no channels");
        return;
    }
    let width = body.width;

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
        buf.set_string(body.x, y, fit(&text, width), style);
    }

    let track = scrollbar::track(outer, body);
    let thumb = scrollbar::rows(v.scroll, v.rows.len(), body.height);
    scrollbar::render(track, buf, t, thumb);
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
            message_count: None,
            thread_metadata: None,
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
        // And every one of them is one column wide, which is the property the
        // rows depend on.
        for kind in [
            ChannelKind::GuildText,
            ChannelKind::GuildAnnouncement,
            ChannelKind::GuildForum,
            ChannelKind::GuildMedia,
            ChannelKind::GuildVoice,
            ChannelKind::GuildStageVoice,
            ChannelKind::PublicThread,
            ChannelKind::Unknown(99),
        ] {
            let mark = sigil(kind);
            assert!(mark.is_ascii(), "{kind:?} is marked with {mark:?}");
        }
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
        };
        let body = Rect::new(0, 2, 20, 2);
        assert_eq!(row_at(body, &v, 1), None);
        assert_eq!(row_at(body, &v, 2), Some(1));
        assert_eq!(row_at(body, &v, 3), Some(2));
        assert_eq!(row_at(body, &v, 4), None);
    }

    /// A thread hangs under the channel it belongs to, indented like a
    /// category's children and marked with its own sigil.
    ///
    /// `State::channels_ordered` is what puts it there; what this asserts is
    /// that the row builder keeps it rather than dropping it for not being a
    /// plain text channel.
    #[test]
    fn a_thread_is_a_row_under_its_channel() {
        use crate::discord::model::channel::ThreadMetadata;
        let general = std::sync::Arc::new(crate::discord::model::Channel {
            id: ChannelId(1),
            kind: ChannelKind::GuildText,
            name: Some("general".into()),
            ..Default::default()
        });
        let thread = std::sync::Arc::new(crate::discord::model::Channel {
            id: ChannelId(11),
            kind: ChannelKind::PublicThread,
            name: Some("about the deploy".into()),
            parent_id: Some(ChannelId(1)),
            thread_metadata: Some(ThreadMetadata {
                archived: false,
                ..Default::default()
            }),
            ..Default::default()
        });
        let rows = rows(&[general, thread], &HashSet::new(), false, |_| {
            crate::discord::state::Unread::default()
        });
        assert_eq!(rows.len(), 2, "{rows:?}");
        match &rows[1] {
            Row::Channel {
                name, kind, nested, ..
            } => {
                assert_eq!(name, "about the deploy");
                assert!(kind.is_thread());
                assert!(nested, "a thread is drawn under its channel");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(sigil(ChannelKind::PublicThread), '>');
    }
}
