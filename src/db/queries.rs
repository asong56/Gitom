use anyhow::{Context, Result};
use sqlx::SqlitePool;
use time;

use super::models::*;
// =============================================================================
// Repositories
// =============================================================================

pub async fn get_repo(pool: &SqlitePool, reponame: &str) -> Result<Option<Repository>> {
    sqlx::query_as::<_, Repository>(
        "SELECT id, name, description, is_private, default_branch, git_path, created_at, updated_at
         FROM repositories WHERE name = ?",
    )
    .bind(reponame)
    .fetch_optional(pool)
    .await
    .context("get_repo")
}

pub async fn get_repo_by_id(pool: &SqlitePool, repo_id: i64) -> Result<Option<Repository>> {
    sqlx::query_as::<_, Repository>(
        "SELECT id, name, description, is_private, default_branch, git_path, created_at, updated_at
         FROM repositories WHERE id = ?",
    )
    .bind(repo_id)
    .fetch_optional(pool)
    .await
    .context("get_repo_by_id")
}

pub async fn list_repos(pool: &SqlitePool) -> Result<Vec<Repository>> {
    sqlx::query_as::<_, Repository>(
        "SELECT id, name, description, is_private, default_branch, git_path, created_at, updated_at
         FROM repositories ORDER BY updated_at DESC",
    )
    .fetch_all(pool)
    .await
    .context("list_repos")
}

pub async fn list_public_repos(pool: &SqlitePool, page: i64, per_page: i64) -> Result<Vec<Repository>> {
    sqlx::query_as::<_, Repository>(
        "SELECT id, name, description, is_private, default_branch, git_path, created_at, updated_at
         FROM repositories WHERE is_private = 0 ORDER BY updated_at DESC LIMIT ? OFFSET ?",
    )
    .bind(per_page)
    .bind((page - 1) * per_page)
    .fetch_all(pool)
    .await
    .context("list_public_repos")
}


pub async fn create_repo(
    pool:        &SqlitePool,
    name:        &str,
    description: Option<&str>,
    is_private:  bool,
    git_path:    &str,
) -> Result<Repository> {
    let id = sqlx::query(
        "INSERT INTO repositories (name, description, is_private, git_path) VALUES (?, ?, ?, ?)",
    )
    .bind(name)
    .bind(description)
    .bind(is_private)
    .bind(git_path)
    .execute(pool)
    .await
    .context("create_repo")?
    .last_insert_rowid();

    get_repo_by_id(pool, id)
        .await?
        .context("create_repo: row not found after insert")
}

pub async fn delete_repo(pool: &SqlitePool, repo_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM repositories WHERE id = ?")
        .bind(repo_id)
        .execute(pool)
        .await
        .context("delete_repo")?;
    Ok(())
}

// =============================================================================
// Webhooks
// =============================================================================

fn rows_to_webhooks(rows: Vec<WebhookRow>) -> Vec<Webhook> {
    rows.into_iter().map(Webhook::from).collect()
}

pub async fn list_repo_webhooks(pool: &SqlitePool, repo_id: i64) -> Result<Vec<Webhook>> {
    let rows = sqlx::query_as::<_, WebhookRow>(
        "SELECT id, repo_id, url, secret, events, is_active
         FROM webhooks WHERE repo_id = ? ORDER BY id",
    )
    .bind(repo_id)
    .fetch_all(pool)
    .await
    .context("list_repo_webhooks")?;
    Ok(rows_to_webhooks(rows))
}

pub async fn list_push_webhooks(pool: &SqlitePool, repo_id: i64) -> Result<Vec<Webhook>> {
    let rows = sqlx::query_as::<_, WebhookRow>(
        r#"SELECT DISTINCT w.id, w.repo_id, w.url, w.secret, w.events, w.is_active
           FROM webhooks w, json_each(w.events) je
           WHERE w.repo_id = ?
             AND w.is_active = 1
             AND je.value IN ('push', '*')"#,
    )
    .bind(repo_id)
    .fetch_all(pool)
    .await
    .context("list_push_webhooks")?;
    Ok(rows_to_webhooks(rows))
}

pub async fn create_webhook(
    pool:    &SqlitePool,
    repo_id: i64,
    url:     &str,
    secret:  Option<&str>,
    events:  &[String],
) -> Result<Webhook> {
    let events_json = serde_json::to_string(events).unwrap_or_else(|_| "[]".into());
    let id = sqlx::query(
        "INSERT INTO webhooks (repo_id, url, secret, events) VALUES (?, ?, ?, ?)",
    )
    .bind(repo_id)
    .bind(url)
    .bind(secret)
    .bind(&events_json)
    .execute(pool)
    .await
    .context("create_webhook")?
    .last_insert_rowid();

    let row = sqlx::query_as::<_, WebhookRow>(
        "SELECT id, repo_id, url, secret, events, is_active FROM webhooks WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .context("create_webhook: fetch")?;
    Ok(Webhook::from(row))
}

pub async fn delete_webhook(pool: &SqlitePool, webhook_id: i64, repo_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM webhooks WHERE id = ? AND repo_id = ?")
        .bind(webhook_id)
        .bind(repo_id)
        .execute(pool)
        .await
        .context("delete_webhook")?;
    Ok(())
}

// =============================================================================
// Login failure persistence
// =============================================================================

#[derive(sqlx::FromRow)]
pub struct LoginFailureRow {
    pub count:        i64,
    pub window_start: String,
}

pub async fn load_login_state(pool: &SqlitePool) -> Result<LoginFailureRow> {
    sqlx::query_as::<_, LoginFailureRow>(
        "SELECT count, window_start FROM login_failures WHERE id = 1",
    )
    .fetch_one(pool)
    .await
    .context("load_login_state")
}

pub async fn save_login_state(pool: &SqlitePool, count: u32, window_start_unix: i64) -> Result<()> {
    let window_start = time::OffsetDateTime::from_unix_timestamp(window_start_unix)
        .map(|t| t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default())
        .unwrap_or_default();
    sqlx::query(
        "UPDATE login_failures SET count = ?, window_start = ? WHERE id = 1",
    )
    .bind(count as i64)
    .bind(window_start)
    .execute(pool)
    .await
    .context("save_login_state")?;
    Ok(())
}
