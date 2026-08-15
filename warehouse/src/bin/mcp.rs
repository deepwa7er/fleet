use std::future::Future;

use anyhow::Context;
use warehouse::config::Config;
use rmcp::handler::server::tool::{ToolRouter, Parameters};
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Warehouse MCP server - agents query it for "add feature to X" context.
/// Exposes library-style tools, not an app. Heuristic-only integrations.
#[derive(Clone)]
struct WarehouseMcp {
    cfg: Arc<Config>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RepoQuery {
    /// repo name or substring, e.g. "skiff" or "fleet"
    repo: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchQuery {
    /// natural question, e.g. "how do I build Rust in this repo?"
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SqlQuery {
    /// read-only SQL, e.g. SELECT * FROM repo_profile WHERE name='skiff'
    sql: String,
}

#[derive(Debug, Serialize)]
struct RepoContext {
    repo: serde_json::Value,
    files_sample: Vec<serde_json::Value>,
    integrations: Vec<serde_json::Value>,
    dependencies: Vec<serde_json::Value>,
    recent_commits: Vec<serde_json::Value>,
    tool_hints: Vec<String>,
}

#[tool_router]
impl WarehouseMcp {
    fn new(cfg: Config) -> Self {
        Self {
            cfg: Arc::new(cfg),
            tool_router: Self::tool_router(),
        }
    }

    fn open_db(&self) -> anyhow::Result<rusqlite::Connection> {
        warehouse::warehouse::db::open_and_migrate(&self.cfg.warehouse_db)
    }

    #[tool(description = "Get full context for a repo by name - languages, build system, integrations, deps, recent commits. Use for 'add feature to X' planning.")]
    async fn get_repo_context(
        &self,
        Parameters(RepoQuery { repo: query_repo }): Parameters<RepoQuery>,
    ) -> Result<CallToolResult, McpError> {
        let conn = self.open_db().map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let repo_row = query_one(
            &conn,
            "SELECT * FROM repo_profile WHERE name LIKE ?1 OR repo_id LIKE ?1 LIMIT 1",
            rusqlite::params![format!("%{query_repo}%")],
        )
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        if repo_row.is_none() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "no repo matching '{query_repo}'"
            ))]));
        }
        let repo = repo_row.unwrap();
        let repo_id = repo
            .get("repo_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let files = query_many(
            &conn,
            "SELECT rel_path, language FROM fact_file WHERE repo_id = ?1 LIMIT 30",
            rusqlite::params![repo_id.clone()],
        )
        .unwrap_or_default();
        let integrations = query_many(
            &conn,
            "SELECT dst_name, type, confidence, evidence FROM fact_integration WHERE src_repo_id = ?1 ORDER BY confidence DESC LIMIT 20",
            rusqlite::params![repo_id.clone()],
        )
        .unwrap_or_default();
        let deps = query_many(
            &conn,
            "SELECT dependency, version, source_file FROM fact_dependency WHERE repo_id = ?1 LIMIT 30",
            rusqlite::params![repo_id.clone()],
        )
        .unwrap_or_default();
        let commits = query_many(
            &conn,
            "SELECT commit_hash, message, ts FROM fact_git WHERE repo_id = ?1 ORDER BY ts DESC LIMIT 10",
            rusqlite::params![repo_id.clone()],
        )
        .unwrap_or_default();
        let tools = query_many(
            &conn,
            "SELECT tool_name FROM fact_shell WHERE repo_id = ?1 GROUP BY tool_name LIMIT 20",
            rusqlite::params![repo_id],
        )
        .unwrap_or_default();
        let tool_hints: Vec<String> = tools
            .iter()
            .filter_map(|v| v.get("tool_name").and_then(|x| x.as_str()).map(|s| s.to_string()))
            .collect();

        let ctx = RepoContext {
            repo,
            files_sample: files,
            integrations,
            dependencies: deps,
            recent_commits: commits,
            tool_hints,
        };
        Ok(CallToolResult::success(vec![Content::json(ctx).unwrap()]))
    }

    #[tool(description = "Search across all repos for tool/language/build hints. E.g. query='rust' returns repos using rust.")]
    async fn search_build_knowledge(
        &self,
        Parameters(SearchQuery { query, limit }): Parameters<SearchQuery>,
    ) -> Result<CallToolResult, McpError> {
        let conn = self.open_db().map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let pat = format!("%{query}%");
        let rows = query_many(
            &conn,
            "SELECT * FROM repo_profile WHERE name LIKE ?1 OR primary_language LIKE ?1 OR languages LIKE ?1 OR build_system LIKE ?1 LIMIT ?2",
            rusqlite::params![pat, limit as i64],
        )
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::json(rows).unwrap()]))
    }

    #[tool(description = "Run read-only SQL against warehouse (SELECT only). Tables: dim_repo, fact_file, fact_dependency, fact_integration (heuristic-only), fact_git, fact_shell, views: repo_profile, integration_graph, tool_preferences")]
    async fn query_warehouse(
        &self,
        Parameters(SqlQuery { sql }): Parameters<SqlQuery>,
    ) -> Result<CallToolResult, McpError> {
        let trimmed = sql.trim().to_ascii_lowercase();
        if !trimmed.starts_with("select") && !trimmed.starts_with("with") {
            return Err(McpError::invalid_params("only SELECT/WITH allowed", None));
        }
        if trimmed.contains("insert")
            || trimmed.contains("update")
            || trimmed.contains("delete")
            || trimmed.contains("drop")
        {
            return Err(McpError::invalid_params("write statements not allowed", None));
        }
        let conn = self.open_db().map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let rows = query_sql(&conn, &sql).map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::json(rows).unwrap()]))
    }

    #[tool(description = "List all repos with primary language and build system")]
    async fn list_repos(&self) -> Result<CallToolResult, McpError> {
        let conn = self.open_db().map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let rows = query_many(&conn, "SELECT * FROM repo_profile ORDER BY name", rusqlite::params![])
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::json(rows).unwrap()]))
    }
}

fn query_one(
    conn: &rusqlite::Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> anyhow::Result<Option<serde_json::Value>> {
    let mut stmt = conn.prepare(sql)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(params)?;
    if let Some(row) = rows.next()? {
        Ok(Some(row_to_json(row, &col_names)?))
    } else {
        Ok(None)
    }
}

fn query_many(
    conn: &rusqlite::Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(sql)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_json(row, &col_names)?);
    }
    Ok(out)
}

fn query_sql(conn: &rusqlite::Connection, sql: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(sql)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_json(row, &col_names)?);
    }
    Ok(out)
}

fn row_to_json(row: &rusqlite::Row, col_names: &[String]) -> anyhow::Result<serde_json::Value> {
    use rusqlite::types::ValueRef as SqliteValueRef;
    let mut map = serde_json::Map::new();
    for (i, name) in col_names.iter().enumerate() {
        let val = row.get_ref(i)?;
        let json_val = match val {
            SqliteValueRef::Null => serde_json::Value::Null,
            SqliteValueRef::Integer(v) => serde_json::Value::Number(v.into()),
            SqliteValueRef::Real(v) => serde_json::Value::Number(
                serde_json::Number::from_f64(v).unwrap_or(serde_json::Number::from(0)),
            ),
            SqliteValueRef::Text(t) => serde_json::Value::String(String::from_utf8_lossy(t).to_string()),
            SqliteValueRef::Blob(b) => serde_json::Value::String(hex::encode(b)),
        };
        map.insert(name.clone(), json_val);
    }
    Ok(serde_json::Value::Object(map))
}

#[tool_handler]
impl ServerHandler for WarehouseMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "dev-warehouse".to_string(),
                version: "0.1.0".to_string(),
            },
            instructions: Some(
                "Query warehouse for repo context, integrations (heuristic-only), and build knowledge. Use get_repo_context for 'add feature to X'."
                    .to_string(),
            ),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cfg = Config::from_env().context("load config")?;
    let mcp = WarehouseMcp::new(cfg);
    let service = mcp.serve(rmcp::transport::stdio()).await.context("serve")?;
    service.waiting().await?;
    Ok(())
}
