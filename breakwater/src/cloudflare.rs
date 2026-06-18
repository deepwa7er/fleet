//! A minimal Cloudflare API client for solving ACME DNS-01 challenges:
//! look up the zone, create the `_acme-challenge` TXT record, and delete it
//! afterwards.
//!
//! It speaks HTTPS over the TLS stack breakwater already depends on
//! (tokio-rustls / ring), with the webpki root bundle, rather than pulling in a
//! second HTTP/TLS dependency.

use std::sync::Arc;

use anyhow::{Context, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::rustls::pki_types::ServerName;

const API_HOST: &str = "api.cloudflare.com";

/// A DNS zone, with the authoritative nameservers that serve it.
pub struct Zone {
    pub id: String,
    pub name_servers: Vec<String>,
}

pub struct Cloudflare {
    token: String,
    tls: TlsConnector,
}

impl Cloudflare {
    pub fn new(token: String) -> Self {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Self {
            token,
            tls: TlsConnector::from(Arc::new(config)),
        }
    }

    /// Resolve a zone name (e.g. `deepwa7er.com`) to its id and authoritative
    /// nameservers.
    pub async fn zone(&self, zone_name: &str) -> anyhow::Result<Zone> {
        let json = self
            .send(Method::GET, &format!("/client/v4/zones?name={zone_name}"), None)
            .await?;
        let zone = json["result"]
            .get(0)
            .with_context(|| format!("Cloudflare zone {zone_name:?} not found (check the token's zone scope)"))?;
        let id = zone["id"]
            .as_str()
            .context("Cloudflare zone has no id")?
            .to_string();
        let name_servers = zone["name_servers"]
            .as_array()
            .map(|values| values.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        Ok(Zone { id, name_servers })
    }

    /// Create a TXT record and return its id.
    pub async fn create_txt(&self, zone_id: &str, name: &str, content: &str) -> anyhow::Result<String> {
        let body = json!({ "type": "TXT", "name": name, "content": content, "ttl": 60 });
        let json = self
            .send(Method::POST, &format!("/client/v4/zones/{zone_id}/dns_records"), Some(body))
            .await?;
        json["result"]["id"]
            .as_str()
            .map(String::from)
            .context("Cloudflare did not return a record id")
    }

    /// Delete a DNS record by id.
    pub async fn delete_record(&self, zone_id: &str, record_id: &str) -> anyhow::Result<()> {
        self.send(
            Method::DELETE,
            &format!("/client/v4/zones/{zone_id}/dns_records/{record_id}"),
            None,
        )
        .await?;
        Ok(())
    }

    /// Issue one request to the Cloudflare API and return the parsed JSON,
    /// erroring if the API reports failure. A fresh connection per call is fine:
    /// these are infrequent, made only during issuance.
    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let tcp = TcpStream::connect((API_HOST, 443))
            .await
            .context("connecting to the Cloudflare API")?;
        let server_name = ServerName::try_from(API_HOST).context("invalid API server name")?;
        let stream = self
            .tls
            .connect(server_name, tcp)
            .await
            .context("TLS handshake with the Cloudflare API")?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let payload = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value)?),
            None => Bytes::new(),
        };
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, API_HOST)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(ACCEPT, "application/json");
        if body.is_some() {
            builder = builder.header(CONTENT_TYPE, "application/json");
        }
        let request = builder.body(Full::new(payload)).context("building Cloudflare request")?;

        let response = sender.send_request(request).await.context("Cloudflare request failed")?;
        let status = response.status();
        let bytes = response.into_body().collect().await?.to_bytes();
        let json: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("Cloudflare returned a non-JSON response (status {status})"))?;
        if json.get("success").and_then(Value::as_bool) != Some(true) {
            bail!(
                "Cloudflare API error (status {status}): {}",
                json.get("errors").unwrap_or(&Value::Null)
            );
        }
        Ok(json)
    }
}
