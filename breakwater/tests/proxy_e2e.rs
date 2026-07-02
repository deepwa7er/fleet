//! End-to-end smoke test: run `proxy::handle` over a plain-TCP listener (the TLS
//! layer is orthogonal and tested separately) and drive a real request through
//! it to a dummy upstream, asserting routing, header surgery, and body delivery.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};

use breakwater::config::RouteTarget;
use breakwater::proxy::{self, Router};

/// A dummy upstream that echoes back the request it received, so the test can
/// assert what breakwater forwarded. It reports, line by line: the original
/// Host, the X-Forwarded-* trio, and whether two headers that should have been
/// stripped (a hop-by-hop one and a Connection-named one) survived.
async fn spawn_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let service = service_fn(|req: Request<Incoming>| async move {
                    let h = req.headers();
                    let get = |name: &str| {
                        h.get(name)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("<none>")
                            .to_string()
                    };
                    let body = format!(
                        "host={}\nxff={}\nxfp={}\nxfh={}\nsaw_keepalive={}\nsaw_custom_hop={}\n",
                        get("host"),
                        get("x-forwarded-for"),
                        get("x-forwarded-proto"),
                        get("x-forwarded-host"),
                        h.contains_key("keep-alive"),
                        h.contains_key("x-custom-hop"),
                    );
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    addr
}

/// Run breakwater's proxy handler over plain TCP and return its address.
async fn spawn_proxy(router: Arc<Router>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, peer) = listener.accept().await.unwrap();
            let router = router.clone();
            tokio::spawn(async move {
                let ip = peer.ip();
                let service = service_fn(move |req: Request<Incoming>| {
                    let router = router.clone();
                    async move { Ok::<_, Infallible>(proxy::handle(req, router, ip).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
                    .await;
            });
        }
    });
    addr
}

/// Issue a `GET /` to `addr` with the given headers and return (status, body).
async fn request(addr: SocketAddr, headers: &[(&str, &str)]) -> (u16, String) {
    request_path(addr, "/", headers).await
}

/// Issue a `GET <path>` to `addr` with the given headers and return (status, body).
async fn request_path(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = Request::builder().uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let req = builder.body(Empty::<Bytes>::new()).unwrap();

    let resp = sender.send_request(req).await.unwrap();
    let status = resp.status().as_u16();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn forwards_to_routed_upstream_and_rewrites_headers() {
    let upstream = spawn_upstream().await;
    let mut table = std::collections::HashMap::new();
    table.insert("test.local".to_string(), RouteTarget::Proxy(upstream.to_string()));
    let proxy_addr = spawn_proxy(Arc::new(Router::new(table))).await;

    let (status, body) = request(
        proxy_addr,
        &[
            ("host", "test.local"),
            // These must NOT reach the upstream.
            ("connection", "keep-alive, X-Custom-Hop"),
            ("keep-alive", "timeout=5"),
            ("x-custom-hop", "leak"),
        ],
    )
    .await;

    assert_eq!(status, 200, "body was: {body}");
    assert!(body.contains("host=test.local"), "Host not preserved: {body}");
    assert!(body.contains("xfp=https"), "X-Forwarded-Proto wrong: {body}");
    assert!(body.contains("xfh=test.local"), "X-Forwarded-Host wrong: {body}");
    assert!(body.contains("xff=127.0.0.1"), "X-Forwarded-For missing client: {body}");
    assert!(body.contains("saw_keepalive=false"), "hop-by-hop header leaked: {body}");
    assert!(body.contains("saw_custom_hop=false"), "Connection-named header leaked: {body}");
}

#[tokio::test]
async fn unrouted_host_yields_404() {
    let proxy_addr = spawn_proxy(Arc::new(Router::new(std::collections::HashMap::new()))).await;
    let (status, _) = request(proxy_addr, &[("host", "nope.local")]).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn static_route_serves_files_and_falls_back_to_index() {
    // A built site: an app shell at the root plus a nested file standing in for
    // a rustdoc page under /doc/<repo>/.
    let dir = temp_site();
    std::fs::write(dir.join("index.html"), "<!doctype html><title>app-shell</title>").unwrap();
    std::fs::create_dir_all(dir.join("doc/ferry")).unwrap();
    std::fs::write(dir.join("doc/ferry/index.html"), "ferry-rustdoc-page").unwrap();

    let mut table = std::collections::HashMap::new();
    table.insert("docs.local".to_string(), RouteTarget::Static(dir.clone()));
    let proxy_addr = spawn_proxy(Arc::new(Router::new(table))).await;

    // A real nested file is served directly (directory index resolution).
    let (status, body) = request_path(proxy_addr, "/doc/ferry/", &[("host", "docs.local")]).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("ferry-rustdoc-page"), "expected the rustdoc file, got: {body}");

    // An unknown path — a SPA client-side route — falls back to index.html.
    let (status, body) = request_path(proxy_addr, "/services/ferry", &[("host", "docs.local")]).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("app-shell"), "expected the SPA shell, got: {body}");

    // Percent-encoded `..` traversal must not escape the root: ServeDir rejects
    // it, so the request lands on the SPA fallback rather than reading Cargo.toml.
    let (_, body) = request_path(proxy_addr, "/%2e%2e/Cargo.toml", &[("host", "docs.local")]).await;
    assert!(!body.contains("[package]"), "path traversal leaked a file outside the root: {body}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Create a unique, empty temp directory for a test's served site.
fn temp_site() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("breakwater-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
