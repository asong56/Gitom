use axum::{
    extract::State,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use minijinja::context;
use serde::Deserialize;

use crate::{
    auth::{constant_time_eq, jwt, password},
    db::{git, queries},
    error::AppError,
    middleware::auth::Authenticated,
    AppState,
};

const PASSWORD_MAX_LEN: usize = 128;

fn reponame_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^[a-zA-Z0-9_.\-]{1,100}$").unwrap())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login",              get(login_page).post(login_post))
        .route("/logout",             post(logout))
        .route("/settings",           get(settings_page))
        .route("/settings/repos/new", get(new_repo_page).post(new_repo_post))
}

// =============================================================================
// Login
// =============================================================================

async fn login_page(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    Ok(Html(state.tmpl.render("user/login.html", context! {})?)
    )
}

#[derive(Deserialize)]
struct LoginForm { username: String, password: String }

async fn login_post(
    State(state):  State<AppState>,
    axum::Form(f): axum::Form<LoginForm>,
) -> Result<Response, AppError> {
    if state.login_limiter.is_blocked() {
        let html = state.tmpl.render("user/login.html",
            context! { error => "too many login attempts, please try again later" })?;
        return Ok(Html(html).into_response());
    }

    let ok = verify_credentials(&f.username, &f.password, &state);

    if !ok {
        let allowed = state.login_limiter.record_failure(&state.db.pool).await;
        let msg = if allowed { "invalid username or password" } else { "too many login attempts, please try again later" };
        let html = state.tmpl.render("user/login.html", context! { error => msg })?;
        return Ok(Html(html).into_response());
    }

    state.login_limiter.reset(&state.db.pool).await;

    let (token, _) = jwt::sign(&state.config.jwt_secret, state.config.jwt_expire_hours)
        .map_err(AppError::Internal)?;

    Ok((
        [(axum::http::header::SET_COOKIE, build_cookie(&token, state.config.secure_cookie, state.config.jwt_expire_hours))],
        axum::response::Redirect::to("/"),
    ).into_response())
}

/// Verify credentials against config. Accepts static access token or Argon2 password hash.
fn verify_credentials(username: &str, password: &str, state: &AppState) -> bool {
    if password.is_empty() || password.len() > PASSWORD_MAX_LEN {
        return false;
    }
    if username != state.config.username {
        return false;
    }
    if !state.config.access_token.is_empty()
        && constant_time_eq(password.as_bytes(), state.config.access_token.as_bytes())
    {
        return true;
    }
    if !state.config.password_hash.is_empty() {
        return password::verify(password, &state.config.password_hash).is_ok();
    }
    false
}

async fn logout(State(state): State<AppState>) -> Response {
    let mut cookie = "gitom_token=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0".to_string();
    if state.config.secure_cookie {
        cookie.push_str("; Secure");
    }
    (
        [(axum::http::header::SET_COOKIE, cookie)],
        axum::response::Redirect::to("/"),
    ).into_response()
}

// =============================================================================
// Settings
// =============================================================================

async fn settings_page(
    State(state): State<AppState>,
    auth: Option<axum::Extension<Authenticated>>,
) -> Result<Html<String>, AppError> {
    let auth = require_auth(auth)?;
    Ok(Html(state.tmpl.render("user/settings.html", context! { auth })?)
    )
}

// =============================================================================
// New repository
// =============================================================================

async fn new_repo_page(
    State(state): State<AppState>,
    auth: Option<axum::Extension<Authenticated>>,
) -> Result<Html<String>, AppError> {
    let auth = require_auth(auth)?;
    Ok(Html(state.tmpl.render("user/new_repo.html", context! { auth })?)
    )
}

#[derive(Deserialize)]
struct NewRepoForm { name: String, description: Option<String>, is_private: Option<String> }

async fn new_repo_post(
    State(state):  State<AppState>,
    auth:          Option<axum::Extension<Authenticated>>,
    axum::Form(f): axum::Form<NewRepoForm>,
) -> Result<Response, AppError> {
    require_auth(auth)?;
    validate_repo_name(&f.name).map_err(AppError::BadRequest)?;

    if queries::get_repo(&state.db.pool, &f.name).await?.is_some() {
        return Err(AppError::BadRequest("repository name already exists".into()));
    }

    let is_private = f.is_private.as_deref() == Some("on");
    let git_rel = format!("{}/{}.git", state.config.username, f.name);
    let git_abs = state.config.resolve_git_path(&git_rel);

    // Bug fix: clean up the (possibly partially-created) directory when init_bare
    // fails, not only when the subsequent DB insert fails.
    if let Err(e) = git::init_bare(
        git_abs.to_str()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("path contains non-UTF-8 characters")))?
    ) {
        let _ = std::fs::remove_dir_all(&git_abs);
        return Err(AppError::Internal(e));
    }

    if let Err(e) = queries::create_repo(
        &state.db.pool,
        &f.name,
        f.description.as_deref().filter(|d| !d.is_empty()),
        is_private,
        &git_rel,
    ).await {
        let _ = std::fs::remove_dir_all(&git_abs);
        return Err(AppError::Internal(e));
    }

    Ok(axum::response::Redirect::to(
        &format!("/{}/{}", state.config.username, f.name)
    ).into_response())
}

// =============================================================================
// Shared helpers
// =============================================================================

pub fn require_auth(auth: Option<axum::Extension<Authenticated>>) -> Result<Authenticated, AppError> {
    auth.map(|e| e.0).ok_or(AppError::Unauthorized)
}

pub fn build_cookie(token: &str, secure: bool, expire_hours: i64) -> String {
    let max_age = expire_hours * 3600;
    let mut s = format!("gitom_token={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}");
    if secure { s.push_str("; Secure"); }
    s
}

pub fn validate_repo_name(name: &str) -> Result<(), String> {
    if name.is_empty()                      { return Err("repository name cannot be empty".into()); }
    if name.len() > 100                     { return Err("repository name cannot exceed 100 characters".into()); }
    if matches!(name, ".git" | "." | "..") { return Err("repository name is reserved".into()); }
    if name.contains("..") || name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err("repository name contains illegal characters".into());
    }
    if name.ends_with(".git") { return Err("repository name cannot end with .git".into()); }
    if name.ends_with('.')    { return Err("repository name cannot end with a dot".into()); }
    if name.starts_with('.')  { return Err("repository name cannot start with a dot".into()); }
    if !reponame_re().is_match(name) { return Err("repository name is invalid (letters, digits, underscores, dots, hyphens only)".into()); }
    Ok(())
}
