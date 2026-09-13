//! The inline pass: everything that happens inside one line of a message.
//!
//! Hand-written, no regex, and one rule running through all of it: **the index
//! always advances.** Every branch either consumes what it matched or falls
//! through to "this character is text", so the loop terminates on any input at
//! all, which is what the property test under `timeout` is checking.
//!
//! A marker with no partner is not an error and is not dropped. `**unfinished`
//! is the text `**unfinished`, because that is what Discord shows and because a
//! parser that silently eats characters out of somebody's message is worse than
//! one that renders an asterisk.
//!
//! The delimiter rules are Discord's rather than CommonMark's, and the two
//! disagree in ways that matter:
//!
//! - `_` is italic, `__` is underline. CommonMark has no underline and treats
//!   `__` as bold.
//! - `_` only opens and closes at a word boundary, so `snake_case_name` is a
//!   name and not a word with an italic in the middle.
//! - `*` needs a non-space beside it on both sides, so `2 * 3 * 4` is
//!   arithmetic.
//! - A backtick run closes on a run of the same length, so `` ``a ` b`` ``
//!   holds a backtick.

use super::ast::{Emoji, Inline, Mention};
use super::emoji;
use crate::discord::snowflake::{ChannelId, EmojiId, RoleId, UserId};

/// How deep a marker may nest before markers become ordinary characters.
///
/// Sixteen is past anything a person writes and well short of anything that
/// costs time. It is the only defence the parser needs, because every other
/// construct is bounded by the input length.
pub const MAX_DEPTH: u8 = 16;

/// How far past a `<` to look for its `>` before giving up.
///
/// A mention is never longer than this, and without a bound a message full of
/// `<` characters is a quadratic scan.
const ANGLE_LOOKAHEAD: usize = 96;

/// Parse one run of characters into inlines.
pub fn parse(chars: &[char], depth: u8) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    let mut text = String::new();
    let mut i = 0usize;

    // Markers stop being markers once the nesting cap is reached. Everything
    // that does not recurse -- escapes, mentions, emoji -- still applies.
    let markers = depth < MAX_DEPTH;

    while i < chars.len() {
        let c = chars[i];

        if c == '\\' {
            // Discord escapes punctuation and nothing else: `\n` in a message
            // is a backslash and the letter n.
            match chars.get(i + 1) {
                Some(&next) if next.is_ascii_punctuation() => {
                    text.push(next);
                    i += 2;
                }
                _ => {
                    text.push('\\');
                    i += 1;
                }
            }
            continue;
        }

        if c == '\n' {
            flush(&mut text, &mut out);
            out.push(Inline::LineBreak);
            i += 1;
            continue;
        }

        if c == '`' {
            if let Some((code, next)) = code_span(chars, i) {
                flush(&mut text, &mut out);
                out.push(Inline::Code(code));
                i = next;
                continue;
            }
            text.push(c);
            i += 1;
            continue;
        }

        if c == '<' {
            if let Some((inline, next)) = angle(chars, i) {
                flush(&mut text, &mut out);
                out.push(inline);
                i = next;
                continue;
            }
            text.push(c);
            i += 1;
            continue;
        }

        if c == '@' {
            if let Some((mention, next)) = bare_mention(chars, i) {
                flush(&mut text, &mut out);
                out.push(Inline::Mention(mention));
                i = next;
                continue;
            }
            text.push(c);
            i += 1;
            continue;
        }

        if markers && c == '[' {
            if let Some((inline, next)) = masked_link(chars, i, depth) {
                flush(&mut text, &mut out);
                out.push(inline);
                i = next;
                continue;
            }
            text.push(c);
            i += 1;
            continue;
        }

        if (c == 'h' || c == 'H') && starts_url(chars, i) {
            let (url, next, trailing) = autolink(chars, i);
            flush(&mut text, &mut out);
            out.push(Inline::Link {
                text: Vec::new(),
                url,
                suppressed: false,
            });
            // Punctuation that was trimmed off the end of the URL is still part
            // of the sentence.
            text.push_str(&trailing);
            i = next;
            continue;
        }

        if markers {
            if let Some((inline, next)) = delimited(chars, i, depth) {
                flush(&mut text, &mut out);
                out.push(inline);
                i = next;
                continue;
            }
        }

        if let Some((found, len)) = emoji::scan(chars, i) {
            flush(&mut text, &mut out);
            out.push(Inline::Emoji(Emoji::Unicode(found)));
            i += len;
            continue;
        }

        text.push(c);
        i += 1;
    }

    flush(&mut text, &mut out);
    out
}

fn flush(text: &mut String, out: &mut Vec<Inline>) {
    if !text.is_empty() {
        out.push(Inline::Text(std::mem::take(text)));
    }
}

/// How many of `c` there are starting at `i`.
fn run(chars: &[char], i: usize, c: char) -> usize {
    let mut n = 0;
    while chars.get(i + n) == Some(&c) {
        n += 1;
    }
    n
}

/// A backtick run, closed by a run of the same length.
fn code_span(chars: &[char], i: usize) -> Option<(String, usize)> {
    let open = run(chars, i, '`');
    let start = i + open;
    let mut j = start;
    while j < chars.len() {
        if chars[j] == '`' {
            let close = run(chars, j, '`');
            if close == open {
                if j == start {
                    // ```` `` ```` with nothing between is not a code span.
                    return None;
                }
                let code: String = chars[start..j].iter().collect();
                return Some((code, j + close));
            }
            j += close;
            continue;
        }
        j += 1;
    }
    None
}

/// The delimiter table, longest spelling first so `**` is never read as two
/// italics and `***` is never read as a bold with a stray asterisk.
fn delimited(chars: &[char], i: usize, depth: u8) -> Option<(Inline, usize)> {
    let c = chars[i];
    if !matches!(c, '*' | '_' | '~' | '|') {
        return None;
    }
    let length = run(chars, i, c);

    match c {
        '*' => {
            if length >= 3 {
                if let Some(j) = find_run(chars, i + 3, '*', 3) {
                    let inner = parse(&chars[i + 3..j], depth + 2);
                    return Some((Inline::Bold(vec![Inline::Italic(inner)]), j + 3));
                }
            }
            if length >= 2 {
                if let Some(j) = find_run(chars, i + 2, '*', 2) {
                    let inner = parse(&chars[i + 2..j], depth + 1);
                    return Some((Inline::Bold(inner), j + 2));
                }
            }
            // A lone asterisk needs a non-space on the inside of both ends, or
            // `2 * 3 * 4` is an italic.
            if !chars.get(i + 1).is_some_and(|c| !c.is_whitespace()) {
                return None;
            }
            let j = find_run(chars, i + 1, '*', 1)?;
            if j == i + 1 || chars[j - 1].is_whitespace() {
                return None;
            }
            let inner = parse(&chars[i + 1..j], depth + 1);
            Some((Inline::Italic(inner), j + 1))
        }
        '_' => {
            if length >= 2 {
                if let Some(j) = find_run(chars, i + 2, '_', 2) {
                    let inner = parse(&chars[i + 2..j], depth + 1);
                    return Some((Inline::Underline(inner), j + 2));
                }
            }
            // The word-boundary rule, which is the whole reason `_` is not
            // simply `*` with a different character: a message full of
            // `some_variable_name` must come out unchanged.
            if i > 0 && is_word(chars[i - 1]) {
                return None;
            }
            let j = find_run(chars, i + 1, '_', 1)?;
            if j == i + 1 {
                return None;
            }
            if chars.get(j + 1).is_some_and(|&c| is_word(c)) {
                return None;
            }
            let inner = parse(&chars[i + 1..j], depth + 1);
            Some((Inline::Italic(inner), j + 1))
        }
        '~' => {
            if length < 2 {
                return None;
            }
            let j = find_run(chars, i + 2, '~', 2)?;
            let inner = parse(&chars[i + 2..j], depth + 1);
            Some((Inline::Strike(inner), j + 2))
        }
        '|' => {
            if length < 2 {
                return None;
            }
            let j = find_run(chars, i + 2, '|', 2)?;
            let inner = parse(&chars[i + 2..j], depth + 1);
            Some((Inline::Spoiler(inner), j + 2))
        }
        _ => None,
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Find a run of at least `want` copies of `c`, skipping escapes and code
/// spans, so `` `**` `` inside a code span does not close a bold outside one.
fn find_run(chars: &[char], from: usize, c: char, want: usize) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        match chars[j] {
            '\\' => {
                j += 2;
                continue;
            }
            '`' => {
                if let Some((_, next)) = code_span(chars, j) {
                    j = next;
                } else {
                    j += 1;
                }
                continue;
            }
            other if other == c => {
                if run(chars, j, c) >= want {
                    return Some(j);
                }
                j += run(chars, j, c);
                continue;
            }
            _ => j += 1,
        }
    }
    None
}

/// Everything spelled `<...>`: mentions, custom emoji, timestamps and links
/// whose unfurl the author suppressed.
fn angle(chars: &[char], i: usize) -> Option<(Inline, usize)> {
    let limit = (i + ANGLE_LOOKAHEAD).min(chars.len());
    let mut close = None;
    for (offset, &c) in chars[i + 1..limit].iter().enumerate() {
        if c == '>' {
            close = Some(i + 1 + offset);
            break;
        }
        // A mention has no whitespace and no second `<`; stopping early keeps
        // a message full of angle brackets linear.
        if c.is_whitespace() || c == '<' {
            break;
        }
    }
    let close = close?;
    let inner: String = chars[i + 1..close].iter().collect();
    let next = close + 1;

    if let Some(rest) = inner.strip_prefix("@&") {
        return digits(rest).map(|id| (Inline::Mention(Mention::Role(RoleId(id))), next));
    }
    if let Some(rest) = inner.strip_prefix("@!") {
        return digits(rest).map(|id| (Inline::Mention(Mention::User(UserId(id))), next));
    }
    if let Some(rest) = inner.strip_prefix('@') {
        return digits(rest).map(|id| (Inline::Mention(Mention::User(UserId(id))), next));
    }
    if let Some(rest) = inner.strip_prefix('#') {
        return digits(rest).map(|id| (Inline::Mention(Mention::Channel(ChannelId(id))), next));
    }
    if let Some(rest) = inner.strip_prefix('/') {
        // `</name sub:id>`: a subcommand's name has a space in it, which the
        // lookahead above already refused, so only the simple form parses.
        let (name, id) = rest.rsplit_once(':')?;
        if name.is_empty() {
            return None;
        }
        let id = digits(id)?;
        return Some((
            Inline::Mention(Mention::Command {
                name: name.to_string(),
                id,
            }),
            next,
        ));
    }
    if let Some(rest) = inner.strip_prefix("t:") {
        let (unix, style) = match rest.split_once(':') {
            Some((unix, style)) => {
                let mut styles = style.chars();
                let first = styles.next()?;
                if styles.next().is_some() || !first.is_ascii_alphabetic() {
                    return None;
                }
                (unix, Some(first))
            }
            None => (rest, None),
        };
        let parsed: i64 = unix.parse().ok()?;
        // Same rule as an id: only a canonical number, so that writing it back
        // out gives what was written in.
        if parsed.to_string() != unix.trim_start_matches('+') {
            return None;
        }
        return Some((
            Inline::Timestamp {
                unix: parsed,
                style,
            },
            next,
        ));
    }
    if let Some(rest) = inner.strip_prefix("a:") {
        return custom_emoji(rest, true).map(|e| (Inline::Emoji(e), next));
    }
    if let Some(rest) = inner.strip_prefix(':') {
        return custom_emoji(rest, false).map(|e| (Inline::Emoji(e), next));
    }
    if inner.starts_with("https://") || inner.starts_with("http://") {
        return Some((
            Inline::Link {
                text: Vec::new(),
                url: inner,
                suppressed: true,
            },
            next,
        ));
    }
    None
}

fn custom_emoji(rest: &str, animated: bool) -> Option<Emoji> {
    let (name, id) = rest.rsplit_once(':')?;
    if name.is_empty() {
        return None;
    }
    Some(Emoji::Custom {
        name: name.to_string(),
        id: EmojiId(digits(id)?),
        animated,
    })
}

/// A snowflake written as a canonical decimal.
///
/// `0123` is refused rather than read as 123. The id is all that is kept of a
/// mention, and a plain-text round trip that turns `<@0123>` into `<@123>` has
/// changed the message.
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

/// `@everyone` and `@here`, which are the only mentions written without
/// brackets.
fn bare_mention(chars: &[char], i: usize) -> Option<(Mention, usize)> {
    for (word, mention) in [("@everyone", Mention::Everyone), ("@here", Mention::Here)] {
        let len = word.chars().count();
        if chars[i..].len() >= len && chars[i..i + len].iter().copied().eq(word.chars()) {
            // `@hereabouts` is a word.
            if chars.get(i + len).is_some_and(|&c| is_word(c)) {
                continue;
            }
            return Some((mention, i + len));
        }
    }
    None
}

fn starts_url(chars: &[char], i: usize) -> bool {
    if i > 0 && is_word(chars[i - 1]) {
        return false;
    }
    ["https://", "http://"].iter().any(|scheme| {
        let len = scheme.chars().count();
        chars.len() >= i + len
            && chars[i..i + len]
                .iter()
                .copied()
                .flat_map(char::to_lowercase)
                .eq(scheme.chars())
    })
}

/// A bare URL, and the punctuation that followed the sentence rather than the
/// address.
fn autolink(chars: &[char], i: usize) -> (String, usize, String) {
    let mut end = i;
    while end < chars.len() {
        let c = chars[end];
        if c.is_whitespace() || c == '<' || c == '>' || c == '|' {
            break;
        }
        end += 1;
    }

    // "See https://example.com/a." ends a sentence; the full stop is not part
    // of the address. A closing bracket is, but only if the URL opened one.
    let mut stop = end;
    while stop > i {
        let c = chars[stop - 1];
        let trim = match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => true,
            ')' => !balanced(&chars[i..stop], '(', ')'),
            ']' => !balanced(&chars[i..stop], '[', ']'),
            '}' => !balanced(&chars[i..stop], '{', '}'),
            _ => false,
        };
        if !trim {
            break;
        }
        stop -= 1;
    }

    let url: String = chars[i..stop].iter().collect();
    let trailing: String = chars[stop..end].iter().collect();
    (url, end, trailing)
}

fn balanced(chars: &[char], open: char, close: char) -> bool {
    let mut depth = 0i32;
    for &c in chars {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth < 0 {
                return false;
            }
        }
    }
    depth == 0
}

/// `[text](https://url)`.
///
/// Only `http` and `https`, because a masked link is the one construct in a
/// message where what is shown and what is opened differ, and a scheme the user
/// cannot see is how that becomes a problem.
fn masked_link(chars: &[char], i: usize, depth: u8) -> Option<(Inline, usize)> {
    let mut j = i + 1;
    let mut close = None;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 2,
            ']' => {
                close = Some(j);
                break;
            }
            '\n' => break,
            _ => j += 1,
        }
    }
    let close = close?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let mut k = close + 2;
    let mut paren = None;
    while k < chars.len() {
        match chars[k] {
            ')' => {
                paren = Some(k);
                break;
            }
            c if c.is_whitespace() => break,
            _ => k += 1,
        }
    }
    let paren = paren?;

    let raw: String = chars[close + 2..paren].iter().collect();
    // `[text](<url>)` is how a masked link suppresses its own unfurl.
    let (url, suppressed) = match raw.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
        Some(inner) => (inner.to_string(), true),
        None => (raw, false),
    };
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }

    let text = parse(&chars[i + 1..close], depth + 1);
    Some((
        Inline::Link {
            text,
            url,
            suppressed,
        },
        paren + 1,
    ))
}
