use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use minijinja::context;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::warn;

use crate::{
    auth::{constant_time_eq, jwt, password},
    db::{git, queries},
    db::models::RepoView,
    error::AppError,
    middleware::auth::Authenticated,
    routes::user::require_auth,
    webhook::{HttpHeader, WebhookTask},
    AppState,
};

const GIT_TIMEOUT:      std::time::Duration = std::time::Duration::from_secs(120);
const GIT_MAX_RESPONSE: usize               = 256 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/",                    get(repo_home))
        .route("/tree/:git_ref",       get(repo_tree_root))
        .route("/tree/:git_ref/*path", get(repo_tree))
        .route("/blob/:git_ref/*path", get(repo_blob))
        .route("/raw/:git_ref/*path",  get(repo_raw))
        .route("/commits",             get(repo_commits_default))
        .route("/commits/:git_ref",    get(repo_commits))
        .route("/commit/:sha",         get(repo_commit))
        .route("/branches",            get(repo_branches))
        .route("/tags",                get(repo_tags))
        .route("/settings",                     get(repo_settings))
        .route("/settings/delete",              post(repo_delete))
        .route("/settings/webhooks",            get(repo_webhooks).post(repo_webhook_create))
        .route("/settings/webhooks/:id/delete", post(repo_webhook_delete))
        .route("/info/refs",        get(git_info_refs))
        .route("/git-upload-pack",  post(git_upload_pack))
        .route("/git-receive-pack", post(git_receive_pack))
}

// ── Path parameter types ──────────────────────────────────────────────────────

#[derive(Deserialize)] struct RP        { user: String, repo: String }
#[derive(Deserialize)] struct RPRef     { user: String, repo: String, git_ref: String }
#[derive(Deserialize)] struct RPRefPath { user: String, repo: String, git_ref: String, path: String }
#[derive(Deserialize)] struct RPSha     { user: String, repo: String, sha: String }
#[derive(Deserialize)] struct RPWebhook { user: String, repo: String, id: i64 }
#[derive(Deserialize)] struct PageQ     { page: Option<u32>, per_page: Option<u32> }
#[derive(Deserialize)] struct InfoRefsQ { service: Option<String> }

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn resolve_repo(
    state:    &AppState,
    username: &str,
    reponame: &str,
    auth:     Option<&Authenticated>,
) -> Result<RepoView, AppError> {
    // Guard against crafted URLs targeting a different owner: the app has only
    // one configured user, so any other username is always 404.
    if username != state.config.username {
        return Err(AppError::NotFound);
    }

    let repo = queries::get_repo(&state.db.pool, reponame)
        .await?
        .ok_or(AppError::NotFound)?;

    // Private repos require authentication. Authenticated == owner (single-user app).
    if repo.is_private && auth.is_none() {
        return Err(AppError::NotFound);
    }

    Ok(RepoView { repo, owner_username: state.config.username.clone() })
}

fn validate_path_segment(s: &str) -> Result<(), AppError> {
    if s.is_empty() || s.len() > 255 {
        return Err(AppError::BadRequest("path segment length out of range".into()));
    }
    if s.contains("..") || s.contains('/') || s.contains('\\') || s.contains('\0') {
        return Err(AppError::BadRequest("path segment contains illegal characters".into()));
    }
    Ok(())
}

fn repo_disk_path(state: &AppState, username: &str, reponame: &str) -> Result<std::path::PathBuf, AppError> {
    validate_path_segment(username)?;
    validate_path_segment(reponame)?;
    let name = if reponame.ends_with(".git") { reponame.to_string() } else { format!("{reponame}.git") };
    let abs  = state.config.resolve_git_path(&format!("{username}/{name}"));
    let root = std::path::Path::new(&state.config.repo_root());
    if !abs.starts_with(root) {
        return Err(AppError::BadRequest("path traversal detected".into()));
    }
    Ok(abs)
}

fn validate_webhook_url(raw: &str) -> Result<(), AppError> {
    use std::net::IpAddr;

    let url = url::Url::parse(raw)
        .map_err(|_| AppError::BadRequest("invalid webhook URL".into()))?;

    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(AppError::BadRequest("webhook URL must use http or https".into())),
    }

    let host = url.host_str()
        .ok_or_else(|| AppError::BadRequest("webhook URL missing host".into()))?;

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(&ip) {
            return Err(AppError::BadRequest("webhook URL must not target a private or loopback address".into()));
        }
    }
    if host.eq_ignore_ascii_case("localhost") {
        return Err(AppError::BadRequest("webhook URL must not target localhost".into()));
    }

    Ok(())
}

fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 127
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 169 && o[1] == 254)
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

macro_rules! open_repo {
    ($state:expr, $rv:expr) => {{
        let path = $state.config.resolve_git_path(&$rv.repo.git_path);
        if !path.exists() {
            return Err(AppError::NotFound);
        }
        git2::Repository::open_bare(&path).map_err(|e| {
            tracing::error!(path = %path.display(), error = %e, "failed to open git repo");
            AppError::NotFound
        })?
    }};
}

// ── Page handlers ─────────────────────────────────────────────────────────────

async fn repo_home(State(s): State<AppState>, Path(p): Path<RP>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let br = rv.repo.default_branch.as_deref().unwrap_or("main").to_string();
    let (entries, readme, commits) = match gr.is_empty() {
        Ok(false) => (
            git::list_tree(&gr, &br, "").unwrap_or_default(),
            git::read_readme(&gr, &br).unwrap_or(None),
            git::commits_for_tree(&gr, &br, "", 1).unwrap_or_default(),
        ),
        _ => (vec![], None, vec![]),
    };
    Ok(Html(s.tmpl.render("repo/home.html", context! {
        auth => auth.map(|e| e.0), rv, branch => br, entries, readme, commits
    })?))
}

async fn repo_tree_root(State(s): State<AppState>, Path(p): Path<RPRef>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let entries = git::list_tree(&gr, &p.git_ref, "")?;
    Ok(Html(s.tmpl.render("repo/tree.html", context! {
        auth => auth.map(|e| e.0), rv, git_ref => p.git_ref, path => "", entries
    })?))
}

async fn repo_tree(State(s): State<AppState>, Path(p): Path<RPRefPath>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let entries = git::list_tree(&gr, &p.git_ref, &p.path)?;
    Ok(Html(s.tmpl.render("repo/tree.html", context! {
        auth => auth.map(|e| e.0), rv, git_ref => p.git_ref, path => p.path, entries
    })?))
}

async fn repo_blob(State(s): State<AppState>, Path(p): Path<RPRefPath>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let blob_limit = s.config.blob_size_limit_mb * 1024 * 1024;
    let (content, is_binary) = git::read_blob_limited(&gr, &p.git_ref, &p.path, blob_limit)?;
    let text_content = if is_binary { None } else { Some(String::from_utf8_lossy(&content).into_owned()) };
    let ext = std::path::Path::new(&p.path).extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
    Ok(Html(s.tmpl.render("repo/blob.html", context! {
        auth => auth.map(|e| e.0), rv, git_ref => p.git_ref,
        path => p.path, text_content, is_binary, ext, size => content.len()
    })?))
}

async fn repo_raw(State(s): State<AppState>, Path(p): Path<RPRefPath>, auth: Option<axum::Extension<Authenticated>>) -> Result<Response, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let blob_limit = s.config.blob_size_limit_mb * 1024 * 1024;
    let (content, _) = git::read_blob_limited(&gr, &p.git_ref, &p.path, blob_limit)?;
    let mime = mime_guess::from_path(&p.path).first_or_octet_stream().as_ref().to_string();
    Ok(([(header::CONTENT_TYPE, mime)], content).into_response())
}

async fn repo_commits_default(State(s): State<AppState>, Path(p): Path<RP>, Query(q): Query<PageQ>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let git_ref  = rv.repo.default_branch.clone().unwrap_or_else(|| "main".into());
    let page     = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(30).min(100);
    let commits  = git::list_commits(&gr, &git_ref, page, per_page)?;
    Ok(Html(s.tmpl.render("repo/commits.html", context! {
        auth => auth.map(|e| e.0), rv, git_ref, commits, page, per_page
    })?))
}

async fn repo_commits(State(s): State<AppState>, Path(p): Path<RPRef>, Query(q): Query<PageQ>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let page     = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(30).min(100);
    let commits  = git::list_commits(&gr, &p.git_ref, page, per_page)?;
    Ok(Html(s.tmpl.render("repo/commits.html", context! {
        auth => auth.map(|e| e.0), rv, git_ref => p.git_ref, commits, page, per_page
    })?))
}

async fn repo_commit(State(s): State<AppState>, Path(p): Path<RPSha>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let (commit, diff) = git::get_commit_with_diff(&gr, &p.sha)?;
    Ok(Html(s.tmpl.render("repo/commit.html", context! {
        auth => auth.map(|e| e.0), rv, commit, diff
    })?))
}

async fn repo_branches(State(s): State<AppState>, Path(p): Path<RP>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let branches = git::list_branches(&gr)?;
    Ok(Html(s.tmpl.render("repo/branches.html", context! {
        auth => auth.map(|e| e.0), rv, branches
    })?))
}

async fn repo_tags(State(s): State<AppState>, Path(p): Path<RP>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    let gr = open_repo!(s, rv);
    let tags = git::list_tags(&gr)?;
    Ok(Html(s.tmpl.render("repo/tags.html", context! {
        auth => auth.map(|e| e.0), rv, tags
    })?))
}

async fn repo_settings(State(s): State<AppState>, Path(p): Path<RP>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    require_auth(auth.clone())?;
    Ok(Html(s.tmpl.render("repo/settings.html", context! { auth => auth.map(|e| e.0), rv })?)
    )
}

async fn repo_delete(
    State(s): State<AppState>,
    Path(p):  Path<RP>,
    auth:     Option<axum::Extension<Authenticated>>,
) -> Result<Response, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    require_auth(auth)?;
    queries::delete_repo(&s.db.pool, rv.repo.id).await?;
    let git_abs = s.config.resolve_git_path(&rv.repo.git_path);
    if let Err(e) = std::fs::remove_dir_all(&git_abs) {
        warn!(path = %git_abs.display(), error = %e, "failed to remove repo directory");
    }
    Ok(axum::response::Redirect::to("/").into_response())
}

async fn repo_webhooks(State(s): State<AppState>, Path(p): Path<RP>, auth: Option<axum::Extension<Authenticated>>) -> Result<Html<String>, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    require_auth(auth.clone())?;
    let hooks = queries::list_repo_webhooks(&s.db.pool, rv.repo.id).await?;
    Ok(Html(s.tmpl.render("repo/webhooks.html", context! { auth => auth.map(|e| e.0), rv, hooks })?)
    )
}

#[derive(Deserialize)]
struct WebhookCreateForm { url: String, secret: Option<String>, events: Option<String> }

async fn repo_webhook_create(
    State(s): State<AppState>,
    Path(p):  Path<RP>,
    auth:     Option<axum::Extension<Authenticated>>,
    axum::Form(f): axum::Form<WebhookCreateForm>,
) -> Result<Response, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    require_auth(auth)?;
    validate_webhook_url(&f.url)?;
    let events: Vec<String> = f.events.unwrap_or_default()
        .split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    queries::create_webhook(&s.db.pool, rv.repo.id, &f.url, f.secret.as_deref().filter(|s| !s.is_empty()), &events).await?;
    Ok(axum::response::Redirect::to(&format!("/{}/{}/settings/webhooks", p.user, p.repo)).into_response())
}

async fn repo_webhook_delete(
    State(s): State<AppState>,
    Path(p):  Path<RPWebhook>,
    auth:     Option<axum::Extension<Authenticated>>,
) -> Result<Response, AppError> {
    let au = auth.as_ref().map(|e| &e.0);
    let rv = resolve_repo(&s, &p.user, &p.repo, au).await?;
    require_auth(auth)?;
    queries::delete_webhook(&s.db.pool, p.id, rv.repo.id).await?;
    Ok(axum::response::Redirect::to(&format!("/{}/{}/settings/webhooks", p.user, p.repo)).into_response())
}

// =============================================================================
// Git smart-HTTP
// =============================================================================

async fn git_info_refs(
    State(s): State<AppState>,
    Path(p):  Path<RP>,
    Query(q): Query<InfoRefsQ>,
    headers:  axum::http::HeaderMap,
) -> Result<Response, AppError> {
    if p.user != s.config.username {
        return Err(AppError::NotFound);
    }
    let service = q.service.as_deref().unwrap_or("git-upload-pack");
    let is_push = service == "git-receive-pack";
    let repo    = queries::get_repo(&s.db.pool, &p.repo).await?.ok_or(AppError::NotFound)?;

    if is_push || repo.is_private {
        authenticate_git(&headers, &s).await.map_err(|_| AppError::GitUnauthorized)?;
    }

    let repo_path   = repo_disk_path(&s, &p.user, &p.repo)?;
    let service_cmd = service.strip_prefix("git-").unwrap_or(service);

    let buf = run_git_command(
        tokio::process::Command::new("git")
            .arg(service_cmd)
            .arg("--stateless-rpc")
            .arg("--advertise-refs")
            .arg(&repo_path),
        None,
    ).await?;

    let header  = format!("# service={service}\n");
    let pkt_len = format!("{:04x}", header.len() + 4);
    let body    = [format!("{pkt_len}{header}0000").into_bytes(), buf].concat();

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", format!("application/x-{service}-advertisement"))
        .header("Cache-Control", "no-cache")
        .body(Body::from(body))
        .unwrap())
}

async fn git_upload_pack(
    State(s): State<AppState>,
    Path(p):  Path<RP>,
    headers:  axum::http::HeaderMap,
    body:     axum::body::Bytes,
) -> Result<Response, AppError> {
    if p.user != s.config.username {
        return Err(AppError::NotFound);
    }
    let repo = queries::get_repo(&s.db.pool, &p.repo).await?.ok_or(AppError::NotFound)?;
    if repo.is_private {
        authenticate_git(&headers, &s).await.map_err(|_| AppError::GitUnauthorized)?;
    }
    let pack_limit = s.config.git_pack_limit_mb * 1024 * 1024;
    if body.len() as u64 > pack_limit {
        return Err(AppError::BadRequest(format!("pack exceeds {} MiB limit", s.config.git_pack_limit_mb)));
    }
    let repo_path = repo_disk_path(&s, &p.user, &p.repo)?;
    run_git_pack("upload-pack", &repo_path, body).await
}

async fn git_receive_pack(
    State(s): State<AppState>,
    Path(p):  Path<RP>,
    headers:  axum::http::HeaderMap,
    body:     axum::body::Bytes,
) -> Result<Response, AppError> {
    if p.user != s.config.username {
        return Err(AppError::NotFound);
    }
    authenticate_git(&headers, &s).await.map_err(|_| AppError::GitUnauthorized)?;
    let repo = queries::get_repo(&s.db.pool, &p.repo).await?.ok_or(AppError::NotFound)?;
    let pack_limit = s.config.git_pack_limit_mb * 1024 * 1024;
    if body.len() as u64 > pack_limit {
        return Err(AppError::BadRequest(format!("pack exceeds {} MiB limit", s.config.git_pack_limit_mb)));
    }
    let repo_path = repo_disk_path(&s, &p.user, &p.repo)?;
    let resp = run_git_pack("receive-pack", &repo_path, body).await?;
    if resp.status().is_success() {
        let s2       = s.clone();
        let repo_id  = repo.id;
        let owner    = p.user.clone();
        let reponame = p.repo.clone();
        tokio::spawn(async move { fire_push_webhooks(&s2, repo_id, &owner, &reponame).await; });
    }
    Ok(resp)
}

/// Authenticate a git HTTP request via Bearer JWT or HTTP Basic credentials.
/// Basic-auth failures are counted by the global LoginLimiter; JWT paths are exempt.
async fn authenticate_git(headers: &axum::http::HeaderMap, state: &AppState) -> Result<(), ()> {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let auth_hdr = headers.get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(())?;

    if let Some(token) = auth_hdr.strip_prefix("Bearer ") {
        return jwt::verify(token, &state.config.jwt_secret).map(|_| ()).map_err(|_| ());
    }

    if let Some(encoded) = auth_hdr.strip_prefix("Basic ") {
        if state.login_limiter.is_blocked() {
            return Err(());
        }

        let decoded = STANDARD.decode(encoded).map_err(|_| ())?;
        let creds   = std::str::from_utf8(&decoded).map_err(|_| ())?;
        let (uname, pass) = creds.split_once(':').ok_or(())?;

        if uname != state.config.username || pass.is_empty() {
            state.login_limiter.record_failure(&state.db.pool).await;
            return Err(());
        }

        if jwt::verify(pass, &state.config.jwt_secret).is_ok() {
            state.login_limiter.reset(&state.db.pool).await;
            return Ok(());
        }

        if !state.config.access_token.is_empty()
            && constant_time_eq(pass.as_bytes(), state.config.access_token.as_bytes())
        {
            state.login_limiter.reset(&state.db.pool).await;
            return Ok(());
        }

        if !state.config.password_hash.is_empty()
            && password::verify(pass, &state.config.password_hash).is_ok()
        {
            state.login_limiter.reset(&state.db.pool).await;
            return Ok(());
        }

        state.login_limiter.record_failure(&state.db.pool).await;
        return Err(());
    }

    Err(())
}

async fn run_git_command(
    cmd:   &mut tokio::process::Command,
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>, AppError> {
    if stdin.is_some() { cmd.stdin(Stdio::piped()); }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git spawn failed: {e}")))?;

    if let Some(data) = stdin {
        if let Some(mut stdin_handle) = child.stdin.take() {
            stdin_handle.write_all(data).await
                .map_err(|e| AppError::Internal(anyhow::anyhow!("git stdin write failed: {e}")))?;
        }
    }

    let result = tokio::time::timeout(GIT_TIMEOUT, async {
        let mut buf     = Vec::new();
        let mut err_buf = Vec::new();
        {
            let mut stdout = child.stdout.take();
            let mut stderr = child.stderr.take();
            tokio::join!(
                async {
                    if let Some(ref mut s) = stdout {
                        let mut limited = s.take(GIT_MAX_RESPONSE as u64);
                        limited.read_to_end(&mut buf).await.ok();
                    }
                },
                async {
                    if let Some(ref mut s) = stderr {
                        s.read_to_end(&mut err_buf).await.ok();
                    }
                }
            );
        }
        let status = child.wait().await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("git wait failed: {e}")))?;
        if !status.success() {
            let stderr_text = String::from_utf8_lossy(&err_buf);
            return Err(AppError::Internal(anyhow::anyhow!(
                "git exited with {status}: {stderr_text}"
            )));
        }
        Ok::<Vec<u8>, AppError>(buf)
    }).await;

    match result {
        Ok(inner) => inner,
        Err(_) => {
            let _ = child.kill().await;
            Err(AppError::Internal(anyhow::anyhow!("git timed out after {}s", GIT_TIMEOUT.as_secs())))
        }
    }
}

async fn run_git_pack(cmd: &str, repo_path: &std::path::Path, body: axum::body::Bytes) -> Result<Response, AppError> {
    let out = run_git_command(
        tokio::process::Command::new("git")
            .arg(cmd)
            .arg("--stateless-rpc")
            .arg(repo_path),
        Some(&body),
    ).await?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", format!("application/x-git-{cmd}-result"))
        .header("Cache-Control", "no-cache")
        .body(Body::from(out))
        .unwrap())
}

// =============================================================================
// Webhook dispatch
// =============================================================================

#[derive(Serialize)]
struct PushPayload<'a> { event: &'static str, repository: RepoRef<'a> }
#[derive(Serialize)]
struct RepoRef<'a> { owner: &'a str, name: &'a str }

async fn fire_push_webhooks(state: &AppState, repo_id: i64, owner: &str, repo_name: &str) {
    let hooks = match queries::list_push_webhooks(&state.db.pool, repo_id).await {
        Ok(h)  => h,
        Err(e) => { warn!("failed to query push webhooks: {e}"); return; }
    };
    if hooks.is_empty() { return; }

    let payload = serde_json::to_string(&PushPayload {
        event:      "push",
        repository: RepoRef { owner, name: repo_name },
    }).unwrap_or_default();

    for hook in hooks {
        let mut extra = vec![
            HttpHeader { key: "X-Gitom-Event".into(), value: "push".into() },
        ];
        if let Some(ref secret) = hook.secret {
            extra.push(HttpHeader {
                key:   "X-Gitom-Signature".into(),
                value: format!("sha256={}", hmac_hex(secret.as_bytes(), payload.as_bytes())),
            });
        }
        state.webhook.enqueue(WebhookTask {
            url:     hook.url,
            payload: payload.clone(),
            headers: extra,
            hook_id: hook.id,
        });
    }
}

fn hmac_hex(key: &[u8], data: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().iter().map(|b| format!("{:02x}", b)).collect()
}
