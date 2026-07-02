//! Character-level n-gram model over a word list.
//!
//! Ported from `wordgen.py`'s `MarkovModel`. The model maps each context (the
//! previous `order` bytes) to a frequency table of the bytes that followed it in
//! the training data. Sampling from those tables, starting from the padded word
//! boundary, reproduces the statistical "texture" of the source language.

use std::collections::BTreeMap;

use rand::Rng;

use super::Constraints;
use super::preference::Preference;

/// Sentinels that cannot appear inside a real (a–z) word. They mark the start
/// and end of a token during training.
const START: u8 = b'^';
const END: u8 = b'$';

pub struct MarkovModel {
    order: usize,
    /// context (`order` bytes) → { next byte → count }. `BTreeMap` keys keep
    /// both the contexts and the per-context candidates in a fixed sorted order,
    /// so sampling is reproducible for a given seed regardless of the training
    /// set's iteration order (the determinism fix carried over from the Python
    /// version — must not regress).
    table: BTreeMap<Vec<u8>, BTreeMap<u8, u32>>,
}

impl MarkovModel {
    /// Train a model of the given `order` over `words` (each lowercase a–z).
    pub fn train<'a>(order: usize, words: impl Iterator<Item = &'a str>) -> Self {
        debug_assert!(order >= 1);
        let mut table: BTreeMap<Vec<u8>, BTreeMap<u8, u32>> = BTreeMap::new();
        for word in words {
            let mut padded = vec![START; order];
            padded.extend_from_slice(word.as_bytes());
            padded.push(END);
            for window in padded.windows(order + 1) {
                let ctx = window[..order].to_vec();
                let next = window[order];
                *table.entry(ctx).or_default().entry(next).or_insert(0) += 1;
            }
        }
        Self { order, table }
    }

    /// Sample the next byte after `ctx`, weighting each candidate by its training
    /// frequency times the preference boost. Returns `None` only if the context
    /// is unseen (cannot happen for contexts reached from `START` padding).
    fn sample_next(&self, ctx: &[u8], rng: &mut impl Rng, prefer: &Preference) -> Option<u8> {
        let counter = self.table.get(ctx)?;
        let total: f64 = counter
            .iter()
            .map(|(&ch, &count)| count as f64 * prefer.boost_byte(ch))
            .sum();
        let mut pick = rng.gen_range(0.0..total);
        for (&ch, &count) in counter {
            pick -= count as f64 * prefer.boost_byte(ch);
            if pick < 0.0 {
                return Some(ch);
            }
        }
        // Floating-point round-off only: fall back to the last (sorted) key.
        counter.keys().next_back().copied()
    }

    /// One generation attempt. Returns `None` if the run exceeds `max_len`
    /// (bail rather than emit a freakishly long word) — the caller retries.
    fn generate_once(&self, rng: &mut impl Rng, max_len: usize, prefer: &Preference) -> Option<String> {
        let mut ctx = vec![START; self.order];
        let mut out: Vec<u8> = Vec::new();
        loop {
            let next = self.sample_next(&ctx, rng, prefer)?;
            if next == END {
                break;
            }
            out.push(next);
            if out.len() >= max_len {
                return None;
            }
            ctx.push(next);
            ctx.remove(0);
        }
        // `out` is ASCII a–z by construction.
        Some(String::from_utf8(out).expect("markov output is ASCII"))
    }

    /// Produce one novel word satisfying `c`. Returns `None` if no candidate
    /// satisfies the constraints within `c.attempts` tries.
    pub fn generate(&self, rng: &mut impl Rng, c: &Constraints) -> Option<String> {
        for _ in 0..c.attempts {
            let Some(word) = self.generate_once(rng, c.max_len, c.prefer) else {
                continue;
            };
            if c.accepts(&word) {
                return Some(word);
            }
        }
        None
    }
}
