//! The `driftword gen` subcommand — the CLI that replaces `wordgen.py`.

use clap::Args;

use crate::generator::{Engine, Mode, Request};

/// Flags for `driftword gen`, mirroring the former `wordgen.py` interface.
#[derive(Args, Debug)]
pub struct GenArgs {
    /// Generation engine.
    #[arg(long, default_value = "markov", value_parser = ["markov", "phono"])]
    mode: String,
    /// How many words.
    #[arg(short = 'n', long, default_value_t = 10)]
    count: usize,
    /// Minimum word length.
    #[arg(long, default_value_t = 4)]
    min: usize,
    /// Maximum word length.
    #[arg(long, default_value_t = 10)]
    max: usize,
    /// [markov] context length; higher = closer to real words.
    #[arg(long, default_value_t = 3)]
    order: usize,
    /// [phono] syllables per word.
    #[arg(long, default_value_t = 2)]
    syllables: usize,
    /// Favor these letters, e.g. --prefer aoeinum (works in both modes).
    #[arg(long, default_value = "")]
    prefer: String,
    /// How strongly each preferred letter is favored (>0).
    #[arg(long = "prefer-strength", default_value_t = 4.0)]
    strength: f64,
    /// Strict: use ONLY the --prefer letters (requires --prefer).
    #[arg(long)]
    only: bool,
    /// RNG seed for reproducible output.
    #[arg(long)]
    seed: Option<u64>,
}

/// Run `gen`, printing one word per line to stdout.
pub fn run(engine: &Engine, args: &GenArgs) -> Result<(), String> {
    let mode: Mode = args.mode.parse().map_err(|e: crate::generator::GenError| e.to_string())?;
    let req = Request {
        mode,
        count: args.count,
        min_len: args.min,
        max_len: args.max,
        order: args.order,
        syllables: args.syllables,
        prefer: args.prefer.clone(),
        strength: args.strength,
        only: args.only,
        seed: args.seed,
    };
    let words = engine.run(&req).map_err(|e| e.to_string())?;
    let mut out = String::new();
    for w in words {
        out.push_str(&w);
        out.push('\n');
    }
    print!("{out}");
    Ok(())
}
