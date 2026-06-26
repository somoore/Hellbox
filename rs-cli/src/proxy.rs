//! Fork B — local loopback reverse-proxy (feature `proxy`, on by default).
//!
//! Browsers cannot set the `X-aws-proxy-auth` header on `WebSocket`/navigation,
//! and the MicroVM endpoint returns 403 unless that header (plus
//! `X-aws-proxy-port`) is present (the JWE in a query string is rejected). So
//! `shrink open` runs this tiny `127.0.0.1` proxy: the browser talks plain
//! `http://127.0.0.1:<port>`, and we inject the auth/port headers and forward to
//! the MicroVM over TLS — both for plain HTTP and for WebSocket (WSS) upgrades
//! (KasmVNC's pixel stream).
//!
//! Design:
//!   * HTTP: a `hyper` 1.x server on loopback. For each request we rebuild it as
//!     an HTTPS request to the upstream, drop hop-by-hop headers, add the two
//!     auth headers, and `reqwest`-forward it; the response is streamed back.
//!   * WebSocket: detect `Upgrade: websocket`. We open the upstream `wss://`
//!     socket with `tokio-tungstenite` (custom handshake request carrying the
//!     auth/port headers + any `Sec-WebSocket-Protocol`), answer the browser's
//!     handshake ourselves, take over the browser connection via
//!     `hyper::upgrade::on`, and pump frames in both directions until either
//!     side closes.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use aws_sdk_lambdamicrovms::Client as MicrovmClient;
use aws_sdk_lambdamicrovms::types::PortSpecification;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::poll::{PollOpts, poll_until};
use crate::state::State;

/// The map key the auth JWE is delivered under (SDK contract; also the header name).
const AUTH_TOKEN_KEY: &str = "X-aws-proxy-auth";
/// Re-minted token validity on resume (minutes; AWS caps at 60).
const TOKEN_TTL_MINUTES: i32 = 30;

/// The two headers the MicroVM endpoint requires (verified live: missing → 403).
/// Used by the tests below to assert injection; the proxy itself writes the
/// lower-cased static names directly.
#[cfg(test)]
const AUTH_HEADER: &str = "x-aws-proxy-auth";
#[cfg(test)]
const PORT_HEADER: &str = "x-aws-proxy-port";

/// Live count of active WebSocket sessions (the "is a viewer connected?" signal).
/// A browser tab holds the display WS (and the audio WS once enabled); when the tab
/// closes, both pumps end and the count returns to 0. `shrink open`'s idle monitor
/// (Lever 4) watches this to auto-suspend the MicroVM after an idle window.
#[derive(Default)]
pub struct ProxyActivity {
    sessions: AtomicUsize,
}

impl ProxyActivity {
    /// Number of WebSocket sessions currently being pumped.
    pub fn active(&self) -> usize {
        self.sessions.load(Ordering::Relaxed)
    }
    fn enter(&self) {
        self.sessions.fetch_add(1, Ordering::Relaxed);
    }
    fn leave(&self) {
        self.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Live, mutable upstream connection params. Both the endpoint host and the auth
/// JWE can change across a suspend/resume (the endpoint may move; the token has a
/// ≤60-min TTL), so they live behind shared locks the forwarding path reads on
/// every request and the control handler rewrites on resume. Never locked across
/// an `.await`.
#[derive(Clone)]
pub struct Upstream {
    host: Arc<RwLock<String>>,
    auth_token: Arc<RwLock<String>>,
}

impl Upstream {
    /// New shared upstream from an initial endpoint host + auth JWE.
    pub fn new(host: String, auth_token: String) -> Self {
        Self {
            host: Arc::new(RwLock::new(host)),
            auth_token: Arc::new(RwLock::new(auth_token)),
        }
    }
    fn host(&self) -> String {
        self.host.read().expect("upstream host lock").clone()
    }
    fn token(&self) -> String {
        self.auth_token.read().expect("upstream token lock").clone()
    }
    /// Swap in a fresh endpoint + token after a resume (host may move, token expires).
    fn set(&self, host: String, auth_token: String) {
        *self.host.write().expect("upstream host lock") = host;
        *self.auth_token.write().expect("upstream token lock") = auth_token;
    }
}

/// What the proxy needs to drive suspend/resume for the browser buttons. Present
/// only when `shrink open` wires it; tests/headless leave it `None` (no control
/// endpoints, no injected panel).
pub struct ProxyControl {
    /// Control-plane client (SigV4) — works even while the VM is frozen.
    pub microvm: MicrovmClient,
    pub microvm_id: String,
    /// Capsule name, for `state.json` bookkeeping.
    pub name: String,
    /// Ports the re-minted auth token must allow (display/audio/video/input).
    pub token_ports: Vec<i32>,
    /// Shared upstream cells to rewrite (host + token) after a resume.
    pub upstream: Upstream,
}

/// Everything the proxy needs to reach the capsule.
#[derive(Clone)]
pub struct ProxyConfig {
    /// Live upstream host + auth token (both mutable across resume).
    pub upstream: Upstream,
    /// Default capsule port carried via `X-aws-proxy-port` (the display port).
    pub upstream_port: i32,
    /// Local loopback port to bind (0 = ephemeral).
    pub local_port: u16,
    /// Path-prefix → internal capsule port routing table. Requests whose path
    /// starts with a prefix are routed to that port via `X-aws-proxy-port` (first
    /// match wins); anything unmatched falls back to `upstream_port`.
    /// (e.g. `[("/shrinkaudio", 6902), ("/shrinkvideo", 6903), ("/shrinkinput", 6904)]`.)
    pub routes: Vec<(String, i32)>,
    /// Shared live-session counter for the idle monitor. `None` disables
    /// activity tracking.
    pub activity: Option<Arc<ProxyActivity>>,
    /// Suspend/resume control wiring for the browser buttons. `None` = disabled.
    pub control: Option<Arc<ProxyControl>>,
}

impl ProxyConfig {
    /// The internal capsule port for a given request path. Returns the port of the
    /// first route whose prefix the path starts with, else `upstream_port`.
    fn port_for(&self, path: &str) -> i32 {
        for (prefix, port) in &self.routes {
            if !prefix.is_empty() && path.starts_with(prefix.as_str()) {
                return *port;
            }
        }
        self.upstream_port
    }
}

/// Bind the loopback listener, spawn the accept loop, and return the
/// `http://127.0.0.1:<port>` URL the caller should open.
pub async fn start(cfg: ProxyConfig) -> Result<String> {
    let addr: SocketAddr = ([127, 0, 0, 1], cfg.local_port).into();
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding loopback proxy on {addr}"))?;
    let local = listener.local_addr()?;
    let url = format!("http://{local}");

    // ponytail: a single shared reqwest client (rustls) for all forwarded HTTP.
    let client = reqwest::Client::builder()
        .build()
        .context("building forward HTTP client")?;
    let cfg = Arc::new(cfg);

    tracing::info!(
        target: "shrink::proxy",
        "Fork B loopback proxy on {local} -> https://{} (port {}, header-injecting)",
        cfg.upstream.host(), cfg.upstream_port
    );

    tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(target: "shrink::proxy", "accept failed: {e:#}");
                    break;
                }
            };
            tracing::debug!(target: "shrink::proxy", "accepted {peer}");
            let cfg = cfg.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let svc = service_fn(move |req| handle(req, cfg.clone(), client.clone()));
                if let Err(e) = hyper::server::conn::http1::Builder::new()
                    // with_upgrades: needed so `hyper::upgrade::on` works for WS.
                    .serve_connection(io, svc)
                    .with_upgrades()
                    .await
                {
                    tracing::debug!(target: "shrink::proxy", "connection closed: {e:#}");
                }
            });
        }
    });

    Ok(url)
}

/// Route each request to the WebSocket or plain-HTTP path.
async fn handle(
    req: Request<Incoming>,
    cfg: Arc<ProxyConfig>,
    client: reqwest::Client,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let result = if req.uri().path().starts_with("/__shrink/") {
        // Local control plane (suspend/resume/state) — never forwarded upstream, so
        // it answers even while the capsule is frozen.
        handle_control(req, cfg).await
    } else if is_websocket_upgrade(req.headers()) {
        handle_ws(req, cfg).await
    } else {
        handle_http(req, cfg, client).await
    };
    Ok(result.unwrap_or_else(|e| {
        tracing::warn!(target: "shrink::proxy", "proxy error: {e:#}");
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Full::new(Bytes::from_static(b"proxy error")))
            .expect("static bad-gateway response")
    }))
}

/// Plain HTTP: rebuild the request against the upstream with the auth headers and
/// forward it via reqwest, streaming the response back.
async fn handle_http(
    req: Request<Incoming>,
    cfg: Arc<ProxyConfig>,
    client: reqwest::Client,
) -> Result<Response<Full<Bytes>>> {
    let (mut parts, body) = req.into_parts();
    let pq = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();
    let is_root = parts.uri.path() == "/";
    let is_get = parts.method == hyper::Method::GET;
    let host = cfg.upstream.host();
    let upstream = format!("https://{host}{pq}");

    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).context("method")?;
    let port = cfg.port_for(parts.uri.path());
    let token = cfg.upstream.token();
    // A root navigation while SUSPENDED would silently auto-resume the VM — AWS thaws a
    // suspended MicroVM on ANY data-plane hit to its endpoint, so merely forwarding a
    // refresh restarts billing. Check state via the CONTROL plane first (that does NOT
    // resume) and serve the local Resume page instead of forwarding, so a refresh keeps
    // the capsule paused until the user explicitly clicks Resume. (If the state call
    // fails, fall through and forward as before.)
    if is_root
        && is_get
        && let Some(ctrl) = &cfg.control
        && let Ok(state) = current_state(ctrl).await
        && state != "RUNNING"
    {
        return Ok(html_response(control_only_page()));
    }
    // Force the root page through as a full 200 so we can inject the panel: a
    // conditional request would let the capsule answer 304 (no body), and the browser
    // would reuse a stale, un-injected cached copy.
    if is_root && is_get && cfg.control.is_some() {
        parts.headers.remove(hyper::header::IF_NONE_MATCH);
        parts.headers.remove(hyper::header::IF_MODIFIED_SINCE);
    }
    let fwd_headers = build_upstream_headers(&parts.headers, &token, port);

    let body_bytes = body
        .collect()
        .await
        .context("reading inbound body")?
        .to_bytes();

    let resp = match client
        .request(method, &upstream)
        .headers(fwd_headers)
        .body(body_bytes)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // Upstream unreachable — usually because the capsule is suspended. For a
            // root-page navigation, serve the local control page (with a Resume button)
            // instead of a bare 502, so a reload-while-frozen can still thaw it.
            if is_root && is_get && cfg.control.is_some() {
                return Ok(html_response(control_only_page()));
            }
            return Err(e).with_context(|| format!("forwarding to {upstream}"));
        }
    };

    let status = resp.status();
    // Capsule not actually serving the page (suspended → platform 5xx, terminated, etc.)
    // on a root navigation: show the local control page (Resume) instead of a raw error.
    if is_root && is_get && cfg.control.is_some() && status.as_u16() >= 500 {
        return Ok(html_response(control_only_page()));
    }
    let upstream_headers = resp.headers().clone();
    let is_html = upstream_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false);
    let bytes = resp.bytes().await.context("reading upstream body")?;

    // Inject the suspend/resume control panel into the page HTML — so the baked image
    // never needs rebuilding to gain the buttons, and any capsule gets them for free.
    let injected = is_root && is_get && is_html && cfg.control.is_some();
    let bytes = if injected {
        inject_panel(&bytes)
    } else {
        bytes
    };

    let mut response = Response::new(Full::new(bytes));
    *response.status_mut() =
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let out = response.headers_mut();
    for (name, value) in upstream_headers.iter() {
        let n = name.as_str();
        if is_hop_by_hop(n) {
            continue;
        }
        // Drop upstream Content-Length: injection changes the body size, and hyper
        // derives the correct length from the `Full<Bytes>` body either way.
        if n.eq_ignore_ascii_case("content-length") {
            continue;
        }
        // For the injected page, drop cache validators/policy so the browser never
        // reuses an un-injected copy or 304s us on the next load.
        if injected
            && (n.eq_ignore_ascii_case("etag")
                || n.eq_ignore_ascii_case("last-modified")
                || n.eq_ignore_ascii_case("cache-control")
                || n.eq_ignore_ascii_case("expires"))
        {
            continue;
        }
        if let (Ok(hn), Ok(hv)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out.append(hn, hv);
        }
    }
    if injected {
        out.insert(
            hyper::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        );
    }
    Ok(response)
}

/// WebSocket: answer the browser handshake locally, dial the upstream `wss://`
/// with the auth headers, and pump frames both ways after both upgrades land.
async fn handle_ws(req: Request<Incoming>, cfg: Arc<ProxyConfig>) -> Result<Response<Full<Bytes>>> {
    // The browser's handshake key → the Sec-WebSocket-Accept we must echo.
    let key = req
        .headers()
        .get("Sec-WebSocket-Key")
        .context("WS upgrade missing Sec-WebSocket-Key")?
        .clone();
    let accept = derive_accept_key(key.as_bytes());
    let subprotocol = req.headers().get("Sec-WebSocket-Protocol").cloned();

    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();

    // Build the upstream WSS handshake request with the auth/port headers.
    let upstream = format!("wss://{}{}", cfg.upstream.host(), path);
    let mut up_req = upstream
        .clone()
        .into_client_request()
        .with_context(|| format!("building upstream WS request for {upstream}"))?;
    {
        let h = up_req.headers_mut();
        h.insert(
            HeaderName::from_static("x-aws-proxy-auth"),
            HeaderValue::from_str(&cfg.upstream.token()).context("auth header")?,
        );
        h.insert(
            HeaderName::from_static("x-aws-proxy-port"),
            HeaderValue::from_str(&cfg.port_for(&path).to_string()).context("port header")?,
        );
        if let Some(sp) = &subprotocol {
            h.insert("Sec-WebSocket-Protocol", sp.clone());
        }
    }

    // Dial the upstream first; if it fails, fail the browser handshake.
    let (upstream_ws, _resp) = tokio_tungstenite::connect_async(up_req)
        .await
        .with_context(|| format!("connecting upstream WSS {upstream}"))?;

    // Take over the browser connection once hyper finishes the 101. Count this as a
    // live session for the whole duration of the pump (Lever 4 idle detection).
    let on_upgrade = hyper::upgrade::on(req);
    let activity = cfg.activity.clone();
    if let Some(a) = &activity {
        a.enter();
    }
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let browser_ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    TokioIo::new(upgraded),
                    Role::Server,
                    None,
                )
                .await;
                pump(browser_ws, upstream_ws).await;
            }
            Err(e) => tracing::warn!(target: "shrink::proxy", "browser upgrade failed: {e:#}"),
        }
        if let Some(a) = &activity {
            a.leave();
        }
    });

    // 101 Switching Protocols back to the browser.
    let mut resp = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header(
            "Sec-WebSocket-Accept",
            HeaderValue::from_str(&accept).context("accept header")?,
        );
    if let Some(sp) = subprotocol {
        resp = resp.header("Sec-WebSocket-Protocol", sp);
    }
    resp.body(Full::new(Bytes::new()))
        .context("building 101 response")
}

/// Copy WS frames between the browser and upstream sockets until either closes.
async fn pump<B, U>(browser: B, upstream: U)
where
    B: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
    U: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    let (mut b_tx, mut b_rx) = browser.split();
    let (mut u_tx, mut u_rx) = upstream.split();

    let b2u = async {
        while let Some(msg) = b_rx.next().await {
            match msg {
                Ok(m) => {
                    if u_tx.send(m).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = u_tx.close().await;
    };
    let u2b = async {
        while let Some(msg) = u_rx.next().await {
            match msg {
                Ok(m) => {
                    if b_tx.send(m).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = b_tx.close().await;
    };

    tokio::select! {
        _ = b2u => {}
        _ = u2b => {}
    }
    tracing::debug!(target: "shrink::proxy", "WS session closed");
}

/// True if the request is a WebSocket upgrade.
fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let has_upgrade = headers
        .get(hyper::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false);
    let is_ws = headers
        .get(hyper::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    has_upgrade && is_ws
}

/// Copy the inbound headers minus hop-by-hop/host, and add the two auth headers.
/// Extracted so the rewrite is unit-testable without a live socket.
fn build_upstream_headers(
    inbound: &HeaderMap,
    auth_token: &str,
    port: i32,
) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for (name, value) in inbound.iter() {
        let n = name.as_str();
        // Drop hop-by-hop and host (reqwest sets Host from the upstream URL).
        if is_hop_by_hop(n) || n.eq_ignore_ascii_case("host") {
            continue;
        }
        if let (Ok(hn), Ok(hv)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out.append(hn, hv);
        }
    }
    // The two headers that make auth work (verified live).
    if let Ok(v) = reqwest::header::HeaderValue::from_str(auth_token) {
        out.insert(
            reqwest::header::HeaderName::from_static("x-aws-proxy-auth"),
            v,
        );
    }
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&port.to_string()) {
        out.insert(
            reqwest::header::HeaderName::from_static("x-aws-proxy-port"),
            v,
        );
    }
    out
}

/// RFC 7230 §6.1 hop-by-hop headers — must not be forwarded end-to-end.
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// True if a `Host` or `Origin` authority points at loopback. Locks the `/__shrink/*`
/// control endpoints to our own served page: a DNS-rebound or cross-origin attacker
/// carries its own hostname/origin in these headers, not `127.0.0.1`. Accepts the
/// `Origin` form (`http://127.0.0.1:6080`) and the `Host` form (`127.0.0.1:6080`,
/// `localhost`, `[::1]:6080`). A suffix trick like `127.0.0.1.evil.com` is rejected
/// (the whole label must equal a loopback name).
fn is_loopback_authority(value: &str) -> bool {
    let v = value.trim();
    let v = v
        .strip_prefix("http://")
        .or_else(|| v.strip_prefix("https://"))
        .unwrap_or(v);
    let v = v.split('/').next().unwrap_or(v); // drop any path the Origin shouldn't have
    let host = if let Some(rest) = v.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest) // [::1]:port -> ::1
    } else {
        v.rsplit_once(':').map(|(h, _)| h).unwrap_or(v) // host:port -> host
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

// `IntoClientRequest` is needed for `.into_client_request()` above.
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

// ----------------------------------------------------------------------------
// Browser control plane: suspend / resume / state + the injected UI panel.
// Suspend/resume are CONTROL-plane calls (SigV4), so they work even while the
// capsule is frozen and unreachable through the data path. Resume re-mints the
// auth JWE and refreshes the (possibly moved) endpoint so the page reconnects.
// ----------------------------------------------------------------------------

/// Wrap an HTML body in a `text/html` response.
fn html_response(body: String) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::from(body)));
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

/// Wrap a JSON body in a response with the given status.
fn json_response(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::from(body)));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

/// Handle the local `/__shrink/*` control endpoints (never forwarded upstream):
///   GET  /__shrink/state    → {"state":"RUNNING"|"SUSPENDED"|…}
///   POST /__shrink/suspend  → suspend + poll SUSPENDED → {"state":…}
///   POST /__shrink/resume   → resume + poll RUNNING, re-mint token + refresh endpoint
async fn handle_control(
    req: Request<Incoming>,
    cfg: Arc<ProxyConfig>,
) -> Result<Response<Full<Bytes>>> {
    let ctrl = match &cfg.control {
        Some(c) => c.clone(),
        None => {
            return Ok(json_response(
                StatusCode::NOT_FOUND,
                r#"{"error":"control disabled"}"#.to_string(),
            ));
        }
    };
    // --- CSRF / DNS-rebinding guard ---------------------------------------------------
    // These endpoints drive the AWS control plane with the user's creds, so they must be
    // reachable ONLY from our own loopback-served page, never from another site or a
    // DNS-rebound page. The proxy binds 127.0.0.1, but the browser will still forward a
    // cross-origin `fetch` (CORS blocks reading the reply, not sending the request), and a
    // rebound page makes the request "same-origin". We defend on the request metadata the
    // browser sets honestly:
    //   * Host must be loopback — a rebound/foreign page carries its own hostname here.
    //   * Origin, if present, must be our loopback origin — a cross-origin fetch carries
    //     the attacker's Origin; our own page sends ours (or none for a same-origin GET).
    let host_ok = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(is_loopback_authority)
        .unwrap_or(false);
    let origin_ok = match req
        .headers()
        .get(hyper::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        Some(o) => is_loopback_authority(o),
        None => true,
    };
    if !host_ok || !origin_ok {
        tracing::warn!(target: "shrink::proxy", "rejected control request (host_ok={host_ok} origin_ok={origin_ok})");
        return Ok(json_response(
            StatusCode::FORBIDDEN,
            r#"{"error":"control endpoints are loopback-only"}"#.to_string(),
        ));
    }

    let method = req.method().clone();
    let action = req
        .uri()
        .path()
        .trim_start_matches("/__shrink/")
        .to_string();

    // Mutating actions require POST: a cross-origin `<img>`/simple GET sends no Origin and
    // a loopback Host, so the checks above would pass it; requiring POST blocks it (a real
    // cross-origin POST always carries an Origin and is rejected above).
    if matches!(action.as_str(), "suspend" | "resume") && method != hyper::Method::POST {
        return Ok(json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            r#"{"error":"use POST"}"#.to_string(),
        ));
    }

    let result: Result<String> = match action.as_str() {
        "state" => current_state(&ctrl).await,
        "suspend" => do_suspend(&ctrl).await,
        "resume" => do_resume(&ctrl).await,
        _ => {
            return Ok(json_response(
                StatusCode::NOT_FOUND,
                r#"{"error":"unknown control action"}"#.to_string(),
            ));
        }
    };

    Ok(match result {
        Ok(state) => json_response(
            StatusCode::OK,
            format!(r#"{{"state":{}}}"#, json_str(&state)),
        ),
        Err(e) => {
            tracing::warn!(target: "shrink::proxy", "control {action} failed: {e:#}");
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(r#"{{"error":{}}}"#, json_str(&format!("{e:#}"))),
            )
        }
    })
}

/// GetMicrovm → current state string.
async fn current_state(ctrl: &ProxyControl) -> Result<String> {
    let out = ctrl
        .microvm
        .get_microvm()
        .microvm_identifier(&ctrl.microvm_id)
        .send()
        .await
        .context("get_microvm")?;
    Ok(out.state().as_str().to_string())
}

/// Suspend the MicroVM and poll until SUSPENDED; record it in state.json.
async fn do_suspend(ctrl: &ProxyControl) -> Result<String> {
    tracing::info!(target: "shrink::proxy", "browser requested suspend of {}", ctrl.microvm_id);
    ctrl.microvm
        .suspend_microvm()
        .microvm_identifier(&ctrl.microvm_id)
        .send()
        .await
        .context("suspend_microvm")?;
    let state = poll_state(ctrl, &["SUSPENDED", "TERMINATED", "FAILED"]).await?;
    record_state(&ctrl.name, &state);
    Ok(state)
}

/// Resume the MicroVM, poll to RUNNING, then refresh the endpoint and re-mint the
/// auth token so the reloaded page can reconnect (old endpoint/token may be stale).
async fn do_resume(ctrl: &ProxyControl) -> Result<String> {
    tracing::info!(target: "shrink::proxy", "browser requested resume of {}", ctrl.microvm_id);
    ctrl.microvm
        .resume_microvm()
        .microvm_identifier(&ctrl.microvm_id)
        .send()
        .await
        .context("resume_microvm")?;
    let state = poll_state(ctrl, &["RUNNING", "TERMINATED", "FAILED"]).await?;
    if state == "RUNNING" {
        let out = ctrl
            .microvm
            .get_microvm()
            .microvm_identifier(&ctrl.microvm_id)
            .send()
            .await
            .context("get_microvm (post-resume)")?;
        let host = host_of(out.endpoint());
        let token = mint_token(ctrl).await?;
        ctrl.upstream.set(host.clone(), token);
        record_endpoint(&ctrl.name, &state, &host);
        tracing::info!(target: "shrink::proxy", "resumed {} — endpoint+token refreshed", ctrl.microvm_id);
    } else {
        record_state(&ctrl.name, &state);
    }
    Ok(state)
}

/// Mint a fresh auth JWE allowing the capsule's display/audio/video/input ports.
async fn mint_token(ctrl: &ProxyControl) -> Result<String> {
    let mut req = ctrl
        .microvm
        .create_microvm_auth_token()
        .microvm_identifier(&ctrl.microvm_id)
        .expiration_in_minutes(TOKEN_TTL_MINUTES);
    for p in &ctrl.token_ports {
        req = req.allowed_ports(PortSpecification::Port(*p));
    }
    let out = req.send().await.context("create_microvm_auth_token")?;
    out.auth_token()
        .get(AUTH_TOKEN_KEY)
        .cloned()
        .with_context(|| format!("auth token response missing '{AUTH_TOKEN_KEY}'"))
}

/// Poll get_microvm until a terminal state (short cadence — suspend/resume are quick).
async fn poll_state(ctrl: &ProxyControl, terminal: &[&str]) -> Result<String> {
    let opts = PollOpts {
        interval: std::time::Duration::from_secs(2),
        timeout: std::time::Duration::from_secs(180),
    };
    let label = format!("microvm {}", ctrl.name);
    poll_until(&label, terminal, opts, || async move {
        current_state(ctrl).await
    })
    .await
}

/// Best-effort: record a new state in state.json.
fn record_state(name: &str, state: &str) {
    if let Ok(mut st) = State::load() {
        let _ = st.upsert(name, |c| c.state = Some(state.to_string()));
    }
}

/// Best-effort: record state + refreshed endpoint in state.json.
fn record_endpoint(name: &str, state: &str, host: &str) {
    if let Ok(mut st) = State::load() {
        let _ = st.upsert(name, |c| {
            c.state = Some(state.to_string());
            c.endpoint = Some(host.to_string());
        });
    }
}

/// Strip scheme/trailing slash from an endpoint, leaving the bare host the proxy dials.
fn host_of(endpoint: &str) -> String {
    endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("wss://")
        .trim_end_matches('/')
        .to_string()
}

/// Minimal JSON string-escaping for the small values we emit (state, error text).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Inject the control-panel markup just before `</body>` (or append if absent).
fn inject_panel(body: &Bytes) -> Bytes {
    match std::str::from_utf8(body) {
        Ok(html) => {
            let injected = if let Some(idx) = html.rfind("</body>") {
                let mut s = String::with_capacity(html.len() + CONTROL_PANEL.len());
                s.push_str(&html[..idx]);
                s.push_str(CONTROL_PANEL);
                s.push_str(&html[idx..]);
                s
            } else {
                format!("{html}{CONTROL_PANEL}")
            };
            Bytes::from(injected)
        }
        // Non-UTF8 (shouldn't happen for an HTML doc) — pass through unchanged.
        Err(_) => body.clone(),
    }
}

/// Self-contained local page served by the proxy when the capsule is not serving
/// the stream (suspended → 5xx, or unreachable). Depends on nothing in the capsule:
/// it polls `/__shrink/state` and offers Resume/Suspend, so there is always
/// "something to click" to thaw the session even with no MicroVM running.
fn control_only_page() -> String {
    CONTROL_ONLY_PAGE.to_string()
}

/// The standalone control page (see `control_only_page`). Plain `const` — not a
/// format string — so the `{`/`#` in CSS/JS need no escaping.
const CONTROL_ONLY_PAGE: &str = r##"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>LambdaDoom — paused</title>
<style>
:root{--bg:#0A0B0D;--text:#ECE8DF;--muted:#8B919B;--muted2:#7E848E;
  --ember:#FF6B1A;--ember2:#FF8A3D;--green:#57C77E;--amber:#FFB020;--hairline:#23272E;
  --font-ui:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,Roboto,sans-serif;
  --font-mono:ui-monospace,"SF Mono","JetBrains Mono",Menlo,Consolas,monospace}
*{box-sizing:border-box}
html,body{margin:0;height:100%}
body{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:20px;
  background:radial-gradient(80% 60% at 50% 42%,rgba(255,107,26,.06) 0%,rgba(0,0,0,0) 60%),var(--bg);
  color:var(--text);font-family:var(--font-ui)}
.brand{position:fixed;top:20px;left:24px;display:flex;align-items:center;gap:12px}
.mark{display:flex;align-items:center;justify-content:center;width:36px;height:30px;border-radius:8px;
  color:#1A0E06;font-weight:900;font-size:15px;letter-spacing:-.04em;
  background:linear-gradient(150deg,var(--ember2),var(--ember) 55%,#D9480F);
  box-shadow:0 0 0 1px rgba(255,138,61,.35),0 4px 14px rgba(255,107,26,.3)}
.word{display:flex;align-items:baseline;gap:1px;font-size:16px;letter-spacing:.01em}
.word .dim{font-weight:600;color:#9CA1AB}.word .strong{font-weight:900;color:var(--text)}
.badge{display:flex;align-items:center;justify-content:center;width:64px;height:64px;border-radius:16px;
  background:#101216;border:1px solid var(--hairline);box-shadow:0 12px 40px rgba(0,0,0,.5)}
.chip{display:flex;align-items:center;gap:9px;height:30px;padding:0 13px;border-radius:8px;
  background:#0E1014;border:1px solid #20242B}
#dot{width:8px;height:8px;border-radius:50%;background:var(--amber)}
#status{font-family:var(--font-mono);font-weight:500;font-size:12px;color:#9AA0AA}
#head{margin:0;font-weight:700;font-size:26px;letter-spacing:-.01em}
#sub{margin:0;max-width:440px;text-align:center;color:var(--muted);font-size:14px;line-height:20px}
#btn{display:flex;align-items:center;gap:9px;height:46px;padding:0 24px;border:0;border-radius:11px;
  font-family:var(--font-ui);font-weight:700;font-size:15px;letter-spacing:.01em;color:#1A0E06;cursor:pointer;
  background:linear-gradient(180deg,var(--ember2),var(--ember));
  box-shadow:0 0 0 1px rgba(255,138,61,.4),0 10px 28px rgba(255,107,26,.36)}
#btn:disabled{opacity:.55;cursor:default;box-shadow:none}
.note{display:flex;align-items:center;gap:8px;font-family:var(--font-mono);font-size:12px;color:var(--muted2)}
</style></head>
<body>
  <div class="brand"><div class="mark">&#955;D</div><div class="word"><span class="dim">LAMBDA</span><span class="strong">DOOM</span></div></div>
  <div class="badge"><svg width="26" height="26" viewBox="0 0 24 24" fill="#FF6B1A"><rect x="6" y="5" width="4" height="14" rx="1.2"/><rect x="14" y="5" width="4" height="14" rx="1.2"/></svg></div>
  <div class="chip"><span id="dot"></span><b id="status">Checking&#8230;</b></div>
  <h2 id="head">Session suspended</h2>
  <p id="sub">Your microVM is frozen and compute billing has stopped.</p>
  <button id="btn" disabled>&#8230;</button>
  <div class="note"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="#7E848E" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 7v5l3 2"/><circle cx="12" cy="12" r="9"/></svg>Back on the exact frame in ~2.6s</div>
<script>
var dot=document.getElementById('dot'),status=document.getElementById('status'),
    head=document.getElementById('head'),sub=document.getElementById('sub'),
    btn=document.getElementById('btn'),busy=false,cur='';
function paint(s){
  cur=s;
  if(s==='RUNNING'){dot.style.background='#57C77E';status.textContent='running';
    head.textContent='Session running';
    sub.textContent='The stream should be live. Reload the tab if it does not appear.';
    btn.textContent='Suspend session';btn.disabled=busy;}
  else if(s==='SUSPENDED'){dot.style.background='#FFB020';
    status.textContent='suspended · billing paused';head.textContent='Session suspended';
    sub.textContent='Your microVM is frozen and compute billing has stopped.';
    btn.textContent='Resume game';btn.disabled=busy;}
  else{dot.style.background='#888';status.textContent=s||'…';btn.textContent='…';btn.disabled=true;}
}
function poll(){if(busy)return;
  fetch('/__shrink/state').then(function(r){return r.json();})
    .then(function(j){if(j.state)paint(j.state);})
    .catch(function(){status.textContent='proxy offline';dot.style.background='#888';});}
btn.onclick=function(){
  if(busy)return;
  var act=cur==='RUNNING'?'suspend':cur==='SUSPENDED'?'resume':null;if(!act)return;
  busy=true;btn.disabled=true;dot.style.background='#58a6ff';
  status.textContent=act==='suspend'?'suspending…':'resuming…';
  fetch('/__shrink/'+act,{method:'POST'}).then(function(r){return r.json();})
    .then(function(j){busy=false;
      if(act==='resume'&&j.state==='RUNNING'){status.textContent='resumed · loading…';
        setTimeout(function(){var u=new URL(location.href);u.searchParams.set('resumed','1');location.href=u.toString();},700);return;}
      if(j.state)paint(j.state);else status.textContent=j.error||'error';})
    .catch(function(){busy=false;status.textContent='error';});
};
poll();setInterval(poll,3000);
</script></body></html>"##;

/// The floating Suspend/Resume control panel injected into the capsule page. Pure
/// vanilla JS talking to the proxy-local `/__shrink/*` endpoints. (Plain `const` —
/// not a format string — so the `{`/`#` in the CSS/JS need no escaping.)
const CONTROL_PANEL: &str = r##"
<div id="shrink-ctl" style="position:fixed;bottom:16px;right:16px;z-index:2147483647;
  font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',system-ui,sans-serif;color:#ECE8DF;
  background:rgba(9,10,13,.74);border:1px solid #1F232A;border-radius:12px;padding:8px 8px 8px 14px;
  display:flex;gap:12px;align-items:center;box-shadow:0 12px 34px rgba(0,0,0,.5)">
  <span id="shrink-dot" style="width:8px;height:8px;border-radius:50%;flex-shrink:0;
    background:#888;display:inline-block"></span>
  <span id="shrink-status" style="font-size:13px;font-weight:600;letter-spacing:.01em;white-space:nowrap">…</span>
  <button id="shrink-btn" style="font-family:inherit;font-size:13px;font-weight:600;
    padding:0 14px;height:34px;cursor:pointer;border-radius:8px;border:1px solid #2C313A;
    background:#15181D;color:#ECE8DF;white-space:nowrap" disabled>…</button>
</div>
<script>
(function(){
  var dot=document.getElementById('shrink-dot');
  var st=document.getElementById('shrink-status');
  var btn=document.getElementById('shrink-btn');
  var busy=false, cur='';
  function paint(state){
    cur=state;
    var map={RUNNING:['#57C77E','Running','Suspend'],
             SUSPENDED:['#FFB020','Suspended','Resume']};
    var m=map[state];
    if(m){dot.style.background=m[0];dot.style.boxShadow='0 0 7px '+m[0];
      st.textContent=m[1];btn.textContent=m[2];btn.disabled=busy;}
    else{dot.style.background='#888';dot.style.boxShadow='none';st.textContent=state||'…';btn.textContent='…';btn.disabled=true;}
  }
  function poll(){
    if(busy)return;
    fetch('/__shrink/state').then(function(r){return r.json();})
      .then(function(j){if(j.state)paint(j.state);}).catch(function(){});
  }
  btn.onclick=function(){
    if(busy)return;
    var act=cur==='RUNNING'?'suspend':cur==='SUSPENDED'?'resume':null;
    if(!act)return;
    busy=true;btn.disabled=true;dot.style.background='#58a6ff';
    st.textContent=act==='suspend'?'Suspending…':'Resuming…';
    fetch('/__shrink/'+act,{method:'POST'}).then(function(r){return r.json();})
      .then(function(j){
        busy=false;
        if(act==='resume'&&j.state==='RUNNING'){
          st.textContent='Reconnecting…';
          setTimeout(function(){var u=new URL(location.href);u.searchParams.set('resumed','1');location.href=u.toString();},600);return;
        }
        if(j.state)paint(j.state);else st.textContent=j.error||'error';
      })
      .catch(function(){busy=false;st.textContent='error';poll();});
  };
  poll();setInterval(poll,4000);
})();
</script>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ProxyConfig {
        ProxyConfig {
            upstream: Upstream::new(
                "abc.lambda-microvm.us-east-2.on.aws".into(),
                "the.secret.jwe".into(),
            ),
            upstream_port: 6901,
            local_port: 0,
            routes: vec![
                ("/shrinkaudio".into(), 6902),
                ("/shrinkvideo".into(), 6903),
                ("/shrinkinput".into(), 6904),
            ],
            activity: None,
            control: None,
        }
    }

    #[test]
    fn inject_panel_splices_before_body() {
        let html = Bytes::from("<html><body><h1>hi</h1></body></html>");
        let out = inject_panel(&html);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("shrink-ctl"), "panel markup injected");
        // Panel sits after the original content and before the closing body tag.
        let panel_at = s.find("shrink-ctl").unwrap();
        let body_at = s.find("</body>").unwrap();
        let h1_at = s.find("<h1>hi</h1>").unwrap();
        assert!(
            h1_at < panel_at && panel_at < body_at,
            "panel between content and </body>"
        );
    }

    #[test]
    fn inject_panel_appends_when_no_body_tag() {
        let html = Bytes::from("<div>no body tag</div>");
        let out = inject_panel(&html);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            s.starts_with("<div>no body tag</div>"),
            "original content kept"
        );
        assert!(s.contains("shrink-ctl"), "panel appended when no </body>");
    }

    #[test]
    fn injected_panel_drives_control_endpoints() {
        // The panel must talk to the proxy-local control plane and offer both actions.
        assert!(CONTROL_PANEL.contains("/__shrink/state"), "polls state");
        assert!(CONTROL_PANEL.contains("method:'POST'"), "POSTs the action");
        assert!(CONTROL_PANEL.contains("'Suspend'"), "offers Suspend");
        assert!(CONTROL_PANEL.contains("'Resume'"), "offers Resume");
    }

    #[test]
    fn control_only_page_offers_resume() {
        // The standalone page (served when the capsule is frozen) must stand on its own:
        // poll state, offer Resume, and reload to reconnect once running.
        let page = control_only_page();
        assert!(page.contains("Resume game"), "has a Resume control");
        assert!(page.contains("/__shrink/state"), "polls live state");
        assert!(
            page.contains("resumed"),
            "reconnects after resume (reloads with ?resumed=1)"
        );
    }

    #[test]
    fn json_str_escapes_special_chars() {
        assert_eq!(json_str("RUNNING"), "\"RUNNING\"");
        assert_eq!(json_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_str("line\nbreak"), "\"line\\nbreak\"");
    }

    #[test]
    fn host_of_strips_scheme_and_slash() {
        assert_eq!(
            host_of("https://x.lambda-microvm.us-east-2.on.aws/"),
            "x.lambda-microvm.us-east-2.on.aws"
        );
        assert_eq!(host_of("wss://h/"), "h");
        assert_eq!(host_of("bare.host"), "bare.host");
    }

    #[test]
    fn routes_audio_path_to_audio_port() {
        let c = cfg();
        assert_eq!(c.port_for("/shrinkaudio"), 6902);
        assert_eq!(c.port_for("/shrinkaudio?x=1"), 6902);
        assert_eq!(c.port_for("/"), 6901);
        assert_eq!(c.port_for("/websockify"), 6901);
        assert_eq!(c.port_for("/vnc.html"), 6901);
    }

    #[test]
    fn routes_video_and_input_paths() {
        let c = cfg();
        assert_eq!(c.port_for("/shrinkvideo"), 6903);
        assert_eq!(c.port_for("/shrinkvideo?x=1"), 6903);
        assert_eq!(c.port_for("/shrinkinput"), 6904);
        assert_eq!(c.port_for("/shrinkinput/ev"), 6904);
        // Fallback to the display port.
        assert_eq!(c.port_for("/"), 6901);
    }

    #[test]
    fn injects_auth_and_port_headers() {
        let mut inbound = HeaderMap::new();
        inbound.insert("host", HeaderValue::from_static("127.0.0.1:6080"));
        inbound.insert("user-agent", HeaderValue::from_static("test"));
        let out = build_upstream_headers(&inbound, "the.secret.jwe", 6901);

        assert_eq!(out.get(AUTH_HEADER).unwrap(), "the.secret.jwe");
        assert_eq!(out.get(PORT_HEADER).unwrap(), "6901");
        // User-Agent is copied through.
        assert_eq!(out.get("user-agent").unwrap(), "test");
    }

    #[test]
    fn strips_hop_by_hop_and_host() {
        let mut inbound = HeaderMap::new();
        inbound.insert("host", HeaderValue::from_static("127.0.0.1:6080"));
        inbound.insert("connection", HeaderValue::from_static("keep-alive"));
        inbound.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        inbound.insert("upgrade", HeaderValue::from_static("h2c"));
        let out = build_upstream_headers(&inbound, "the.secret.jwe", 6901);

        assert!(out.get("host").is_none(), "host must be dropped");
        assert!(out.get("connection").is_none());
        assert!(out.get("keep-alive").is_none());
        assert!(out.get("upgrade").is_none());
    }

    #[test]
    fn loopback_authority_accepts_local_rejects_foreign() {
        // Host forms.
        assert!(is_loopback_authority("127.0.0.1:6080"));
        assert!(is_loopback_authority("127.0.0.1"));
        assert!(is_loopback_authority("localhost:6080"));
        assert!(is_loopback_authority("[::1]:6080"));
        // Origin forms.
        assert!(is_loopback_authority("http://127.0.0.1:6080"));
        assert!(is_loopback_authority("http://localhost:6080"));
        assert!(is_loopback_authority("http://[::1]:6080"));
        // Foreign authorities (RFC 2606 reserved `.example` TLD — illustrative stand-ins
        // for an attacker domain; this is pure string parsing, no network I/O).
        assert!(!is_loopback_authority("foreign.example"));
        assert!(!is_loopback_authority("http://foreign.example"));
        assert!(!is_loopback_authority("http://foreign.example:6080"));
        // DNS-rebinding suffix trick must not slip through: a naive starts_with/contains
        // check on "127.0.0.1" would be fooled; we compare the whole host label.
        assert!(!is_loopback_authority("127.0.0.1.foreign.example"));
        assert!(!is_loopback_authority(
            "http://127.0.0.1.foreign.example:6080"
        ));
    }

    #[test]
    fn hop_by_hop_classification() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("Transfer-Encoding"));
        assert!(is_hop_by_hop("upgrade"));
        assert!(!is_hop_by_hop("content-type"));
        assert!(!is_hop_by_hop("x-aws-proxy-auth"));
    }

    #[test]
    fn detects_websocket_upgrade() {
        let mut h = HeaderMap::new();
        h.insert("connection", HeaderValue::from_static("Upgrade"));
        h.insert("upgrade", HeaderValue::from_static("websocket"));
        assert!(is_websocket_upgrade(&h));

        let mut plain = HeaderMap::new();
        plain.insert("connection", HeaderValue::from_static("keep-alive"));
        assert!(!is_websocket_upgrade(&plain));
    }
}
