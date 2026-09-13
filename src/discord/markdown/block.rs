//! The block pass: what a message looks like line by line.
//!
//! Discord's block constructs are all line-anchored — a quote, a heading, a
//! bullet and a fence are only those things at the start of a line — so this
//! pass works on lines and hands whatever is left to the inline parser.
//!
//! A construct that does not complete is not a construct. A fence with no
//! closing fence is three backticks and some text, and that is what the reader
//! sees in Discord too.

use super::ast::{Block, ListItem};
use super::inline;

/// Strip a quote marker, returning what is quoted.
fn quote_body(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix("> ") {
        return Some(rest);
    }
    if line == ">" {
        return Some("");
    }
    None
}

/// `#`, `##`, `###` and nothing deeper: Discord has three heading levels.
fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if !(1..=3).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    let body = rest.strip_prefix(' ')?;
    if body.trim().is_empty() {
        return None;
    }
    Some((hashes as u8, body))
}

/// A bullet or a number, and what follows it.
struct Marker<'a> {
    ordered: bool,
    number: Option<u64>,
    indent: u8,
    body: &'a str,
}

fn list_marker(line: &str) -> Option<Marker<'_>> {
    let spaces = line.len() - line.trim_start_matches(' ').len();
    // Two columns to a level, as Discord's own editor indents.
    let indent = (spaces / 2).min(u8::MAX as usize) as u8;
    let rest = &line[spaces..];

    for bullet in ["- ", "* ", "+ "] {
        if let Some(body) = rest.strip_prefix(bullet) {
            return Some(Marker {
                ordered: false,
                number: None,
                indent,
                body,
            });
        }
    }

    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    // Discord's own limit; more than three digits is text that happens to have
    // a full stop after it.
    if digits == 0 || digits > 3 {
        return None;
    }
    let after = &rest[digits..];
    let body = after
        .strip_prefix(". ")
        .or_else(|| after.strip_prefix(") "))?;
    // `007.` is not a list marker. The number is stored rather than the text it
    // was written as, and a renderer drawing `7.` where the message says `007.`
    // is showing something nobody typed.
    let written = &rest[..digits];
    if written.len() > 1 && written.starts_with('0') {
        return None;
    }
    Some(Marker {
        ordered: true,
        number: written.parse().ok(),
        indent,
        body,
    })
}

/// Parse a whole message body into blocks.
pub fn parse(text: &str, depth: u8) -> Vec<Block> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut blocks: Vec<Block> = Vec::new();
    let mut i = 0usize;
    let markers = depth < inline::MAX_DEPTH;

    while i < lines.len() {
        let line = lines[i];

        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        if let Some(Fenced {
            block,
            next,
            trailing,
        }) = fence(&lines, i)
        {
            blocks.push(block);
            if !trailing.trim().is_empty() {
                blocks.push(Block::Paragraph(inline::parse(
                    &trailing.chars().collect::<Vec<_>>(),
                    depth,
                )));
            }
            i = next;
            continue;
        }

        if markers {
            // `>>> ` quotes the rest of the message and has no closing marker.
            if let Some(first) =
                line.strip_prefix(">>> ")
                    .or_else(|| if line == ">>>" { Some("") } else { None })
            {
                let mut body = vec![first];
                body.extend_from_slice(&lines[i + 1..]);
                blocks.push(Block::Quote(parse(&body.join("\n"), depth + 1)));
                break;
            }

            if quote_body(line).is_some() {
                let mut body = Vec::new();
                while i < lines.len() {
                    let Some(rest) = quote_body(lines[i]) else {
                        break;
                    };
                    body.push(rest);
                    i += 1;
                }
                blocks.push(Block::Quote(parse(&body.join("\n"), depth + 1)));
                continue;
            }
        }

        if let Some((level, body)) = heading(line) {
            blocks.push(Block::Heading {
                level,
                content: inline::parse(&body.chars().collect::<Vec<_>>(), depth),
            });
            i += 1;
            continue;
        }

        if let Some(body) = line.strip_prefix("-# ") {
            blocks.push(Block::Subtext(inline::parse(
                &body.chars().collect::<Vec<_>>(),
                depth,
            )));
            i += 1;
            continue;
        }

        if let Some(marker) = list_marker(line) {
            let ordered = marker.ordered;
            let mut items: Vec<ListItem> = Vec::new();
            while i < lines.len() {
                let Some(marker) = list_marker(lines[i]) else {
                    break;
                };
                if marker.ordered != ordered {
                    // A numbered list following a bulleted one is a second
                    // list, not a continuation of the first.
                    break;
                }
                let mut body = vec![marker.body.to_string()];
                i += 1;
                // An indented line that is not itself a bullet continues the
                // item it follows.
                while i < lines.len()
                    && list_marker(lines[i]).is_none()
                    && lines[i].starts_with("  ")
                    && !lines[i].trim().is_empty()
                {
                    body.push(lines[i].trim_start().to_string());
                    i += 1;
                }
                items.push(ListItem {
                    number: marker.number,
                    indent: marker.indent,
                    blocks: vec![Block::Paragraph(inline::parse(
                        &body.join("\n").chars().collect::<Vec<_>>(),
                        depth,
                    ))],
                });
            }
            blocks.push(Block::List { ordered, items });
            continue;
        }

        // Anything else is a paragraph, running until something else starts.
        // The first line is taken unconditionally: it reached here because
        // nothing else claimed it, and a branch that can consume nothing is a
        // loop that never ends.
        let start = i;
        i += 1;
        while i < lines.len() && is_paragraph_line(lines[i], markers) {
            i += 1;
        }
        let body = lines[start..i].join("\n");
        blocks.push(Block::Paragraph(inline::parse(
            &body.chars().collect::<Vec<_>>(),
            depth,
        )));
    }

    blocks
}

fn is_paragraph_line(line: &str, markers: bool) -> bool {
    if line.trim().is_empty() {
        return false;
    }
    if line.trim_start().starts_with("```") {
        return false;
    }
    if markers && (quote_body(line).is_some() || line.starts_with(">>>")) {
        return false;
    }
    if heading(line).is_some() || line.starts_with("-# ") {
        return false;
    }
    list_marker(line).is_none()
}

/// A code block, and whatever shared the closing fence's line.
pub struct Fenced {
    pub block: Block,
    pub next: usize,
    /// Text after the closing fence. Kept rather than dropped: a message is not
    /// a place where a parser gets to decide some of it did not happen.
    pub trailing: String,
}

/// A fenced code block, if it closes.
fn fence(lines: &[&str], i: usize) -> Option<Fenced> {
    let opening = lines[i].trim_start();
    let rest = opening.strip_prefix("```")?;

    // ```` ```rust code``` ```` on one line.
    if let Some(end) = rest.find("```") {
        let (head, after) = rest.split_at(end);
        let (lang, text) = split_lang(head);
        return Some(Fenced {
            block: Block::CodeBlock {
                lang,
                text: text.to_string(),
            },
            next: i + 1,
            trailing: after[3..].to_string(),
        });
    }

    let (lang, first) = split_lang_line(rest);
    let mut body: Vec<String> = Vec::new();
    if let Some(first) = first {
        body.push(first.to_string());
    }

    let mut j = i + 1;
    while j < lines.len() {
        if let Some(end) = lines[j].find("```") {
            body.push(lines[j][..end].to_string());
            let text = body.join("\n");
            // Discord drops the newline before the closing fence.
            return Some(Fenced {
                block: Block::CodeBlock {
                    lang,
                    text: text.strip_suffix('\n').unwrap_or(&text).to_string(),
                },
                next: j + 1,
                trailing: lines[j][end + 3..].to_string(),
            });
        }
        body.push(lines[j].to_string());
        j += 1;
    }

    // No closing fence: this was never a code block.
    None
}

fn is_lang(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 20
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '#')
}

/// Split `rust\ncode` inside a one-line fence.
fn split_lang(head: &str) -> (Option<String>, &str) {
    match head.split_once('\n') {
        Some((word, rest)) if is_lang(word) => (Some(word.to_string()), rest),
        _ => (None, head),
    }
}

/// Split the remainder of an opening fence line into a language tag and
/// whatever else was on it.
fn split_lang_line(rest: &str) -> (Option<String>, Option<&str>) {
    if rest.is_empty() {
        return (None, None);
    }
    if is_lang(rest) {
        return (Some(rest.to_string()), None);
    }
    (None, Some(rest))
}
