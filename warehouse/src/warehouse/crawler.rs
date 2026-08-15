use super::{DependencyRecord, FileRecord, RepoRecord, ext_to_language, hash_repo_id};
use anyhow::Context;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug)]
pub struct CrawlResult {
    pub repos: Vec<RepoRecord>,
    pub files: Vec<FileRecord>,
    pub dependencies: Vec<DependencyRecord>,
    pub repo_name_to_id: HashMap<String, String>,
}

pub fn crawl_code_root(code_root: &Path) -> anyhow::Result<CrawlResult> {
    let mut repos = Vec::new();
    let mut files = Vec::new();
    let mut dependencies = Vec::new();
    let mut repo_name_to_id = HashMap::new();

    // Find immediate children that are dirs; treat any dir with or without .git as a repo candidate.
    // This matches "all of ~/code" without requiring git.
    let top_level: Vec<PathBuf> = std::fs::read_dir(code_root)
        .with_context(|| format!("read code_root {}", code_root.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();

    // Also support nested repos: if code_root itself is a repo, include it. Else walk one level deep.
    let candidates = if top_level.is_empty() {
        vec![code_root.to_path_buf()]
    } else {
        top_level
    };

    for repo_path in candidates {
        if should_skip_dir(&repo_path) {
            continue;
        }
        let name = repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let repo_id = hash_repo_id(&repo_path.display().to_string());
        repo_name_to_id.insert(name.clone(), repo_id.clone());

        let (repo_files, file_records, deps) = crawl_one_repo(&repo_path, &repo_id)?;
        let (build_system, test_cmd) = super::detect_build_system(&repo_files);

        // languages
        let mut lang_counts: HashMap<String, usize> = HashMap::new();
        for f in &file_records {
            if let Some(lang) = &f.language {
                *lang_counts.entry(lang.clone()).or_default() += 1;
            }
        }
        let mut languages: Vec<(String, usize)> = lang_counts.into_iter().collect();
        languages.sort_by_key(|b| Reverse(b.1));
        let languages_vec: Vec<String> = languages.iter().map(|(l, _)| l.clone()).collect();
        let primary_language = languages_vec.first().cloned();

        repos.push(RepoRecord {
            repo_id: repo_id.clone(),
            path: repo_path.display().to_string(),
            name,
            primary_language,
            languages: languages_vec,
            build_system,
            test_cmd,
            deploy_target: None,
        });
        files.extend(file_records);
        dependencies.extend(deps);
    }

    Ok(CrawlResult {
        repos,
        files,
        dependencies,
        repo_name_to_id,
    })
}

fn crawl_one_repo(repo_path: &Path, repo_id: &str) -> anyhow::Result<(Vec<String>, Vec<FileRecord>, Vec<DependencyRecord>)> {
    let mut file_names = Vec::new();
    let mut file_records = Vec::new();
    let mut dependencies = Vec::new();
    let mut seen_deps: HashSet<(String, String)> = HashSet::new();

    // Use ignore crate to respect .gitignore but also handle non-git dirs
    let walker = WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // skip hidden / vendor dirs
            !matches!(name.as_ref(), ".git" | "target" | "node_modules" | ".venv" | "__pycache__" | "dist" | "build")
        });

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let rel = path.strip_prefix(repo_path).unwrap_or(path);
        let rel_str = rel.display().to_string();
        file_names.push(rel_str.clone());

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
        let language = if ext.is_empty() {
            None
        } else {
            ext_to_language(&ext).map(|s| s.to_string())
        };

        let bytes = entry.metadata().map(|m| m.len() as i64).unwrap_or(0);
        // hash file content for dedup; on error use path hash
        let hash = hash_file(path).unwrap_or_else(|_| {
            let h = blake3::hash(rel.display().to_string().as_bytes());
            h.to_hex().to_string()
        });

        file_records.push(FileRecord {
            repo_id: repo_id.to_string(),
            path: path.display().to_string(),
            rel_path: rel_str,
            language,
            ext: if ext.is_empty() { None } else { Some(ext) },
            bytes,
            hash,
        });

        // dependency extraction for known manifests
        if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
            match fname {
                "Cargo.toml" => {
                    if let Ok(deps) = parse_cargo_deps(path) {
                        for (dep, ver) in deps {
                            let key = (dep.clone(), rel.display().to_string());
                            if seen_deps.insert(key) {
                                dependencies.push(DependencyRecord {
                                    repo_id: repo_id.to_string(),
                                    dependency: dep,
                                    version: ver,
                                    source_file: rel.display().to_string(),
                                });
                            }
                        }
                    }
                }
                "package.json" => {
                    if let Ok(deps) = parse_package_json_deps(path) {
                        for (dep, ver) in deps {
                            let key = (dep.clone(), rel.display().to_string());
                            if seen_deps.insert(key) {
                                dependencies.push(DependencyRecord {
                                    repo_id: repo_id.to_string(),
                                    dependency: dep,
                                    version: ver,
                                    source_file: rel.display().to_string(),
                                });
                            }
                        }
                    }
                }
                "go.mod" => {
                    if let Ok(deps) = parse_go_mod_deps(path) {
                        for (dep, ver) in deps {
                            let key = (dep.clone(), rel.display().to_string());
                            if seen_deps.insert(key) {
                                dependencies.push(DependencyRecord {
                                    repo_id: repo_id.to_string(),
                                    dependency: dep,
                                    version: ver,
                                    source_file: rel.display().to_string(),
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // cap per repo to avoid huge warehouses on first run
        if file_records.len() > 50_000 {
            break;
        }
    }

    Ok((file_names, file_records, dependencies))
}

fn hash_file(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    let h = blake3::hash(&bytes);
    Ok(h.to_hex().to_string())
}

fn parse_cargo_deps(path: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let content = std::fs::read_to_string(path)?;
    let mut deps = Vec::new();
    let mut in_deps = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("[dependencies") {
            in_deps = true;
            continue;
        }
        if t.starts_with('[') && in_deps {
            break;
        }
        if in_deps && !t.is_empty() && !t.starts_with('#') && t.contains('=') {
            let name = t.split('=').next().unwrap().trim().trim_matches('"').to_string();
            // rough version extraction
            let ver = t.split("version").nth(1).and_then(|v| {
                let v = v.split('"').nth(1)?;
                Some(v.to_string())
            });
            if !name.is_empty() {
                deps.push((name, ver));
            }
        }
    }
    Ok(deps)
}

fn parse_package_json_deps(path: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let content = std::fs::read_to_string(path)?;
    let v: serde_json::Value = serde_json::from_str(&content)?;
    let mut deps = Vec::new();
    for key in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(obj) = v.get(key).and_then(|x| x.as_object()) {
            for (k, val) in obj {
                let ver = val.as_str().map(|s| s.to_string());
                deps.push((k.clone(), ver));
            }
        }
    }
    Ok(deps)
}

fn parse_go_mod_deps(path: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let content = std::fs::read_to_string(path)?;
    let mut deps = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("require ") || t.starts_with("require(") {
            continue;
        }
        // heuristic: lines like "github.com/foo/bar v1.2.3"
        let parts: Vec<&str> = t.split_whitespace().collect();
        if parts.len() >= 2 && parts[0].contains('/') && parts[1].starts_with('v') {
            deps.push((parts[0].to_string(), Some(parts[1].to_string())));
        }
    }
    Ok(deps)
}

fn should_skip_dir(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    matches!(name, ".git" | ".cargo" | ".rustup" | "node_modules" | ".claude")
}
