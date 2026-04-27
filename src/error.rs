use axum::{
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    /// Git smart-HTTP 401: must include WWW-Authenticate so git clients prompt for credentials.
    #[error("authentication required")]
    GitUnauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("template error: {0}")]
    Template(#[from] minijinja::Error),
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
     .replace('\'', "&#x27;")
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::GitUnauthorized => (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, r#"Basic realm="Gitom", charset="UTF-8""#)],
                "Unauthorized",
            ).into_response(),

            other => {
                let (status, msg) = match &other {
                    Self::NotFound      => (StatusCode::NOT_FOUND,            "not found"),
                    Self::Unauthorized  => (StatusCode::UNAUTHORIZED,         "login required"),
                    Self::Forbidden     => (StatusCode::FORBIDDEN,            "forbidden"),
                    Self::BadRequest(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid input"),
                    Self::Template(e) => {
                        tracing::error!("template error: {e}");
                        (StatusCode::INTERNAL_SERVER_ERROR, "template render failed")
                    }
                    Self::Git(e) => {
                        tracing::error!("git error: {e}");
                        (StatusCode::INTERNAL_SERVER_ERROR, "git operation failed")
                    }
                    Self::Internal(e) => {
                        tracing::error!("internal error: {e:#}");
                        (StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
                    }
                    Self::Db(e) => {
                        tracing::error!("database error: {e}");
                        (StatusCode::INTERNAL_SERVER_ERROR, "database error")
                    }
                    Self::GitUnauthorized => unreachable!(),
                };
                (status, Html(format!("<h2>{}</h2><p>{}</p>", status.as_u16(), html_escape(msg)))).into_response()
            }
        }
    }
}
