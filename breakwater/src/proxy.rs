//! The reverse-proxy core: pick an upstream by `Host`, forward the request with
//! the right header surgery, and stream the response back — including WebSocket
//! / other `Upgrade` tunnels.
//!
//! Header handling follows the rules in the rproxy notes: strip hop-by-hop
//! headers (and anything named in `Connection`), preserve end-to-end headers,
//! and add the `X-Forwarded-*` trio so the backend can see the original client.
//!
//! Connection strategy: one upstream TCP connection per request (no pooling).
//! For a personal tailnet proxy this is simpler and obviously correct; pooling
//! is a future optimization, not a correctness requirement.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full, combinators::UnsyncBoxBody};
use hyper::body::Incoming;
use hyper::header::{self, HeaderMap, HeaderName, HeaderValue};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use crate::config::RouteTarget;
use crate::static_files;

/// A uniform response body: proxied upstream bodies, files served by tower-http,
/// and small generated pages (404 / 502 / the 101 tunnel) share one type so a
/// connection can return any of them. It is `Send` (the connection task is
/// spawned) but need not be `Sync` — which `tower-http`'s served body is not —
/// so this is the *unsync* boxed body, the widest type all sources satisfy.
pub type ProxyBody = UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

/// Time budget for establishing a fresh upstream connection — the TCP connect
/// and the HTTP/1.1 handshake. Backends are loopback fleet services, so this
/// only trips on a wedged or half-open backend, never normal operation; on
/// elapse the request fails with 504 rather than hanging the connection task
/// (and its sockets/FDs) indefinitely.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Time budget for the upstream to return *response headers* (time to first
/// byte). This bounds a backend that accepts the connection but never responds.
/// It deliberately does NOT limit the response body, so long-lived streams (SSE
/// log tails, deploy consoles) are unaffected — only the initial head must
/// arrive within this window.
const UPSTREAM_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// Hop-by-hop headers (RFC 7230 §6.1): meaningful only for a single transport
/// link, never forwarded across a proxy. `Connection` may name additional ones.
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// The hostname → target routing table, shared across all connections.
pub struct Router {
    routes: std::collections::HashMap<String, RouteTarget>,
}

impl Router {
    pub fn new(routes: std::collections::HashMap<String, RouteTarget>) -> Self {
        Self { routes }
    }

    /// Resolve a `Host` header value (which may carry a `:port`) to a target.
    fn target_for(&self, host: &str) -> Option<&RouteTarget> {
        let host = host.split(':').next().unwrap_or(host).trim().to_ascii_lowercase();
        self.routes.get(&host)
    }
}

/// Entry point for one request. Never fails outward — routing misses become 404,
/// upstream failures become 502, and static files are served by tower-http — so
/// the caller's service is infallible.
pub async fn handle(req: Request<Incoming>, router: Arc<Router>, client_ip: IpAddr) -> Response<ProxyBody> {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Resolve to an owned target so no borrow of the shared router is held across
    // the await; cloning a target is a single String/PathBuf clone per request.
    match router.target_for(&host).cloned() {
        None => status_page(StatusCode::NOT_FOUND, "no route for host"),
        Some(RouteTarget::Static(root)) => static_files::serve(req, &root).await,
        Some(RouteTarget::Proxy(upstream)) => match proxy(req, &upstream, client_ip).await {
            Ok(response) => response,
            Err(err) => {
                eprintln!("breakwater: upstream error: {err:#}");
                status_page(StatusCode::BAD_GATEWAY, "upstream error")
            }
        },
    }
}

async fn proxy(
    mut req: Request<Incoming>,
    upstream: &str,
    client_ip: IpAddr,
) -> anyhow::Result<Response<ProxyBody>> {
    let is_upgrade = is_upgrade_request(req.headers());
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string();
    let method = req.method().clone();
    let forwarded = forward_request_headers(req.headers(), client_ip, is_upgrade);

    // One fresh upstream connection per request. `.with_upgrades()` lets the
    // connection task hand back the raw stream if the response is a 101. Each
    // setup stage is bounded by a timeout so a wedged backend can't pin the
    // spawned connection task (and its socket/FDs) open forever.
    let stream = match tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, TcpStream::connect(upstream)).await {
        Err(_) => return Ok(upstream_timeout(upstream, "connect")),
        Ok(result) => result.with_context(|| format!("connecting to upstream {upstream}"))?,
    };
    let (mut sender, connection) = match tokio::time::timeout(
        UPSTREAM_CONNECT_TIMEOUT,
        hyper::client::conn::http1::handshake(TokioIo::new(stream)),
    )
    .await
    {
        Err(_) => return Ok(upstream_timeout(upstream, "handshake")),
        Ok(result) => result.context("handshaking with upstream")?,
    };
    tokio::spawn(async move {
        if let Err(err) = connection.with_upgrades().await {
            eprintln!("breakwater: upstream connection closed: {err}");
        }
    });

    // An upgrade request carries no normal body (the bytes flow after the 101),
    // so forward an empty body and capture the client side of the future tunnel.
    // A normal request streams its body straight through.
    let (body, client_upgrade) = if is_upgrade {
        (empty_body(), Some(hyper::upgrade::on(&mut req)))
    } else {
        let body = req.into_body().map_err(box_err).boxed_unsync();
        (body, None)
    };

    let mut upstream_req = Request::builder().method(&method).uri(&path);
    *upstream_req.headers_mut().expect("fresh builder has headers") = forwarded;
    let upstream_req = upstream_req.body(body).expect("valid request parts");

    // Bounds time-to-first-byte only; `send_request` resolves once the response
    // head arrives, so streaming bodies (SSE) are not cut off by this.
    let response = match tokio::time::timeout(UPSTREAM_RESPONSE_TIMEOUT, sender.send_request(upstream_req)).await {
        Err(_) => return Ok(upstream_timeout(upstream, "response")),
        Ok(result) => result.context("awaiting upstream response")?,
    };

    if is_upgrade && response.status() == StatusCode::SWITCHING_PROTOCOLS {
        return Ok(tunnel(response, client_upgrade.expect("upgrade path sets this")));
    }

    // Normal response: strip hop-by-hop headers and stream the body back.
    let status = response.status();
    let headers = strip_hop_by_hop(response.headers());
    let body = response.into_body().map_err(box_err).boxed_unsync();
    let mut out = Response::new(body);
    *out.status_mut() = status;
    *out.headers_mut() = headers;
    Ok(out)
}

/// Wire the two halves of an upgraded (101) connection together. The downstream
/// response mirrors the upstream's 101 verbatim — `Upgrade`/`Connection` and any
/// `Sec-*` handshake headers must reach the client intact — and a background
/// task pipes bytes both ways once both sides have switched protocols.
fn tunnel(mut upstream_response: Response<Incoming>, client_upgrade: hyper::upgrade::OnUpgrade) -> Response<ProxyBody> {
    let upstream_upgrade = hyper::upgrade::on(&mut upstream_response);
    tokio::spawn(async move {
        match tokio::try_join!(client_upgrade, upstream_upgrade) {
            Ok((client_io, upstream_io)) => {
                let mut client_io = TokioIo::new(client_io);
                let mut upstream_io = TokioIo::new(upstream_io);
                if let Err(err) =
                    tokio::io::copy_bidirectional(&mut client_io, &mut upstream_io).await
                {
                    eprintln!("breakwater: tunnel closed: {err}");
                }
            }
            Err(err) => eprintln!("breakwater: upgrade handshake failed: {err}"),
        }
    });

    let mut out = Response::new(empty_body());
    *out.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *out.headers_mut() = upstream_response.headers().clone();
    out
}

/// True when the request is asking to switch protocols: it must both carry an
/// `Upgrade` header and list `upgrade` as a `Connection` option.
fn is_upgrade_request(headers: &HeaderMap) -> bool {
    headers.contains_key(header::UPGRADE) && connection_tokens(headers).contains("upgrade")
}

/// The set of header names listed across all `Connection` header values, each of
/// which is itself hop-by-hop for this request.
fn connection_tokens(headers: &HeaderMap) -> HashSet<String> {
    let mut tokens = HashSet::new();
    for value in headers.get_all(header::CONNECTION) {
        if let Ok(value) = value.to_str() {
            for token in value.split(',') {
                let token = token.trim();
                if !token.is_empty() {
                    tokens.insert(token.to_ascii_lowercase());
                }
            }
        }
    }
    tokens
}

/// Whether a header should be dropped when crossing the proxy: the fixed
/// hop-by-hop list plus anything the request's own `Connection` header names,
/// plus the `X-Forwarded-*` headers we (re)generate authoritatively.
fn is_hop_by_hop(name: &str, connection_tokens: &HashSet<String>) -> bool {
    HOP_BY_HOP.contains(&name) || connection_tokens.contains(name)
}

/// Build the header set to send upstream from the client's headers.
///
/// Keeps end-to-end headers (including the original `Host`, so backends that
/// build absolute URLs see the public name), drops hop-by-hop headers, and sets
/// the `X-Forwarded-*` trio. For an upgrade request the `Upgrade`/`Connection`
/// machinery is re-added afterwards so the backend will perform the switch.
fn forward_request_headers(incoming: &HeaderMap, client_ip: IpAddr, is_upgrade: bool) -> HeaderMap {
    let tokens = connection_tokens(incoming);
    let mut out = HeaderMap::new();
    for (name, value) in incoming {
        let lower = name.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lower, &tokens) {
            continue;
        }
        // We set these ourselves below; never trust an inbound value verbatim.
        if matches!(lower.as_str(), "x-forwarded-for" | "x-forwarded-proto" | "x-forwarded-host") {
            continue;
        }
        out.append(name.clone(), value.clone());
    }

    out.insert(
        HeaderName::from_static("x-forwarded-for"),
        forwarded_for(incoming, client_ip),
    );
    // breakwater terminates TLS, so from the client's perspective the request
    // was always HTTPS, regardless of the plaintext hop to the backend.
    out.insert(
        HeaderName::from_static("x-forwarded-proto"),
        HeaderValue::from_static("https"),
    );
    if let Some(host) = incoming.get(header::HOST) {
        out.insert(HeaderName::from_static("x-forwarded-host"), host.clone());
    }

    if is_upgrade
        && let Some(upgrade) = incoming.get(header::UPGRADE)
    {
        out.insert(header::UPGRADE, upgrade.clone());
        out.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    }

    out
}

/// Append the client IP to any existing `X-Forwarded-For` chain.
fn forwarded_for(incoming: &HeaderMap, client_ip: IpAddr) -> HeaderValue {
    let mut chain: Vec<String> = incoming
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    chain.push(client_ip.to_string());
    // The chain is IPs and commas — always a valid header value.
    HeaderValue::from_str(&chain.join(", ")).expect("forwarded-for is ascii")
}

/// Copy a response's headers, dropping hop-by-hop ones (and anything its own
/// `Connection` header names). Used for ordinary responses; 101s are mirrored
/// verbatim instead.
fn strip_hop_by_hop(incoming: &HeaderMap) -> HeaderMap {
    let tokens = connection_tokens(incoming);
    let mut out = HeaderMap::new();
    for (name, value) in incoming {
        let lower = name.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lower, &tokens) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

fn empty_body() -> ProxyBody {
    Empty::<Bytes>::new().map_err(box_err).boxed_unsync()
}

/// Log and build the 504 returned when an upstream setup stage exceeds its
/// timeout. `stage` names which step elapsed (connect / handshake / response).
fn upstream_timeout(upstream: &str, stage: &str) -> Response<ProxyBody> {
    eprintln!("breakwater: upstream {stage} timed out after waiting: {upstream}");
    status_page(StatusCode::GATEWAY_TIMEOUT, "upstream timeout")
}

/// A small plain-text status page (404 / 502 / 504).
fn status_page(status: StatusCode, message: &str) -> Response<ProxyBody> {
    let body = Full::new(Bytes::from(format!("{} {}\n", status.as_u16(), message)))
        .map_err(box_err)
        .boxed_unsync();
    let mut out = Response::new(body);
    *out.status_mut() = status;
    out.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    out
}

pub(crate) fn box_err<E: std::error::Error + Send + Sync + 'static>(
    err: E,
) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn ip() -> IpAddr {
        "203.0.113.7".parse().unwrap()
    }

    #[test]
    fn detects_upgrade_request() {
        assert!(is_upgrade_request(&headers(&[
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
        ])));
        // Connection lists upgrade among several tokens.
        assert!(is_upgrade_request(&headers(&[
            ("connection", "keep-alive, Upgrade"),
            ("upgrade", "websocket"),
        ])));
        // Upgrade header without the Connection token is not an upgrade.
        assert!(!is_upgrade_request(&headers(&[("upgrade", "websocket")])));
        assert!(!is_upgrade_request(&headers(&[("connection", "keep-alive")])));
    }

    #[test]
    fn strips_hop_by_hop_and_connection_named_headers() {
        let incoming = headers(&[
            ("host", "lighthouse.intern.deepwa7er.net"),
            ("connection", "keep-alive, X-Custom-Hop"),
            ("keep-alive", "timeout=5"),
            ("x-custom-hop", "secret"),
            ("accept", "text/html"),
        ]);
        let out = forward_request_headers(&incoming, ip(), false);
        assert!(out.get("connection").is_none());
        assert!(out.get("keep-alive").is_none());
        assert!(out.get("x-custom-hop").is_none(), "Connection-named header must be dropped");
        assert_eq!(out.get("accept").unwrap(), "text/html");
        // Host is preserved end-to-end.
        assert_eq!(out.get("host").unwrap(), "lighthouse.intern.deepwa7er.net");
    }

    #[test]
    fn sets_forwarded_trio() {
        let incoming = headers(&[("host", "svc.intern.deepwa7er.net")]);
        let out = forward_request_headers(&incoming, ip(), false);
        assert_eq!(out.get("x-forwarded-for").unwrap(), "203.0.113.7");
        assert_eq!(out.get("x-forwarded-proto").unwrap(), "https");
        assert_eq!(out.get("x-forwarded-host").unwrap(), "svc.intern.deepwa7er.net");
    }

    #[test]
    fn appends_to_existing_forwarded_for_chain() {
        let incoming = headers(&[
            ("host", "svc.intern.deepwa7er.net"),
            ("x-forwarded-for", "198.51.100.1, 198.51.100.2"),
        ]);
        let out = forward_request_headers(&incoming, ip(), false);
        assert_eq!(out.get("x-forwarded-for").unwrap(), "198.51.100.1, 198.51.100.2, 203.0.113.7");
    }

    #[test]
    fn does_not_trust_inbound_forwarded_proto() {
        let incoming = headers(&[
            ("host", "svc.intern.deepwa7er.net"),
            ("x-forwarded-proto", "http"),
        ]);
        let out = forward_request_headers(&incoming, ip(), false);
        // Exactly one value, ours.
        let values: Vec<_> = out.get_all("x-forwarded-proto").iter().collect();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0], "https");
    }

    #[test]
    fn upgrade_request_keeps_upgrade_machinery() {
        let incoming = headers(&[
            ("host", "svc.intern.deepwa7er.net"),
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
        ]);
        let out = forward_request_headers(&incoming, ip(), true);
        assert_eq!(out.get("upgrade").unwrap(), "websocket");
        assert_eq!(out.get("connection").unwrap(), "upgrade");
    }
}
