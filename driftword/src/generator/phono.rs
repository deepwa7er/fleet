//! Phonotactic syllable model.
//!
//! Ported from `wordgen.py`'s phono engine. Words are assembled from
//! hand-curated English onset/nucleus/coda inventories. Weights bias toward the
//! common parts so output doesn't feel uniformly random.

use rand::Rng;

use super::Constraints;
use super::preference::Preference;

/// Syllable onsets (leading consonants / clusters) with relative weights.
const ONSETS: &[(&str, u32)] = &[
    ("", 6), ("b", 8), ("c", 8), ("d", 8), ("f", 7), ("g", 6), ("h", 6),
    ("j", 3), ("k", 5), ("l", 6), ("m", 8), ("n", 7), ("p", 8), ("r", 7),
    ("s", 9), ("t", 9), ("v", 4), ("w", 5), ("y", 2), ("z", 2),
    ("bl", 4), ("br", 5), ("cl", 4), ("cr", 5), ("dr", 4), ("fl", 4),
    ("fr", 4), ("gl", 3), ("gr", 4), ("pl", 4), ("pr", 4), ("sl", 4),
    ("sm", 3), ("sn", 3), ("sp", 4), ("st", 5), ("sk", 3), ("sw", 3),
    ("tr", 5), ("tw", 2), ("th", 5), ("sh", 5), ("ch", 4), ("wh", 2),
    ("str", 3), ("spr", 2), ("scr", 2), ("thr", 2),
];

/// Syllable nuclei (vowels / vowel digraphs) with relative weights.
const NUCLEI: &[(&str, u32)] = &[
    ("a", 10), ("e", 11), ("i", 10), ("o", 9), ("u", 6),
    ("ai", 3), ("ay", 2), ("ea", 4), ("ee", 3), ("ie", 2), ("oa", 2),
    ("oo", 3), ("ou", 3), ("ow", 2), ("oy", 1), ("au", 2),
];

/// Syllable codas (trailing consonants / clusters) with relative weights.
const CODAS: &[(&str, u32)] = &[
    ("", 8), ("b", 4), ("ck", 5), ("d", 7), ("f", 4), ("g", 4), ("l", 7),
    ("m", 6), ("n", 9), ("p", 5), ("r", 8), ("s", 7), ("t", 9), ("x", 1),
    ("z", 1), ("ld", 3), ("lt", 3), ("mp", 3), ("nd", 4), ("ng", 4),
    ("nk", 3), ("nt", 4), ("rd", 3), ("rk", 3), ("rn", 3), ("rt", 3),
    ("sh", 3), ("sk", 2), ("sp", 2), ("ss", 3), ("st", 4), ("th", 3),
    ("ct", 2), ("ft", 2),
];

/// Preference-resolved syllable inventories: boosts applied, and under strict
/// (`only`) mode, parts containing letters outside the chosen alphabet dropped.
pub struct Inventory {
    onsets: Vec<(&'static str, f64)>,
    nuclei: Vec<(&'static str, f64)>,
    codas: Vec<(&'static str, f64)>,
}

/// Apply a preference to one inventory: drop disallowed parts (strict mode),
/// scale the rest by the preference boost.
fn resolve(table: &[(&'static str, u32)], prefer: &Preference) -> Vec<(&'static str, f64)> {
    table
        .iter()
        .filter(|(part, _)| prefer.allows(part))
        .map(|(part, weight)| (*part, *weight as f64 * prefer.boost_str(part)))
        .collect()
}

impl Inventory {
    /// Resolve all three inventories against `prefer`. Returns `None` if strict
    /// mode emptied any syllable position (no usable parts left).
    pub fn resolve(prefer: &Preference) -> Option<Self> {
        let inv = Self {
            onsets: resolve(ONSETS, prefer),
            nuclei: resolve(NUCLEI, prefer),
            codas: resolve(CODAS, prefer),
        };
        if inv.onsets.is_empty() || inv.nuclei.is_empty() || inv.codas.is_empty() {
            None
        } else {
            Some(inv)
        }
    }
}

/// Weighted pick from a resolved inventory slice.
fn pick<'a>(table: &[(&'a str, f64)], rng: &mut impl Rng) -> &'a str {
    let total: f64 = table.iter().map(|(_, w)| w).sum();
    let mut r = rng.gen_range(0.0..total);
    for (part, weight) in table {
        r -= weight;
        if r < 0.0 {
            return part;
        }
    }
    table.last().map(|(p, _)| *p).unwrap_or("")
}

/// Produce one novel word of `syllables` syllables satisfying `c`. Returns
/// `None` if no candidate satisfies the constraints within `c.attempts` tries.
pub fn generate(inv: &Inventory, rng: &mut impl Rng, syllables: usize, c: &Constraints) -> Option<String> {
    for _ in 0..c.attempts {
        let mut word = String::new();
        for _ in 0..syllables {
            word.push_str(pick(&inv.onsets, rng));
            word.push_str(pick(&inv.nuclei, rng));
            word.push_str(pick(&inv.codas, rng));
        }
        if c.accepts(&word) {
            return Some(word);
        }
    }
    None
}
