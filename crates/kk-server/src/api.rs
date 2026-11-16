//! JSON API handlers.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use kk_agent::Workspace;
use kk_core::*;
use kk_home::vault::{KdfParams, UnlockError, UnlockedVault};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::auth::{clear_cookie, set_cookie, Authed};
use crate::AppState;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::BAD_REQUEST, format!("{e:#}"))
    }
}

type R<T = Json<Value>> = Result<T, ApiError>;

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(code, msg.into())
}

async fn ws(st: &AppState, id: &str) -> Result<Arc<Workspace>, ApiError> {
    st.rt.workspace(id).await.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such workspace"))
}

fn logged_in(st: &AppState, token: String, extra: Value) -> Response {
    let mut res = Json(extra).into_response();
    res.headers_mut().insert(header::SET_COOKIE, set_cookie(&token, st.tls).parse().unwrap());
    let _ = st;
    res
}

// ------------------------------------------------------------------ auth

pub async fn get_state(State(st): State<AppState>, headers: HeaderMap) -> Json<Value> {
    Json(json!({
        "version": VERSION,
        "setup_required": !st.rt.vault_exists(),
        "locked": st.rt.is_locked().await,
        "authenticated": st.sessions.check(&headers).is_some(),
        "home": st.rt.home.root,
        "machine": st.rt.home.machine.hostname,
    }))
}

#[derive(Deserialize)]
pub struct SetupReq {
    code: String,
    password: String,
}

pub async fn setup(State(st): State<AppState>, Json(r): Json<SetupReq>) -> Result<Response, ApiError> {
    if st.rt.vault_exists() {
        return Err(err(StatusCode::CONFLICT, "already set up"));
    }
    kk_home::vault::validate_password(&r.password)?;
    if !st.sessions.take_setup_code(&r.code) {
        return Err(err(StatusCode::FORBIDDEN, "wrong setup code (see the terminal where KodoKuru was started)"));
    }
    let path = st.rt.home.vault_path();
    let (v, recovery) = tokio::task::spawn_blocking(move || UnlockedVault::create(&path, &r.password, KdfParams::DEFAULT))
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    st.rt.unlock(v).await;
    let _ = std::fs::remove_file(st.rt.home.run_dir().join("setup-code"));
    let token = st.sessions.create();
    Ok(logged_in(&st, token, json!({"recovery_key": recovery})))
}

#[derive(Deserialize)]
pub struct LoginReq {
    password: String,
}

pub async fn login(State(st): State<AppState>, Json(r): Json<LoginReq>) -> Result<Response, ApiError> {
    if !st.rt.vault_exists() {
        return Err(err(StatusCode::CONFLICT, "not set up yet"));
    }
    tokio::time::sleep(st.sessions.backoff()).await;
    let ok = if st.rt.is_locked().await {
        let path = st.rt.home.vault_path();
        let pw = r.password.clone();
        match tokio::task::spawn_blocking(move || UnlockedVault::unlock_password(&path, &pw)).await {
            Ok(Ok(v)) => {
                st.rt.unlock(v).await;
                true
            }
            Ok(Err(UnlockError::WrongCredentials)) => false,
            Ok(Err(UnlockError::Other(e))) => return Err(e.into()),
            Err(e) => return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        }
    } else {
        st.rt.verify_password(&r.password).await?
    };
    st.sessions.record(ok);
    if !ok {
        return Err(err(StatusCode::UNAUTHORIZED, "wrong password"));
    }
    let token = st.sessions.create();
    Ok(logged_in(&st, token, json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct RecoverReq {
    recovery_key: String,
    new_password: String,
}

pub async fn recover(State(st): State<AppState>, Json(r): Json<RecoverReq>) -> Result<Response, ApiError> {
    kk_home::vault::validate_password(&r.new_password)?;
    tokio::time::sleep(st.sessions.backoff()).await;
    let path = st.rt.home.vault_path();
    let res = tokio::task::spawn_blocking(move || -> Result<UnlockedVault, UnlockError> {
        let mut v = UnlockedVault::unlock_recovery(&path, &r.recovery_key)?;
        v.change_password(&r.new_password)?;
        Ok(v)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    match res {
        Ok(v) => {
            st.sessions.record(true);
            st.sessions.clear();
            st.rt.unlock(v).await;
            let token = st.sessions.create();
            Ok(logged_in(&st, token, json!({"ok": true})))
        }
        Err(UnlockError::WrongCredentials) => {
            st.sessions.record(false);
            Err(err(StatusCode::UNAUTHORIZED, "wrong recovery key"))
        }
        Err(UnlockError::Other(e)) => Err(e.into()),
    }
}

pub async fn logout(State(st): State<AppState>, a: Authed) -> Response {
    st.sessions.remove(&a.token);
    let mut res = Json(json!({"ok": true})).into_response();
    res.headers_mut().insert(header::SET_COOKIE, clear_cookie(st.tls).parse().unwrap());
    res
}

pub async fn lock(State(st): State<AppState>, _a: Authed) -> R {
    st.rt.lock().await;
    st.sessions.clear();
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct PasswordReq {
    current: String,
    new: String,
}

pub async fn change_password(State(st): State<AppState>, _a: Authed, Json(r): Json<PasswordReq>) -> R {
    if !st.rt.verify_password(&r.current).await? {
        return Err(err(StatusCode::UNAUTHORIZED, "current password is wrong"));
    }
    st.rt.with_vault(|v| v.change_password(&r.new)).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn rotate_recovery(State(st): State<AppState>, _a: Authed, Json(r): Json<LoginReq>) -> R {
    if !st.rt.verify_password(&r.password).await? {
        return Err(err(StatusCode::UNAUTHORIZED, "wrong password"));
    }
    let key = st.rt.with_vault(|v| v.rotate_recovery()).await?;
    Ok(Json(json!({"recovery_key": key})))
}

// ---------------------------------------------------- engines & secrets

pub async fn get_engines(State(st): State<AppState>, _a: Authed) -> R {
    Ok(Json(json!({
        "raw": st.rt.home.engines_raw()?,
        "engines": st.rt.engines().await,
        "health": st.rt.health().await,
    })))
}

#[derive(Deserialize)]
pub struct RawReq {
    raw: String,
}

pub async fn put_engines(State(st): State<AppState>, _a: Authed, Json(r): Json<RawReq>) -> R {
    st.rt.set_engines(&r.raw).await?;
    Ok(Json(json!({"engines": st.rt.engines().await})))
}

pub async fn check_engines(State(st): State<AppState>, _a: Authed) -> R {
    st.rt.check_health().await;
    Ok(Json(json!({"health": st.rt.health().await})))
}

pub async fn list_secrets(State(st): State<AppState>, _a: Authed) -> R {
    let names = st.rt.with_vault(|v| Ok(v.names())).await?;
    Ok(Json(json!({"names": names})))
}

#[derive(Deserialize)]
pub struct SecretReq {
    value: String,
}

pub async fn put_secret(State(st): State<AppState>, _a: Authed, Path(name): Path<String>, Json(r): Json<SecretReq>) -> R {
    st.rt.with_vault(|v| v.set(&name, &r.value)).await?;
    let rt = st.rt.clone();
    tokio::spawn(async move { rt.check_health().await });
    Ok(Json(json!({"ok": true})))
}

pub async fn delete_secret(State(st): State<AppState>, _a: Authed, Path(name): Path<String>) -> R {
    let had = st.rt.with_vault(|v| v.remove(&name)).await?;
    Ok(Json(json!({"deleted": had})))
}

// -------------------------------------------------- roles & templates

pub async fn list_roles(State(st): State<AppState>, _a: Authed) -> R {
    let roles: Vec<Value> = st.rt.home.roles()?.into_iter().map(|(r, s)| json!({"role": r, "source": s})).collect();
    Ok(Json(json!({"roles": roles, "groups": kk_agent::tools::GROUPS})))
}

pub async fn put_role(State(st): State<AppState>, _a: Authed, Path(id): Path<String>, Json(mut role): Json<Role>) -> R {
    role.id = id;
    st.rt.home.save_role(&role)?;
    Ok(Json(json!({"role": role})))
}

pub async fn delete_role(State(st): State<AppState>, _a: Authed, Path(id): Path<String>) -> R {
    Ok(Json(json!({"deleted": st.rt.home.delete_role(&id)?})))
}

pub async fn list_templates(State(st): State<AppState>, _a: Authed) -> R {
    let t: Vec<Value> = st.rt.home.templates()?.into_iter().map(|(t, s)| json!({"template": t, "source": s})).collect();
    Ok(Json(json!({"templates": t})))
}

pub async fn doctor(State(st): State<AppState>, _a: Authed) -> R {
    let report = kk_env::doctor::check(&st.rt.home.cache);
    Ok(Json(json!({"report": report, "vm_ready": report.vm_ready()})))
}

// ----------------------------------------------------------- workspaces

async fn agent_views(w: &Workspace) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    for a in w.agents()? {
        let st = w.store().run_state(&a.id)?.unwrap_or(RunState::Idle);
        out.push(json!({
            "agent": a,
            "run_state": st,
            "running": w.is_running(&a.id),
            "checklist": w.store().checklist(&a.id)?,
        }));
    }
    Ok(out)
}

pub async fn list_workspaces(State(st): State<AppState>, _a: Authed) -> R {
    let mut out = Vec::new();
    for w in st.rt.workspaces().await {
        let agents = w.agents()?;
        let running = agents.iter().filter(|a| w.is_running(&a.id)).count();
        out.push(json!({
            "id": w.id(),
            "name": w.name(),
            "template": w.manifest().template,
            "created_at": w.created_at(),
            "agents": agents.len(),
            "running": running,
            "approvals": w.pending_approvals().len(),
        }));
    }
    Ok(Json(json!({"workspaces": out})))
}

#[derive(Deserialize)]
pub struct CreateWs {
    name: String,
    template: String,
}

pub async fn create_workspace(State(st): State<AppState>, _a: Authed, Json(r): Json<CreateWs>) -> R {
    let w = st.rt.create_workspace(&r.name, &r.template).await?;
    Ok(Json(json!({"id": w.id()})))
}

pub async fn get_workspace(State(st): State<AppState>, _a: Authed, Path(id): Path<String>) -> R {
    let w = ws(&st, &id).await?;
    let envs: Vec<Value> = w
        .env_statuses()
        .await
        .into_iter()
        .map(|(r, s)| json!({"recipe": r, "status": s}))
        .collect();
    Ok(Json(json!({
        "manifest": w.manifest(),
        "root": w.root(),
        "agents": agent_views(&w).await?,
        "tasks": w.store().tasks()?,
        "roles": w.roles()?,
        "envs": envs,
        "approvals": w.pending_approvals(),
        "last_seq": w.store().last_seq()?,
    })))
}

pub async fn put_settings(State(st): State<AppState>, _a: Authed, Path(id): Path<String>, Json(s): Json<WorkspaceSettings>) -> R {
    let w = ws(&st, &id).await?;
    w.update_manifest(|m| m.settings = s)?;
    Ok(Json(json!({"settings": w.settings()})))
}

#[derive(Deserialize)]
pub struct EventsQ {
    #[serde(default)]
    after: i64,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    agent: Option<String>,
}

pub async fn get_events(State(st): State<AppState>, _a: Authed, Path(id): Path<String>, Query(q): Query<EventsQ>) -> R {
    let w = ws(&st, &id).await?;
    let events = match q.agent {
        Some(a) => w.store().agent_events(&a, q.after)?,
        None => w.store().events_after(q.after, q.limit.unwrap_or(5000).min(20000))?,
    };
    Ok(Json(json!({"events": events})))
}

#[derive(Deserialize)]
pub struct MessageReq {
    #[serde(default)]
    agent_id: Option<String>,
    text: String,
}

pub async fn post_message(State(st): State<AppState>, _a: Authed, Path(id): Path<String>, Json(r): Json<MessageReq>) -> R {
    let w = ws(&st, &id).await?;
    if r.text.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "empty message"));
    }
    let to = match r.agent_id {
        Some(a) => a,
        None => w.lead()?.id,
    };
    let ev = w.send_message(&to, Sender::User, &r.text, MessageKind::Chat)?;
    let locked = st.rt.is_locked().await;
    Ok(Json(json!({"event": ev, "locked": locked})))
}

#[derive(Deserialize)]
pub struct SpawnReq {
    role: String,
    #[serde(default)]
    name: Option<String>,
}

pub async fn spawn_agent(State(st): State<AppState>, _a: Authed, Path(id): Path<String>, Json(r): Json<SpawnReq>) -> R {
    let w = ws(&st, &id).await?;
    let a = w.create_agent(&r.role, r.name.as_deref(), Sender::User, false)?;
    Ok(Json(json!({"agent": a})))
}

pub async fn stop_agent(State(st): State<AppState>, _a: Authed, Path((id, aid)): Path<(String, String)>) -> R {
    let w = ws(&st, &id).await?;
    w.stop(&aid);
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct ApprovalReq {
    approved: bool,
    #[serde(default)]
    note: String,
}

pub async fn resolve_approval(
    State(st): State<AppState>,
    _a: Authed,
    Path((id, apid)): Path<(String, String)>,
    Json(r): Json<ApprovalReq>,
) -> R {
    let w = ws(&st, &id).await?;
    w.resolve_approval(&apid, r.approved, &r.note)?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct PathQ {
    #[serde(default)]
    path: String,
}

pub async fn get_files(State(st): State<AppState>, _a: Authed, Path(id): Path<String>, Query(q): Query<PathQ>) -> R {
    let w = ws(&st, &id).await?;
    let p = kk_store::resolve_in(&w.files_dir(), &q.path)?;
    let meta = std::fs::metadata(&p).map_err(|_| err(StatusCode::NOT_FOUND, "not found"))?;
    if meta.is_dir() {
        let mut entries = Vec::new();
        for e in std::fs::read_dir(&p).map_err(anyhow::Error::from)? {
            let e = e.map_err(anyhow::Error::from)?;
            let m = e.metadata().map_err(anyhow::Error::from)?;
            entries.push(json!({"name": e.file_name().to_string_lossy(), "dir": m.is_dir(), "size": m.len()}));
        }
        entries.sort_by(|a, b| {
            (!a["dir"].as_bool().unwrap_or(false), a["name"].as_str().unwrap_or(""))
                .cmp(&(!b["dir"].as_bool().unwrap_or(false), b["name"].as_str().unwrap_or("")))
        });
        return Ok(Json(json!({"kind": "dir", "path": q.path, "entries": entries})));
    }
    const MAX: u64 = 1 << 20;
    let bytes = if meta.len() <= MAX { std::fs::read(&p).map_err(anyhow::Error::from)? } else { vec![] };
    let binary = meta.len() > MAX || bytes.iter().take(8192).any(|b| *b == 0);
    Ok(Json(json!({
        "kind": "file",
        "path": q.path,
        "size": meta.len(),
        "binary": binary,
        "content": if binary { Value::Null } else { Value::String(String::from_utf8_lossy(&bytes).into_owned()) },
    })))
}

pub async fn download(State(st): State<AppState>, _a: Authed, Path(id): Path<String>, Query(q): Query<PathQ>) -> Result<Response, ApiError> {
    let w = ws(&st, &id).await?;
    let p = kk_store::resolve_in(&w.files_dir(), &q.path)?;
    if !p.is_file() {
        return Err(err(StatusCode::NOT_FOUND, "not a file"));
    }
    let f = tokio::fs::File::open(&p).await.map_err(anyhow::Error::from)?;
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file").replace('"', "");
    let mime = mime_guess::from_path(&p).first_or_octet_stream();
    let stream = tokio_util_compat_stream(f);
    Ok((
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename=\"{name}\"")),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

fn tokio_util_compat_stream(f: tokio::fs::File) -> impl futures::Stream<Item = std::io::Result<Vec<u8>>> {
    futures::stream::unfold(f, |mut f| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(buf), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    })
}

pub async fn get_world_state(State(st): State<AppState>, _a: Authed, Path(id): Path<String>) -> R {
    let w = ws(&st, &id).await?;
    let entries: serde_json::Map<String, Value> = w.store().state("")?.into_iter().collect();
    Ok(Json(json!({"state": entries})))
}

pub async fn env_action(
    State(st): State<AppState>,
    _a: Authed,
    Path((id, name, action)): Path<(String, String, String)>,
) -> R {
    let w = ws(&st, &id).await?;
    match action.as_str() {
        "start" => {
            let w2 = w.clone();
            // VM creation can take many minutes: run in the background; progress streams over the WebSocket.
            tokio::spawn(async move {
                if let Err(e) = w2.start_env(&name).await {
                    tracing::warn!("env start failed: {e:#}");
                }
            });
        }
        "stop" => w.stop_env(&name).await?,
        "rebuild" => {
            let env = w.env(&name).await?;
            env.stop().await?;
            env.destroy().await?;
        }
        _ => return Err(err(StatusCode::BAD_REQUEST, "action must be start, stop or rebuild")),
    }
    Ok(Json(json!({"ok": true})))
}

pub async fn prepare_migration(State(st): State<AppState>, _a: Authed, Path(id): Path<String>) -> R {
    let w = ws(&st, &id).await?;
    w.prepare_migration().await?;
    Ok(Json(json!({"ok": true, "path": w.root()})))
}
