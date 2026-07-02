//! The ticket state machine. Every mutation that changes `tickets.state` goes
//! through one of these transitions, so the legal graph lives in exactly one
//! place. `core::store` validates the ticket's current state against
//! `Transition::from` before applying any change.

use super::model::State;

/// A single legal edge in the lifecycle graph.
pub struct Transition {
    /// Verb used in error messages, e.g. "claim".
    pub action: &'static str,
    /// State the ticket must currently be in for this transition to apply.
    pub from: State,
    /// State the ticket moves to.
    pub to: State,
}

macro_rules! transition {
    ($name:ident, $action:literal, $from:ident => $to:ident) => {
        pub const $name: Transition = Transition {
            action: $action,
            from: State::$from,
            to: State::$to,
        };
    };
}

// Worker-driven edges.
transition!(CLAIM, "claim", Open => InProgress);
transition!(NEEDS_INPUT, "ask for input", InProgress => NeedsInput);
transition!(BLOCK, "block", InProgress => Blocked);
transition!(RESOLVE, "resolve", InProgress => InReview);
// An investigate ticket's deliverable is its report, not a PR; same review gate.
transition!(REPORT, "report", InProgress => InReview);

// System-driven edge: a crashed run leaves a ticket stuck in-progress.
transition!(RECLAIM, "reclaim", InProgress => Open);

// Human-driven edges.
transition!(ANSWER, "answer", NeedsInput => Open);
transition!(UNBLOCK, "unblock", Blocked => Open);
transition!(MARK_DONE, "mark done", InReview => Done);
