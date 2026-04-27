mod auth;
mod config;
mod db;
mod error;
mod middleware;
mod rate_limiter;
mod routes;
mod templates;
mod webhook;

use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    middleware::from_fn_with_state,
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use minijinja::context;
use rust_embed::RustEmbed;
use tokio::signal;
use tower_http::compression::CompressionLayer;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use auth::password;
use config::AppConfig;
use db::Database;
use error::AppError;
use middleware::auth::Authenticated;
use rate_limiter::LoginLimiter;
use templates::TemplateEngine;
use webhook::DeliverQueue;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

#[derive(Clone)]
pub struct AppState {
    pub db:            Arc<Database>,
    pub tmpl:          Arc<TemplateEngine>,
    pub config:        Arc<AppConfig>,
    pub login_limiter: Arc<LoginLimiter>,
    pub webhook:       Arc<DeliverQueue>,
}

#[tokio::main]
async fn main() {
    let use_json = std::env::var("GITOM_LOG_JSON").as_deref() == Ok("true");
    let registry = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("gitom=info")));
    if use_json {
        registry.with(tracing_subscriber::fmt::layer().json()).init();
    } else {
        registry.with(tracing_subscriber::fmt::layer().compact()).init();
    }

    let mut cfg = AppConfig::load().unwrap_or_else(|e| {
        error!("config load failed: {e}");
        std::process::exit(1);
    });

    // ── JWT secret validation ─────────────────────────────────────────────────
    if cfg.jwt_secret == "change-me-in-production" {
        error!(
            "GITOM_JWT_SECRET is set to the default value — anyone who knows this string can \
             forge session tokens.\nGenerate a secret: export GITOM_JWT_SECRET=$(openssl rand -hex 32)"
        );
        std::process::exit(1);
    }
    if cfg.jwt_secret.len() < 32 {
        warn!("GITOM_JWT_SECRET is shorter than 32 characters; consider using a longer random string");
    }

    // ── Password initialisation ───────────────────────────────────────────────
    // GITOM_PASSWORD env var is hashed at startup and stored in-memory.
    // Alternatively, a pre-computed hash can be placed in the config file as `password_hash`.
    if let Ok(pw) = std::env::var("GITOM_PASSWORD") {
        if pw.is_empty() {
            error!("GITOM_PASSWORD is set but empty");
            std::process::exit(1);
        }
        cfg.password_hash = password::hash(&pw).unwrap_or_else(|e| {
            error!("failed to hash GITOM_PASSWORD: {e}");
            std::process::exit(1);
        });
    }
    if cfg.password_hash.is_empty() && cfg.access_token.is_empty() {
        error!(
            "No credentials configured. Set GITOM_PASSWORD to a password, \
             or GITOM_ACCESS_TOKEN to a static token (or both).\n\
             Example: export GITOM_PASSWORD=my-secret-password"
        );
        std::process::exit(1);
    }

    // ── Access token from env ─────────────────────────────────────────────────
    if let Ok(token) = std::env::var("GITOM_ACCESS_TOKEN") {
        if token.len() < 16 {
            warn!("GITOM_ACCESS_TOKEN is very short; use at least 32 random characters");
        }
        cfg.access_token = token;
    }

    // ── Filesystem setup ──────────────────────────────────────────────────────
    std::fs::create_dir_all(cfg.repo_root()).unwrap_or_else(|e| {
        error!("cannot create repo dir {}: {e}", cfg.repo_root());
        std::process::exit(1);
    });

    let db = Arc::new(Database::connect(&cfg.db_path()).await.unwrap_or_else(|e| {
        error!("database connection failed: {e}");
        std::process::exit(1);
    }));

    db.run_migrations().await.unwrap_or_else(|e| {
        error!("migration failed: {e}");
        std::process::exit(1);
    });

    let tmpl = Arc::new(TemplateEngine::new().unwrap_or_else(|e| {
        error!("template engine init failed: {e}");
        std::process::exit(1);
    }));

    let login_limiter = Arc::new(LoginLimiter::new(
        cfg.login_rate_window_secs,
        cfg.login_rate_max_attempts,
    ));
    // Restore failure counter persisted by the previous run.
    login_limiter.load_from_db(&db.pool).await;
    let webhook = Arc::new(DeliverQueue::new(
        cfg.webhook_workers,
        cfg.webhook_timeout_secs,
        cfg.webhook_max_retries,
        cfg.webhook_retry_base_delay_ms,
    ));

    let cfg = Arc::new(cfg);
    let state = AppState { db, tmpl, config: cfg.clone(), login_limiter, webhook };

    let app = Router::new()
        .route("/assets/*path", get(serve_asset))
        .route("/health",       get(handle_health))
        .route("/metrics",      get(handle_metrics))
        .route("/:username",    get(user_profile))
        .nest("/:user/:repo",   routes::repo::router())
        .nest("/user",          routes::user::router())
        .merge(routes::home::router())
        .layer(from_fn_with_state(state.clone(), middleware::auth::authenticate))
        .layer(CompressionLayer::new())
        .with_state(state);

    let addr: SocketAddr = cfg.listen.parse()
        .unwrap_or_else(|_| "0.0.0.0:3000".parse().unwrap());

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| {
        error!("bind failed on {addr}: {e}");
        std::process::exit(1);
    });

    info!(
        "Gitom v{} listening on http://{addr}  user: {}  data: {}",
        env!("CARGO_PKG_VERSION"),
        cfg.username,
        cfg.data_dir,
    );

    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|e| error!("server error: {e}"));

    info!("Gitom stopped");
}

async fn serve_asset(Path(path): Path<String>) -> Response {
    if path.contains("..") || path.starts_with('/') {
        return StatusCode::NOT_FOUND.into_response();
    }
    match Assets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE,  mime.essence_str()),
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                ],
                content.data,
            ).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Profile page: only valid for the configured username; shows all repos to
/// authenticated visitors, public repos to anonymous visitors.
async fn user_profile(
    State(state):   State<AppState>,
    Path(username): Path<String>,
    auth:           Option<axum::Extension<Authenticated>>,
) -> Result<Html<String>, AppError> {
    // Guard reserved path segments and other usernames.
    if matches!(username.as_str(), "user" | "assets" | "health" | "metrics") || username.starts_with('-') {
        return Err(AppError::NotFound);
    }
    if username != state.config.username {
        return Err(AppError::NotFound);
    }

    let auth_val = auth.as_ref().map(|e| &e.0);
    let repos = if auth_val.is_some() {
        db::queries::list_repos(&state.db.pool).await?
    } else {
        db::queries::list_public_repos(&state.db.pool, 1, 100).await?
    };

    Ok(Html(state.tmpl.render("user/profile.html", context! {
        auth => auth.map(|e| e.0),
        owner_username => state.config.username,
        repos,
    })?)
    )
}

async fn handle_health(State(state): State<AppState>) -> Response {
    let db_ok = sqlx::query("SELECT 1")
        .execute(&state.db.pool)
        .await
        .is_ok();

    let repo_root = state.config.repo_root();
    let disk_ok = {
        let probe = std::path::PathBuf::from(&repo_root).join(".health_probe");
        std::fs::write(&probe, b"ok").is_ok() && std::fs::remove_file(&probe).is_ok()
    };

    let all_ok      = db_ok && disk_ok;
    let http_status = if all_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };

    (http_status, axum::Json(serde_json::json!({
        "status":  if all_ok { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
        "checks":  { "database": db_ok, "disk": disk_ok },
    }))).into_response()
}

async fn handle_metrics(
    State(state): State<AppState>,
    headers:      axum::http::HeaderMap,
) -> Response {
    if !state.config.metrics_token.is_empty() {
        let provided = headers.get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if provided != state.config.metrics_token {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    }
    use std::sync::atomic::Ordering::Relaxed;
    let m = &state.webhook.metrics;
    axum::Json(serde_json::json!({
        "webhook": {
            "enqueued":  m.enqueued.load(Relaxed),
            "delivered": m.delivered.load(Relaxed),
            "failed":    m.failed.load(Relaxed),
            "in_flight": m.in_flight.load(Relaxed),
        }
    })).into_response()
}

async fn shutdown_signal() {
    let ctrl_c = async { signal::ctrl_c().await.expect("ctrl-c listener failed") };
    #[cfg(unix)]
    let term = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("SIGTERM listener failed").recv().await;
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    info!("shutdown signal received");
}
