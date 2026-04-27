use axum::{extract::{Request, State}, middleware::Next, response::Response};
use serde::Serialize;
use crate::{auth::jwt, AppState};

/// Proves the request is authenticated. Carries the configured username for templates.
#[derive(Debug, Clone, Serialize)]
pub struct Authenticated {
    pub username: String,
}

pub async fn authenticate(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    if let Some(token) = extract_bearer_token(&req) {
        if jwt::verify(&token, &state.config.jwt_secret).is_ok() {
            req.extensions_mut().insert(Authenticated {
                username: state.config.username.clone(),
            });
        }
    }
    next.run(req).await
}

fn extract_bearer_token(req: &Request) -> Option<String> {
    // Cookie: gitom_token=<jwt>
    if let Some(v) = req.headers().get("Cookie") {
        if let Ok(s) = v.to_str() {
            for part in s.split(';') {
                if let Some(val) = part.trim().strip_prefix("gitom_token=") {
                    if !val.is_empty() {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }
    // Authorization: Bearer <jwt>
    req.headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}
