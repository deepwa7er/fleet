//! HTTP client for keep, the fleet's central database service.
//!
//! keep speaks SQL + bound params over HTTP/JSON (the contract is documented
//! in `keep/README.md`). This client is the only way fleet services talk to
//! it — apps never hand-roll the HTTP or the value encoding, for the same
//! reason they never hand-roll the migration bracket: one home for the seam.
//!
//! [`Value`] is the wire type: SQLite's five storage classes with an explicit
//! tag, so a blob never degrades into lossy text and an integer never arrives
//! as a float. Both sides of the wire use this enum, so the encoding is an
//! internal detail — but it is deliberately plain JSON (blobs as byte
//! arrays), not base64 or msgpack: at fleet data volume readability beats
//! density, and no extra dependency rides along.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// One bound parameter or result cell: SQLite's five storage classes, tagged.
///
/// `From` impls cover the shapes services actually bind (ints, floats,
/// strings, bools as 0/1 like rusqlite, byte vecs, and `Option` of any of
/// those). Anything else goes through the enum directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Integer(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Integer(v as i64)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Real(v)
    }
}

/// SQLite has no boolean; 0/1, matching rusqlite's mapping.
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Integer(i64::from(v))
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Blob(v)
    }
}

impl From<&[u8]> for Value {
    fn from(v: &[u8]) -> Self {
        Value::Blob(v.to_vec())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

/// One statement in a [`Client::tx`] batch: either a single prepared
/// statement with bound params ([`Statement::new`]) or a multi-statement
/// script with none ([`Statement::batch`], for DDL and migrations).
///
/// Entries with params must be a single statement — anything after the first
/// is ignored, SQLite semantics, enforced by nothing. Keep migration scripts
/// in `batch` entries where that footgun cannot fire.
#[derive(Debug, Clone, Serialize)]
pub struct Statement {
    sql: String,
    params: Vec<Value>,
    batch: bool,
}

impl Statement {
    pub fn new(sql: impl Into<String>, params: Vec<Value>) -> Self {
        Statement {
            sql: sql.into(),
            params,
            batch: false,
        }
    }

    pub fn batch(sql: impl Into<String>) -> Self {
        Statement {
            sql: sql.into(),
            params: Vec::new(),
            batch: true,
        }
    }
}

/// A result column: the name plus the declared type, when the engine knows
/// one (expressions and some PRAGMAs report none).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub decl_type: Option<String>,
}

/// The outcome of one statement: its rows plus the write accounting.
/// `rowid` is `last_insert_rowid` (meaningful after INSERT); `changes` is
/// the rows the statement wrote. Batch entries report no changes — a
/// multi-statement script has no single count to give, and migrations (the
/// only batch users) never need one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    pub rowid: i64,
    pub changes: u64,
}

/// A connection to one keep database: base URL plus that database's Bearer [REDACTED]
///
/// One `Client` per database, mirroring today's one-file-per-app: the token
/// selects the database server-side, so a service can never address a
/// database it holds no token for.
pub struct Client {
    base: String,
    db: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    /// `base_url` is keep's origin (`http://100.73.64.99:8106`), `db` the
    /// database name (`recipes`); `token` is that database's Bearer [REDACTED]
    /// kept opaquely — it is only ever sent back as an `Authorization`
    /// header, never logged.
    pub fn new(base_url: &str, db: &str, token: &str) -> Self {
        Client {
            base: base_url.trim_end_matches('/').to_owned(),
            db: db.to_owned(),
            token: token.to_owned(),
            http: reqwest::Client::new(),
        }
    }

    /// Run one prepared statement (params may be empty — a param-less
    /// SELECT still comes here, not [`Client::batch`]).
    pub async fn query(&self, sql: &str, params: Vec<Value>) -> Result<Outcome> {
        self.post(
            "/query",
            &serde_json::json!({ "sql": sql, "params": params }),
        )
        .await
    }

    /// Run a multi-statement script with no params (DDL, migrations).
    pub async fn batch(&self, sql: &str) -> Result<Outcome> {
        self.post(
            "/query",
            &serde_json::json!({ "sql": sql, "params": [], "batch": true }),
        )
        .await
    }

    /// Run statements atomically: all commit, or (on any error) all roll
    /// back and the error is returned. The response holds one [`Outcome`]
    /// per statement, in order.
    pub async fn tx(&self, statements: Vec<Statement>) -> Result<Vec<TxOutcome>> {
        #[derive(Deserialize)]
        struct TxResponse {
            results: Vec<Outcome>,
        }
        let res: TxResponse = self
            .post(
                "/tx",
                &serde_json::json!({ "statements": statements }),
            )
            .await?;
        Ok(res.results.into_iter().map(TxOutcome).collect())
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let url = format!("{}/v1/{}{}", self.base, self.db, path);
        let res = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Internal(format!("keep unreachable at {}: {e}", self.base)))?;
        let status = res.status();
        if status.is_success() {
            res.json().await.map_err(|e| {
                Error::Internal(format!("keep at {} spoke invalid JSON: {e}", self.base))
            })
        } else {
            // keep speaks the fleet {"error"} shape; fall back to the status
            // when it somehow does not.
            let message = res
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| format!("keep returned {status}"));
            if status == reqwest::StatusCode::BAD_REQUEST {
                Err(Error::BadRequest(message))
            } else {
                // 401 (wrong token), 404 (unknown database), 503 (store
                // busy), 5xx: all operator-visible failures. The services'
                // posture is hard-fail — surface loudly, retry nothing.
                Err(Error::Internal(format!("keep error: {message}")))
            }
        }
    }
}

/// One statement's outcome inside a transaction; a newtype so `tx` cannot be
/// confused with a bare list of outcomes from elsewhere.
#[derive(Debug, Clone)]
pub struct TxOutcome(pub Outcome);

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire encoding every fleet service depends on: all five classes
    /// round-trip, and null carries no value key.
    #[test]
    fn value_wire_encoding_round_trips() {
        let values = vec![
            Value::Null,
            Value::Integer(-42),
            Value::Real(1.5),
            Value::Text("héllo".into()),
            Value::Blob(vec![0, 255, 1]),
        ];
        let json = serde_json::to_string(&values).unwrap();
        assert!(json.contains(r#"{"type":"null"}"#), "got: {json}");
        let back: Vec<Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, values);
    }

    /// The `From` impls services actually use, including `None` → Null.
    #[test]
    fn value_conversions_cover_service_shapes() {
        assert_eq!(Value::from(7i32), Value::Integer(7));
        assert_eq!(Value::from(true), Value::Integer(1));
        assert_eq!(Value::from("s"), Value::Text("s".into()));
        assert_eq!(Value::from(None::<String>), Value::Null);
        assert_eq!(
            Value::from(Some("s")),
            Value::Text("s".into())
        );
        assert_eq!(Value::from(vec![9u8]), Value::Blob(vec![9]));
    }

    /// Base URL normalization: a trailing slash must not double up paths.
    #[test]
    fn client_trims_trailing_slash() {
        let c = Client::new("http://keep:8106/", "recipes", "t");
        assert_eq!(c.base, "http://keep:8106");
        assert_eq!(c.db, "recipes");
    }
}
