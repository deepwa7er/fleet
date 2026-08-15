use super::{IntegrationRecord, hash_repo_id};
use std::collections::HashMap;
use std::path::Path;

/// Heuristic-only integration detection.
/// No manual edges in v0. We grep for:
/// - imports of local repo names (e.g., `use ferry_config`, `from skiff`)
/// - config refs containing repo names
/// - URLs containing localhost ports or repo names
///
/// All matches are low confidence (0.4-0.7) and evidence is file:line snippet.
pub fn detect_integrations(
    repo_id: &str,
    _repo_path: &Path,
    repo_name_to_id: &HashMap<String, String>,
    file_snippets: &[(String, String)], // (rel_path, line)
) -> Vec<IntegrationRecord> {
    let mut out = Vec::new();
    // precompute lower names for substring search
    let lower_names: Vec<(String, String)> = repo_name_to_id
        .iter()
        .map(|(name, id)| (name.to_ascii_lowercase(), id.clone()))
        .collect();

    for (rel_path, line) in file_snippets {
        let lower = line.to_ascii_lowercase();
        for (lname, did) in &lower_names {
            // skip self
            if let Some(self_id) = repo_name_to_id.values().find(|id| *id == repo_id) {
                let _ = self_id;
            }
            // naive: if line contains repo name as word-ish substring
            if !contains_word(&lower, lname) {
                continue;
            }
            // self-reference skip: if the file's repo is the same as target, ignore
            let target_id = did.clone();
            if target_id == repo_id {
                continue;
            }
            let (kind, conf) = classify_line(&lower);
            let dst_name = repo_name_to_id
                .iter()
                .find(|(_, id)| *id == &target_id)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| lname.clone());
            out.push(IntegrationRecord {
                src_repo_id: repo_id.to_string(),
                dst_repo_id: Some(target_id),
                dst_name,
                kind,
                evidence: format!("{}: {}", rel_path, truncate(line, 240)),
                confidence: conf,
            });
        }

        // external heuristic: localhost / 127.0.0.1 / api url
        if lower.contains("localhost") || lower.contains("127.0.0.1") || lower.contains("://") && lower.contains("api") {
            // don't create duplicate per-line external if already matched
            if !out.iter().any(|r| r.evidence.contains(rel_path) && r.evidence.contains(line)) {
                // only if line also looks like config/url
                if lower.contains("http") || lower.contains("port") || lower.contains("endpoint") {
                    out.push(IntegrationRecord {
                        src_repo_id: repo_id.to_string(),
                        dst_repo_id: None,
                        dst_name: "external".to_string(),
                        kind: "heuristic_api_url".to_string(),
                        evidence: format!("{}: {}", rel_path, truncate(line, 240)),
                        confidence: 0.4,
                    });
                }
            }
        }
    }

    // dedup by (dst_name, evidence)
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert((r.dst_name.clone(), r.evidence.clone())));
    // cap per repo
    out.truncate(500);
    out
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    // simple: contains and not part of longer alphanumeric
    if let Some(pos) = haystack.find(needle) {
        let before = haystack[..pos].chars().last().map(|c| c.is_alphanumeric() || c == '_' || c == '-').unwrap_or(false);
        let after = haystack[pos + needle.len()..].chars().next().map(|c| c.is_alphanumeric() || c == '_' || c == '-').unwrap_or(false);
        // allow if at boundary or surrounded by non-alnum like / . " '
        !before && !after || haystack.contains(&format!("/{needle}")) || haystack.contains(&format!("\"{needle}\"")) || haystack.contains(&format!("'{needle}'"))
    } else {
        false
    }
}

fn classify_line(lower: &str) -> (String, f64) {
    if lower.contains("import ") || lower.contains("use ") || lower.contains("from ") || lower.contains("require(") {
        ("heuristic_import".to_string(), 0.65)
    } else if lower.contains("config") || lower.contains("endpoint") || lower.contains("url") {
        ("heuristic_config_ref".to_string(), 0.55)
    } else {
        ("heuristic_api_url".to_string(), 0.45)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…", &s[..n]) }
}

pub fn repo_id_for_path(path: &Path, _code_root: &Path) -> String {
    hash_repo_id(&path.display().to_string())
}
