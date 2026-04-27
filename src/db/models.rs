use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Repository {
    pub id:             i64,
    pub name:           String,
    pub description:    Option<String>,
    pub is_private:     bool,
    pub default_branch: Option<String>,
    pub git_path:       String,
    pub created_at:     String,
    pub updated_at:     String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    pub id:        i64,
    pub repo_id:   i64,
    pub url:       String,
    pub secret:    Option<String>,
    pub events:    Vec<String>,
    pub is_active: bool,
}

#[derive(sqlx::FromRow)]
pub struct WebhookRow {
    pub id:        i64,
    pub repo_id:   i64,
    pub url:       String,
    pub secret:    Option<String>,
    pub events:    String,
    pub is_active: bool,
}

impl From<WebhookRow> for Webhook {
    fn from(r: WebhookRow) -> Self {
        Self {
            id:        r.id,
            repo_id:   r.repo_id,
            url:       r.url,
            secret:    r.secret,
            events:    serde_json::from_str(&r.events).unwrap_or_default(),
            is_active: r.is_active,
        }
    }
}

/// A repository plus the owner's username (always the configured single user).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoView {
    pub repo:           Repository,
    pub owner_username: String,
}
