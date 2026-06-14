//! A bias toward a chosen set of letters, shared by both engines.
//!
//! Ported from `wordgen.py`'s `Preference`. The preferred `letters` are favored
//! during sampling: each occurrence multiplies a candidate's weight by
//! `strength` (>1 favors). When `only` is set, any candidate containing a letter
//! outside `letters` is rejected outright, restricting output to that alphabet.
//!
//! A preference with no letters is inert: [`Preference::boost_byte`] /
//! [`Preference::boost_str`] always return `1.0` and [`Preference::allows`] is
//! always true, so the engines need no special-casing.

use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct Preference {
    /// Lowercase ASCII letters to favor.
    letters: BTreeSet<u8>,
    /// Per-preferred-letter weight multiplier.
    strength: f64,
    /// Strict mode: reject anything outside `letters`.
    only: bool,
}

impl Preference {
    /// An inert preference (no bias, no restriction).
    pub fn inert() -> Self {
        Self {
            letters: BTreeSet::new(),
            strength: 1.0,
            only: false,
        }
    }

    /// Construct from an already-validated set of lowercase ASCII letters.
    pub fn new(letters: BTreeSet<u8>, strength: f64, only: bool) -> Self {
        Self {
            letters,
            strength,
            only,
        }
    }

    /// Weight multiplier for a single candidate byte (a Markov next-char).
    /// The Markov end-of-word sentinel is never a letter, so its boost is a
    /// neutral `1.0` — preference never distorts where words terminate.
    pub fn boost_byte(&self, b: u8) -> f64 {
        if self.letters.contains(&b) {
            self.strength
        } else {
            1.0
        }
    }

    /// Weight multiplier for a syllable part (the product over its bytes).
    pub fn boost_str(&self, s: &str) -> f64 {
        if self.letters.is_empty() {
            return 1.0;
        }
        s.bytes().map(|b| self.boost_byte(b)).product()
    }

    /// Whether `word` is acceptable under strict (`only`) mode.
    pub fn allows(&self, word: &str) -> bool {
        if !self.only || self.letters.is_empty() {
            return true;
        }
        word.bytes().all(|b| self.letters.contains(&b))
    }
}
