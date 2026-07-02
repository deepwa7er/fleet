//! Fake-word generation: two engines (Markov n-gram, phonotactic) behind one
//! validated [`Request`] → [`Engine::run`] API.
//!
//! This is the single canonical implementation, replacing the former
//! `wordgen.py`. The web layer and the CLI both build a [`Request`] and call
//! [`Engine::run`].

pub mod markov;
pub mod phono;
pub mod preference;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use rand::{Rng, SeedableRng};
use rand_pcg::Pcg64Mcg;

use markov::MarkovModel;
use phono::Inventory;
use preference::Preference;

/// How many candidate draws to make before giving up on the constraints.
const ATTEMPTS: usize = 1000;

/// The constraints every candidate word must satisfy, shared by both engines:
/// length bounds, the real-word set to avoid, the letter preference, and the
/// retry budget. Bundled so the engine `generate` calls stay legible.
pub struct Constraints<'a> {
    pub real_words: &'a HashSet<String>,
    pub min_len: usize,
    pub max_len: usize,
    pub prefer: &'a Preference,
    pub attempts: usize,
}

impl Constraints<'_> {
    /// Whether `word` satisfies the length bounds, is not a real word, and is
    /// allowed by the preference.
    fn accepts(&self, word: &str) -> bool {
        word.len() >= self.min_len
            && word.len() <= self.max_len
            && !self.real_words.contains(word)
            && self.prefer.allows(word)
    }
}

/// Which generation engine to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Markov,
    Phono,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Markov => "markov",
            Mode::Phono => "phono",
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = GenError;
    fn from_str(s: &str) -> Result<Self, GenError> {
        match s {
            "markov" => Ok(Mode::Markov),
            "phono" => Ok(Mode::Phono),
            other => Err(GenError::BadMode(other.to_string())),
        }
    }
}

/// A fully-specified, not-yet-validated generation request.
#[derive(Debug, Clone)]
pub struct Request {
    pub mode: Mode,
    pub count: usize,
    pub min_len: usize,
    pub max_len: usize,
    /// Markov context length.
    pub order: usize,
    /// Phono syllables per word.
    pub syllables: usize,
    /// Letters to favor (case-insensitive); empty for no bias.
    pub prefer: String,
    /// Per-preferred-letter weight multiplier.
    pub strength: f64,
    /// Strict: restrict output to the preferred letters.
    pub only: bool,
    /// RNG seed for reproducible output; `None` seeds from entropy.
    pub seed: Option<u64>,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            mode: Mode::Markov,
            count: 10,
            min_len: 4,
            max_len: 10,
            order: 3,
            syllables: 2,
            prefer: String::new(),
            strength: 4.0,
            only: false,
            seed: None,
        }
    }
}

/// Everything that can go wrong with a request — surfaced as HTTP 422 in the web
/// layer and a non-zero exit in the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenError {
    BadMode(String),
    BadBounds,
    BadCount,
    BadOrder,
    BadSyllables,
    BadStrength,
    OnlyNeedsPrefer,
    PreferNotAlpha,
    EmptyInventory,
    Exhausted,
}

impl fmt::Display for GenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            GenError::BadMode(m) => return write!(f, "unknown mode {m:?} (use markov or phono)"),
            GenError::BadBounds => "require 1 <= min <= max",
            GenError::BadCount => "count must be >= 1",
            GenError::BadOrder => "order must be >= 1",
            GenError::BadSyllables => "syllables must be >= 1",
            GenError::BadStrength => "prefer-strength must be > 0",
            GenError::OnlyNeedsPrefer => "only requires prefer letters",
            GenError::PreferNotAlpha => "prefer must contain only letters",
            GenError::EmptyInventory => {
                "only left a syllable position with no usable parts; \
                 include at least one vowel and one consonant in prefer"
            }
            GenError::Exhausted => {
                "could not generate words within the given constraints; \
                 try widening min/max, lowering order, or relaxing prefer/only"
            }
        };
        f.write_str(msg)
    }
}

impl std::error::Error for GenError {}

/// Validate `prefer` and build the lowercase letter set.
fn parse_prefer(prefer: &str, strength: f64, only: bool) -> Result<Preference, GenError> {
    if strength <= 0.0 {
        return Err(GenError::BadStrength);
    }
    let lowered = prefer.to_ascii_lowercase();
    if !lowered.is_empty() && !lowered.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(GenError::PreferNotAlpha);
    }
    if only && lowered.is_empty() {
        return Err(GenError::OnlyNeedsPrefer);
    }
    if lowered.is_empty() {
        return Ok(Preference::inert());
    }
    let letters: BTreeSet<u8> = lowered.bytes().collect();
    Ok(Preference::new(letters, strength, only))
}

/// The generator: owns the embedded real-word set and a cache of trained Markov
/// models (one per order, built on first use).
pub struct Engine {
    real_words: HashSet<String>,
    models: Mutex<HashMap<usize, Arc<MarkovModel>>>,
}

impl Engine {
    /// Build from a real-word list (one lowercase a–z word per entry). Used both
    /// for the Markov training corpus and for filtering out real words.
    pub fn new(real_words: HashSet<String>) -> Self {
        Self {
            real_words,
            models: Mutex::new(HashMap::new()),
        }
    }

    /// Number of real words loaded (for the `/healthz` / startup log).
    pub fn corpus_len(&self) -> usize {
        self.real_words.len()
    }

    /// Fetch (or train and cache) the Markov model for `order`.
    fn model(&self, order: usize) -> Arc<MarkovModel> {
        let mut models = self.models.lock().expect("model cache poisoned");
        Arc::clone(
            models
                .entry(order)
                .or_insert_with(|| Arc::new(MarkovModel::train(order, self.real_words.iter().map(String::as_str)))),
        )
    }

    /// Bundle the per-request acceptance constraints.
    fn constraints<'a>(&'a self, req: &Request, prefer: &'a Preference) -> Constraints<'a> {
        Constraints {
            real_words: &self.real_words,
            min_len: req.min_len,
            max_len: req.max_len,
            prefer,
            attempts: ATTEMPTS,
        }
    }

    /// Validate and run a request, returning the generated words.
    pub fn run(&self, req: &Request) -> Result<Vec<String>, GenError> {
        if req.min_len < 1 || req.max_len < req.min_len {
            return Err(GenError::BadBounds);
        }
        if req.count < 1 {
            return Err(GenError::BadCount);
        }

        let prefer = parse_prefer(&req.prefer, req.strength, req.only)?;
        let mut rng = match req.seed {
            Some(seed) => Pcg64Mcg::seed_from_u64(seed),
            None => Pcg64Mcg::from_entropy(),
        };

        match req.mode {
            Mode::Markov => self.run_markov(req, &prefer, &mut rng),
            Mode::Phono => self.run_phono(req, &prefer, &mut rng),
        }
    }

    fn run_markov(&self, req: &Request, prefer: &Preference, rng: &mut impl Rng) -> Result<Vec<String>, GenError> {
        if req.order < 1 {
            return Err(GenError::BadOrder);
        }
        let model = self.model(req.order);
        let constraints = self.constraints(req, prefer);
        let mut out = Vec::with_capacity(req.count);
        for _ in 0..req.count {
            match model.generate(rng, &constraints) {
                Some(word) => out.push(word),
                None => return Err(GenError::Exhausted),
            }
        }
        Ok(out)
    }

    fn run_phono(&self, req: &Request, prefer: &Preference, rng: &mut impl Rng) -> Result<Vec<String>, GenError> {
        if req.syllables < 1 {
            return Err(GenError::BadSyllables);
        }
        let inv = Inventory::resolve(prefer).ok_or(GenError::EmptyInventory)?;
        let constraints = self.constraints(req, prefer);
        let mut out = Vec::with_capacity(req.count);
        for _ in 0..req.count {
            match phono::generate(&inv, rng, req.syllables, &constraints) {
                Some(word) => out.push(word),
                None => return Err(GenError::Exhausted),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small synthetic corpus is enough to exercise the engine deterministically.
    fn engine() -> Engine {
        let words: HashSet<String> = [
            "apple", "banana", "cherry", "delta", "echo", "foxtrot", "golf", "hotel",
            "india", "juliet", "kilo", "lima", "mike", "november", "oscar", "papa",
            "quebec", "romeo", "sierra", "tango", "uniform", "victor", "whiskey", "xray",
            "yankee", "zulu", "manana", "minimum", "anemone", "nominal",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        Engine::new(words)
    }

    fn req(seed: u64) -> Request {
        Request {
            count: 20,
            min_len: 3,
            max_len: 12,
            seed: Some(seed),
            ..Request::default()
        }
    }

    #[test]
    fn markov_is_deterministic_for_a_seed() {
        let e = engine();
        let a = e.run(&req(42)).unwrap();
        let b = e.run(&req(42)).unwrap();
        assert_eq!(a, b, "same seed must reproduce identical output");
    }

    #[test]
    fn phono_is_deterministic_for_a_seed() {
        let e = engine();
        let r = Request { mode: Mode::Phono, ..req(7) };
        assert_eq!(e.run(&r).unwrap(), e.run(&r).unwrap());
    }

    #[test]
    fn different_seeds_differ() {
        let e = engine();
        assert_ne!(e.run(&req(1)).unwrap(), e.run(&req(2)).unwrap());
    }

    #[test]
    fn never_emits_a_real_word() {
        let e = engine();
        for mode in [Mode::Markov, Mode::Phono] {
            for seed in 0..25 {
                let r = Request { mode, count: 30, seed: Some(seed), ..req(seed) };
                for w in e.run(&r).unwrap() {
                    assert!(!e.real_words.contains(&w), "emitted real word {w:?}");
                }
            }
        }
    }

    #[test]
    fn respects_length_bounds() {
        let e = engine();
        let r = Request { min_len: 5, max_len: 7, count: 40, seed: Some(3), ..Request::default() };
        for w in e.run(&r).unwrap() {
            assert!((5..=7).contains(&w.len()), "{w:?} out of bounds");
        }
    }

    #[test]
    fn only_restricts_alphabet() {
        let e = engine();
        let allowed: BTreeSet<u8> = "aoeinum".bytes().collect();
        for mode in [Mode::Markov, Mode::Phono] {
            let r = Request {
                mode,
                prefer: "aoeinum".to_string(),
                only: true,
                count: 25,
                seed: Some(11),
                ..req(11)
            };
            for w in e.run(&r).unwrap() {
                assert!(
                    w.bytes().all(|b| allowed.contains(&b)),
                    "{w:?} contains a letter outside the allowed set"
                );
            }
        }
    }

    #[test]
    fn only_without_prefer_errors() {
        let e = engine();
        let r = Request { only: true, ..req(1) };
        assert_eq!(e.run(&r), Err(GenError::OnlyNeedsPrefer));
    }

    #[test]
    fn only_with_no_consonant_errors_in_phono() {
        let e = engine();
        // Vowels only → every onset/coda except "" is dropped, but nuclei survive;
        // onsets/codas still have the empty part, so this must NOT error. Use a
        // single consonant with no vowel to empty the nucleus position instead.
        let r = Request {
            mode: Mode::Phono,
            prefer: "n".to_string(),
            only: true,
            ..req(1)
        };
        assert_eq!(e.run(&r), Err(GenError::EmptyInventory));
    }

    #[test]
    fn bad_bounds_error() {
        let e = engine();
        let r = Request { min_len: 8, max_len: 4, ..req(1) };
        assert_eq!(e.run(&r), Err(GenError::BadBounds));
    }
}
