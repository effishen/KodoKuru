//! HTTP(S) server: login/unlock, the JSON API, the live WebSocket and the
//! embedded web UI.

mod api;
mod auth;
pub mod tls;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::Router;
use kk_agent::Runtime;
use std::net::SocketAddr;
use std::sync::Arc;

pub use auth::Sessions;

#[derive(Clone)]
pub struct AppState {
    pub rt: Arc<Runtime>,
    pub sessions: Arc<Sessions>,
    pub tls: bool,
}

#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

pub struct ServeOpts {
    pub addr: SocketAddr,
    pub tls: bool,
    /// One-time code required to create the vault on first run.
    pub setup_code: Option<String>,
}

pub fn router(state: AppState) -> Router {
    use api::*;
    let api = Router::new()
        .route("/state", get(get_state))
        .route("/setup", post(setup))
        .route("/login", post(login))
        .route("/recover", post(recover))
        .route("/logout", post(logout))
        .route("/lock", post(lock))
        .route("/password", post(change_password))
        .route("/recovery-key", post(rotate_recovery))
        .route("/engines", get(get_engines).put(put_engines))
        .route("/engines/check", post(check_engines))
        .route("/secrets", get(list_secrets))
        .route("/secrets/{name}", put(put_secret).delete(delete_secret))
        .route("/roles", get(list_roles))
        .route("/roles/{id}", put(put_role).delete(delete_role))
        .route("/templates", get(list_templates))
        .route("/doctor", get(doctor))
        .route("/workspaces", get(list_workspaces).post(create_workspace))
        .route("/workspaces/{id}", get(get_workspace))
        .route("/workspaces/{id}/settings", put(put_settings))
        .route("/workspaces/{id}/events", get(get_events))
        .route("/workspaces/{id}/messages", post(post_message))
        .route("/workspaces/{id}/agents", post(spawn_agent))
        .route("/workspaces/{id}/agents/{aid}/stop", post(stop_agent))
        .route("/workspaces/{id}/approvals/{apid}", post(resolve_approval))
        .route("/workspaces/{id}/files", get(get_files))
        .route("/workspaces/{id}/download", get(download))
        .route("/workspaces/{id}/state", get(get_world_state))
        .route("/workspaces/{id}/envs/{name}/{action}", post(env_action))
        .route("/workspaces/{id}/prepare-migration", post(prepare_migration))
        .route("/ws", get(ws_handler));
    Router::new()
        .nest("/api", api)
        .fallback(static_handler)
        .with_state(state)
        .layer(axum::middleware::map_response(security_headers))
}

async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    res
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let (file, name) = match Assets::get(path) {
        Some(f) => (f, path.to_string()),
        // Single-page app: unknown routes get index.html.
        None if !path.starts_with("api/") && !path.contains('.') => match Assets::get("index.html") {
            Some(f) => (f, "index.html".into()),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let mime = mime_guess::from_path(&name).first_or_octet_stream();
    let cache = if name.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    ([(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.to_string())], file.data).into_response()
}

async fn ws_handler(State(st): State<AppState>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if st.sessions.check(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // Refuse cross-site WebSocket connections.
    if let (Some(origin), Some(host)) = (headers.get(header::ORIGIN), headers.get(header::HOST)) {
        let o = origin.to_str().unwrap_or("");
        let h = host.to_str().unwrap_or("");
        if !o.ends_with(&format!("://{h}")) {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    ws.on_upgrade(move |socket| live(socket, st))
}

async fn live(mut socket: WebSocket, st: AppState) {
    let mut rx = st.rt.live.subscribe();
    let hello = serde_json::json!({"live": "hello", "locked": st.rt.is_locked().await, "health": st.rt.health().await});
    if socket.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(m) => {
                    let Ok(text) = serde_json::to_string(&m) else { continue };
                    if socket.send(Message::Text(text.into())).await.is_err() { break; }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // The UI re-fetches events after this.
                    if socket.send(Message::Text(r#"{"live":"resync"}"#.into())).await.is_err() { break; }
                }
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}

/// Serve until SIGINT/SIGTERM, then stop agent runs so they resume next time.
pub async fn serve(rt: Arc<Runtime>, opts: ServeOpts) -> Result<()> {
    let sessions = Arc::new(Sessions::new(opts.setup_code));
    let state = AppState { rt: rt.clone(), sessions, tls: opts.tls };
    let app = router(state).into_make_service();
    let handle = axum_server::Handle::new();
    let h2 = handle.clone();
    let rt2 = rt.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutting down: pausing agent runs (they resume on next start)");
        rt2.shutdown().await;
        h2.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    });
    if opts.tls {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certs = tls::ensure(&rt.home.tls_dir())?;
        tracing::info!("TLS certificate SHA-256 fingerprint: {}", certs.fingerprint);
        let cfg = axum_server::tls_rustls::RustlsConfig::from_pem_file(&certs.cert, &certs.key).await?;
        axum_server::bind_rustls(opts.addr, cfg).handle(handle).serve(app).await?;
    } else {
        axum_server::bind(opts.addr).handle(handle).serve(app).await?;
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}
