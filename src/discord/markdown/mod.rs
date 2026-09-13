//! Discord's markdown, parsed into something the chat panel can draw.
//!
//! Three properties, and they are the whole design:
//!
//! **[`parse`] is total.** It has no error type and no failure mode. Whatever
//! bytes arrive, a `Document` comes back, because the alternative is a message
//! somebody sent that this client refuses to show. A marker with no partner is
//! text; a construct that does not complete is text; a nesting bomb stops
//! nesting and becomes text.
//!
//! **It is bounded.** The input is cut at [`MAX_INPUT`] characters and markers
//! stop nesting at sixteen levels. Discord's own limit is 4000 characters for a
//! message and 2000 for one this client sends, so the cap is only ever reached
//! by something that is not a message.
//!
//! **It is hand-written.** No regex, for the ordinary reason that Discord's
//! rules are not regular — matched backtick runs, nesting, the word-boundary
//! behaviour of `_` — and for the less ordinary one that a regex over
//! attacker-supplied text is a performance question nobody wants to have to
//! answer.
//!
//! What it is *not* is a renderer. Nothing here knows a colour, a width or a
//! terminal, which is what lets `starcord probe` print
//! `parse(&content).plain_text()` with no UI compiled in at all.

pub mod ast;
pub mod block;
pub mod emoji;
pub mod inline;

// Named here rather than reached for through `ast`, because the UI writes
// `markdown::Block`. Most of them have no consumer until the chat panel
// exists.
#[allow(unused_imports)]
pub use ast::{Block, Document, Emoji, Inline, ListItem, Mention, Spoilers};

/// The longest message this parser will look at.
///
/// Discord's own maximum is 4000 characters with Nitro and 2000 without. The
/// cap is not about those; it is about the message that arrives from somewhere
/// else claiming to be forty megabytes.
pub const MAX_INPUT: usize = 4096;

/// Parse a message body. Never fails.
pub fn parse(text: &str) -> Document {
    let mut truncated = false;
    let source = if text.chars().count() > MAX_INPUT {
        truncated = true;
        text.chars().take(MAX_INPUT).collect::<String>()
    } else {
        text.to_string()
    };

    Document {
        blocks: block::parse(&source, 0),
        truncated,
    }
}

/// The message with every marker taken out, for a notification body, a
/// one-line preview, or `starcord probe`.
pub fn plain_text(text: &str) -> String {
    parse(text).plain_text()
}

/// The same, with `||spoilers||` replaced by `[spoiler]`.
///
/// For desktop notifications, which are the one place the plain text is shown
/// to somebody who has not asked to see it: a spoiler is the one piece of a
/// message its author deliberately hid, and a popup that reveals it has
/// defeated the point of writing it that way.
pub fn plain_text_hiding_spoilers(text: &str) -> String {
    parse(text).plain_text_with(ast::Spoilers::Hide)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::snowflake::{ChannelId, EmojiId, RoleId, UserId};

    /// The inlines of a document that is one paragraph, which most of the table
    /// tests below are.
    fn inlines(text: &str) -> Vec<Inline> {
        match parse(text).blocks.as_slice() {
            [Block::Paragraph(inlines)] => inlines.clone(),
            other => panic!("expected one paragraph, got {other:?}"),
        }
    }

    fn text(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    #[test]
    fn plain_text_is_the_message_without_the_markers() {
        assert_eq!(plain_text("**bold** and *italic*"), "bold and italic");
        assert_eq!(plain_text("||a spoiler||"), "a spoiler");
        assert_eq!(plain_text("~~struck~~"), "struck");
        assert_eq!(plain_text("`code`"), "code");
    }

    #[test]
    fn the_emphasis_markers_are_discords_rather_than_commonmarks() {
        assert_eq!(inlines("**b**"), vec![Inline::Bold(vec![text("b")])]);
        assert_eq!(inlines("*i*"), vec![Inline::Italic(vec![text("i")])]);
        assert_eq!(inlines("_i_"), vec![Inline::Italic(vec![text("i")])]);
        assert_eq!(
            inlines("__u__"),
            vec![Inline::Underline(vec![text("u")])],
            "__ is underline here, not bold"
        );
        assert_eq!(inlines("~~s~~"), vec![Inline::Strike(vec![text("s")])]);
        assert_eq!(inlines("||s||"), vec![Inline::Spoiler(vec![text("s")])]);
        assert_eq!(
            inlines("***bi***"),
            vec![Inline::Bold(vec![Inline::Italic(vec![text("bi")])])]
        );
    }

    #[test]
    fn an_underscore_inside_a_word_is_part_of_the_word() {
        assert_eq!(
            inlines("some_variable_name"),
            vec![text("some_variable_name")],
            "snake_case became an italic"
        );
        assert_eq!(plain_text("a_b_c"), "a_b_c");
        assert_eq!(inlines("_yes_"), vec![Inline::Italic(vec![text("yes")])]);
    }

    #[test]
    fn an_asterisk_with_spaces_around_it_is_arithmetic() {
        assert_eq!(inlines("2 * 3 * 4"), vec![text("2 * 3 * 4")]);
        assert_eq!(plain_text("2 * 3 * 4"), "2 * 3 * 4");
    }

    #[test]
    fn an_unclosed_marker_is_just_a_character() {
        assert_eq!(inlines("**unfinished"), vec![text("**unfinished")]);
        assert_eq!(inlines("a | b"), vec![text("a | b")]);
        assert_eq!(plain_text("~~~"), "~~~");
    }

    #[test]
    fn a_backslash_escapes_punctuation_and_nothing_else() {
        assert_eq!(inlines("\\*not italic\\*"), vec![text("*not italic*")]);
        assert_eq!(
            inlines("C:\\\\path"),
            vec![text("C:\\path")],
            "a doubled backslash is one backslash"
        );
        assert_eq!(
            inlines("\\n"),
            vec![text("\\n")],
            "a backslash before a letter is a backslash"
        );
    }

    #[test]
    fn a_backtick_run_closes_on_a_run_of_the_same_length() {
        assert_eq!(inlines("`a`"), vec![Inline::Code("a".into())]);
        assert_eq!(
            inlines("``a ` b``"),
            vec![Inline::Code("a ` b".into())],
            "a doubled run must be able to hold a backtick"
        );
        assert_eq!(
            inlines("`**not bold**`"),
            vec![Inline::Code("**not bold**".into())],
            "markers inside code are text"
        );
    }

    #[test]
    fn a_code_span_keeps_a_marker_from_closing_one_outside_it() {
        assert_eq!(
            inlines("**a `**` b**"),
            vec![Inline::Bold(vec![
                text("a "),
                Inline::Code("**".into()),
                text(" b")
            ])]
        );
    }

    #[test]
    fn the_mention_forms_all_parse() {
        assert_eq!(
            inlines("<@123>"),
            vec![Inline::Mention(Mention::User(UserId(123)))]
        );
        assert_eq!(
            inlines("<@!123>"),
            vec![Inline::Mention(Mention::User(UserId(123)))],
            "the legacy nickname spelling is the same mention"
        );
        assert_eq!(
            inlines("<#456>"),
            vec![Inline::Mention(Mention::Channel(ChannelId(456)))]
        );
        assert_eq!(
            inlines("<@&789>"),
            vec![Inline::Mention(Mention::Role(RoleId(789)))]
        );
        assert_eq!(
            inlines("</settings:12>"),
            vec![Inline::Mention(Mention::Command {
                name: "settings".into(),
                id: 12
            })]
        );
        assert_eq!(
            inlines("@everyone"),
            vec![Inline::Mention(Mention::Everyone)]
        );
        assert_eq!(inlines("@here"), vec![Inline::Mention(Mention::Here)]);
        assert_eq!(
            inlines("@hereabouts"),
            vec![text("@hereabouts")],
            "a word beginning with @here is a word"
        );
    }

    #[test]
    fn something_shaped_like_a_mention_but_is_not_stays_text() {
        for source in ["<@abc>", "<@>", "<#>", "</name>", "<@&>", "<not a mention>"] {
            assert_eq!(
                plain_text(source),
                source,
                "{source} lost characters on the way through"
            );
        }
    }

    #[test]
    fn custom_emoji_and_timestamps() {
        assert_eq!(
            inlines("<:pepe:12345>"),
            vec![Inline::Emoji(Emoji::Custom {
                name: "pepe".into(),
                id: EmojiId(12345),
                animated: false
            })]
        );
        assert_eq!(
            inlines("<a:dance:12345>"),
            vec![Inline::Emoji(Emoji::Custom {
                name: "dance".into(),
                id: EmojiId(12345),
                animated: true
            })]
        );
        assert_eq!(
            inlines("<t:1700000000>"),
            vec![Inline::Timestamp {
                unix: 1_700_000_000,
                style: None
            }]
        );
        assert_eq!(
            inlines("<t:1700000000:R>"),
            vec![Inline::Timestamp {
                unix: 1_700_000_000,
                style: Some('R')
            }]
        );
    }

    #[test]
    fn a_unicode_emoji_comes_out_as_one_unit() {
        assert_eq!(
            inlines("hi 👋"),
            vec![text("hi "), Inline::Emoji(Emoji::Unicode("👋".into()))]
        );
        let family = "👨‍👩‍👧‍👦";
        assert_eq!(
            inlines(family),
            vec![Inline::Emoji(Emoji::Unicode(family.into()))],
            "a joined family must not become four people"
        );
        assert_eq!(plain_text("a 🎉 b"), "a 🎉 b");
    }

    #[test]
    fn links_are_recognised_in_all_three_spellings() {
        assert_eq!(
            inlines("https://example.invalid/x"),
            vec![Inline::Link {
                text: Vec::new(),
                url: "https://example.invalid/x".into(),
                suppressed: false
            }]
        );
        assert_eq!(
            inlines("<https://example.invalid/x>"),
            vec![Inline::Link {
                text: Vec::new(),
                url: "https://example.invalid/x".into(),
                suppressed: true
            }]
        );
        assert_eq!(
            inlines("[a page](https://example.invalid/x)"),
            vec![Inline::Link {
                text: vec![text("a page")],
                url: "https://example.invalid/x".into(),
                suppressed: false
            }]
        );
    }

    #[test]
    fn a_masked_link_only_accepts_a_web_scheme() {
        // The one construct where what is shown and what is opened differ.
        for source in [
            "[click](file:///etc/passwd)",
            "[click](javascript:alert(1))",
            "[click](discord://x)",
        ] {
            assert_eq!(
                plain_text(source),
                source,
                "{source} was turned into a link"
            );
        }
    }

    #[test]
    fn the_sentence_around_a_bare_link_is_not_part_of_the_address() {
        let parsed = inlines("see https://example.invalid/a.");
        assert_eq!(
            parsed,
            vec![
                text("see "),
                Inline::Link {
                    text: Vec::new(),
                    url: "https://example.invalid/a".into(),
                    suppressed: false
                },
                text(".")
            ]
        );

        // A bracket the URL opened is part of it.
        let wiki = inlines("https://example.invalid/Foo_(bar)");
        assert_eq!(
            wiki,
            vec![Inline::Link {
                text: Vec::new(),
                url: "https://example.invalid/Foo_(bar)".into(),
                suppressed: false
            }]
        );
    }

    #[test]
    fn the_block_constructs_all_parse() {
        assert_eq!(
            parse("# Title").blocks,
            vec![Block::Heading {
                level: 1,
                content: vec![text("Title")]
            }]
        );
        assert_eq!(
            parse("### Small").blocks,
            vec![Block::Heading {
                level: 3,
                content: vec![text("Small")]
            }]
        );
        assert_eq!(
            parse("-# quiet").blocks,
            vec![Block::Subtext(vec![text("quiet")])]
        );
        assert_eq!(
            parse("> quoted").blocks,
            vec![Block::Quote(vec![Block::Paragraph(vec![text("quoted")])])]
        );
        assert_eq!(
            parse("#### not a heading").blocks,
            vec![Block::Paragraph(vec![text("#### not a heading")])],
            "Discord has three heading levels"
        );
    }

    #[test]
    fn a_triple_quote_takes_the_rest_of_the_message() {
        let doc = parse(">>> first\nsecond\nthird");
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.plain_text(), "first\nsecond\nthird");
    }

    #[test]
    fn a_quote_ends_where_the_markers_stop() {
        let doc = parse("> in\nout");
        assert_eq!(
            doc.blocks,
            vec![
                Block::Quote(vec![Block::Paragraph(vec![text("in")])]),
                Block::Paragraph(vec![text("out")]),
            ]
        );
    }

    #[test]
    fn lists_keep_the_numbers_that_were_written() {
        let doc = parse("1. one\n7. seven");
        match doc.blocks.as_slice() {
            [Block::List { ordered, items }] => {
                assert!(ordered);
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].number, Some(1));
                assert_eq!(
                    items[1].number,
                    Some(7),
                    "a renderer that counts for itself shows a number nobody typed"
                );
            }
            other => panic!("{other:?}"),
        }

        let bullets = parse("- a\n- b");
        match bullets.blocks.as_slice() {
            [Block::List { ordered, items }] => {
                assert!(!ordered);
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].number, None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_indented_bullet_records_its_level() {
        let doc = parse("- top\n  - under");
        match doc.blocks.as_slice() {
            [Block::List { items, .. }] => {
                assert_eq!(items[0].indent, 0);
                assert_eq!(items[1].indent, 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_fenced_block_keeps_its_language_and_its_text() {
        assert_eq!(
            parse("```rust\nfn main() {}\n```").blocks,
            vec![Block::CodeBlock {
                lang: Some("rust".into()),
                text: "fn main() {}".into()
            }]
        );
        assert_eq!(
            parse("```\nplain\n```").blocks,
            vec![Block::CodeBlock {
                lang: None,
                text: "plain".into()
            }]
        );
    }

    #[test]
    fn an_unclosed_fence_is_not_a_code_block() {
        let doc = parse("```rust\nfn main() {}");
        assert_eq!(doc.plain_text(), "```rust\nfn main() {}");
    }

    #[test]
    fn text_after_a_closing_fence_is_not_thrown_away() {
        let doc = parse("```\ncode\n``` after");
        assert_eq!(doc.plain_text(), "code\n after");
    }

    #[test]
    fn a_very_long_message_is_cut_rather_than_parsed_whole() {
        let long = "a".repeat(MAX_INPUT + 500);
        let doc = parse(&long);
        assert!(doc.truncated);
        assert_eq!(doc.plain_text().chars().count(), MAX_INPUT);

        let ordinary = parse("short");
        assert!(!ordinary.truncated);
    }

    /// The bomb. Every one of these used to be a way to make a naive recursive
    /// descent parser either recurse to a stack overflow or go quadratic, and
    /// none of them may take longer than the test runner's patience.
    #[test]
    fn a_nesting_bomb_terminates() {
        let cases = [
            "*".repeat(MAX_INPUT),
            "**".repeat(MAX_INPUT / 2),
            "||".repeat(MAX_INPUT / 2),
            "`".repeat(MAX_INPUT),
            "[".repeat(MAX_INPUT),
            "<".repeat(MAX_INPUT),
            ">".repeat(MAX_INPUT),
            "> ".repeat(MAX_INPUT / 2),
            "\\".repeat(MAX_INPUT),
            "```".repeat(MAX_INPUT / 3),
            "**_~~||".repeat(MAX_INPUT / 7),
            "- ".repeat(MAX_INPUT / 2),
            format!("{}x{}", "**a ".repeat(200), " a**".repeat(200)),
            format!("{}deep{}", "||".repeat(200), "||".repeat(200)),
        ];

        for case in cases {
            let started = std::time::Instant::now();
            let doc = parse(&case);
            let _ = doc.plain_text();
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "parsing {} characters of {:?} took {:?}",
                case.chars().count(),
                case.chars().take(8).collect::<String>(),
                started.elapsed()
            );
        }
    }

    /// Nesting stops at the cap, and the markers past it are ordinary
    /// characters rather than a deeper tree.
    ///
    /// A quote is the construct that nests most easily -- `> > > x` is three
    /// levels and one line -- so it is the one the cap is measured on.
    #[test]
    fn nesting_stops_at_the_cap() {
        fn depth_of(blocks: &[Block]) -> usize {
            blocks
                .iter()
                .map(|block| match block {
                    Block::Quote(inner) => 1 + depth_of(inner),
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
        }

        let source = format!("{}x", "> ".repeat(inline::MAX_DEPTH as usize * 3));
        let doc = parse(&source);
        let depth = depth_of(&doc.blocks);
        assert!(
            depth <= inline::MAX_DEPTH as usize,
            "quotes nested {depth} deep past a cap of {}",
            inline::MAX_DEPTH
        );
        assert!(
            doc.plain_text().contains('x'),
            "the content under the cap was lost"
        );
    }

    /// The same for inlines, over every marker the parser recurses through.
    #[test]
    fn inline_nesting_stops_at_the_cap() {
        fn depth_of(inlines: &[Inline]) -> usize {
            inlines
                .iter()
                .map(|inline| match inline {
                    Inline::Bold(inner)
                    | Inline::Italic(inner)
                    | Inline::Underline(inner)
                    | Inline::Strike(inner)
                    | Inline::Spoiler(inner) => 1 + depth_of(inner),
                    Inline::Link { text, .. } => 1 + depth_of(text),
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
        }

        // Four marker kinds cycling, which is the only way to nest without a
        // run of one character closing itself.
        let levels = inline::MAX_DEPTH as usize;
        let source = format!(
            "{}x{}",
            "**__~~||".repeat(levels),
            "||~~__**".repeat(levels)
        );
        let doc = parse(&source);
        let depth = match doc.blocks.as_slice() {
            [Block::Paragraph(inlines)] => depth_of(inlines),
            other => panic!("{other:?}"),
        };
        assert!(
            depth <= inline::MAX_DEPTH as usize + 1,
            "markers nested {depth} deep past a cap of {}",
            inline::MAX_DEPTH
        );
        assert!(doc.plain_text().contains('x'));
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(512))]

        /// The contract, asserted rather than described: parsing anything at
        /// all produces a document, and the plain text of that document
        /// contains every alphanumeric character of the source, in the order
        /// they were written.
        ///
        /// Alphanumerics rather than all characters because markers are
        /// *supposed* to disappear — the point of the parser — while the
        /// content they mark is not.
        #[test]
        fn plain_text_keeps_every_alphanumeric_in_order(
            source in r"[a-zA-Z0-9 *_~|`<>@#:/\\\[\]()\-.\n]{0,240}"
        ) {
            let plain = parse(&source).plain_text();
            let mut written = plain.chars().filter(|c| c.is_alphanumeric());
            for wanted in source.chars().filter(|c| c.is_alphanumeric()) {
                let mut found = false;
                for got in written.by_ref() {
                    if got == wanted {
                        found = true;
                        break;
                    }
                }
                proptest::prop_assert!(
                    found,
                    "{:?} lost {:?} on the way to {:?}",
                    source,
                    wanted,
                    plain
                );
            }
        }

        /// Bytes somebody else wrote, over the whole of unicode.
        #[test]
        fn parsing_anything_never_panics(source in ".{0,400}") {
            let doc = parse(&source);
            let _ = doc.plain_text();
        }

        #[test]
        fn message_content_is_never_a_panic(source in r"(\PC|\n){0,200}") {
            let doc = parse(&source);
            let _ = doc.plain_text();
            let _ = doc.is_empty();
        }
    }
}
