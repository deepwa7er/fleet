//! Static-directory routes: serve a built site (e.g. the fleet docs) straight
//! from disk instead of proxying an upstream.
//!
//! Path safety, MIME types, and conditional/range requests come from
//! `tower-http`'s [`ServeDir`]; the only behaviour layered on top is the
//! single-page-app fallback — any path that doesn't resolve to a file returns
//! `index.html`, so a SPA's client-side routes load the app shell. The response
//! body is boxed into the proxy's uniform [`ProxyBody`] so a connection can
//! return either a proxied or a served response.

use std::path::Path;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::proxy::{ProxyBody, box_err};

/// Serve `req` from `root`. Real files — assets and the rustdoc trees under
/// `/doc/<repo>/…` — are returned directly; anything that doesn't resolve to a
/// file falls back to `root/index.html`. `ServeDir` rejects `..` traversal, so
/// `root` is a hard boundary on what can be read.
pub async fn serve(req: Request<Incoming>, root: &Path) -> Response<ProxyBody> {
    let service = ServeDir::new(root).fallback(ServeFile::new(root.join("index.html")));
    match service.oneshot(req).await {
        Ok(response) => response.map(|body| body.map_err(box_err).boxed_unsync()),
        // `ServeDir` turns every IO error into an HTTP response (404 for a
        // missing file, 500 otherwise), so its service error is `Infallible` —
        // this arm is unreachable and exists only to discharge the `Result`.
        Err(infallible) => match infallible {},
    }
}
