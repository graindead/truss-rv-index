//! Fretwork difficulty ratings: a port of the scoring in Fretwork by Staycation
//! (github.com/Staycation44/fretwork, MIT; formulas v3, see its Methodology.md). Only the maths
//! is ported; it runs on notes Truss has already read and never writes to song files.
//!
//! A 1 s window slides over the Expert chart in 250 ms steps, measuring note rate and how much
//! the hand moves. Peak, average and median combine into a score `d`, which maps to a 0–6 tier
//! calibrated against official tiers (`remap`) and an open-ended log tier (`tier`).

use crate::chart::{Track, nf, nt};

const WINDOW_MS: f64 = 1000.0;
const STEP_MS: f64 = 250.0;
const T_REF: f64 = 230.0;
const S_STAM: f64 = 0.20;
const ROLL_CAP_HPS: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Rating {
    /// Fretwork's difficulty score.
    pub d: f64,
    /// 0–6, spread like official tiers.
    pub remap: u8,
    /// Open-ended (very hard charts go past 6).
    pub tier: u32,
    /// Average hand movement per second: fret changes (5-fret) or newly struck pads (drums).
    pub movement: f64,
    /// How much the note rate swings over the song (Fretwork's coefficient of variation of
    /// notes or hits per second): low for an even chart, high for one with spikes.
    pub spread: f64,
    /// Drums: average single-pedal kicks per second.
    pub kicks: Option<f64>,
    /// Drums: the open-ended tier counting double-kick notes too (when the chart has any).
    pub tier_2x: Option<u32>,
}

/// Per-second rate summaries: peak, average over the song, median and spread of active windows.
#[derive(Debug, Default, Clone, Copy)]
struct Stats {
    p: f64,
    a: f64,
    med: f64,
    std: f64,
}

/// Sums `values` (at sorted `times`) over each [start, start + 1 s) window of a grid from 0 to
/// the last event.
fn windowed(times: &[f64], values: &[f64], last: f64) -> Vec<f64> {
    let n = (last / STEP_MS).floor() as usize + 1;
    let mut prefix = Vec::with_capacity(values.len() + 1);
    prefix.push(0.0);
    for v in values {
        prefix.push(prefix.last().unwrap() + v);
    }
    let (mut l, mut r) = (0, 0);
    (0..n)
        .map(|i| {
            let start = i as f64 * STEP_MS;
            while l < times.len() && times[l] < start {
                l += 1;
            }
            r = r.max(l);
            while r < times.len() && times[r] < start + WINDOW_MS {
                r += 1;
            }
            prefix[r] - prefix[l]
        })
        .collect()
}

/// numpy-style median and population standard deviation.
fn median_std(v: &mut [f64]) -> (f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    let med = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
    let mean = v.iter().sum::<f64>() / n as f64;
    let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    (med, var.sqrt())
}

fn stats(windows: &[f64], total: f64, dur_s: f64, active: &[bool]) -> Stats {
    let mut act: Vec<f64> = windows.iter().zip(active).filter(|(_, a)| **a).map(|(w, _)| *w).collect();
    let (med, std) = median_std(&mut act);
    Stats {
        p: windows.iter().cloned().fold(0.0, f64::max),
        a: if dur_s > 0.0 { total / dur_s } else { 0.0 },
        med,
        std,
    }
}

/// Pseudo-geometric mean of median (guarded by epsilon), average and peak.
fn combo(s: Stats) -> f64 {
    ((s.med + s.a * 0.05) * s.a * s.p).cbrt()
}

fn cv(s: Stats) -> f64 {
    if s.med + s.a > 0.0 { s.std / (s.med + s.a) } else { 0.0 }
}

fn stam(dur_s: f64) -> f64 {
    if dur_s > 0.0 { (dur_s / T_REF).powf(S_STAM) } else { 0.0 }
}

fn remap(d: f64, bins: &[f64; 6]) -> u8 {
    if d <= 0.0 {
        return 0;
    }
    bins.iter().position(|&upper| d <= upper).unwrap_or(6) as u8
}

fn calc_tier(d: f64, base: f64, inc: f64) -> u32 {
    if d < base { 0 } else { ((d / base).ln() / inc).floor() as u32 + 1 }
}

/// Rates one instrument's Expert track. `None` for instruments Fretwork doesn't rate
/// (6-fret, vocals) or tracks with no notes.
pub fn rate(instrument: &str, expert: &Track) -> Option<Rating> {
    match instrument {
        "guitar" | "guitarcoop" | "rhythm" | "bass" | "keys" => rate_frets(instrument, expert),
        "drums" => rate_drums(expert),
        _ => None,
    }
}

fn rate_frets(instrument: &str, t: &Track) -> Option<Rating> {
    // One entry per chord/note: its time and fret mask (bits 0-4 GRYBO, bit 7 open).
    let mut times = Vec::new();
    let mut masks: Vec<u8> = Vec::new();
    for g in &t.note_groups {
        let mut m = 0u8;
        for n in g {
            m |= match n.ty {
                nt::OPEN => 1 << 7,
                ty @ nt::GREEN..=nt::ORANGE => 1 << (ty - nt::GREEN),
                _ => 0,
            };
        }
        if m != 0 {
            times.push(g[0].ms);
            masks.push(m);
        }
    }
    let last = *times.last()?;
    // Fret changes: the larger of frets lifted and frets added (the first note adds all).
    let mut changes = Vec::with_capacity(masks.len());
    let mut prev = 0u8;
    for &m in &masks {
        changes.push((prev & !m).count_ones().max((m & !prev).count_ones()) as f64);
        prev = m;
    }
    let dur_s = last / 1000.0;
    let nps = windowed(&times, &vec![1.0; times.len()], last);
    let vps = windowed(&times, &changes, last);
    let active: Vec<bool> = nps.iter().map(|v| *v > 0.0).collect();
    let n = stats(&nps, times.len() as f64, dur_s, &active);
    let v = stats(&vps, changes.iter().sum(), dur_s, &active);
    let cov = 1.0 + (cv(n) * cv(v)).sqrt();
    let d = (combo(n) + combo(v)) * cov * stam(dur_s);
    let bins = match instrument {
        "bass" => [5.1, 7.2, 8.6, 10.2, 11.9, 14.1],
        "keys" => [3.5, 6.0, 7.8, 9.7, 11.8, 13.6],
        _ => [7.2, 8.6, 10.3, 12.0, 14.0, 17.1],
    };
    Some(Rating {
        d,
        remap: remap(d, &bins),
        tier: calc_tier(d, 7.101, 0.1792),
        movement: v.a,
        spread: cv(n),
        kicks: None,
        tier_2x: None,
    })
}

fn rate_drums(t: &Track) -> Option<Rating> {
    // Hands: lane mask per hit (red, yellow, blue, green). Kicks: single-pedal hits only, since
    // Fretwork anchors its tiers to the 1x reading.
    // Double-kick notes (2x pedal) are kept apart for the 2x reading.
    let (mut hand_t, mut hand_m, mut kick_t, mut kick2_t) = (Vec::new(), Vec::<u8>::new(), Vec::new(), Vec::new());
    for g in &t.note_groups {
        let mut m = 0u8;
        let (mut kick, mut kick2) = (false, false);
        for n in g {
            match n.ty {
                nt::KICK if n.flags & nf::DOUBLE_KICK == 0 => kick = true,
                nt::KICK => kick2 = true,
                ty @ nt::RED_DRUM..=nt::GREEN_DRUM => m |= 1 << (ty - nt::RED_DRUM),
                _ => {}
            }
        }
        if m != 0 {
            hand_t.push(g[0].ms);
            hand_m.push(m);
        }
        if kick {
            kick_t.push(g[0].ms);
        }
        if kick || kick2 {
            kick2_t.push(g[0].ms);
        }
    }
    let has_2x = kick2_t.len() > kick_t.len();
    let last = *hand_t.last()?;
    let dur_s = last.max(kick_t.last().copied().unwrap_or(0.0)) / 1000.0;

    // Roll lanes: hits inside count at a capped rate and add no travel.
    let rolls: Vec<(f64, f64)> = t.flex.iter().map(|(s, _)| (s.ms, s.ms + s.ms_length)).collect();
    let in_roll = |x: f64| rolls.iter().any(|&(a, b)| x >= a && x < b);
    let (mut normal, mut rolled, mut travel) = (Vec::new(), Vec::new(), Vec::new());
    let mut prev = 0u8;
    for (i, &m) in hand_m.iter().enumerate() {
        let hits = m.count_ones() as f64;
        let added = (m & !prev).count_ones() as f64;
        prev = m;
        if in_roll(hand_t[i]) {
            normal.push(0.0);
            rolled.push(hits);
            travel.push(0.0);
        } else {
            normal.push(hits);
            rolled.push(0.0);
            travel.push(added);
        }
    }
    let hn = windowed(&hand_t, &normal, last);
    let hr = windowed(&hand_t, &rolled, last);
    let hps: Vec<f64> = hn.iter().zip(&hr).map(|(a, b)| a + b.min(ROLL_CAP_HPS)).collect();
    let tps = windowed(&hand_t, &travel, last);
    let roll_cap_total = ROLL_CAP_HPS * rolls.iter().map(|(a, b)| (b - a) / 1000.0).sum::<f64>();
    let total_hits = normal.iter().sum::<f64>() + rolled.iter().sum::<f64>().min(roll_cap_total);
    let active: Vec<bool> = hps.iter().map(|v| *v > 0.0).collect();
    let h = stats(&hps, total_hits, dur_s, &active);
    let tr = stats(&tps, travel.iter().sum(), dur_s, &active);

    // One kick reading: (combined kick score, its variation, average kicks per second).
    let kick_reading = |times: &[f64]| match times.last() {
        Some(&kl) => {
            let kps = windowed(times, &vec![1.0; times.len()], kl);
            let act: Vec<bool> = kps.iter().map(|v| *v > 0.0).collect();
            let s = stats(&kps, times.len() as f64, dur_s, &act);
            (combo(s), cv(s), s.a)
        }
        None => (0.0, 0.0, 0.0),
    };
    let score = |(k, cvk, _): (f64, f64, f64)| (combo(h) + combo(tr) + k) * (1.0 + 2.0 * (cv(h) * cvk).sqrt()) * stam(dur_s);
    let single = kick_reading(&kick_t);
    let d = score(single);
    let tier_2x = has_2x.then(|| calc_tier(score(kick_reading(&kick2_t)), 10.115, 0.1516));
    Some(Rating {
        d,
        remap: remap(d, &[9.8, 12.1, 13.6, 15.6, 18.5, 21.8]),
        tier: calc_tier(d, 10.115, 0.1516),
        movement: tr.a,
        spread: cv(h),
        kicks: Some(single.2),
        tier_2x,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TRUSS_TEST_FRETWORK_DIRS=<dir>|<dir>...: print ratings of chart folders (read only).
    #[test]
    fn rate_real_folders() {
        let Ok(dirs) = std::env::var("TRUSS_TEST_FRETWORK_DIRS") else { return };
        for dir in dirs.split('|') {
            let files: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
                .unwrap()
                .flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                    n.ends_with(".mid") || n.ends_with(".chart") || n.ends_with(".ini")
                })
                .map(|e| (e.file_name().to_string_lossy().to_string(), std::fs::read(e.path()).unwrap()))
                .collect();
            let cp = crate::trackhash::hash_files(&files, None).unwrap();
            let name = std::path::Path::new(dir).file_name().unwrap().to_string_lossy().to_string();
            for (i, r) in cp.ratings {
                println!("{name} | {i}: D {:.2} remap {} tier {}", r.d, r.remap, r.tier);
            }
        }
    }

    /// TRUSS_TEST_FRETWORK_ROOT=<folder>: rate up to 400 chart folders under it (read only) and
    /// print the spread of the new numbers, for choosing filter cut-offs.
    #[test]
    fn sample_distribution() {
        let Ok(root) = std::env::var("TRUSS_TEST_FRETWORK_ROOT") else { return };
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            let mut has_chart = false;
            let mut subs = Vec::new();
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                if n == "notes.chart" || n == "notes.mid" {
                    has_chart = true;
                } else if e.file_type().is_ok_and(|t| t.is_dir()) {
                    subs.push(e.path());
                }
            }
            if has_chart {
                out.push(dir.to_path_buf());
            }
            for s in subs {
                if out.len() < 400 {
                    walk(&s, out);
                }
            }
        }
        let mut dirs = Vec::new();
        walk(std::path::Path::new(&root), &mut dirs);
        let mut by: std::collections::BTreeMap<String, Vec<Rating>> = Default::default();
        for d in dirs.iter().take(400) {
            let files: Vec<(String, Vec<u8>)> = std::fs::read_dir(d)
                .unwrap()
                .flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                    n.ends_with(".mid") || n.ends_with(".chart") || n.ends_with(".ini")
                })
                .filter_map(|e| Some((e.file_name().to_string_lossy().to_string(), std::fs::read(e.path()).ok()?)))
                .collect();
            if let Ok(cp) = crate::trackhash::hash_files(&files, None) {
                for (i, r) in cp.ratings {
                    by.entry(i).or_default().push(r);
                }
            }
        }
        let pct = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.total_cmp(b));
            let at = |p: f64| v.get(((v.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0.0);
            format!("p10 {:.2} p33 {:.2} p50 {:.2} p67 {:.2} p90 {:.2}", at(0.1), at(0.33), at(0.5), at(0.67), at(0.9))
        };
        println!("rated {} folders", dirs.len().min(400));
        for (i, rs) in &by {
            println!("{i} (n={}): spread {}", rs.len(), pct(rs.iter().map(|r| r.spread).collect()));
            println!("{i}: movement {}", pct(rs.iter().map(|r| r.movement).collect()));
            if i == "drums" {
                println!("drums: kicks {}", pct(rs.iter().filter_map(|r| r.kicks).collect()));
                let with2x = rs.iter().filter(|r| r.tier_2x.is_some()).count();
                println!("drums: {with2x} with 2x kick; tier vs 2x: {:?}", rs.iter().filter_map(|r| r.tier_2x.map(|t| (r.tier, t))).take(8).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn windows_and_bins() {
        // Notes at 0, 500, 1000 ms: windows start 0,250,...,1000.
        let w = windowed(&[0.0, 500.0, 1000.0], &[1.0, 1.0, 1.0], 1000.0);
        assert_eq!(w, vec![2.0, 2.0, 2.0, 1.0, 1.0]);
        assert_eq!(median_std(&mut [1.0, 3.0, 2.0, 4.0]).0, 2.5);
        assert_eq!(remap(7.2, &[7.2, 8.6, 10.3, 12.0, 14.0, 17.1]), 0);
        assert_eq!(remap(7.21, &[7.2, 8.6, 10.3, 12.0, 14.0, 17.1]), 1);
        assert_eq!(remap(99.0, &[7.2, 8.6, 10.3, 12.0, 14.0, 17.1]), 6);
        assert_eq!(calc_tier(7.0, 7.101, 0.1792), 0);
        assert_eq!(calc_tier(7.101, 7.101, 0.1792), 1);
        assert_eq!(calc_tier(7.101 * (0.1792f64 * 2.5).exp(), 7.101, 0.1792), 3);
    }
}
