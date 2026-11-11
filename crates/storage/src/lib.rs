//! SQLite storage: audit log, key/value settings and per-minute traffic history.

use serde::Serialize;
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Storage error.
pub type Error = sqlx::Error;

/// One audit log entry.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    /// Row id.
    pub id: i64,
    /// Unix seconds.
    pub ts: i64,
    /// Who: `system`, `admin`, …
    pub actor: String,
    /// What, e.g. `mode_change`, `config_update`, `login`.
    pub action: String,
    /// Human-readable detail.
    pub detail: String,
}

/// One minute of traffic.
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct MinuteStat {
    /// Unix seconds at the start of the minute.
    pub ts: i64,
    /// Queries.
    pub queries: i64,
    /// Fresh cache hits.
    pub hits: i64,
    /// Cache misses.
    pub misses: i64,
    /// Blocked queries.
    pub blocked: i64,
    /// SERVFAIL replies.
    pub servfail: i64,
    /// Median latency, ms.
    pub p50: f64,
    /// 95th percentile, ms.
    pub p95: f64,
    /// 99th percentile, ms.
    pub p99: f64,
}

/// Handle to the database. Cheap to clone.
#[derive(Clone)]
pub struct Storage {
    pool: SqlitePool,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

const SCHEMA: [&str; 3] = [
    "CREATE TABLE IF NOT EXISTS audit_log (id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL, actor TEXT NOT NULL, action TEXT NOT NULL, detail TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS stats_minute (ts INTEGER PRIMARY KEY, queries INTEGER NOT NULL, hits INTEGER NOT NULL, misses INTEGER NOT NULL, blocked INTEGER NOT NULL, servfail INTEGER NOT NULL, p50 REAL NOT NULL, p95 REAL NOT NULL, p99 REAL NOT NULL)",
];

impl Storage {
    /// Opens (creating if needed) the database file.
    pub async fn open(path: &Path) -> Result<Self, Error> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        // Switching to WAL needs a brief exclusive lock; retry if another process is opening the file too.
        let mut attempt = 0;
        loop {
            let result = SqlitePoolOptions::new()
                .max_connections(4)
                .connect_with(opts.clone())
                .await;
            match result {
                Ok(pool) => return Self::init(pool).await,
                Err(e) if attempt < 10 && e.to_string().contains("locked") => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// In-memory database, for tests.
    pub async fn memory() -> Result<Self, Error> {
        let opts = SqliteConnectOptions::new().in_memory(true);
        // One connection: each in-memory connection would otherwise be its own empty database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::init(pool).await
    }

    async fn init(pool: SqlitePool) -> Result<Self, Error> {
        for stmt in SCHEMA {
            sqlx::query(stmt).execute(&pool).await?;
        }
        Ok(Self { pool })
    }

    /// Appends an audit entry.
    pub async fn audit(&self, actor: &str, action: &str, detail: &str) -> Result<(), Error> {
        sqlx::query("INSERT INTO audit_log (ts, actor, action, detail) VALUES (?, ?, ?, ?)")
            .bind(now())
            .bind(actor)
            .bind(action)
            .bind(detail)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Newest audit entries first.
    pub async fn audit_log(&self, limit: i64) -> Result<Vec<AuditEntry>, Error> {
        let rows = sqlx::query(
            "SELECT id, ts, actor, action, detail FROM audit_log ORDER BY id DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 10_000))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| AuditEntry {
                id: r.get("id"),
                ts: r.get("ts"),
                actor: r.get("actor"),
                action: r.get("action"),
                detail: r.get("detail"),
            })
            .collect())
    }

    /// Reads a setting.
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, Error> {
        let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get("value")))
    }

    /// Writes a setting.
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<(), Error> {
        sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Stores one minute of traffic (replacing any row for the same minute).
    pub async fn record_minute(&self, s: &MinuteStat) -> Result<(), Error> {
        sqlx::query(
            "INSERT OR REPLACE INTO stats_minute (ts, queries, hits, misses, blocked, servfail, p50, p95, p99) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(s.ts)
        .bind(s.queries)
        .bind(s.hits)
        .bind(s.misses)
        .bind(s.blocked)
        .bind(s.servfail)
        .bind(s.p50)
        .bind(s.p95)
        .bind(s.p99)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Minutes since `since` (unix seconds), oldest first.
    pub async fn history(&self, since: i64) -> Result<Vec<MinuteStat>, Error> {
        let rows = sqlx::query("SELECT * FROM stats_minute WHERE ts >= ? ORDER BY ts")
            .bind(since)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| MinuteStat {
                ts: r.get("ts"),
                queries: r.get("queries"),
                hits: r.get("hits"),
                misses: r.get("misses"),
                blocked: r.get("blocked"),
                servfail: r.get("servfail"),
                p50: r.get("p50"),
                p95: r.get("p95"),
                p99: r.get("p99"),
            })
            .collect())
    }

    /// Deletes traffic history older than `days` and audit entries older than `audit_days`.
    pub async fn prune(&self, days: i64, audit_days: i64) -> Result<(), Error> {
        sqlx::query("DELETE FROM stats_minute WHERE ts < ?")
            .bind(now() - days * 86_400)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM audit_log WHERE ts < ?")
            .bind(now() - audit_days * 86_400)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn audit_settings_and_history() {
        let s = Storage::memory().await.unwrap();
        s.audit("system", "mode_change", "normal → isolation")
            .await
            .unwrap();
        s.audit("admin", "login", "ok").await.unwrap();
        let log = s.audit_log(10).await.unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].action, "login", "newest first");

        assert_eq!(s.get_setting("x").await.unwrap(), None);
        s.set_setting("x", "1").await.unwrap();
        s.set_setting("x", "2").await.unwrap();
        assert_eq!(s.get_setting("x").await.unwrap().as_deref(), Some("2"));

        let m = MinuteStat {
            ts: now() - 60,
            queries: 10,
            hits: 7,
            ..Default::default()
        };
        s.record_minute(&m).await.unwrap();
        s.record_minute(&MinuteStat { queries: 12, ..m })
            .await
            .unwrap();
        let h = s.history(0).await.unwrap();
        assert_eq!(h.len(), 1, "same minute replaced");
        assert_eq!(h[0].queries, 12);
        s.prune(1, 1).await.unwrap();
        assert_eq!(s.history(0).await.unwrap().len(), 1, "recent rows kept");
    }
}
