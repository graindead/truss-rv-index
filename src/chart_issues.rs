//! Chart issues: scan-chart's findChartIssues, copied from Truss's issues.rs (the
//! "scan-parsed-chart.ts: findChartIssues" section, unchanged) with a small wrapper. The folder
//! and metadata checks (audio, album art, video, ini values) are left out: rv-index only reads
//! a chart's notes file and song.ini.

use crate::chart::{self, InstrumentType, ParsedChart, Track, instrument_type, nf, nt};

struct Issues {
    /// (instrument, difficulty, type, description)
    chart: Vec<(Option<String>, Option<String>, &'static str, String)>,
}

/// The distinct chart issue types of a parsed chart (e.g. "noStarPower", "brokenNote").
pub fn chart_issue_kinds(p: &ParsedChart) -> Vec<&'static str> {
    let mut issues = Issues { chart: Vec::new() };
    find_chart_issues(p, &mut issues);
    let mut kinds: Vec<&'static str> = issues.chart.iter().map(|c| c.2).collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

// ---------- scan-parsed-chart.ts: findChartIssues ----------

const LEADING_SILENCE_THRESHOLD_MS: f64 = 1000.0;
const MIN_SUSTAIN_GAP_MS: f64 = 40.0;
const MIN_SUSTAIN_MS: f64 = 100.0;

fn describe(kind: &str) -> &'static str {
    match kind {
        "misalignedTimeSignature" => {
            "This chart has a time signature marker that doesn't appear at the start of a measure. This can't be interpreted correctly in Clone Hero."
        }
        "noNotes" => "This chart has no notes.",
        "noExpert" => "This instrument has Easy, Medium, or Hard charted but not Expert.",
        "difficultyNotReduced" => "The notes of this difficulty are identical to the notes of the expert chart.",
        "isDefaultBPM" => {
            "This chart has only one 120 BPM marker and only one 4/4 time signature. This usually means the chart wasn't tempo-mapped, but you can ignore this if the song is a constant 120 BPM."
        }
        "noSections" => "This chart has no sections.",
        "badEndEvent" => "This end event is in an invalid location and will be ignored by most games.",
        "smallLeadingSilence" => "This track has a note that is less than 2000ms after the start of the track.",
        "noStarPower" => "This track has no star power.",
        "emptyStarPower" => "This star power phrase contains no notes.",
        "badStarPower" => "This star power is being ignored due to the .ini \"multiplier_note\" setting.",
        "emptySoloSection" => "This solo section contains no notes.",
        "noDrumActivationLanes" => "This drums track has no activation lanes.",
        "emptyFlexLane" => "This flex lane contains no notes.",
        "difficultyForbiddenNote" => "This is a note that isn't allowed on the track's difficulty.",
        "invalidChord" => "The use of this type of chord is strongly discouraged.",
        "brokenNote" => "This note is so close to the previous note that this was likely a charting mistake.",
        "badSustainGap" => "This note is not far enough ahead of the previous sustain.",
        "babySustain" => "The sustain on this note is too short.",
        "invalidLyric" => "This lyric is on the EVENTS track instead of PART VOCALS and will not be displayed.",
        "invalidPhraseStart" => {
            "This phrase_start is on the EVENTS track. Vocal phrase boundaries in .mid charts must be MIDI note 105 (player 1) or 106 (player 2) on PART VOCALS."
        }
        "invalidPhraseEnd" => {
            "This phrase_end is on the EVENTS track. Vocal phrase boundaries in .mid charts must be MIDI note 105 (player 1) or 106 (player 2) on PART VOCALS."
        }
        _ => "",
    }
}

/// scan-chart's msToExactTime: [HH:]MM:SS.ss
pub fn ms_to_exact_time(ms: f64) -> String {
    // lodash's _.floor(x, 2) shifts the decimal point through the string form.
    let x = (ms / 1000.0) % 60.0;
    let shifted = format!("{x}e2").parse::<f64>().unwrap_or(x * 100.0).floor();
    let seconds = format!("{shifted}e-2").parse::<f64>().unwrap_or(shifted / 100.0);
    let minutes = ((ms / 1000.0 / 60.0) % 60.0).floor();
    let hours = ((ms / 1000.0 / 3600.0) % 24.0).floor();
    let h = if hours > 0.0 { format!("{hours}:") } else { String::new() };
    format!("{h}{:02}:{:05.2}", minutes as i64, seconds)
}

fn type_count(group: &[chart::Note], types: &[u8]) -> usize {
    group.iter().filter(|n| types.contains(&n.ty)).count()
}

/// Phrases containing at least one note (track-hasher's pruneEmptyPhrases).
fn empty_phrases<'a>(phrases: impl Iterator<Item = (i64, i64, f64)> + 'a, notes: &'a [Vec<chart::Note>]) -> Vec<f64> {
    let mut out = Vec::new();
    let mut i = 0;
    for (tick, length, ms) in phrases {
        while i < notes.len() && notes[i][0].tick < tick {
            i += 1;
        }
        let nonempty = i < notes.len() && notes[i][0].tick < tick + length.max(1);
        if !nonempty {
            out.push(ms);
        }
    }
    out
}

fn same_notes(a: &Track, b: &Track) -> bool {
    a.note_groups.len() == b.note_groups.len()
        && a.note_groups.iter().zip(&b.note_groups).all(|(x, y)| {
            x.len() == y.len()
                && x.iter().zip(y).all(|(n, m)| n.tick == m.tick && n.ty == m.ty && n.flags == m.flags && n.length == m.length)
        })
        && a.star_power.len() == b.star_power.len()
}

fn find_chart_issues(p: &ParsedChart, issues: &mut Issues) {
    let mut global = |kind: &'static str, ms: Option<f64>| {
        let d = describe(kind);
        let desc = match ms {
            Some(ms) => format!("[{}]: {d}", ms_to_exact_time(ms)),
            None => d.to_string(),
        };
        issues.chart.push((None, None, kind, desc));
    };
    for kind in &p.parse_issues {
        global(kind, None);
    }
    // misalignedTimeSignature
    {
        let mut ts: Vec<(i64, f64, u32, u32)> = p.timed_time_sigs.clone();
        let mut last_beatline = 0f64;
        let mut i = 0usize;
        while i < ts.len() {
            if last_beatline != ts[i].0 as f64 {
                global("misalignedTimeSignature", Some(ts[i].1));
                ts.remove(i);
                if i == 0 {
                    continue;
                }
                i -= 1;
            }
            while i + 1 < ts.len() && last_beatline < ts[i + 1].0 as f64 {
                last_beatline += p.resolution as f64 * 4.0 * (ts[i].2 as f64 / ts[i].3 as f64);
            }
            i += 1;
        }
    }
    // noNotes
    if p.tracks.iter().all(|t| t.note_groups.is_empty()) && !p.has_vocals {
        global("noNotes", None);
    }
    // isDefaultBPM
    if p.tempos.len() == 1
        && (p.tempos[0].1 * 1e12).round() / 1e12 == 120.0
        && p.time_sigs.len() == 1
        && p.time_sigs[0].1 == 4
        && p.time_sigs[0].2 == 4
    {
        global("isDefaultBPM", None);
    }
    // noSections
    if p.sections.is_empty() {
        global("noSections", None);
    }
    // badEndEvent
    for e in p.end_events.iter().skip(1) {
        global("badEndEvent", Some(e.ms));
    }
    let last_note_tick = p.tracks.iter().filter_map(|t| t.note_groups.last().map(|g| g[0].tick)).max();
    if let (Some(last), Some(first_end)) = (last_note_tick, p.end_events.first())
        && last != 0
        && first_end.tick < last
    {
        global("badEndEvent", Some(first_end.ms));
    }

    // noExpert, difficultyNotReduced (grouped by instrument, in track order)
    let mut seen: Vec<&str> = Vec::new();
    for t in &p.tracks {
        if seen.contains(&t.instrument) {
            continue;
        }
        seen.push(t.instrument);
        let group: Vec<&Track> = p.tracks.iter().filter(|x| x.instrument == t.instrument).collect();
        match group.iter().find(|x| x.difficulty == "expert") {
            None => issues.chart.push((Some(t.instrument.into()), None, "noExpert", describe("noExpert").into())),
            Some(expert) => {
                for lower in group.iter().filter(|x| x.difficulty != "expert") {
                    if same_notes(expert, lower) && lower.note_groups.len() > 20 {
                        issues.chart.push((
                            Some(lower.instrument.into()),
                            Some(lower.difficulty.into()),
                            "difficultyNotReduced",
                            describe("difficultyNotReduced").into(),
                        ));
                    }
                }
            }
        }
    }

    for track in &p.tracks {
        track_issues(track, issues);
    }
}

fn track_issues(track: &Track, issues: &mut Issues) {
    let mut add = |kind: &'static str, ms: Option<f64>| {
        let d = describe(kind);
        let desc = match ms {
            Some(ms) => format!("[{}]: {d}", ms_to_exact_time(ms)),
            None => d.to_string(),
        };
        issues.chart.push((Some(track.instrument.into()), Some(track.difficulty.into()), kind, desc));
    };
    let groups = &track.note_groups;
    let span_ms = || match (groups.first(), groups.last()) {
        (Some(a), Some(b)) => b[0].ms - a[0].ms,
        _ => 0.0,
    };
    if groups.first().is_some_and(|g| g[0].ms < LEADING_SILENCE_THRESHOLD_MS) {
        add("smallLeadingSilence", None);
    }
    if track.star_power.is_empty() && track.instrument != "drums" && groups.len() > 50 && span_ms() > 60000.0 {
        add("noStarPower", None);
    }
    for ms in empty_phrases(track.star_power.iter().map(|s| (s.tick, s.length, s.ms)), groups) {
        add("emptyStarPower", Some(ms));
    }
    for s in &track.rejected_star_power {
        add("badStarPower", Some(s.ms));
    }
    for ms in empty_phrases(track.solo.iter().map(|s| (s.tick, s.length, s.ms)), groups) {
        add("emptySoloSection", Some(ms));
    }
    if track.instrument == "drums"
        && track.freestyle.is_empty()
        && !track.star_power.is_empty()
        && groups.len() > 50
        && span_ms() > 60000.0
    {
        add("noDrumActivationLanes", None);
    }
    for ms in empty_phrases(track.flex.iter().map(|(s, _)| (s.tick, s.length, s.ms)), groups) {
        add("emptyFlexLane", Some(ms));
    }

    match instrument_type(track.instrument) {
        InstrumentType::Drums => {
            let non_kick = [nt::GREEN_DRUM, nt::RED_DRUM, nt::YELLOW_DRUM, nt::BLUE_DRUM];
            for g in groups {
                if track.difficulty != "expert" {
                    for n in g {
                        if n.flags & nf::DOUBLE_KICK != 0 {
                            add("difficultyForbiddenNote", Some(g[0].ms));
                        }
                    }
                }
                if track.difficulty == "easy" && type_count(g, &non_kick) == 2 && type_count(g, &[nt::KICK]) > 0 {
                    add("difficultyForbiddenNote", Some(g[0].ms));
                }
                if type_count(g, &non_kick) >= 3 {
                    add("invalidChord", Some(g[0].ms));
                }
            }
        }
        InstrumentType::FiveFret => {
            let five = [nt::GREEN, nt::RED, nt::YELLOW, nt::BLUE, nt::ORANGE];
            for g in groups {
                let forbidden = match track.difficulty {
                    "hard" => type_count(g, &[nt::GREEN, nt::ORANGE]) == 2,
                    "medium" => type_count(g, &[nt::ORANGE]) > 0 || type_count(g, &[nt::GREEN, nt::BLUE]) == 2,
                    "easy" => type_count(g, &[nt::ORANGE, nt::BLUE]) > 0,
                    _ => false,
                };
                if forbidden {
                    add("difficultyForbiddenNote", Some(g[0].ms));
                }
                if type_count(g, &five) == 5 {
                    add("invalidChord", Some(g[0].ms));
                }
            }
        }
        InstrumentType::SixFret => {
            let black = [nt::BLACK1, nt::BLACK2, nt::BLACK3];
            let white = [nt::WHITE1, nt::WHITE2, nt::WHITE3];
            let one = [nt::BLACK1, nt::WHITE1];
            let two = [nt::BLACK2, nt::WHITE2];
            let three = [nt::BLACK3, nt::WHITE3];
            for g in groups {
                let forbidden = match track.difficulty {
                    "hard" => g.len() == 3 && (type_count(g, &black) < 3 || type_count(g, &white) < 3),
                    "medium" => {
                        g.len() == 3
                            || (g.len() == 2
                                && type_count(g, &black) > 0
                                && type_count(g, &white) > 0
                                && type_count(g, &one) < 2
                                && type_count(g, &two) < 2
                                && type_count(g, &three) < 2)
                    }
                    "easy" => {
                        g.len() == 3
                            || (g.len() == 2
                                && !(type_count(g, &[nt::WHITE1, nt::WHITE2]) == 2
                                    || type_count(g, &[nt::WHITE2, nt::WHITE3]) == 2))
                    }
                    _ => false,
                };
                if forbidden {
                    add("difficultyForbiddenNote", Some(g[0].ms));
                }
                if g.len() > 3
                    || (type_count(g, &two) == 2 && type_count(g, &one) > 0)
                    || (type_count(g, &three) == 2 && type_count(g, &one) + type_count(g, &two) > 0)
                {
                    add("invalidChord", Some(g[0].ms));
                }
            }
        }
    }

    // brokenNote
    for i in 1..groups.len() {
        let (note, prev) = (&groups[i], &groups[i - 1]);
        let d = note[0].ms - prev[0].ms;
        if d > 0.0 && d <= 15.0 {
            let open_now = type_count(note, &[nt::OPEN]) > 0;
            let open_prev = type_count(prev, &[nt::OPEN]) > 0;
            if open_now != open_prev {
                continue;
            }
            add("brokenNote", Some(note[0].ms));
        }
    }

    // badSustainGap, babySustain
    if instrument_type(track.instrument) != InstrumentType::Drums {
        let mut gaps: Vec<(f64, f64)> = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            let t = g[0].ms;
            gaps.retain(|r| r.1 > t);
            if gaps.iter().any(|r| t >= r.0 && t < r.1) {
                add("badSustainGap", Some(t));
            }
            for n in g {
                if n.ms_length > 0.0 && n.ty != nt::OPEN {
                    gaps.push((n.ms + n.ms_length, n.ms + n.ms_length + MIN_SUSTAIN_GAP_MS));
                }
                if n.ms_length > 0.0 && n.ms_length < MIN_SUSTAIN_MS {
                    let next_open = groups.get(i + 1).and_then(|ng| ng.iter().find(|m| m.ty == nt::OPEN));
                    if let Some(o) = next_open
                        && o.tick < n.tick + n.length
                        && o.flags & (nf::HOPO | nf::TAP) != 0
                    {
                        continue;
                    }
                    add("babySustain", Some(n.ms));
                }
            }
        }
    }
}

