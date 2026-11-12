//! What the index stores about one chart, worked out from its note files.

use crate::chart::{DrumType, ParsedChart, Track, nf, nt};
use crate::fretwork::Rating;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Feature bits (see FORMAT.md).
pub mod feat {
    pub const SOLO: u32 = 1 << 0;
    pub const FORCED: u32 = 1 << 1;
    pub const OPEN: u32 = 1 << 2;
    pub const TAP: u32 = 1 << 3;
    pub const LYRICS: u32 = 1 << 4;
    pub const VOCALS: u32 = 1 << 5;
    pub const ROLL_LANES: u32 = 1 << 6;
    pub const KICK_2X: u32 = 1 << 7;
    pub const PRO_DRUMS: u32 = 1 << 8;
    pub const FIVE_LANE_DRUMS: u32 = 1 << 9;
}

/// One instrument's Expert rating and note density.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstrumentStats {
    /// Fretwork open-ended tier.
    pub t: u32,
    /// Fretwork 0–6 remapped tier.
    pub r: u8,
    /// Fretwork difficulty score.
    pub d: f64,
    /// Movement per second.
    pub mv: f64,
    /// Note-rate spread (coefficient of variation).
    pub sp: f64,
    /// Drums: single-pedal kicks per second.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub k: Option<f64>,
    /// Drums: tier counting 2x kick.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub t2: Option<u32>,
}

/// The data part of a scanned chart.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartData {
    /// instrument -> difficulty letter (x/h/m/e) -> leaderboard track hash (current layout).
    pub h: BTreeMap<String, BTreeMap<String, String>>,
    /// instrument -> Fretwork rating of its Expert track (instruments Fretwork rates).
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub w: BTreeMap<String, InstrumentStats>,
    /// Feature bits (`feat`).
    pub f: u32,
    /// Average notes per second (densest instrument's Expert track), 2 decimals.
    pub an: f64,
    /// Max notes per second in any 1 s window (any Expert track), 2 decimals.
    pub mn: f64,
    /// scan-chart chart issue types found.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub i: Vec<String>,
}

pub fn diff_letter(d: &str) -> &'static str {
    match d {
        "expert" => "x",
        "hard" => "h",
        "medium" => "m",
        _ => "e",
    }
}

fn round(v: f64, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v * m).round() / m
}

/// Picks and parses the chart file the way trackhash::hash_files does (notes.* first, .mid
/// before .chart; song.ini or the last .ini for modifiers).
pub fn parse(files: &[(String, Vec<u8>)]) -> Result<ParsedChart, String> {
    let lower = |n: &str| n.rsplit(['/', '\\']).next().unwrap_or(n).to_ascii_lowercase();
    let is_chart = |n: &str| n.ends_with(".chart") || n.ends_with(".mid");
    let mut charts: Vec<&(String, Vec<u8>)> = files.iter().filter(|(n, _)| is_chart(&lower(n))).collect();
    charts.sort_by_key(|(n, _)| {
        let n = lower(n);
        (!(n == "notes.chart" || n == "notes.mid"), !n.ends_with(".mid"))
    });
    let (name, data) = charts.first().ok_or("This chart has no notes.chart or notes.mid.")?;
    let inis: Vec<&(String, Vec<u8>)> = files.iter().filter(|(n, _)| lower(n).ends_with(".ini")).collect();
    let ini = inis.iter().find(|(n, _)| lower(n) == "song.ini").or(inis.last());
    let mods = ini.map(|(_, d)| crate::trackhash::modifiers_from_ini(&crate::trackhash::ini_pairs(d))).unwrap_or_default();
    crate::chart::parse_chart_file(data, lower(name).ends_with(".mid"), &mods)
}

/// (average, max) note groups per second of one track.
pub fn nps(t: &Track) -> (f64, f64) {
    let times: Vec<f64> = t.note_groups.iter().map(|g| g[0].ms).collect();
    let (Some(first), Some(last)) = (times.first(), times.last()) else { return (0.0, 0.0) };
    let span = (last - first) / 1000.0;
    let avg = if span > 0.0 { times.len() as f64 / span } else { 0.0 };
    let mut max = 0usize;
    let mut l = 0;
    for r in 0..times.len() {
        while times[r] - times[l] >= 1000.0 {
            l += 1;
        }
        max = max.max(r - l + 1);
    }
    (avg, max as f64)
}

fn rating_stats(r: &Rating) -> InstrumentStats {
    InstrumentStats {
        t: r.tier,
        r: r.remap,
        d: round(r.d, 3),
        mv: round(r.movement, 3),
        sp: round(r.spread, 3),
        k: r.kicks.map(|k| round(k, 3)),
        t2: r.tier_2x,
    }
}

/// Everything the index keeps about a chart, from its note files.
pub fn compute(files: &[(String, Vec<u8>)]) -> Result<ChartData, String> {
    // Hashes and ratings come from Truss's own function, so they match Truss exactly.
    let computed = crate::trackhash::hash_files(files, None)?;
    let parsed = parse(files)?;
    let mut d = ChartData::default();
    for (inst, diff, hash) in computed.tracks {
        d.h.entry(inst).or_default().insert(diff_letter(&diff).to_string(), hash);
    }
    for (inst, r) in &computed.ratings {
        d.w.insert(inst.clone(), rating_stats(r));
    }
    let mut f = 0;
    let fret = |t: &Track| !matches!(t.instrument, "drums");
    for t in &parsed.tracks {
        if !t.solo.is_empty() {
            f |= feat::SOLO;
        }
        if !t.flex.is_empty() {
            f |= feat::ROLL_LANES;
        }
        for n in t.note_groups.iter().flatten() {
            if fret(t) && n.ty == nt::OPEN {
                f |= feat::OPEN;
            }
            if fret(t) && n.flags & nf::TAP != 0 {
                f |= feat::TAP;
            }
            if t.instrument == "drums" && n.flags & nf::DOUBLE_KICK != 0 {
                f |= feat::KICK_2X;
            }
        }
        if t.difficulty == "expert" {
            let (a, m) = nps(t);
            d.an = d.an.max(round(a, 2));
            d.mn = d.mn.max(m);
        }
    }
    if parsed.has_forced_notes {
        f |= feat::FORCED;
    }
    if parsed.has_lyrics {
        f |= feat::LYRICS;
    }
    if parsed.has_vocals {
        f |= feat::VOCALS;
    }
    if parsed.tracks.iter().any(|t| t.instrument == "drums") {
        match parsed.drum_type {
            Some(DrumType::FourLanePro) => f |= feat::PRO_DRUMS,
            Some(DrumType::FiveLane) => f |= feat::FIVE_LANE_DRUMS,
            _ => {}
        }
    }
    d.f = f;
    d.i = crate::chart_issues::chart_issue_kinds(&parsed).into_iter().map(String::from).collect();
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHART: &str = "[Song]\n{\n  Resolution = 192\n}\n[SyncTrack]\n{\n  0 = TS 4\n  0 = B 120000\n}\n[Events]\n{\n  0 = E \"section Intro\"\n}\n\
[ExpertSingle]\n{\n  768 = E solo\n  768 = N 0 0\n  864 = N 1 0\n  960 = N 7 0\n  1056 = N 2 0\n  1056 = N 6 0\n  1152 = N 3 0\n  1152 = N 5 0\n  1248 = E soloend\n  1344 = N 4 0\n}\n";

    #[test]
    fn computes_hashes_features_and_nps() {
        let files = vec![("notes.chart".to_string(), CHART.as_bytes().to_vec()), ("song.ini".to_string(), b"[song]\nname = x\n".to_vec())];
        let d = compute(&files).unwrap();
        let g = &d.h["guitar"];
        assert_eq!(g.len(), 1);
        assert_eq!(g["x"].len(), 43);
        // Same hash as Truss's function.
        let truss = crate::trackhash::hash_files(&files, None).unwrap();
        assert_eq!(truss.tracks[0].2, g["x"]);
        assert!(d.f & feat::SOLO != 0, "solo");
        assert!(d.f & feat::OPEN != 0, "open");
        assert!(d.f & feat::TAP != 0, "tap");
        assert!(d.f & feat::LYRICS == 0 && d.f & feat::VOCALS == 0);
        assert!(d.w.contains_key("guitar"));
        // 6 note groups over 1.5 s at 120 BPM (96 ticks = 0.25 s).
        assert_eq!(d.mn, 4.0);
        assert!((d.an - 4.0).abs() < 0.01, "{}", d.an);
    }

    #[test]
    fn forced_notes_flag() {
        // Two quick notes: the second is naturally a HOPO; forcing it (N 5) makes it a strum.
        let forced = CHART.replace("  864 = N 1 0\n", "  864 = N 1 0\n  864 = N 5 0\n");
        let files = vec![("notes.chart".to_string(), forced.into_bytes())];
        assert!(compute(&files).unwrap().f & feat::FORCED != 0);
        let plain = "[Song]\n{\n  Resolution = 192\n}\n[SyncTrack]\n{\n  0 = B 120000\n}\n[ExpertSingle]\n{\n  0 = N 0 0\n  48 = N 1 0\n  480 = N 2 0\n}\n";
        let files = vec![("notes.chart".to_string(), plain.as_bytes().to_vec())];
        assert!(compute(&files).unwrap().f & feat::FORCED == 0);
    }

    #[test]
    fn nps_window() {
        use crate::chart::Note;
        let mk = |ms: f64| vec![Note { tick: 0, length: 0, ms, ms_length: 0.0, ty: nt::GREEN, flags: 0 }];
        let t = Track {
            instrument: "guitar",
            difficulty: "expert",
            star_power: vec![],
            solo: vec![],
            flex: vec![],
            freestyle: vec![],
            rejected_star_power: vec![],
            note_groups: [0.0, 100.0, 200.0, 999.0, 1000.0, 3000.0].iter().map(|&m| mk(m)).collect(),
        };
        let (a, m) = nps(&t);
        assert_eq!(m, 4.0);
        assert!((a - 2.0).abs() < 1e-9);
    }
}
