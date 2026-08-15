use anyhow::Context;
use chrono::Utc;
use clap::Parser;
use warehouse::config::Config;
use warehouse::warehouse::{crawler, git_extract, heuristic, shell_extract};
use rusqlite::params;
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "warehouse-ingest", about = "Ingest all of ~/code + shell/git into SQLite + Parquet")]
struct Args {
    #[arg(long)]
    dry_run: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let cfg = Config::from_env()?;
    info!(?cfg, dry_run = args.dry_run, "starting ingest");

    std::fs::create_dir_all(&cfg.warehouse_dir).context("create warehouse_dir")?;
    std::fs::create_dir_all(&cfg.warehouse_raw_dir).context("create raw dir")?;

    let mut conn = warehouse::warehouse::db::open_and_migrate(&cfg.warehouse_db)?;
    // collect crawl
    let crawl = crawler::crawl_code_root(&cfg.code_root)?;
    info!(repos = crawl.repos.len(), files = crawl.files.len(), "crawled");

    // build path->id map for shell heuristic
    let mut repo_path_to_id: HashMap<String, String> = HashMap::new();
    for r in &crawl.repos {
        repo_path_to_id.insert(r.path.clone(), r.repo_id.clone());
    }

    // collect git records
    let mut git_records = Vec::new();
    for repo in &crawl.repos {
        let recs = git_extract::extract_git(Path::new(&repo.path), &repo.repo_id, cfg.git_commits_per_repo)
            .unwrap_or_else(|e| {
                warn!(repo = %repo.name, error = %e, "git extract failed");
                Vec::new()
            });
        git_records.extend(recs);
    }
    info!(git_commits = git_records.len(), "git extracted");

    // collect shell records
    let shell_records = shell_extract::extract_shell(
        &cfg.shell_history,
        &cfg.code_root,
        &crawl.repo_name_to_id,
        &repo_path_to_id,
    )?;
    info!(shell_lines = shell_records.len(), "shell extracted");

    // heuristic integrations: need snippets per repo
    let mut integrations = Vec::new();
    for repo in &crawl.repos {
        let repo_path = Path::new(&repo.path);
        let snippets = collect_snippets(repo_path, 2000);
        let recs =
            heuristic::detect_integrations(&repo.repo_id, repo_path, &crawl.repo_name_to_id, &snippets);
        integrations.extend(recs);
    }
    info!(integrations = integrations.len(), "heuristic integrations");

    if args.dry_run {
        println!(
            "dry-run: repos={} files={} deps={} git={} shell={} integrations={}",
            crawl.repos.len(),
            crawl.files.len(),
            crawl.dependencies.len(),
            git_records.len(),
            shell_records.len(),
            integrations.len()
        );
        return Ok(());
    }

    let tx = conn.transaction().context("begin tx")?;

    // dim_repo
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO dim_repo (repo_id, path, name, primary_language, languages, build_system, test_cmd, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let now = Utc::now().to_rfc3339();
        for r in &crawl.repos {
            let langs = serde_json::to_string(&r.languages).unwrap_or_else(|_| "[]".to_string());
            stmt.execute(params![
                r.repo_id,
                r.path,
                r.name,
                r.primary_language,
                langs,
                r.build_system,
                r.test_cmd,
                now,
                now
            ])?;
        }
    }
    // dim_tool from build systems + shell
    {
        let mut tools: HashMap<String, String> = HashMap::new();
        for r in &crawl.repos {
            if let Some(bs) = &r.build_system {
                tools.insert(bs.clone(), "build".to_string());
            }
        }
        for s in &shell_records {
            if let Some(t) = &s.tool_name {
                tools.entry(t.clone()).or_insert_with(|| "shell".to_string());
            }
        }
        let mut stmt = tx.prepare("INSERT OR IGNORE INTO dim_tool (tool_name, category) VALUES (?1, ?2)")?;
        for (tool, cat) in tools {
            stmt.execute(params![tool, cat])?;
        }
    }
    // fact_file - replace per repo
    {
        for repo in &crawl.repos {
            tx.execute("DELETE FROM fact_file WHERE repo_id = ?1", params![repo.repo_id])?;
        }
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO fact_file (repo_id, path, rel_path, language, ext, bytes, hash, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let now = Utc::now().to_rfc3339();
        for f in &crawl.files {
            stmt.execute(params![
                f.repo_id,
                f.path,
                f.rel_path,
                f.language,
                f.ext,
                f.bytes,
                f.hash,
                now
            ])?;
        }
    }
    // fact_dependency
    {
        for repo in &crawl.repos {
            tx.execute(
                "DELETE FROM fact_dependency WHERE repo_id = ?1",
                params![repo.repo_id],
            )?;
        }
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO fact_dependency (repo_id, dependency, version, source_file) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for d in &crawl.dependencies {
            stmt.execute(params![d.repo_id, d.dependency, d.version, d.source_file])?;
        }
    }
    // fact_integration heuristic-only: replace all heuristic rows for these repos
    {
        for repo in &crawl.repos {
            tx.execute(
                "DELETE FROM fact_integration WHERE src_repo_id = ?1",
                params![repo.repo_id],
            )?;
        }
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO fact_integration (src_repo_id, dst_repo_id, dst_name, type, evidence, confidence) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for it in &integrations {
            stmt.execute(params![
                it.src_repo_id,
                it.dst_repo_id,
                it.dst_name,
                it.kind,
                it.evidence,
                it.confidence
            ])?;
        }
    }
    // fact_git
    {
        for repo in &crawl.repos {
            tx.execute("DELETE FROM fact_git WHERE repo_id = ?1", params![repo.repo_id])?;
        }
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO fact_git (repo_id, commit_hash, author, author_email, ts, message, files_changed) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for g in &git_records {
            stmt.execute(params![
                g.repo_id,
                g.commit_hash,
                g.author,
                g.author_email,
                g.ts.to_rfc3339(),
                g.message,
                g.files_changed
            ])?;
        }
    }
    // fact_shell - full replace (small)
    {
        tx.execute("DELETE FROM fact_shell", [])?;
        let mut stmt = tx.prepare(
            "INSERT INTO fact_shell (ts, repo_id, cwd, cmd, tool_name, raw_line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for s in &shell_records {
            stmt.execute(params![
                s.ts.map(|t| t.to_rfc3339()),
                s.repo_id,
                s.cwd,
                s.cmd,
                s.tool_name,
                s.raw_line
            ])?;
        }
    }

    tx.commit().context("commit")?;
    // checkpoint so backup sees consistent file
    let conn2 = warehouse::warehouse::db::open_and_migrate(&cfg.warehouse_db)?;
    warehouse::warehouse::db::checkpoint(&conn2)?;

    // also write raw parquet snapshot for disaster recovery + audit (partitioned by dt)
    write_raw_parquet(&cfg, &crawl, &git_records, &shell_records)?;

    info!("ingest complete");
    Ok(())
}

fn collect_snippets(repo_path: &Path, max_lines: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(repo_path)
        .max_depth(4)
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            !matches!(n.as_ref(), ".git" | "target" | "node_modules" | ".venv" | "__pycache__")
        });
    for entry in walker.filter_map(|e| e.ok()) {
        if entry.path().is_dir() {
            continue;
        }
        let ext = entry.path().extension().and_then(|e| e.to_str()).unwrap_or("");
        // only scan text-ish files
        if !matches!(
            ext,
            "rs" | "py" | "js" | "ts" | "go" | "toml" | "json" | "yaml" | "yml" | "md" | "sh" | "env" | "conf" | "cfg" | "ini"
        ) && entry.metadata().map(|m| m.len() > 200_000).unwrap_or(true) {
            continue;
        }
        if entry.metadata().map(|m| m.len() > 200_000).unwrap_or(true) {
            continue;
        }
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let rel = entry
            .path()
            .strip_prefix(repo_path)
            .unwrap_or(entry.path())
            .display()
            .to_string();
        for line in content.lines().take(500) {
            let t = line.trim();
            if t.is_empty() || t.starts_with("//") || t.starts_with('#') && t.len() < 4 {
                continue;
            }
            if t.len() > 200 {
                continue;
            }
            out.push((rel.clone(), line.to_string()));
            if out.len() >= max_lines {
                return out;
            }
        }
    }
    out
}

fn write_raw_parquet(
    cfg: &Config,
    crawl: &crawler::CrawlResult,
    git_records: &[warehouse::warehouse::GitRecord],
    shell_records: &[warehouse::warehouse::ShellRecord],
) -> anyhow::Result<()> {
    use arrow::array::StringArray;
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;
    use std::fs::File;
    use std::sync::Arc;

    let dt = Utc::now().format("%Y-%m-%d").to_string();
    let dir = cfg.warehouse_raw_dir.join(format!("dt={dt}"));
    std::fs::create_dir_all(&dir)?;

    // dim_repo parquet
    {
        let path = dir.join("dim_repo.parquet");
        let schema = Arc::new(Schema::new(vec![
            Field::new("repo_id", DataType::Utf8, false),
            Field::new("path", DataType::Utf8, false),
            Field::new("name", DataType::Utf8, false),
            Field::new("primary_language", DataType::Utf8, true),
            Field::new("languages", DataType::Utf8, true),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from_iter_values(
                    crawl.repos.iter().map(|r| r.repo_id.as_str()),
                )),
                Arc::new(StringArray::from_iter_values(
                    crawl.repos.iter().map(|r| r.path.as_str()),
                )),
                Arc::new(StringArray::from_iter_values(
                    crawl.repos.iter().map(|r| r.name.as_str()),
                )),
                Arc::new(StringArray::from(
                    crawl
                        .repos
                        .iter()
                        .map(|r| r.primary_language.as_deref())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    crawl
                        .repos
                        .iter()
                        .map(|r| serde_json::to_string(&r.languages).ok())
                        .collect::<Vec<_>>(),
                )),
            ],
        )?;
        let file = File::create(&path)?;
        let mut writer = ArrowWriter::try_new(file, schema, None)?;
        writer.write(&batch)?;
        writer.close()?;
    }
    let _ = (git_records, shell_records);
    Ok(())
}
