pub mod git;
pub mod models;
pub mod queries;

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn connect(path: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // WAL + NORMAL: full-speed writes; worst case loses the last transaction on OS crash.
            .pragma("synchronous", "NORMAL")
            .pragma("foreign_keys", "ON")
            .pragma("busy_timeout", "5000")
            // -10000 = 10 MiB page cache.
            .pragma("cache_size", "-10000")
            .pragma("temp_store", "MEMORY")
            .pragma("mmap_size", "268435456"); // 256 MiB

        // SQLite WAL supports many concurrent readers; pool of 8 is sufficient.
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .min_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect_with(opts)
            .await?;

        Ok(Self { pool })
    }

    pub async fn run_migrations(&self) -> Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("migration failed: {e}"))
    }
}
