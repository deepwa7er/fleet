//! keep's HTTP surface: `GET /healthz` plus the versioned database API.
//!
//! ```text
//! POST /v1/{db}/query   {sql, params?, batch?} -> Outcome
//! POST /v1/{db}/tx      {statements: [{sql, params?, batch?}]} -> {results}
//! ```
//!
//! Every database route requires `Authorization: Bearer <token>` for that
//! database; the listener itself is tailnet-only (the unit binds OVH's
//! tailnet address), so the token is the second layer, not the first.
//! Errors use the fleet `{"error": …}` shape.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use fleet_common::http::{error_response, healthz};
use fleet_common::keep::Value;
use serde::Deserialize;

use crate::store::{error_status, Registry};

pub struct AppState {
    pub registry: Arc<Registry>,
}

pub fn router(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/{db}/query", post(query))
        .route("/v1/{db}/tx", post(tx))
        .fallback(fleet_common::http::api_not_found)
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct QueryBody {
    sql: String,
    #[serde(default)]
    params: Vec<Value>,
    #[serde(default)]
    batch: bool,
}

#[derive(Debug, Deserialize)]
struct TxBody {
    statements: Vec<TxStatement>,
}

#[derive(Debug, Deserialize)]
struct TxStatement {
    sql: String,
    #[serde(default)]
    params: Vec<Value>,
    #[serde(default)]
    batch: bool,
}

/// Check the Bearer [REDACTED] against the database's token. Unknown databases fail
/// closed here (404) before any comparison runs, so existence never leaks
/// through timing — only through the status, which an authenticated client
/// for another database could already infer. That is accepted: database
/// names are not secrets, tokens are.
/// Rejects as a small `(status, message)` pair — not a `Response`, which is
/// too large for an `Err` variant (clippy `result_large_err`); handlers
/// render it with [`error_response`].
fn authorize(
    registry: &Registry,
    db: &str,
    headers: &HeaderMap,
) -> std::result::Result<(), (StatusCode, String)> {
    if !registry.contains(db) {
        return Err((StatusCode::NOT_FOUND, format!("unknown database: {db}")));
    }
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if presented.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, "missing bearer token".into()));
    }
    if !registry.authorized(db, presented) {
        return Err((StatusCode::UNAUTHORIZED, "invalid token".into()));
    }
    Ok(())
}

async fn query(
    State(state): State<Arc<AppState>>,
    Path(db): Path<String>,
    headers: HeaderMap,
    Json(body): Json<QueryBody>,
) -> Response {
    if let Err((status, message)) = authorize(&state.registry, &db, &headers) {
        return error_response(status, message);
    }
    if body.sql.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "empty sql");
    }
    match state
        .registry
        .run_one(&db, &body.sql, &body.params, body.batch)
        .await
    {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => {
            let status = error_status(&e);
            if status.is_server_error() {
                tracing::error!("query on {db:?} failed: {e}");
            }
            error_response(status, e.to_string())
        }
    }
}

async fn tx(
    State(state): State<Arc<AppState>>,
    Path(db): Path<String>,
    headers: HeaderMap,
    Json(body): Json<TxBody>,
) -> Response {
    if let Err((status, message)) = authorize(&state.registry, &db, &headers) {
        return error_response(status, message);
    }
    if body.statements.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "empty transaction");
    }
    if body.statements.len() > 1000 {
        return error_response(
            StatusCode::BAD_REQUEST,
            "transaction too large (max 1000 statements)",
        );
    }
    let statements: Vec<(String, Vec<Value>, bool)> = body
        .statements
        .into_iter()
        .map(|s| (s.sql, s.params, s.batch))
        .collect();
    if statements.iter().any(|(sql, _, _)| sql.trim().is_empty()) {
        return error_response(StatusCode::BAD_REQUEST, "empty statement in transaction");
    }
    match state.registry.run_tx(&db, &statements).await {
        Ok(results) => Json(serde_json::json!({ "results": results })).into_response(),
        Err(e) => {
            let status = error_status(&e);
            if status.is_server_error() {
                tracing::error!("transaction on {db:?} failed: {e}");
            }
            error_response(status, e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_common::keep::{Client, Statement};
    use std::net::SocketAddr;
    use std::path::PathBuf;

    /// A live keep on an ephemeral loopback port, driven through the real
    /// fleet-common client — these tests pin the wire contract both sides
    /// implement, not either side's internals.
    struct TestKeep {
        addr: SocketAddr,
        _dir: PathBuf,
    }

    async fn start(name: &str) -> (TestKeep, Client) {
        let dir = std::env::temp_dir().join(format!("keep-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let registry = Registry::open(&dir, vec![("testdb".into(), "secret".into())])
            .await
            .unwrap();
        let app = router(Arc::new(AppState {
            registry: Arc::new(registry),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = Client::new(&format!("http://{addr}"), "testdb", "secret");
        (TestKeep { addr, _dir: dir }, client)
    }

    fn bad_client(addr: &SocketAddr, token: &str) -> Client {
        Client::new(&format!("http://{addr}"), "testdb", token)
    }

    #[tokio::test]
    async fn query_round_trips_all_types() {
        let (_keep, client) = start("types").await;
        client
            .batch("CREATE TABLE t (i INTEGER, r REAL, t TEXT, b BLOB, n TEXT)")
            .await
            .unwrap();
        let inserted = client
            .query(
                "INSERT INTO t VALUES (?1, ?2, ?3, ?4, ?5)",
                vec![
                    Value::Integer(42),
                    Value::Real(1.5),
                    Value::Text("héllo".into()),
                    Value::Blob(vec![0, 255]),
                    Value::Null,
                ],
            )
            .await
            .unwrap();
        assert_eq!(inserted.rowid, 1);
        assert_eq!(inserted.changes, 1);

        let selected = client.query("SELECT i, r, t, b, n FROM t", vec![]).await.unwrap();
        assert_eq!(
            selected.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            vec!["i", "r", "t", "b", "n"]
        );
        assert_eq!(
            selected.rows,
            vec![vec![
                Value::Integer(42),
                Value::Real(1.5),
                Value::Text("héllo".into()),
                Value::Blob(vec![0, 255]),
                Value::Null,
            ]]
        );
    }

    #[tokio::test]
    async fn update_reports_changes() {
        let (_keep, client) = start("changes").await;
        client.batch("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").await.unwrap();
        client
            .query("INSERT INTO t (v) VALUES (?1)", vec![Value::from("a")])
            .await
            .unwrap();
        let hit = client
            .query("UPDATE t SET v = 'b' WHERE id = 1", vec![])
            .await
            .unwrap();
        assert_eq!(hit.changes, 1);
        let miss = client
            .query("UPDATE t SET v = 'b' WHERE id = 999", vec![])
            .await
            .unwrap();
        assert_eq!(miss.changes, 0);
    }

    #[tokio::test]
    async fn tx_commits_and_rolls_back() {
        let (_keep, client) = start("tx").await;
        client.batch("CREATE TABLE t (id INTEGER PRIMARY KEY)").await.unwrap();
        let results = client
            .tx(vec![
                Statement::new("INSERT INTO t (id) VALUES (?1)", vec![Value::Integer(1)]),
                Statement::new("INSERT INTO t (id) VALUES (?1)", vec![Value::Integer(2)]),
            ])
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[1].0.rowid, 2);

        // Second statement violates the primary key: the whole batch rolls
        // back, including the first insert.
        let err = client
            .tx(vec![
                Statement::new("INSERT INTO t (id) VALUES (?1)", vec![Value::Integer(3)]),
                Statement::new("INSERT INTO t (id) VALUES (?1)", vec![Value::Integer(1)]),
            ])
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("CONSTRAINT") || msg.contains("constraint"),
            "got: {msg}"
        );
        let count = client.query("SELECT COUNT(*) AS n FROM t", vec![]).await.unwrap();
        assert_eq!(count.rows, vec![vec![Value::Integer(2)]]);
    }

    #[tokio::test]
    async fn auth_failures_are_loud() {
        let (keep, _client) = start("auth").await;
        // Wrong token and unknown database, through the real wire path.
        let cases = [
            (bad_client(&keep.addr, "wrong"), "invalid token"),
            (
                Client::new(&format!("http://{}", keep.addr), "nosuchdb", "secret"),
                "unknown database",
            ),
        ];
        for (client, want) in cases {
            let err = client.query("SELECT 1", vec![]).await.unwrap_err();
            assert!(err.to_string().contains(want), "got: {err}");
        }
    }

    /// Missing, empty, and malformed credentials fail closed at the helper —
    /// no live server needed, and no extra HTTP client dependency to test a
    /// header parse.
    #[tokio::test]
    async fn authorize_rejects_bad_credentials() {
        let dir = std::env::temp_dir().join(format!("keep-test-{}-authz", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let registry = Registry::open(&dir, vec![("testdb".into(), "secret".into())])
            .await
            .unwrap();
        let missing = HeaderMap::new();
        assert!(authorize(&registry, "testdb", &missing).is_err());
        let mut wrong_scheme = HeaderMap::new();
        wrong_scheme.insert("authorization", "Basic abc".parse().unwrap());
        assert!(authorize(&registry, "testdb", &wrong_scheme).is_err());
        let mut empty = HeaderMap::new();
        empty.insert("authorization", "Bearer ".parse().unwrap());
        assert!(authorize(&registry, "testdb", &empty).is_err());
        let mut good = HeaderMap::new();
        good.insert("authorization", "Bearer secret".parse().unwrap());
        assert!(authorize(&registry, "testdb", &good).is_ok());
        // Unknown database fails before any comparison.
        assert!(authorize(&registry, "nosuchdb", &good).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sql_errors_map_honestly() {
        let (_keep, client) = start("errors").await;
        client.batch("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT NOT NULL)").await.unwrap();
        // Constraint violation: the client's fault, 400.
        let err = client
            .query("INSERT INTO t (v) VALUES (?1)", vec![Value::Null])
            .await
            .unwrap_err();
        assert!(
            matches!(err, fleet_common::Error::BadRequest(_)),
            "got: {err:?}"
        );
        // Missing table: typed as an engine error, 500 with the message.
        // Deliberately not sniffed into a 400 — matching message strings
        // would couple the contract to the engine's wording.
        let err = client.query("SELECT * FROM nosuchtable", vec![]).await.unwrap_err();
        assert!(
            matches!(err, fleet_common::Error::Internal(_)),
            "got: {err:?}"
        );
    }
}
