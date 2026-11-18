//! `[replacements]`: phrases earwig hears and what it writes instead, in a
//! dictation's final text before the `filter` hook gets it.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::BTreeMap;

use unicode_segmentation::UnicodeSegmentation as _;

/// A heard phrase as the table keys it: lowercase, one space between its
/// words. Empty for a phrase without words, which the table refuses.
pub fn normalize(heard: &str) -> String {
    heard
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// English words whose `'s` means "is", not a possessive: `it's` stays one word.
const CONTRACTED: [&str; 13] = [
    "he", "here", "how", "it", "let", "she", "that", "there", "what", "when", "where", "who", "why",
];

/// Where the text says a phrase of the table.
struct Heard<'a> {
    start: usize,
    end: usize,
    /// Characters of the phrase, spaces aside.
    size: usize,
    written: &'a str,
}

/// `text` with every heard phrase of `table` replaced by what is written for it.
///
/// A phrase is heard as whole words, in any capitals and with any whitespace
/// between them; punctuation around it stays. An apostrophe between letters
/// keeps them one word (`can't`, `it's`) but for a name's `'s`. Where two
/// overlap the longer phrase wins, of equals the left one. What a replacement
/// wrote is not read again.
pub fn apply<'a>(table: &BTreeMap<String, String>, text: &'a str) -> Cow<'a, str> {
    let phrases: Vec<(&str, usize, &str)> = table
        .iter()
        .map(|(heard, written)| {
            let size = heard.chars().filter(|c| !c.is_whitespace()).count();
            (heard.as_str(), size, written.as_str())
        })
        .filter(|&(_, size, _)| size > 0)
        .collect();
    if phrases.is_empty() {
        return Cow::Borrowed(text);
    }
    let edges = word_edges(text);
    let mut heard = Vec::new();
    for &start in &edges {
        for &(phrase, size, written) in &phrases {
            if let Some(len) = phrase_at(&text[start..], phrase)
                && edges.binary_search(&(start + len)).is_ok()
            {
                heard.push(Heard {
                    start,
                    end: start + len,
                    size,
                    written,
                });
            }
        }
    }
    heard.sort_by_key(|found| (Reverse(found.size), found.start));
    let mut kept: Vec<Heard> = Vec::new();
    for found in heard {
        if kept
            .iter()
            .all(|other| found.end <= other.start || other.end <= found.start)
        {
            kept.push(found);
        }
    }
    if kept.is_empty() {
        return Cow::Borrowed(text);
    }
    kept.sort_by_key(|found| found.start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for found in kept {
        out.push_str(&text[at..found.start]);
        out.push_str(found.written);
        at = found.end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

/// The byte offsets of `text` where a word may start or end, in order: all
/// but those inside a run of letters and digits that Unicode keeps in one
/// word (UAX #29), those between a character and its marks, and those beside
/// an apostrophe inside a word, unless it starts a name's `'s`. Chinese,
/// Thai, kanji and hiragana are written without spaces: there every
/// character is a word.
fn word_edges(text: &str) -> Vec<usize> {
    let mut edges = Vec::new();
    for (start, word) in text.split_word_bound_indices() {
        let mut after_letter = false;
        let mut after_apostrophe = false;
        for (at, cluster) in word.grapheme_indices(true) {
            let letter = cluster.starts_with(char::is_alphanumeric);
            let apostrophe = at > 0 && matches!(cluster, "'" | "\u{2019}");
            let joined = (after_letter && letter)
                || after_apostrophe
                || (apostrophe && !possessive(&word[..at], &word[at + cluster.len()..]));
            if !joined {
                edges.push(start + at);
            }
            after_letter = letter;
            after_apostrophe = apostrophe;
        }
    }
    edges.push(text.len());
    edges
}

/// Whether the apostrophe between the two parts of a word makes a possessive
/// (`GitHub's`) rather than a contraction (`it's`, `we're`, `can't`).
fn possessive(stem: &str, rest: &str) -> bool {
    rest.eq_ignore_ascii_case("s")
        && !CONTRACTED
            .iter()
            .any(|word| word.eq_ignore_ascii_case(stem))
}

/// How many bytes at the start of `text` say `phrase`: its words in any
/// capitals, with whitespace between them.
fn phrase_at(text: &str, phrase: &str) -> Option<usize> {
    let mut at = 0;
    for (n, word) in phrase.split_whitespace().enumerate() {
        if n > 0 {
            let rest = text[at..].trim_start();
            if rest.len() == text.len() - at {
                return None;
            }
            at = text.len() - rest.len();
        }
        at += word_at(&text[at..], word)?;
    }
    Some(at)
}

/// How many bytes at the start of `text` spell `word` in any capitals.
fn word_at(text: &str, word: &str) -> Option<usize> {
    let mut wanted = word.chars().flat_map(char::to_lowercase).peekable();
    let mut len = 0;
    for c in text.chars() {
        if wanted.peek().is_none() {
            break;
        }
        if !c.to_lowercase().all(|lower| wanted.next() == Some(lower)) {
            return None;
        }
        len += c.len_utf8();
    }
    wanted.peek().is_none().then_some(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first user's table.
    const TABLE: [(&str, &str); 6] = [
        ("nix os", "NixOS"),
        ("nix packages", "nixpkgs"),
        ("get hub", "GitHub"),
        ("jujitsu", "jujutsu"),
        ("type script", "TypeScript"),
        ("java script", "JavaScript"),
    ];

    fn written(table: &[(&str, &str)], text: &str) -> String {
        let table = table
            .iter()
            .map(|&(heard, written)| (heard.to_owned(), written.to_owned()))
            .collect();
        apply(&table, text).into_owned()
    }

    #[test]
    fn a_phrase_is_heard_in_the_capitals_and_punctuation_a_model_writes() {
        let said = |text| written(&TABLE, text);
        assert_eq!(said("Nix OS builds Nix packages."), "NixOS builds nixpkgs.");
        assert_eq!(
            said("Push it to Get hub, please."),
            "Push it to GitHub, please."
        );
        assert_eq!(said("Jujitsu? JUJITSU!"), "jujutsu? jujutsu!");
        assert_eq!(
            said("Type Script, then Java script: done"),
            "TypeScript, then JavaScript: done"
        );
        assert_eq!(
            said("(get hub) \u{201c}nix os\u{201d} Get hub's"),
            "(GitHub) \u{201c}NixOS\u{201d} GitHub's"
        );
    }

    #[test]
    fn any_whitespace_may_separate_the_words_of_a_phrase() {
        let said = |text| written(&TABLE, text);
        assert_eq!(said("nix   os"), "NixOS");
        assert_eq!(said("nix\nos"), "NixOS");
        assert_eq!(said("nix\u{a0}os"), "NixOS");
        assert_eq!(said("nixos, nix-os"), "nixos, nix-os");
    }

    #[test]
    fn a_phrase_is_only_heard_as_whole_words() {
        let said = |text| written(&TABLE, text);
        for text in [
            "A phoenix ostrich.",
            "Unix os",
            "nix osmosis",
            "nix os2",
            "get hubs",
            "jujitsus",
        ] {
            assert_eq!(said(text), text);
        }
    }

    #[test]
    fn a_contraction_is_never_taken_apart() {
        let table = [
            ("can", "may"),
            ("we", "they"),
            ("it", "this"),
            ("re", "Re:"),
        ];
        for text in [
            "I can't.",
            "we're here",
            "We\u{2019}re here",
            "it's late",
            "It\u{2019}s late",
        ] {
            assert_eq!(written(&table, text), text);
        }
        assert_eq!(written(&table, "we can do it"), "they may do this");
    }

    #[test]
    fn a_name_keeps_its_s() {
        let said = |text| written(&TABLE, text);
        assert_eq!(
            said("Get hub's API, GET HUB\u{2019}S app, that's get hub's"),
            "GitHub's API, GitHub\u{2019}S app, that's GitHub's"
        );
        // Any other letters after the apostrophe go on with the word.
        assert_eq!(said("get hub'd, get hub'sy"), "get hub'd, get hub'sy");
    }

    #[test]
    fn the_longest_of_overlapping_phrases_wins() {
        let table = [("nix", "Nix"), ("nix os", "NixOS"), ("os", "OS")];
        assert_eq!(
            written(&table, "nix os and nix on any os"),
            "NixOS and Nix on any OS"
        );
        // The longer one starts later, and still wins.
        let table = [("new york", "NY"), ("york city hall", "the hall")];
        assert_eq!(written(&table, "new york city hall"), "new the hall");
        // Of two as long, the left one.
        let table = [("ab cd", "1"), ("cd ef", "2")];
        assert_eq!(written(&table, "ab cd ef"), "1 ef");
    }

    #[test]
    fn what_a_replacement_wrote_is_not_replaced_again() {
        let table = [("jujitsu", "jujutsu"), ("jujutsu", "jj")];
        assert_eq!(written(&table, "jujitsu and jujutsu"), "jujutsu and jj");
        let table = [("get", "get hub"), ("get hub", "GitHub")];
        assert_eq!(written(&table, "get it"), "get hub it");
    }

    #[test]
    fn the_written_text_goes_in_as_entered() {
        let table = [("new line", "\n"), ("um", ""), ("e g", " e.g. ")];
        assert_eq!(written(&table, "one new line two"), "one \n two");
        assert_eq!(written(&table, "Um, yes"), ", yes");
        assert_eq!(written(&table, "fruit, e g apples"), "fruit,  e.g.  apples");
    }

    #[test]
    fn letters_of_any_script_make_a_word() {
        let table = [("ana", "Anna"), ("er", "he"), ("über", "Uber")];
        assert_eq!(
            written(&table, "Mañana wird es größer, Über"),
            "Mañana wird es größer, Uber"
        );
        // An accent written as its own character stays with its letter.
        assert_eq!(
            written(&[("cafe", "shop")], "cafe\u{301} cafe"),
            "cafe\u{301} shop"
        );
        // सत inside सत्य.
        assert_eq!(
            written(&[("\u{938}\u{924}", "x")], "\u{938}\u{924}\u{94d}\u{92f}"),
            "\u{938}\u{924}\u{94d}\u{92f}"
        );
    }

    #[test]
    fn writing_without_spaces_is_replaced_anywhere() {
        assert_eq!(
            written(&[("东京", "Tokyo")], "我在东京工作"),
            "我在Tokyo工作"
        );
        assert_eq!(written(&[("ไทย", "Thai")], "ภาษาไทย"), "ภาษาThai");
        assert_eq!(written(&[("がな", "kana")], "ひらがな"), "ひらkana");
        // But a run of katakana is one word.
        let table = [("タワ", "x"), ("東京", "Tokyo")];
        assert_eq!(written(&table, "東京タワーへ"), "Tokyoタワーへ");
        assert_eq!(
            written(&[("タワー", "tower")], "東京タワーへ"),
            "東京towerへ"
        );
    }

    #[test]
    fn a_phrase_without_words_is_never_heard() {
        assert_eq!(written(&[("", "x"), (" \t", "y")], "a b"), "a b");
    }

    #[test]
    fn a_phrase_is_keyed_in_lowercase_with_single_spaces() {
        assert_eq!(normalize("  Nix \t OS\n"), "nix os");
        assert_eq!(normalize("Über"), "über");
        assert_eq!(normalize(" \n"), "");
        assert_eq!(written(&[("  Nix \t OS\n", "NixOS")], "nix os"), "NixOS");
    }
}
