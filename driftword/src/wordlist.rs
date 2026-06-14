//! The real-word corpus, embedded in the binary so the service is fully
//! self-contained (no dependency on `/usr/share/dict/words` existing on the
//! host). `assets/words.txt` is a cleaned snapshot — lowercase ASCII a–z,
//! length 2–24, deduped and sorted — committed to the repo for reproducible
//! builds.

use std::collections::HashSet;

/// The embedded word list, one word per line.
const WORDS_TXT: &str = include_str!("../assets/words.txt");

/// Parse the embedded list into a set.
pub fn load() -> HashSet<String> {
    WORDS_TXT
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}
