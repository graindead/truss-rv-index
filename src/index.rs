//! The published index: zstd-compressed JSON (see FORMAT.md).

use crate::record::ChartData;
use crate::store::{Status, Store};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Index {
    /// Format version (FORMAT.md). Readers should refuse a major version they don't know.
    pub format: u32,
    /// When the index was written (UTC, RFC 3339).
    pub generated: String,
    /// The track hash layout: "scan-chart-2025" (current scan-chart / Clone Hero layout).
    pub hash_layout: String,
    pub charts: Vec<Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    /// RhythmVerse file_id.
    pub id: String,
    /// RhythmVerse update_date the data belongs to.
    pub m: String,
    /// "ok", "failed" or "unavailable".
    pub s: String,
    /// Why it isn't ok.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub r: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none", default)]
    pub d: Option<ChartData>,
}

pub fn build(store: &Store) -> Index {
    let charts = store
        .rows()
        .into_iter()
        .map(|row| Entry {
            id: row.file_id,
            m: row.modified,
            s: row.status.as_str().into(),
            r: if row.status == Status::Ok { None } else { row.reason },
            d: if row.status == Status::Ok { row.data } else { None },
        })
        .collect();
    Index { format: FORMAT_VERSION, generated: chrono::Utc::now().to_rfc3339(), hash_layout: "scan-chart-2025".into(), charts }
}

/// Writes the index (atomically) and returns its size in bytes.
pub fn write(index: &Index, path: &Path) -> Result<u64, String> {
    let json = serde_json::to_vec(index).map_err(|e| e.to_string())?;
    let packed = zstd::encode_all(&json[..], 19).map_err(|e| e.to_string())?;
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &packed).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(packed.len() as u64)
}

pub fn read(path: &Path) -> Result<Index, String> {
    let packed = std::fs::read(path).map_err(|e| e.to_string())?;
    let json = zstd::decode_all(&packed[..]).map_err(|e| e.to_string())?;
    serde_json::from_slice(&json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let mut d = ChartData::default();
        d.h.entry("guitar".into()).or_default().insert("x".into(), "abc".into());
        d.f = 5;
        d.an = 3.25;
        let index = Index {
            format: FORMAT_VERSION,
            generated: "now".into(),
            hash_layout: "scan-chart-2025".into(),
            charts: vec![
                Entry { id: "a".into(), m: "2026-01-01 00:00:00".into(), s: "ok".into(), r: None, d: Some(d.clone()) },
                Entry { id: "b".into(), m: "2026-01-02 00:00:00".into(), s: "unavailable".into(), r: Some("dead link".into()), d: None },
            ],
        };
        let path = std::env::temp_dir().join(format!("rv-index-test-{}.json.zst", std::process::id()));
        write(&index, &path).unwrap();
        let back = read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(back.charts.len(), 2);
        assert_eq!(back.charts[0].d.as_ref(), Some(&d));
        assert!(back.charts[1].d.is_none());
        assert_eq!(back.charts[1].r.as_deref(), Some("dead link"));
    }
}
