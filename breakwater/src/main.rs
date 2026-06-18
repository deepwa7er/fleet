//! breakwater — a tailnet reverse proxy.
//!
//! Terminates TLS on the Tailscale interface and routes HTTPS requests to local
//! services by hostname (`<name>.internal.deepwa7er.com` → `127.0.0.1:<port>`),
//! so services are reached by name over HTTPS instead of by raw port. Reachable
//! only from the tailnet, which is the security boundary.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};
use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{HOST, LOCATION};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use breakwater::config::Config;
use breakwater::proxy::{self, Router};
use breakwater::tls;

const DEFAULT_CONFIG_PATH: &str = "/etc/breakwater/breakwater.toml";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // rustls needs a process-wide crypto provider. We compile with ring only
    // (see Cargo.toml), so install it explicitly rather than relying on a
    // default that isn't wired up when aws-lc-rs is disabled.
    tokio_rustls::rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;

    let config_path = config_path_from_args()?;
    let config = Config::load(&config_path)?;

    let acceptor = tls::acceptor(&config.tls)?;
    let router = Arc::new(Router::new(config.routing_table()));

    // The TLS proxy is mandatory; the redirect and health listeners are optional
    // and degrade to a never-resolving future when not configured. Any bind
    // failure surfaces immediately and stops the process.
    let https = serve_https(config.https_addr, acceptor, router);
    let redirect = optional(config.http_redirect_addr, serve_redirect);
    let health = optional(config.health_addr, serve_health);

    tokio::try_join!(https, redirect, health)?;
    Ok(())
}

/// The config path is the single optional positional argument; default otherwise.
fn config_path_from_args() -> anyhow::Result<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (None, _) => Ok(PathBuf::from(DEFAULT_CONFIG_PATH)),
        (Some(path), None) => Ok(PathBuf::from(path)),
        (Some(_), Some(_)) => bail!("usage: breakwater [config-path]"),
    }
}

/// Run `serve` on `addr` if present, otherwise a future that never completes —
/// so an unconfigured listener simply doesn't participate in the `try_join`.
async fn optional<F, Fut>(addr: Option<SocketAddr>, serve: F) -> anyhow::Result<()>
where
    F: FnOnce(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    match addr {
        Some(addr) => serve(addr).await,
        None => std::future::pending().await,
    }
}

/// Accept TLS connections and proxy each one. Per-connection failures (a TLS
/// handshake that aborts, a client that hangs up) are logged and dropped; only
/// a failure to accept at all propagates.
async fn serve_https(addr: SocketAddr, acceptor: TlsAcceptor, router: Arc<Router>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind https listener on {addr}"))?;
    println!("breakwater: https proxy on {addr}");

    loop {
        let (stream, peer) = listener
            .accept()
            .await
            .with_context(|| format!("accept failed on {addr}"))?;
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let tls = match acceptor.accept(stream).await {
                Ok(tls) => tls,
                Err(err) => {
                    eprintln!("breakwater: tls handshake from {peer} failed: {err}");
                    return;
                }
            };
            let client_ip = peer.ip();
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let router = router.clone();
                async move { Ok::<_, Infallible>(proxy::handle(req, router, client_ip).await) }
            });
            // `with_upgrades` lets WebSocket / other Upgrade tunnels hand off the
            // raw connection after the 101.
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(tls), service)
                .with_upgrades()
                .await
            {
                eprintln!("breakwater: connection from {peer} ended: {err}");
            }
        });
    }
}

/// Plain-HTTP listener that 308-redirects every request to its HTTPS equivalent
/// on the same host and path.
async fn serve_redirect(addr: SocketAddr) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind redirect listener on {addr}"))?;
    println!("breakwater: http→https redirect on {addr}");

    loop {
        let (stream, _peer) = listener
            .accept()
            .await
            .with_context(|| format!("accept failed on {addr}"))?;
        tokio::spawn(async move {
            let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                Ok::<_, Infallible>(redirect_to_https(&req))
            });
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                eprintln!("breakwater: redirect connection ended: {err}");
            }
        });
    }
}

fn redirect_to_https(req: &Request<hyper::body::Incoming>) -> Response<Full<Bytes>> {
    let host = req.headers().get(HOST).and_then(|v| v.to_str().ok());
    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
    match host {
        Some(host) => {
            let location = format!("https://{host}{path}");
            let mut response = Response::new(Full::new(Bytes::new()));
            *response.status_mut() = StatusCode::PERMANENT_REDIRECT;
            if let Ok(value) = location.parse() {
                response.headers_mut().insert(LOCATION, value);
            }
            response
        }
        None => {
            let mut response = Response::new(Full::new(Bytes::from_static(b"400 missing host\n")));
            *response.status_mut() = StatusCode::BAD_REQUEST;
            response
        }
    }
}

/// Loopback health endpoint for tugboat's deploy health check.
async fn serve_health(addr: SocketAddr) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind health listener on {addr}"))?;
    println!("breakwater: health endpoint on {addr}");

    loop {
        let (stream, _peer) = listener
            .accept()
            .await
            .with_context(|| format!("accept failed on {addr}"))?;
        tokio::spawn(async move {
            let service = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok\n"))))
            });
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                eprintln!("breakwater: health connection ended: {err}");
            }
        });
    }
}
