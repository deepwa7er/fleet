//! breakwater — a tailnet reverse proxy.
//!
//! Terminates TLS on the Tailscale interface and routes HTTPS requests to local
//! services by hostname (`<name>.intern.deepwa7er.net` → `127.0.0.1:<port>`),
//! so services are reached by name over HTTPS instead of by raw port. Reachable
//! only from the tailnet, which is the security boundary.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{HOST, LOCATION};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use breakwater::access::{self, Recorder};
use breakwater::cert::{self, CertMaterial};
use breakwater::config::Config;
use breakwater::proxy::{self, Router};
use breakwater::public_ip;
use breakwater::tailscale;
use breakwater::tls::{self, CertResolver};

const DEFAULT_CONFIG_PATH: &str = "/etc/breakwater/breakwater.toml";

/// How long to wait for tailscaled to assign the node's tailnet address before
/// giving up at startup (systemd then restarts us via `Restart=on-failure`).
const TAILSCALE_RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
const PUBLIC_IP_RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Diagnostic: resolve and print the host's Tailscale IPv4, then exit. Lets a
    // deploy confirm the resolver finds the address on a host before relying on
    // it to bind the front door. Runs before any config/cert work.
    if std::env::args().nth(1).as_deref() == Some("--check-tailnet-ip") {
        let ip = tailscale::resolve(TAILSCALE_RESOLVE_TIMEOUT).await?;
        println!("{ip}");
        return Ok(());
    }

    // rustls needs a process-wide crypto provider. We compile with ring only
    // (see Cargo.toml), so install it explicitly rather than relying on a
    // default that isn't wired up when aws-lc-rs is disabled.
    tokio_rustls::rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;

    let config_path = config_path_from_args()?;
    let config = Config::load(&config_path)?;

    // Static-cert mode loads from disk; ACME mode obtains/renews automatically.
    // Config validation guarantees exactly one of the two is set.
    let resolver = match (config.tls.as_ref(), config.acme.as_ref()) {
        (Some(tls), None) => {
            let material = CertMaterial::from_files(&tls.cert, &tls.key)?;
            Arc::new(CertResolver::new(&material)?)
        }
        (None, Some(acme)) => cert::start_acme(acme.clone()).await?,
        _ => unreachable!("config validation guarantees exactly one cert mode"),
    };
    let acceptor = tls::acceptor(resolver);
    let router = Arc::new(Router::new(config.routing_table()));

    // Optional public listener gets its own TLS material and routing table —
    // completely disjoint from the tailnet ones.
    let (public_acceptor, public_router) = if config.has_public_listener() {
        let resolver = match (config.public_tls.as_ref(), config.public_acme.as_ref()) {
            (Some(tls), None) => {
                let material = CertMaterial::from_files(&tls.cert, &tls.key)?;
                Arc::new(CertResolver::new(&material)?)
            }
            (None, Some(acme)) => cert::start_acme(acme.clone()).await?,
            _ => unreachable!("public config validation guarantees exactly one cert mode"),
        };
        let acceptor = tls::acceptor(resolver);
        let router = Arc::new(Router::new(config.public_routing_table()));
        (Some(acceptor), Some(router))
    } else {
        (None, None)
    };

    // The tailnet-facing listeners bind the host's Tailscale IP, discovered at
    // startup so the config holds no host-specific address. tailscaled may still
    // be assigning that address as we come up, so allow a short grace period.
    let tailnet_ip = tailscale::resolve(TAILSCALE_RESOLVE_TIMEOUT).await?;
    let https_addr = SocketAddr::new(IpAddr::V4(tailnet_ip), config.https_port);
    let redirect_addr = config
        .http_redirect_port
        .map(|port| SocketAddr::new(IpAddr::V4(tailnet_ip), port));
    println!("breakwater: tailnet address {tailnet_ip}");

    // Public IP discovery only when the public listener is configured.
    let (public_https_addr, public_redirect_addr) = if let Some(port) = config.public_https_port {
        let public_ip = public_ip::resolve(PUBLIC_IP_RESOLVE_TIMEOUT).await?;
        let https = SocketAddr::new(IpAddr::V4(public_ip), port);
        let redirect = config
            .public_http_redirect_port
            .map(|p| SocketAddr::new(IpAddr::V4(public_ip), p));
        println!("breakwater: public address {public_ip}");
        (Some(https), redirect)
    } else {
        (None, None)
    };

    // One access-log writer task for the whole process; every connection records
    // through a clone of the returned handle.
    let recorder = access::spawn();

    // The TLS proxies are mandatory where configured; the redirects and health
    // are optional and degrade to never-resolving futures when not configured.
    let https = serve_https(https_addr, acceptor, router, recorder.clone());
    let public_https = match (public_https_addr, public_acceptor, public_router) {
        (Some(addr), Some(acc), Some(rtr)) => {
            // Box to unify the future type with the pending case.
            Box::pin(serve_https(addr, acc, rtr, recorder.clone())) as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
        }
        _ => Box::pin(std::future::pending()) as _,
    };
    let redirect = optional(redirect_addr, serve_redirect);
    let public_redirect = optional(public_redirect_addr, serve_redirect);
    let health = optional(config.health_addr, serve_health);

    tokio::try_join!(https, public_https, redirect, public_redirect, health)?;
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
async fn serve_https(addr: SocketAddr, acceptor: TlsAcceptor, router: Arc<Router>, recorder: Recorder) -> anyhow::Result<()> {
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
        let recorder = recorder.clone();
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
                let recorder = recorder.clone();
                async move { Ok::<_, Infallible>(proxy::handle(req, router, recorder, client_ip).await) }
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
