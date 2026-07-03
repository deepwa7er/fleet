use serde::Serialize;

/// A course is a countdown over exactly this many DIFFERENT activities: the
/// first is done ten times, the second nine, … the last once — "10 of one
/// thing, 9 of another". Enforced by the store on creation.
pub const COURSE_STEPS: usize = 10;

/// The quantity a step demands, determined entirely by its position in the
/// countdown: position 1 → 10, position 10 → 1. Stored nowhere — a derived
/// value stored twice is a value that can disagree with itself.
pub fn quantity_for(position: i64) -> i64 {
    COURSE_STEPS as i64 + 1 - position
}

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

/// One step of a proposed course: an activity done [`quantity_for`]`(position)`
/// times. The activity's display fields (including its category's name) are
/// denormalized in so the list view is one request.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub position: i64,
    pub activity_id: i64,
    pub activity: String,
    pub category: String,
    pub unit: String,
    /// Derived from `position` on the way out; see [`quantity_for`].
    pub quantity: i64,
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
