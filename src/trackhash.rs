//! Track hashes for Clone Hero leaderboards: a port of scan-chart's `calculateTrackHash`
//! (src/chart/track-hasher.ts, MIT, by Geomitron). Encore lists these hashes for its charts;
//! for anything else (RhythmVerse charts, local folders) we parse the chart with chart.rs and
//! hash it ourselves.

use crate::chart::{Modifiers, Note, ParsedChart, Track};

/// The hash layout. scan-chart changed it in Aug 2025 (note types renumbered, lengths widened
/// to 64 bits); Encore still lists older charts with the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Layout {
    pub type_offset: u32,
    pub wide_lengths: bool,
}

pub const CURRENT: Layout = Layout { type_offset: 0, wide_lengths: true };
#[allow(dead_code)] // older hashes, for the Encore comparison test
pub const LEGACY: Layout = Layout { type_offset: 1, wide_lengths: false };

/// One track's hash in the given layout, base64url without padding.
pub fn track_hash(chart: &ParsedChart, t: &Track, layout: Layout) -> String {
    fn dedup_last<T: Copy>(v: &[T], tick: impl Fn(&T) -> i64) -> Vec<T> {
        let mut s: Vec<T> = v.to_vec();
        s.sort_by_key(|x| tick(x)); // stable
        let mut out: Vec<T> = Vec::new();
        for x in s {
            match out.last_mut() {
                Some(l) if tick(l) == tick(&x) => *l = x,
                _ => out.push(x),
            }
        }
        out
    }
    // Phrases with no notes in them don't count.
    fn prune<T: Copy>(phrases: &[T], span: impl Fn(&T) -> (i64, i64), groups: &[Vec<Note>]) -> Vec<T> {
        let mut out = Vec::new();
        let mut i = 0;
        for p in phrases {
            let (tick, len) = span(p);
            while i < groups.len() && groups[i][0].tick < tick {
                i += 1;
            }
            if i < groups.len() && groups[i][0].tick < tick + if len == 0 { 1 } else { len } {
                out.push(*p);
            }
        }
        out
    }
    let tempos = dedup_last(&chart.tempos, |t| t.0);
    let tss = dedup_last(&chart.time_sigs, |t| t.0);
    let sp = prune(&t.star_power, |s| (s.tick, s.length), &t.note_groups);
    let solo = prune(&t.solo, |s| (s.tick, s.length), &t.note_groups);
    let flex = prune(&t.flex, |s| (s.0.tick, s.0.length), &t.note_groups);

    let mut b: Vec<u8> = Vec::new();
    b.extend(0x43484e46u32.to_be_bytes()); // "CHNF"
    b.extend(20240320u32.to_le_bytes());
    b.extend((chart.resolution as u32).to_le_bytes());
    b.extend((tempos.len() as u32).to_le_bytes());
    for (tick, bpm) in &tempos {
        b.extend(tick.to_le_bytes());
        b.extend(bpm.to_le_bytes());
    }
    b.extend((tss.len() as u32).to_le_bytes());
    for (tick, n, d) in &tss {
        b.extend(tick.to_le_bytes());
        b.extend(n.to_le_bytes());
        b.extend(d.to_le_bytes());
    }
    let len = |b: &mut Vec<u8>, l: i64| {
        if layout.wide_lengths {
            b.extend(l.to_le_bytes())
        } else {
            b.extend((l as u32).to_le_bytes())
        }
    };
    for list in [&sp, &solo] {
        b.extend((list.len() as u32).to_le_bytes());
        for s in list.iter() {
            b.extend(s.tick.to_le_bytes());
            len(&mut b, s.length);
        }
    }
    b.extend((flex.len() as u32).to_le_bytes());
    for (s, double) in &flex {
        b.extend(s.tick.to_le_bytes());
        len(&mut b, s.length);
        b.push(*double as u8);
    }
    b.extend((t.freestyle.len() as u32).to_le_bytes());
    for (s, coda) in &t.freestyle {
        b.extend(s.tick.to_le_bytes());
        len(&mut b, s.length);
        b.push(*coda as u8);
    }
    let notes: Vec<&Note> = t.note_groups.iter().flatten().collect();
    b.extend((notes.len() as u32).to_le_bytes());
    for n in notes {
        b.extend(n.tick.to_le_bytes());
        len(&mut b, n.length);
        b.extend((n.ty as u32 - layout.type_offset).to_le_bytes());
        b.extend(n.flags.to_le_bytes());
    }
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blake3::hash(&b).as_bytes())
}

/// JavaScript's `parseFloat`: the longest numeric prefix, ignoring leading whitespace.
fn parse_float(s: &str) -> Option<f64> {
    let s = s.trim_start();
    let mut end = 0;
    let mut best = None;
    for (i, _) in s.char_indices().chain(std::iter::once((s.len(), ' '))) {
        if i == 0 {
            continue;
        }
        if let Ok(v) = s[..i].parse::<f64>()
            && !s[..i].ends_with(['e', 'E', '+', '-'])
            && !s[..i].eq_ignore_ascii_case("inf")
            && !s[..i].eq_ignore_ascii_case("nan")
        {
            best = Some(v);
            end = i;
        }
    }
    let _ = end;
    best
}

/// The song.ini values that change parsing, read the way scan-chart reads them: keys are
/// case-sensitive, the last duplicate wins, and -1 or junk means "use the default".
pub fn modifiers_from_ini(pairs: &[(String, String)]) -> Modifiers {
    let get = |k: &str| pairs.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let int = |keys: &[&str], d: i64| {
        for k in keys {
            if let Some(v) = get(k).and_then(parse_float)
                && v != -1.0
            {
                return v.round() as i64;
            }
        }
        d
    };
    let flag = |k: &str| match get(k).map(|v| v.to_ascii_lowercase()) {
        Some(v) if v == "true" || v == "1" => true,
        _ => false,
    };
    let d = Modifiers::default();
    let mut multiplier_note = int(&["multiplier_note", "star_power_note"], d.multiplier_note);
    if ![0, 103, 116].contains(&multiplier_note) {
        multiplier_note = 0;
    }
    Modifiers {
        hopo_frequency: int(&["hopo_frequency", "hopofreq"], d.hopo_frequency),
        eighthnote_hopo: flag("eighthnote_hopo"),
        multiplier_note,
        sustain_cutoff_threshold: int(&["sustain_cutoff_threshold"], d.sustain_cutoff_threshold),
        chord_snap_threshold: int(&["chord_snap_threshold"], d.chord_snap_threshold),
        five_lane_drums: flag("five_lane_drums"),
        pro_drums: flag("pro_drums"),
    }
}

/// `[Song]` key/values the way scan-chart's parseIni finds them: only a section named exactly
/// song, Song or SONG.
pub fn ini_pairs(data: &[u8]) -> Vec<(String, String)> {
    let text = decode(data);
    let mut section: Option<String> = None;
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line).trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            if line.ends_with(']') && line.len() > 2 {
                section = Some(line[1..line.len() - 1].trim().to_string());
            }
        } else if let Some((k, v)) = line.split_once('=')
            && let Some(s) = &section
        {
            match sections.iter_mut().find(|(n, _)| n == s) {
                Some((_, kv)) => kv.push((k.trim().to_string(), v.trim().to_string())),
                None => sections.push((s.clone(), vec![(k.trim().to_string(), v.trim().to_string())])),
            }
        }
    }
    for name in ["song", "Song", "SONG"] {
        if let Some((_, kv)) = sections.iter().find(|(n, _)| n == name) {
            return kv.clone();
        }
    }
    Vec::new()
}

fn decode(data: &[u8]) -> String {
    if data.len() >= 2 && (data[..2] == [0xff, 0xfe] || data[..2] == [0xfe, 0xff]) {
        let le = data[0] == 0xff;
        let units: Vec<u16> = data[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let s = String::from_utf8_lossy(data);
    s.strip_prefix('\u{feff}').unwrap_or(&s).to_string()
}

/// A chart's leaderboard hashes, worked out from its files.
#[derive(Debug, Clone)]
pub struct Computed {
    /// (instrument, difficulty, hash) in the current layout.
    pub tracks: Vec<(String, String, String)>,
    pub pro_drums: bool,
    /// Fretwork ratings of each instrument's Expert track.
    pub ratings: Vec<(String, crate::fretwork::Rating)>,
}

/// Hashes every track of a chart from its files (name, bytes): notes.mid or notes.chart, plus
/// song.ini unless `ini` (e.g. a .sng's metadata) is given. Picks the chart file the way
/// scan-chart does.
pub fn hash_files(files: &[(String, Vec<u8>)], ini: Option<&[(String, String)]>) -> Result<Computed, String> {
    let lower = |n: &str| n.rsplit(['/', '\\']).next().unwrap_or(n).to_ascii_lowercase();
    let is_chart = |n: &str| n.ends_with(".chart") || n.ends_with(".mid");
    let mut charts: Vec<&(String, Vec<u8>)> = files.iter().filter(|(n, _)| is_chart(&lower(n))).collect();
    // notes.* first, then .mid before .chart.
    charts.sort_by_key(|(n, _)| {
        let n = lower(n);
        (!(n == "notes.chart" || n == "notes.mid"), !n.ends_with(".mid"))
    });
    let (name, data) = charts.first().ok_or("This chart has no notes.chart or notes.mid.")?;
    let mods = match ini {
        Some(pairs) => modifiers_from_ini(pairs),
        None => {
            let inis: Vec<&(String, Vec<u8>)> = files.iter().filter(|(n, _)| lower(n).ends_with(".ini")).collect();
            let ini = inis.iter().find(|(n, _)| lower(n) == "song.ini").or(inis.last());
            ini.map(|(_, d)| modifiers_from_ini(&ini_pairs(d))).unwrap_or_default()
        }
    };
    let parsed = crate::chart::parse_chart_file(data, lower(name).ends_with(".mid"), &mods)?;
    Ok(Computed {
        tracks: parsed
            .tracks
            .iter()
            .map(|t| (t.instrument.to_string(), t.difficulty.to_string(), track_hash(&parsed, t, CURRENT)))
            .collect(),
        pro_drums: parsed.drum_type == Some(crate::chart::DrumType::FourLanePro),
        ratings: parsed
            .tracks
            .iter()
            .filter(|t| t.difficulty == "expert")
            .filter_map(|t| Some((t.instrument.to_string(), crate::fretwork::rate(t.instrument, t)?)))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_parse_float() {
        assert_eq!(parse_float("170"), Some(170.0));
        assert_eq!(parse_float(" 12.5abc"), Some(12.5));
        assert_eq!(parse_float("1e"), Some(1.0));
        assert_eq!(parse_float("abc"), None);
        assert_eq!(parse_float(""), None);
    }

    #[test]
    fn ini_rules() {
        let ini = b"[Song]\r\nhopo_frequency = 170\r\npro_drums = True\r\nsustain_cutoff_threshold = -1\r\n[song]\r\nhopo_frequency = 1\r\n";
        // "song" wins over "Song".
        let m = modifiers_from_ini(&ini_pairs(ini));
        assert_eq!(m.hopo_frequency, 1);
        assert!(!m.pro_drums);
        let m = modifiers_from_ini(&ini_pairs(b"[Song]\nhopofreq = 80.6\npro_drums = TRUE\nsustain_cutoff_threshold = -1\n"));
        assert_eq!(m.hopo_frequency, 81);
        assert!(m.pro_drums);
        assert_eq!(m.sustain_cutoff_threshold, -1);
    }

    /// TRUSS_TEST_CHART_DIR=<folder>: print every track hash of a chart folder.
    #[test]
    fn hash_real_folder() {
        let Ok(dir) = std::env::var("TRUSS_TEST_CHART_DIR") else { return };
        let files: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().to_string(), std::fs::read(e.path()).unwrap()))
            .collect();
        let cp = hash_files(&files, None).unwrap();
        for (i, d, h) in cp.tracks {
            println!("{i} {d}: {h}");
        }
        for (i, r) in cp.ratings {
            println!("fretwork {i}: D {:.2}, remap {}, tier {}", r.d, r.remap, r.tier);
        }
    }
}
