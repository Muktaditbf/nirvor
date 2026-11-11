//! Operator blocklists from files or URLs, hot-reloaded.
//!
//! Accepted line formats: `example.com` (that name only), `*.example.com` (names below it),
//! hosts files (`0.0.0.0 example.com`), and `||example.com^` (the name and everything below it).
//! `#` and `!` start comments.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// One blocklist source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlocklistSource {
    /// Label shown in the dashboard.
    pub name: String,
    /// Local file (set this or `url`).
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// HTTP(S) URL (set this or `path`).
    #[serde(default)]
    pub url: Option<String>,
    /// Disabled lists are kept in the config but not loaded.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Seconds between URL downloads.
    #[serde(default = "d_refresh")]
    pub refresh_secs: u64,
}

fn yes() -> bool {
    true
}
fn d_refresh() -> u64 {
    6 * 3600
}

/// Parsed entries.
#[derive(Debug, Default, Clone)]
pub struct Blocklist {
    exact: HashSet<String>,
    below: HashSet<String>,
}

impl Blocklist {
    /// Parses list text. Returns the list and the number of lines skipped as invalid.
    pub fn parse(text: &str) -> (Self, usize) {
        let mut list = Self::default();
        let mut bad = 0;
        for raw in text.lines() {
            let line = raw.split(['#', '!']).next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut tokens: Vec<&str> = line.split_whitespace().collect();
            // hosts format: an address followed by names
            if tokens.len() > 1 && tokens[0].parse::<std::net::IpAddr>().is_ok() {
                tokens.remove(0);
            }
            for t in tokens {
                let (name, exact, below) =
                    if let Some(d) = t.strip_prefix("||").and_then(|d| d.strip_suffix('^')) {
                        (d, true, true)
                    } else if let Some(d) = t.strip_prefix("*.") {
                        (d, false, true)
                    } else {
                        (t, true, false)
                    };
                let Some(name) = normalize(name) else {
                    bad += 1;
                    continue;
                };
                if matches!(
                    name.as_str(),
                    "localhost" | "localhost.localdomain" | "local" | "broadcasthost"
                ) {
                    continue; // standard hosts-file lines, never block these
                }
                if exact {
                    list.exact.insert(name.clone());
                }
                if below {
                    list.below.insert(name);
                }
            }
        }
        (list, bad)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.exact.len() + self.below.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn merge(&mut self, other: &Blocklist) {
        self.exact.extend(other.exact.iter().cloned());
        self.below.extend(other.below.iter().cloned());
    }

    /// True if `name` (any case, trailing dot optional) is blocked.
    pub fn is_blocked(&self, name: &str) -> bool {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if self.exact.contains(&name) {
            return true;
        }
        let mut rest = name.as_str();
        while let Some((_, parent)) = rest.split_once('.') {
            if self.below.contains(parent) {
                return true;
            }
            rest = parent;
        }
        false
    }
}

/// Lower-cased name without trailing dot, if it looks like a valid domain name.
fn normalize(s: &str) -> Option<String> {
    let s = s.trim_end_matches('.').to_ascii_lowercase();
    let ok = !s.is_empty()
        && s.len() <= 253
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    ok.then_some(s)
}

/// Load status of one source.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BlocklistStatus {
    /// Source name.
    pub name: String,
    /// Where it comes from.
    pub source: String,
    /// Enabled in config.
    pub enabled: bool,
    /// Entries loaded.
    pub entries: usize,
    /// Lines skipped as invalid.
    pub invalid_lines: usize,
    /// Unix seconds of the last successful load.
    pub loaded_at: Option<u64>,
    /// Last error.
    pub error: Option<String>,
}

#[derive(Default)]
struct Loaded {
    list: Blocklist,
    status: BlocklistStatus,
    file_mtime: Option<SystemTime>,
}

/// Holds all sources and the merged list.
#[derive(Clone, Default)]
pub struct BlocklistManager {
    merged: Arc<RwLock<Arc<Blocklist>>>,
    sources: Arc<RwLock<Vec<(BlocklistSource, Loaded)>>>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl BlocklistManager {
    /// The merged list in use.
    pub fn current(&self) -> Arc<Blocklist> {
        self.merged
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Status of every source.
    pub fn status(&self) -> Vec<BlocklistStatus> {
        self.sources
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(_, l)| l.status.clone())
            .collect()
    }

    /// Replaces the source list and loads everything now.
    pub async fn set_sources(&self, sources: Vec<BlocklistSource>) {
        {
            let mut s = self.sources.write().unwrap_or_else(|p| p.into_inner());
            *s = sources
                .into_iter()
                .map(|src| {
                    let status = BlocklistStatus {
                        name: src.name.clone(),
                        source: src.path.as_ref().map_or_else(
                            || src.url.clone().unwrap_or_default(),
                            |p| p.display().to_string(),
                        ),
                        enabled: src.enabled,
                        ..Default::default()
                    };
                    (
                        src,
                        Loaded {
                            status,
                            ..Default::default()
                        },
                    )
                })
                .collect();
        }
        self.reload(true).await;
    }

    /// Reloads sources. With `force`, every enabled source; otherwise only changed files and due URLs.
    pub async fn reload(&self, force: bool) {
        let snapshot: Vec<(usize, BlocklistSource, Option<SystemTime>, Option<u64>)> = self
            .sources
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .enumerate()
            .map(|(i, (s, l))| (i, s.clone(), l.file_mtime, l.status.loaded_at))
            .collect();
        let mut changed = false;
        for (i, src, old_mtime, loaded_at) in snapshot {
            if !src.enabled {
                continue;
            }
            let result: Result<(String, Option<SystemTime>), String> = if let Some(path) = &src.path
            {
                let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
                if !force && mtime.is_some() && mtime == old_mtime {
                    continue;
                }
                tokio::fs::read_to_string(path)
                    .await
                    .map(|t| (t, mtime))
                    .map_err(|e| e.to_string())
            } else if let Some(url) = &src.url {
                let due =
                    loaded_at.is_none_or(|t| now_unix().saturating_sub(t) >= src.refresh_secs);
                if !force && !due {
                    continue;
                }
                fetch(url).await.map(|t| (t, None))
            } else {
                continue;
            };
            let mut sources = self.sources.write().unwrap_or_else(|p| p.into_inner());
            let Some((_, loaded)) = sources.get_mut(i) else {
                continue;
            };
            match result {
                Ok((text, mtime)) => {
                    let (list, bad) = Blocklist::parse(&text);
                    info!(list = %src.name, entries = list.len(), invalid = bad, "blocklist loaded");
                    loaded.status.entries = list.len();
                    loaded.status.invalid_lines = bad;
                    loaded.status.loaded_at = Some(now_unix());
                    loaded.status.error = None;
                    loaded.list = list;
                    loaded.file_mtime = mtime;
                    changed = true;
                }
                Err(e) => {
                    // Keep the previous contents: a failed download must not unblock everything.
                    warn!(list = %src.name, error = %e, "blocklist load failed");
                    loaded.status.error = Some(e);
                }
            }
        }
        if changed || force {
            let mut merged = Blocklist::default();
            for (src, l) in self
                .sources
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
            {
                if src.enabled {
                    merged.merge(&l.list);
                }
            }
            *self.merged.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(merged);
        }
    }

    /// Checks for changes every `every`.
    pub async fn run(self, every: Duration) {
        loop {
            tokio::time::sleep(every).await;
            self.reload(false).await;
        }
    }
}

async fn fetch(url: &str) -> Result<String, String> {
    // reqwest is built without a default TLS backend; make sure ring is installed (no-op if it already is).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("nirvor/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    resp.text().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_formats() {
        let (l, bad) = Blocklist::parse(
            "# comment\nexact.example\n*.wild.example\n0.0.0.0 hosts.example other.example\n127.0.0.1 localhost\n||ab.example^\nbad*name\n! adblock comment\n",
        );
        assert_eq!(bad, 1);
        assert!(l.is_blocked("exact.example"));
        assert!(l.is_blocked("EXACT.example."));
        assert!(
            !l.is_blocked("sub.exact.example"),
            "plain entries are exact"
        );
        assert!(l.is_blocked("a.b.wild.example"));
        assert!(
            !l.is_blocked("wild.example"),
            "wildcard does not cover the apex"
        );
        assert!(l.is_blocked("hosts.example") && l.is_blocked("other.example"));
        assert!(!l.is_blocked("localhost"));
        assert!(l.is_blocked("ab.example") && l.is_blocked("x.ab.example"));
        assert!(!l.is_blocked("example"));
    }

    #[tokio::test]
    async fn file_source_hot_reload_keeps_list_on_error() {
        let dir = std::env::temp_dir().join(format!("nirvor-bl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("list.txt");
        std::fs::write(&path, "one.example\n").unwrap();
        let m = BlocklistManager::default();
        m.set_sources(vec![BlocklistSource {
            name: "test".into(),
            path: Some(path.clone()),
            url: None,
            enabled: true,
            refresh_secs: 60,
        }])
        .await;
        assert!(m.current().is_blocked("one.example"));
        assert_eq!(m.status()[0].entries, 1);

        // Change the file (with a different mtime) and reload: picked up.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&path, "two.example\n").unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        m.reload(false).await;
        assert!(m.current().is_blocked("two.example"));
        assert!(!m.current().is_blocked("one.example"));

        // File disappears: the old list stays and the error is reported.
        std::fs::remove_file(&path).unwrap();
        m.reload(true).await;
        assert!(m.current().is_blocked("two.example"));
        assert!(m.status()[0].error.is_some());
        std::fs::remove_dir_all(dir).ok();
    }
}
