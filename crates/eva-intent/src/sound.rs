//! How a name sounds to a Spanish speaker, for matching what the speech model
//! wrote against the apps installed: "Breve", "Brive" and "Brave" are one
//! word said three ways, and "Zafarí" is Safari, but letter-by-letter
//! comparison (Jaro-Winkler) sees a different word each time.
//!
//! A name becomes a [`Sound`]: its consonants as a Spanish ear hears them
//! (b/v, c/k/qu, s/z, ll/y, silent h…) and, apart, its vowels. Two names
//! sound alike when the consonants are the same and the vowels are not too
//! far apart — vowels are what an accent and a speech model blur most.

/// The consonant skeleton and the whole sound of one spoken name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sound {
    /// Consonants only, in order, as heard.
    pub consonants: String,
    /// Every sound, vowels included.
    pub full: String,
}

/// Fewest consonants a name needs for a sound match: with less ("Mail",
/// "Mac") half the dictionary sounds alike.
const MIN_CONSONANTS: usize = 3;

/// The sound of `text` (any case, accents ignored, spaces and punctuation skipped).
pub fn sound_of(text: &str) -> Sound {
    let letters: Vec<char> =
        eva_text::fold_diacritics(text).to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let mut full = String::new();
    let mut i = 0;
    while i < letters.len() {
        let (c, next) = (letters[i], letters.get(i + 1).copied());
        let soft = matches!(next, Some('e' | 'i' | 'y'));
        let (sound, used): (&str, usize) = match (c, next) {
            ('c', Some('h')) => ("C", 2),
            ('c', Some('k')) => ("k", 2),
            ('q', Some('u')) => ("k", 2),
            ('l', Some('l')) => ("y", 2),
            ('p', Some('h')) => ("f", 2),
            ('s', Some('h')) => ("C", 2),
            ('t', Some('h')) => ("t", 2),
            ('g', Some('u')) if letters.get(i + 2).is_some_and(|l| matches!(l, 'e' | 'i')) => ("g", 2),
            ('c', _) if soft => ("s", 1),
            ('c' | 'k', _) => ("k", 1),
            ('z' | 's', _) => ("s", 1),
            ('v' | 'w', _) => ("b", 1),
            ('g', _) if soft => ("j", 1),
            ('x', _) => ("ks", 1),
            ('h', _) => ("", 1),
            // A consonant only before a vowel ("yo"); otherwise it is an "i" ("Spotify").
            ('y', Some('a' | 'e' | 'i' | 'o' | 'u')) => ("y", 1),
            ('y', _) => ("i", 1),
            _ => ("", 0),
        };
        if used == 0 {
            full.push(c);
            i += 1;
        } else {
            full.push_str(sound);
            i += used;
        }
    }
    // Doubled sounds are one sound ("Spotify" said "Espotify" aside).
    let mut collapsed = String::new();
    for c in full.chars() {
        if !collapsed.ends_with(c) {
            collapsed.push(c);
        }
    }
    let consonants = collapsed.chars().filter(|c| !"aeiou".contains(*c)).collect();
    Sound { consonants, full: collapsed }
}

/// How alike two names sound, `0.0..=1.0`, or `None` if they do not sound
/// alike at all (different consonants, or too few to tell).
pub fn sounds_like(said: &Sound, name: &Sound) -> Option<f64> {
    if said.consonants.chars().count() < MIN_CONSONANTS || said.consonants != name.consonants {
        return None;
    }
    let vowels = strsim::normalized_levenshtein(&said.full, &name.full);
    (vowels >= 0.6).then_some(vowels)
}

/// A looser, one-consonant-off comparison, for *suggesting* ("¿quisiste decir
/// Brave?") when nothing sounds alike enough to open without asking.
pub fn sounds_close(said: &Sound, name: &Sound) -> Option<f64> {
    let (a, b) = (&said.consonants, &name.consonants);
    if a.chars().count() < MIN_CONSONANTS
        || a.chars().next() != b.chars().next()
        || strsim::levenshtein(a, b) > 1
        || a.chars().count().abs_diff(b.chars().count()) > 1
    {
        return None;
    }
    Some(strsim::normalized_levenshtein(&said.full, &name.full))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn like(said: &str, name: &str) -> bool {
        sounds_like(&sound_of(said), &sound_of(name)).is_some()
    }

    #[test]
    fn brave_said_three_ways_is_one_sound() {
        for said in ["Brave", "Breve", "Brive", "brebe", "Vrave"] {
            assert!(like(said, "Brave"), "{said}");
        }
    }

    #[test]
    fn safari_said_with_a_z_and_spotify_said_with_an_e() {
        assert!(like("Zafarí", "Safari"));
        assert!(like("Espotifai", "Spotify"));
    }

    #[test]
    fn different_words_and_short_names_do_not_match() {
        assert!(!like("Bruno", "Brave"));
        assert!(!like("Slack", "Silk"));
        assert!(!like("Mail", "Mall"), "two consonants tell nothing");
        assert!(!like("Notas", "Notion"));
    }

    #[test]
    fn a_one_consonant_slip_is_close_enough_to_suggest() {
        // "braille" is heard "b r y l"; Brave is "b r b": too far to open,
        // near enough to ask.
        assert!(!like("braille", "Brave"));
        assert!(sounds_close(&sound_of("bravo"), &sound_of("Brave")).is_some());
        assert!(sounds_close(&sound_of("zoom"), &sound_of("Brave")).is_none());
    }
}
