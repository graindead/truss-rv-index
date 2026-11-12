//! The scanner's working state (SQLite): one row per RhythmVerse chart, plus a few settings.
//! The published index is exported from this (see index.rs).

use crate::record::ChartData;
use crate::rhythmverse::{RvEntry, RvFile};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// Will be tried again later (network trouble, rate limits, Drive quota).
    Failed,
    /// Permanent for this version of the chart (dead link, unsupported host, no or bad chart).
    Unavailable,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Failed => "failed",
            Status::Unavailable => "unavailable",
        }
    }
    fn parse(s: &str) -> Status {
        match s {
            "ok" => Status::Ok,
            "unavailable" => Status::Unavailable,
            _ => Status::Failed,
        }
    }
}

pub struct Row {
    pub file_id: String,
    pub modified: String,
    pub status: Status,
    pub reason: Option<String>,
    pub data: Option<ChartData>,
}

pub struct Store {
    con: Connection,
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, String> {
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let con = Connection::open(path).map_err(|e| e.to_string())?;
        con.execute_batch(
            "pragma journal_mode = wal;
             create table if not exists charts (
                file_id text primary key,
                modified text not null,
                status text not null,
                reason text,
                data text,
                attempts integer not null default 0,
                next_retry integer not null default 0,
                last_seen integer not null default 0,
                scanned_at integer not null default 0,
                bytes integer not null default 0,
                ms integer not null default 0,
                download_url text,
                external_url text,
                page_url text,
                title text,
                artist text,
                charter text
             );
             create table if not exists meta (key text primary key, value text not null);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Store { con })
    }

    pub fn get_meta(&self, key: &str) -> Option<String> {
        self.con.query_row("select value from meta where key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_meta(&self, key: &str, value: &str) {
        let _ = self.con.execute("insert or replace into meta (key, value) values (?1, ?2)", [key, value]);
    }

    /// (modified, status) of a chart already in the state.
    pub fn known(&self, file_id: &str) -> Option<(String, Status)> {
        self.con
            .query_row("select modified, status from charts where file_id = ?1", [file_id], |r| {
                Ok((r.get::<_, String>(0)?, Status::parse(&r.get::<_, String>(1)?)))
            })
            .optional()
            .ok()
            .flatten()
    }

    /// True when this exact version is done (ok or permanently unavailable) or is a failure
    /// that is still waiting for its retry time.
    pub fn is_done(&self, e: &RvEntry) -> bool {
        matches!(self.known(&e.file_id), Some((m, s)) if m == e.modified && s != Status::Failed)
    }

    pub fn mark_seen(&self, file_id: &str, run: i64) {
        let _ = self.con.execute("update charts set last_seen = ?2 where file_id = ?1", params![file_id, run]);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save(&self, e: &RvEntry, status: Status, reason: Option<&str>, data: Option<&ChartData>, bytes: u64, ms: u64, run: i64) {
        let data = data.map(|d| serde_json::to_string(d).unwrap_or_default());
        // Failures back off: 1 h, 2 h, 4 h … up to a week.
        let prev: Option<(String, i64)> = self
            .con
            .query_row("select modified, attempts from charts where file_id = ?1", [&e.file_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .ok()
            .flatten();
        let attempts = match (&prev, status) {
            (_, Status::Ok | Status::Unavailable) => 0,
            (Some((m, a)), Status::Failed) if *m == e.modified => a + 1,
            _ => 1,
        };
        let next_retry = if status == Status::Failed { now() + (3600i64 << (attempts - 1).clamp(0, 7)).min(7 * 86400) } else { 0 };
        let _ = self.con.execute(
            "insert or replace into charts (file_id, modified, status, reason, data, attempts, next_retry, last_seen, scanned_at,
                bytes, ms, download_url, external_url, page_url, title, artist, charter)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                e.file_id,
                e.modified,
                status.as_str(),
                reason,
                data,
                attempts,
                next_retry,
                run,
                now(),
                bytes as i64,
                ms as i64,
                e.rv.download_url,
                e.rv.external_url,
                e.rv.page_url,
                e.title,
                e.artist,
                e.charter
            ],
        );
    }

    /// (file_id, scanned_at) of every chart, for tests.
    pub fn scanned_times(&self) -> Vec<(String, i64)> {
        let mut st = self.con.prepare("select file_id, scanned_at from charts where scanned_at > 0").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// Charts done for good (ok or unavailable).
    pub fn count_done(&self) -> u64 {
        self.con.query_row("select count(*) from charts where status != 'failed'", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64
    }

    /// Failed charts whose retry time has come.
    pub fn due_retries(&self, limit: usize) -> Vec<RvEntry> {
        let mut st = self
            .con
            .prepare(
                "select file_id, modified, download_url, external_url, page_url, title, artist, charter from charts
                 where status = 'failed' and next_retry <= ?1 order by next_retry limit ?2",
            )
            .unwrap();
        st.query_map(params![now(), limit as i64], |r| {
            Ok(RvEntry {
                file_id: r.get(0)?,
                modified: r.get(1)?,
                rv: RvFile { download_url: r.get(2)?, external_url: r.get(3)?, page_url: r.get::<_, Option<String>>(4)?.unwrap_or_default(), title: r.get(5)? },
                title: r.get(5)?,
                artist: r.get(6)?,
                charter: r.get(7)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Drops charts not seen in a full pass (deleted from RhythmVerse). Returns how many.
    pub fn drop_unseen(&self, run: i64) -> usize {
        self.con.execute("delete from charts where last_seen < ?1", [run]).unwrap_or(0)
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut st = self.con.prepare("select file_id, modified, status, reason, data from charts order by file_id").unwrap();
        st.query_map([], |r| {
            Ok(Row {
                file_id: r.get(0)?,
                modified: r.get(1)?,
                status: Status::parse(&r.get::<_, String>(2)?),
                reason: r.get(3)?,
                data: r.get::<_, Option<String>>(4)?.and_then(|d| serde_json::from_str(&d).ok()),
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Scan statistics for charts scanned at or after `since` (unix seconds).
    pub fn stats(&self, since: i64) -> Stats {
        let mut s = Stats::default();
        let mut st = self.con.prepare("select status, reason, bytes, ms from charts where scanned_at >= ?1").unwrap();
        let rows = st
            .query_map([since], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))
            .map(|rows| rows.flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        for (status, reason, bytes, ms) in rows {
            s.total += 1;
            s.bytes += bytes as u64;
            s.ms += ms as u64;
            s.max_bytes = s.max_bytes.max(bytes as u64);
            match Status::parse(&status) {
                Status::Ok => s.ok += 1,
                Status::Failed => s.failed += 1,
                Status::Unavailable => s.unavailable += 1,
            }
            if let Some(r) = reason {
                let key = reason_bucket(&r);
                *s.reasons.entry(key).or_default() += 1;
            }
        }
        s
    }
}

/// Groups failure messages into a few buckets for the statistics.
fn reason_bucket(r: &str) -> String {
    let cut: String = r.chars().take(70).collect();
    cut
}

#[derive(Debug, Default)]
pub struct Stats {
    pub total: u64,
    pub ok: u64,
    pub failed: u64,
    pub unavailable: u64,
    pub bytes: u64,
    pub max_bytes: u64,
    pub ms: u64,
    pub reasons: std::collections::BTreeMap<String, u64>,
}
