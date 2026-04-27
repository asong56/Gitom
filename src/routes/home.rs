use axum::{extract::State, response::Html, routing::get, Router};
use minijinja::context;

use crate::{db::queries, error::AppError, middleware::auth::Authenticated, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/",        get(home))
        .route("/explore", get(explore))
}

async fn home(
    State(state): State<AppState>,
    auth: Option<axum::Extension<Authenticated>>,
) -> Result<Html<String>, AppError> {
    let auth = auth.map(|e| e.0);
    // Authenticated: show all repos. Anonymous: show only public repos.
    let repos = if auth.is_some() {
        queries::list_repos(&state.db.pool).await?
    } else {
        queries::list_public_repos(&state.db.pool, 1, 20).await?
    };
    Ok(Html(state.tmpl.render("home.html", context! {
        auth,
        repos,
        owner_username => state.config.username,
    })?)
    )
}

async fn explore(
    State(state): State<AppState>,
    auth: Option<axum::Extension<Authenticated>>,
) -> Result<Html<String>, AppError> {
    let auth  = auth.map(|e| e.0);
    let repos = queries::list_public_repos(&state.db.pool, 1, 50).await?;
    Ok(Html(state.tmpl.render("explore.html", context! {
        auth,
        repos,
        owner_username => state.config.username,
    })?)
    )
}
