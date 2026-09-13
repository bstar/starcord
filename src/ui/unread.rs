//! Hopping to the next thing worth reading, and deciding whether to interrupt.
//!
//! Two small pure functions, kept out of the dispatcher because both are
//! decisions with edge cases and neither needs anything but its arguments —
//! which is what makes them testable without a terminal, a core or a clock.
//!
//! ## Mentions come first
//!
//! `alt+down` walks the channels that have something in them, and it walks the
//! ones that named you before the ones that merely have traffic. That is the
//! order somebody catching up actually wants: eleven unread in `#random` can
//! wait, and the one in `#deploys` that said your name cannot. Within each
//! group the order is the channel list's own, so the hop is predictable rather
//! than a ranking.
//!
//! It is one list rather than two groups walked in turn, and that matters: from
//! the last mention there is nowhere else in that group to go, so two groups
//! would send every press back to the mention it had just left. One list makes
//! every press progress, and still starts at a mention.
//!
//! It wraps, because the list is a loop and stopping at the end would mean
//! pressing a key that does nothing while there is still something unread
//! above.
//!
//! ## Notifying is the UI's half of a decision the core has already made
//!
//! The core decides whether a message deserves a desktop notification and
//! delivers it. What is left here is the terminal's own half — the bell and
//! the line in the status bar — and the one rule is **not for the channel that
//! is open while the window is in front**, because telling somebody about the
//! message they are watching arrive is noise.

use crate::discord::snowflake::ChannelId;

/// One channel, as the hop sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stop {
    pub channel: ChannelId,
    pub unread: bool,
    pub mentions: u32,
    pub muted: bool,
}

/// The next channel worth going to, in `order`, starting after `from`.
///
/// `forward` walks down the list and `!forward` walks up it. `None` when
/// nothing anywhere is unread, which is the state that should leave the key
/// doing nothing rather than moving the cursor somewhere arbitrary.
pub fn hop(order: &[Stop], from: Option<ChannelId>, forward: bool) -> Option<ChannelId> {
    // One list: the channels that named you, in the order the channel list
    // puts them, and then the ones that merely have traffic, in the same
    // order. A muted channel is never a stop -- muting is the one instruction
    // a person gives a client about what may interrupt them.
    //
    // One list rather than two groups walked in turn, because two would mean
    // that leaving a mention sent you straight back to it: from the last
    // mention there is nowhere else in that group to go. This way every press
    // makes progress and the loop still starts at a mention.
    let mut stops: Vec<ChannelId> = order
        .iter()
        .filter(|s| !s.muted && s.mentions > 0)
        .map(|s| s.channel)
        .collect();
    stops.extend(
        order
            .iter()
            .filter(|s| !s.muted && s.mentions == 0 && s.unread)
            .map(|s| s.channel),
    );
    if stops.is_empty() {
        return None;
    }

    let here = from.and_then(|c| stops.iter().position(|s| *s == c));
    let n = stops.len() as isize;
    let next = match here {
        Some(at) => (at as isize + if forward { 1 } else { -1 }).rem_euclid(n) as usize,
        // Not a stop at all: forward starts at the head of the list, which is
        // the first mention, and backward at the end of it. That is what
        // "next" and "previous" mean to somebody who is somewhere calm.
        None => {
            if forward {
                0
            } else {
                stops.len() - 1
            }
        }
    };
    Some(stops[next])
}

/// Whether the terminal should say something about a message.
///
/// The desktop notification is the core's; this is the bell and the status
/// line, which is why `enabled` here is `[notify] enabled` and not
/// `[notify] desktop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Where {
    /// The channel the message arrived in.
    pub channel: ChannelId,
    /// The channel that is open.
    pub open: Option<ChannelId>,
    pub terminal_focused: bool,
}

/// What the terminal does about a mention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Interrupt {
    pub bell: bool,
    pub note: bool,
}

impl Interrupt {
    pub fn nothing(self) -> bool {
        !self.bell && !self.note
    }
}

/// Decide. Pure, so the whole table below runs without a terminal.
pub fn interrupt(where_: Where, enabled: bool, bell: bool, muted: bool) -> Interrupt {
    if !enabled || muted {
        return Interrupt::default();
    }
    // The one suppression: the channel on screen, while the window is in
    // front. Anything else — another channel, another window, the same channel
    // behind another window — is worth saying.
    if where_.terminal_focused && where_.open == Some(where_.channel) {
        return Interrupt::default();
    }
    Interrupt { bell, note: true }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(id: u64, unread: bool, mentions: u32) -> Stop {
        Stop {
            channel: ChannelId(id),
            unread,
            mentions,
            muted: false,
        }
    }

    fn muted(id: u64) -> Stop {
        Stop {
            channel: ChannelId(id),
            unread: true,
            mentions: 3,
            muted: true,
        }
    }

    /// A channel that named you is the next stop even when it is further down
    /// the list than one that merely has traffic.
    #[test]
    fn a_mention_is_reached_before_ordinary_traffic() {
        let order = vec![
            stop(1, true, 0),
            stop(2, false, 0),
            stop(3, true, 2),
            stop(4, true, 0),
        ];
        assert_eq!(hop(&order, None, true), Some(ChannelId(3)));
        assert_eq!(
            hop(&order, Some(ChannelId(2)), true),
            Some(ChannelId(3)),
            "from somewhere with nothing in it, the mention is first"
        );
        // And from the mention, on through the rest in the list's own order.
        assert_eq!(hop(&order, Some(ChannelId(3)), true), Some(ChannelId(1)));
        assert_eq!(hop(&order, Some(ChannelId(1)), true), Some(ChannelId(4)));
        assert_eq!(
            hop(&order, Some(ChannelId(4)), true),
            Some(ChannelId(3)),
            "and round to the mention again"
        );
    }

    /// Once the mentions are dealt with, the rest are walked in the list's own
    /// order and the walk wraps.
    #[test]
    fn without_mentions_it_walks_the_list_and_wraps() {
        let order = vec![
            stop(1, true, 0),
            stop(2, false, 0),
            stop(3, true, 0),
            stop(4, true, 0),
        ];
        assert_eq!(hop(&order, None, true), Some(ChannelId(1)));
        assert_eq!(hop(&order, Some(ChannelId(1)), true), Some(ChannelId(3)));
        assert_eq!(hop(&order, Some(ChannelId(3)), true), Some(ChannelId(4)));
        assert_eq!(
            hop(&order, Some(ChannelId(4)), true),
            Some(ChannelId(1)),
            "past the end is the beginning"
        );
        assert_eq!(
            hop(&order, Some(ChannelId(1)), false),
            Some(ChannelId(4)),
            "and backwards from the first is the last"
        );
    }

    /// Standing in a channel that is not unread at all: forward goes to the
    /// first, backward to the last.
    #[test]
    fn from_somewhere_quiet_it_starts_at_the_right_end() {
        let order = vec![stop(1, true, 0), stop(2, false, 0), stop(3, true, 0)];
        assert_eq!(hop(&order, Some(ChannelId(2)), true), Some(ChannelId(1)));
        assert_eq!(hop(&order, Some(ChannelId(2)), false), Some(ChannelId(3)));
    }

    /// Every stop is reached, once, before any of them is reached twice.
    #[test]
    fn a_lap_visits_everything_unread() {
        let order = vec![
            stop(1, true, 0),
            stop(2, true, 3),
            stop(3, false, 0),
            stop(4, true, 0),
            stop(5, true, 1),
        ];
        let mut seen = Vec::new();
        let mut at = None;
        for _ in 0..4 {
            at = hop(&order, at, true);
            seen.push(at.unwrap());
        }
        assert_eq!(
            seen,
            vec![ChannelId(2), ChannelId(5), ChannelId(1), ChannelId(4)],
            "the mentions lead, then the rest, each once"
        );
        assert_eq!(hop(&order, at, true), Some(ChannelId(2)), "then round");
    }

    /// The last mention leads back into the ordinary unread rather than
    /// looping around the mentions for ever.
    #[test]
    fn the_last_mention_hands_over_to_the_rest() {
        let order = vec![stop(1, true, 1), stop(2, true, 0)];
        // Standing in the only mentioned channel: the next stop is the
        // ordinary unread one, not itself.
        assert_eq!(hop(&order, Some(ChannelId(1)), true), Some(ChannelId(2)));
        // And with two mentions it walks them first.
        let two = vec![stop(1, true, 1), stop(2, true, 0), stop(3, true, 2)];
        assert_eq!(hop(&two, Some(ChannelId(1)), true), Some(ChannelId(3)));
        assert_eq!(hop(&two, Some(ChannelId(3)), true), Some(ChannelId(2)));

        // With nothing mentioned at all it is the plain list.
        let quiet = vec![stop(1, true, 0), stop(2, true, 0)];
        assert_eq!(hop(&quiet, Some(ChannelId(1)), true), Some(ChannelId(2)));

        // And with nowhere else to go it stays put rather than answering
        // nothing, which would be a key that does nothing while something is
        // still unread.
        let only = vec![stop(1, true, 1)];
        assert_eq!(hop(&only, Some(ChannelId(1)), true), Some(ChannelId(1)));
    }

    #[test]
    fn nothing_unread_is_nowhere_to_go() {
        assert_eq!(hop(&[], None, true), None);
        let read = vec![stop(1, false, 0), stop(2, false, 0)];
        assert_eq!(hop(&read, None, true), None);
        assert_eq!(hop(&[muted(9)], None, true), None, "muted is not a stop");
    }

    /// The whole notification table, without a terminal.
    #[test]
    fn the_channel_on_screen_in_front_says_nothing() {
        let open = ChannelId(1);
        let here = Where {
            channel: open,
            open: Some(open),
            terminal_focused: true,
        };
        assert!(interrupt(here, true, true, false).nothing());

        // The same channel, window behind something else.
        let behind = Where {
            terminal_focused: false,
            ..here
        };
        assert_eq!(
            interrupt(behind, true, true, false),
            Interrupt {
                bell: true,
                note: true
            }
        );

        // Another channel, window in front.
        let elsewhere = Where {
            channel: ChannelId(2),
            ..here
        };
        assert_eq!(
            interrupt(elsewhere, true, true, false),
            Interrupt {
                bell: true,
                note: true
            }
        );

        // The bell off leaves the note, which is the point of having two.
        assert_eq!(
            interrupt(elsewhere, true, false, false),
            Interrupt {
                bell: false,
                note: true
            }
        );

        // Off, and muted, are both silence.
        assert!(interrupt(elsewhere, false, true, false).nothing());
        assert!(interrupt(elsewhere, true, true, true).nothing());
    }
}
