use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: String,

    #[serde(default = "default_listen")]
    pub listen: String,

    #[serde(default = "default_jwt_secret")]
    pub jwt_secret: String,

    #[serde(default = "default_jwt_expire")]
    pub jwt_expire_hours: i64,

    /// Set to true when behind an HTTPS reverse proxy to enable Secure cookie flag.
    #[serde(default)]
    pub secure_cookie: bool,

    // ── Credentials (single user) ────────────────────────────────────────────

    /// Display name used in repo URLs and the UI. Defaults to "git".
    #[serde(default = "default_username")]
    pub username: String,

    /// Argon2 hash of the password. Populated at startup from GITOM_PASSWORD
    /// or set directly in the config file. Must not be empty on startup.
    #[serde(default)]
    pub password_hash: String,

    /// Optional static access token (plain random string). When set, git clients
    /// and the web UI can authenticate with this token instead of the password.
    /// Generate one with: openssl rand -hex 32
    #[serde(default)]
    pub access_token: String,

    // ── Rate limiting ────────────────────────────────────────────────────────

    #[serde(default = "default_rate_window")]
    pub login_rate_window_secs: u64,
    #[serde(default = "default_rate_max")]
    pub login_rate_max_attempts: u32,

    // ── Webhook delivery ─────────────────────────────────────────────────────

    #[serde(default = "default_wh_workers")]
    pub webhook_workers: usize,
    #[serde(default = "default_wh_timeout")]
    pub webhook_timeout_secs: u64,
    #[serde(default = "default_wh_retries")]
    pub webhook_max_retries: usize,
    #[serde(default = "default_wh_delay")]
    pub webhook_retry_base_delay_ms: u64,

    /// Reverse proxy hop count for real-IP detection.
    /// 0 = use TCP peer address; N = trust the Nth-from-right X-Forwarded-For entry.
    #[serde(default)]
    pub trusted_proxy_depth: u8,

    /// Maximum file size (MB) rendered in the blob viewer.
    #[serde(default = "default_blob_limit")]
    pub blob_size_limit_mb: u64,

    /// Maximum git pack size (MB) accepted in push/fetch.
    #[serde(default = "default_git_pack_limit")]
    pub git_pack_limit_mb: u64,

    /// Bearer token required to access /metrics. Leave empty for unauthenticated access.
    #[serde(default)]
    pub metrics_token: String,
}

fn default_data_dir()       -> String { "/data".to_string() }
fn default_listen()         -> String { "0.0.0.0:3000".to_string() }
fn default_jwt_secret()     -> String { "change-me-in-production".to_string() }
fn default_jwt_expire()     -> i64    { 24 * 7 }
fn default_username()       -> String { "git".to_string() }
fn default_rate_window()    -> u64    { 15 * 60 }
fn default_rate_max()       -> u32    { 10 }
fn default_wh_workers()     -> usize  { 8 }
fn default_wh_timeout()     -> u64    { 10 }
fn default_wh_retries()     -> usize  { 5 }
fn default_wh_delay()       -> u64    { 500 }
fn default_blob_limit()     -> u64    { 10 }
fn default_git_pack_limit() -> u64    { 512 }

impl AppConfig {
    pub fn load() -> Result<Self, config::ConfigError> {
        config::Config::builder()
            .add_source(config::File::with_name("gitom").required(false))
            .add_source(config::Environment::with_prefix("GITOM").separator("_"))
            .build()?
            .try_deserialize()
    }

    pub fn db_path(&self) -> String {
        format!("{}/gitom.db", self.data_dir.trim_end_matches('/'))
    }

    pub fn repo_root(&self) -> String {
        format!("{}/repos", self.data_dir.trim_end_matches('/'))
    }

    pub fn resolve_git_path(&self, git_path: &str) -> std::path::PathBuf {
        let p = std::path::Path::new(git_path);
        if p.is_absolute() { p.to_owned() } else { std::path::PathBuf::from(self.repo_root()).join(git_path) }
    }
}
