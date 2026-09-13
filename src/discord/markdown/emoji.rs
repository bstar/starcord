//! Finding unicode emoji in a run of text.
//!
//! An emoji is rarely one character. A flag is two regional indicators, a
//! keycap is a digit plus a variation selector plus U+20E3, a person with a
//! profession is three or four code points joined by zero-width joiners, and
//! every one of them has to come out of the scan as a single unit or the chat
//! panel measures a width per code point and draws a ruined line.
//!
//! **This is a hand-written approximation of grapheme clustering, not UAX #29.**
//! STAR/KIT has the real thing in `wrap::clusters`, and this module goes away
//! when that dependency is switched on — the crate is a path dependency that
//! the flake cannot build yet, which is recorded in `AGENTS.md`. Until then the
//! rule here is: build the longest sequence that *could* be one emoji, then ask
//! the `emojis` crate whether it is, shortening from the end until something
//! matches or nothing does. A lookup is the authority; the scan only proposes.
//!
//! Proposing is cheap because the candidate test is deliberately loose: any
//! non-ASCII character may start an emoji, and `emojis::get` answers `None` for
//! every letter in every script. The cost of being wrong is one hash lookup.

/// Zero-width joiner.
const ZWJ: char = '\u{200D}';
/// Variation selectors 15 and 16: "draw the previous character as text" and
/// "…as an emoji".
const VS15: char = '\u{FE0E}';
const VS16: char = '\u{FE0F}';
/// The combining enclosing keycap, which turns `1` into a key.
const KEYCAP: char = '\u{20E3}';

fn is_skin_tone(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Tag characters, which spell out subdivision flags such as the Scottish one.
fn is_tag(c: char) -> bool {
    ('\u{E0020}'..='\u{E007F}').contains(&c)
}

fn is_modifier(c: char) -> bool {
    c == VS15 || c == VS16 || c == KEYCAP || is_skin_tone(c) || is_tag(c)
}

/// Whether a character is worth asking about at all.
///
/// Every non-ASCII character is, because the lookup is the thing that decides.
/// ASCII only qualifies for the keycap sequences — `0`–`9`, `#` and `*` — and
/// only when a keycap or a variation selector actually follows, or every digit
/// in every message would be a candidate.
pub fn could_start(chars: &[char], i: usize) -> bool {
    let Some(&c) = chars.get(i) else {
        return false;
    };
    if !c.is_ascii() {
        return true;
    }
    if !matches!(c, '0'..='9' | '#' | '*') {
        return false;
    }
    matches!(chars.get(i + 1), Some(&VS16) | Some(&KEYCAP))
}

/// The longest emoji starting at `i`, and how many characters it took.
///
/// `None` when what is there is not an emoji, which is the ordinary answer for
/// ordinary text.
pub fn scan(chars: &[char], i: usize) -> Option<(String, usize)> {
    if !could_start(chars, i) {
        return None;
    }

    // Build the candidate and remember every point at which it could plausibly
    // end, longest last.
    let mut end = i + 1;
    let mut boundaries = vec![end];

    // A flag is exactly two regional indicators and never more, so it is taken
    // before the general loop rather than inside it.
    if is_regional_indicator(chars[i]) {
        if chars.get(i + 1).is_some_and(|&c| is_regional_indicator(c)) {
            end = i + 2;
            boundaries.push(end);
        }
    } else {
        loop {
            let mut moved = false;
            while chars.get(end).is_some_and(|&c| is_modifier(c)) {
                end += 1;
                moved = true;
            }
            if moved {
                boundaries.push(end);
            }
            // A joiner only continues the sequence if something follows it.
            if chars.get(end) == Some(&ZWJ) && chars.get(end + 1).is_some() {
                end += 2;
                boundaries.push(end);
                continue;
            }
            break;
        }
    }

    // Longest first: `👍🏽` must not be answered with `👍`.
    for &stop in boundaries.iter().rev() {
        let candidate: String = chars[i..stop].iter().collect();
        if emojis::get(&candidate).is_some() {
            return Some((candidate, stop - i));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(text: &str) -> Option<(String, usize)> {
        let chars: Vec<char> = text.chars().collect();
        scan(&chars, 0)
    }

    #[test]
    fn a_plain_emoji_is_one_unit() {
        assert_eq!(find("👍"), Some(("👍".to_string(), 1)));
        assert_eq!(find("🎉 and more"), Some(("🎉".to_string(), 1)));
    }

    #[test]
    fn a_skin_tone_is_part_of_the_emoji_it_modifies() {
        let (text, len) = find("👍🏽").expect("a modified thumb is an emoji");
        assert_eq!(text, "👍🏽");
        assert_eq!(len, 2, "the tone must not be left behind as its own unit");
    }

    #[test]
    fn a_zero_width_joined_sequence_is_one_unit() {
        // Family: man, woman, girl, boy.
        let family = "👨‍👩‍👧‍👦";
        let (text, len) = find(family).expect("a joined family is an emoji");
        assert_eq!(text, family);
        assert_eq!(len, family.chars().count());
    }

    #[test]
    fn a_flag_is_two_regional_indicators_and_stops_there() {
        let (text, len) = find("🇬🇧🇫🇷").expect("a flag is an emoji");
        assert_eq!(text, "🇬🇧");
        assert_eq!(len, 2, "two flags in a row became one");
    }

    #[test]
    fn a_keycap_is_an_emoji_and_a_bare_digit_is_not() {
        let (text, len) = find("1\u{FE0F}\u{20E3}").expect("a keycap is an emoji");
        assert_eq!(len, 3);
        assert_eq!(text.chars().count(), 3);
        assert_eq!(find("1"), None, "every digit would be an emoji");
        assert_eq!(find("2026"), None);
    }

    #[test]
    fn ordinary_text_in_any_script_is_not_an_emoji() {
        for text in ["a", "word", "こんにちは", "Привет", "ß", "—", "…"] {
            assert_eq!(find(text), None, "{text} was mistaken for an emoji");
        }
    }

    #[test]
    fn a_variation_selector_belongs_to_its_character() {
        let (text, len) = find("\u{2764}\u{FE0F}").expect("a red heart is an emoji");
        assert_eq!(len, 2);
        assert_eq!(text, "\u{2764}\u{FE0F}");
    }

    #[test]
    fn a_trailing_joiner_with_nothing_after_it_is_not_consumed() {
        let chars: Vec<char> = "👍\u{200D}".chars().collect();
        let (text, len) = scan(&chars, 0).expect("the thumb is still an emoji");
        assert_eq!(text, "👍");
        assert_eq!(len, 1, "a dangling joiner was swallowed");
    }

    proptest::proptest! {
        /// Somebody else's bytes, again: the scan must terminate and never
        /// index past the end.
        #[test]
        fn scanning_arbitrary_text_never_panics(text in ".{0,80}") {
            let chars: Vec<char> = text.chars().collect();
            for i in 0..chars.len() {
                if let Some((found, len)) = scan(&chars, i) {
                    proptest::prop_assert!(len >= 1);
                    proptest::prop_assert!(i + len <= chars.len());
                    proptest::prop_assert_eq!(found.chars().count(), len);
                }
            }
        }
    }
}
