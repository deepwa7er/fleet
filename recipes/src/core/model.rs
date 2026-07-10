use serde::Serialize;

/// A recipe. `ingredients` and `steps` hold one entry per line — the storage
/// form matches how they're typed and edited; the web view splits lines for
/// display. `tags` is a string array on the wire and comma-joined in SQLite
/// (see [`join_tags`] / [`split_tags`]).
#[derive(Debug, Clone, Serialize)]
pub struct Recipe {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub ingredients: String,
    pub steps: String,
    pub tags: Vec<String>,
    pub servings: Option<i64>,
    pub prep_minutes: Option<i64>,
    pub cook_minutes: Option<i64>,
    pub source_url: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Normalize a tag list into its canonical stored form: trimmed, lowercased,
/// empties dropped, order-preserving dedupe. Both create and update pass tags
/// through here, so the column never holds a non-canonical value.
pub fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let t = tag.trim().to_lowercase();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Comma-join normalized tags for the TEXT column. Tags never contain commas:
/// [`normalize_tags`] runs on values the API splits on commas, so a comma can
/// never survive into a single tag.
pub fn join_tags(tags: &[String]) -> String {
    tags.join(",")
}

/// Split the stored comma-joined form back into the wire array.
pub fn split_tags(s: &str) -> Vec<String> {
    s.split(',')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_normalize_and_round_trip() {
        let tags = normalize_tags(vec![
            " Dinner ".into(),
            "dinner".into(),
            "".into(),
            "Weeknight".into(),
        ]);
        assert_eq!(tags, vec!["dinner", "weeknight"]);
        assert_eq!(split_tags(&join_tags(&tags)), tags);
        assert_eq!(split_tags(""), Vec::<String>::new());
    }
}
