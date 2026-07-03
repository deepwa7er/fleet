use serde::Serialize;

/// Every course's step quantities must add up to exactly this — the game is
/// "vote on ten": a budget of ten units to spend across activities. Quantities
/// move in half-unit increments (halves are exact in binary floating point, so
/// the budget check is exact integer arithmetic on half-units, never an
/// epsilon comparison). Enforced by the store on creation.
pub const COURSE_TOTAL: f64 = 10.0;

/// A user-editable section of the activity catalog (e.g. "Games & puzzles").
/// Purely for grouping in the picker — steps mix categories freely.
#[derive(Debug, Clone, Serialize)]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub sort_order: i64,
    pub created_at: String,
}

/// One entry in the activity catalog — a thing a step can demand, with the
/// unit its quantity is counted in (e.g. "Distance run" in "miles").
#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    pub id: i64,
    pub name: String,
    pub category_id: i64,
    pub unit: String,
    pub sort_order: i64,
    pub created_at: String,
}

/// One step of a proposed course: an activity plus how much of it. The
/// activity's display fields (including its category's name) are denormalized
/// in so the list view is one request.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub position: i64,
    pub activity_id: i64,
    pub activity: String,
    pub category: String,
    pub unit: String,
    pub quantity: f64,
}

/// A proposed course with its tally. `voted` is whether the requesting voter
/// (the `?voter=` query, if any) has cast a vote — false when unknown.
#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: i64,
    pub title: String,
    pub author: String,
    pub created_at: String,
    pub votes: i64,
    pub voted: bool,
    pub steps: Vec<Step>,
}
