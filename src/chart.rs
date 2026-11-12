//! .chart / .mid note parser. A port of the note-related parts of scan-chart
//! (chart-file-parser.ts, midi-file-parser.ts, parse-chart-file.ts, natural-hopo.ts),
//! keeping its rules for HOPOs, taps, opens, pro-drum cymbals, dynamics, disco flip,
//! star power, sustain trimming, and chord snapping.

use std::collections::HashMap;

// ---------- Raw event types (raw-types.ts) ----------
#[allow(dead_code)]
pub mod ev {
    pub const STAR_POWER: u8 = 0;
    pub const SOLO_SECTION: u8 = 1;
    pub const REJECTED_STAR_POWER: u8 = 2;
    pub const SOLO_SECTION_START: u8 = 3;
    pub const SOLO_SECTION_END: u8 = 4;
    pub const OPEN: u8 = 5;
    pub const GREEN: u8 = 6;
    pub const RED: u8 = 7;
    pub const YELLOW: u8 = 8;
    pub const BLUE: u8 = 9;
    pub const ORANGE: u8 = 10;
    pub const BLACK1: u8 = 11;
    pub const BLACK2: u8 = 12;
    pub const BLACK3: u8 = 13;
    pub const WHITE1: u8 = 14;
    pub const WHITE2: u8 = 15;
    pub const WHITE3: u8 = 16;
    pub const KICK: u8 = 17;
    pub const KICK2X: u8 = 18;
    pub const RED_DRUM: u8 = 19;
    pub const YELLOW_DRUM: u8 = 20;
    pub const BLUE_DRUM: u8 = 21;
    pub const FIVE_ORANGE_FOUR_GREEN_DRUM: u8 = 22;
    pub const FIVE_GREEN_DRUM: u8 = 23;
    pub const FLEX_LANE_SINGLE: u8 = 24;
    pub const FLEX_LANE_DOUBLE: u8 = 25;
    pub const FREESTYLE_SECTION: u8 = 26;
    pub const FORCE_OPEN: u8 = 27;
    pub const FORCE_TAP: u8 = 28;
    pub const FORCE_STRUM: u8 = 29;
    pub const FORCE_HOPO: u8 = 30;
    pub const FORCE_UNNATURAL: u8 = 31;
    pub const FORCE_FLAM: u8 = 32;
    pub const YELLOW_TOM_MARKER: u8 = 33;
    pub const BLUE_TOM_MARKER: u8 = 34;
    pub const GREEN_TOM_MARKER: u8 = 35;
    pub const YELLOW_CYMBAL_MARKER: u8 = 36;
    pub const BLUE_CYMBAL_MARKER: u8 = 37;
    pub const GREEN_CYMBAL_MARKER: u8 = 38;
    pub const RED_GHOST: u8 = 39;
    pub const YELLOW_GHOST: u8 = 40;
    pub const BLUE_GHOST: u8 = 41;
    pub const FIVE_ORANGE_FOUR_GREEN_GHOST: u8 = 42;
    pub const FIVE_GREEN_GHOST: u8 = 43;
    pub const KICK_GHOST: u8 = 44;
    pub const RED_ACCENT: u8 = 45;
    pub const YELLOW_ACCENT: u8 = 46;
    pub const BLUE_ACCENT: u8 = 47;
    pub const FIVE_ORANGE_FOUR_GREEN_ACCENT: u8 = 48;
    pub const FIVE_GREEN_ACCENT: u8 = 49;
    pub const KICK_ACCENT: u8 = 50;
    pub const DISCO_FLIP_OFF: u8 = 51;
    pub const DISCO_FLIP_ON: u8 = 52;
    pub const DISCO_NO_FLIP_ON: u8 = 53;
    pub const ENABLE_CHART_DYNAMICS: u8 = 54;
}

// ---------- Parsed note types and flags (types.ts) ----------
pub mod nt {
    pub const OPEN: u8 = 1;
    pub const GREEN: u8 = 2;
    pub const RED: u8 = 3;
    pub const YELLOW: u8 = 4;
    pub const BLUE: u8 = 5;
    pub const ORANGE: u8 = 6;
    pub const BLACK1: u8 = 7;
    pub const BLACK2: u8 = 8;
    pub const BLACK3: u8 = 9;
    pub const WHITE1: u8 = 10;
    pub const WHITE2: u8 = 11;
    pub const WHITE3: u8 = 12;
    pub const KICK: u8 = 13;
    pub const RED_DRUM: u8 = 14;
    pub const YELLOW_DRUM: u8 = 15;
    pub const BLUE_DRUM: u8 = 16;
    pub const GREEN_DRUM: u8 = 17;
    pub const MAX: usize = 17;
}

#[allow(dead_code)]
pub mod nf {
    pub const NONE: u32 = 0;
    pub const STRUM: u32 = 1;
    pub const HOPO: u32 = 2;
    pub const TAP: u32 = 4;
    pub const DOUBLE_KICK: u32 = 8;
    pub const TOM: u32 = 16;
    pub const CYMBAL: u32 = 32;
    pub const DISCO_NOFLIP: u32 = 64;
    pub const DISCO: u32 = 128;
    pub const FLAM: u32 = 256;
    pub const GHOST: u32 = 512;
    pub const ACCENT: u32 = 1024;
}

pub const INSTRUMENTS: [&str; 10] =
    ["guitar", "guitarcoop", "rhythm", "bass", "drums", "keys", "guitarghl", "guitarcoopghl", "rhythmghl", "bassghl"];
pub const DIFFICULTIES: [&str; 4] = ["expert", "hard", "medium", "easy"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstrumentType {
    SixFret,
    FiveFret,
    Drums,
}

pub fn instrument_type(instrument: &str) -> InstrumentType {
    match instrument {
        "drums" => InstrumentType::Drums,
        "guitarghl" | "guitarcoopghl" | "rhythmghl" | "bassghl" => InstrumentType::SixFret,
        _ => InstrumentType::FiveFret,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrumType {
    FourLane,
    FourLanePro,
    FiveLane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Chart,
    Mid,
}

/// song.ini values that change how notes are parsed.
#[derive(Debug, Clone)]
pub struct Modifiers {
    pub hopo_frequency: i64,
    pub eighthnote_hopo: bool,
    pub multiplier_note: i64,
    pub sustain_cutoff_threshold: i64,
    pub chord_snap_threshold: i64,
    pub five_lane_drums: bool,
    pub pro_drums: bool,
}

impl Default for Modifiers {
    fn default() -> Self {
        Self {
            hopo_frequency: 0,
            eighthnote_hopo: false,
            multiplier_note: 0,
            sustain_cutoff_threshold: -1,
            chord_snap_threshold: 0,
            five_lane_drums: false,
            pro_drums: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TrackEvent {
    tick: i64,
    length: i64,
    ty: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub tick: i64,
    pub length: i64,
}

struct RawTrack {
    instrument: &'static str,
    difficulty: &'static str,
    star_power: Vec<Span>,
    solo: Vec<Span>,
    flex: Vec<(Span, bool)>,
    freestyle: Vec<(Span, bool)>,
    rejected_star_power: Vec<Span>,
    events: Vec<TrackEvent>,
}

/// What the EVENTS track / [Events] section and vocal tracks hold (used by the issue scanner).
#[derive(Debug, Clone, Default)]
struct GlobalEvents {
    sections: Vec<i64>,
    end_events: Vec<i64>,
    has_vocals: bool,
    has_lyrics: bool,
    /// Global parse issues, e.g. lyrics placed on the EVENTS track of a .mid.
    parse_issues: Vec<&'static str>,
}

struct RawChart {
    resolution: i64,
    tempos: Vec<(i64, f64)>,
    time_sigs: Vec<(i64, u32, u32)>,
    tracks: Vec<RawTrack>,
    /// .chart [Song] Offset, in seconds.
    chart_offset_secs: f64,
    global: GlobalEvents,
}

// ---------- Output ----------

#[derive(Debug, Clone, Copy)]
pub struct Note {
    pub tick: i64,
    pub length: i64,
    pub ms: f64,
    pub ms_length: f64,
    pub ty: u8,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct TimedSpan {
    pub tick: i64,
    pub length: i64,
    pub ms: f64,
    pub ms_length: f64,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub instrument: &'static str,
    pub difficulty: &'static str,
    pub star_power: Vec<TimedSpan>,
    pub solo: Vec<TimedSpan>,
    /// (span, is_double)
    pub flex: Vec<(TimedSpan, bool)>,
    /// (span, is_coda)
    pub freestyle: Vec<(Span, bool)>,
    /// Star power ignored because of the ini "multiplier_note" setting.
    pub rejected_star_power: Vec<TimedSpan>,
    pub note_groups: Vec<Vec<Note>>,
}

/// A tick with its time in ms.
#[derive(Debug, Clone, Copy)]
pub struct TimedTick {
    pub tick: i64,
    pub ms: f64,
}

#[derive(Debug, Clone)]
pub struct ParsedChart {
    pub resolution: i64,
    /// (tick, bpm) after the same normalization as scan-chart's timed tempos.
    pub tempos: Vec<(i64, f64)>,
    pub time_sigs: Vec<(i64, u32, u32)>,
    #[allow(dead_code)]
    pub drum_type: Option<DrumType>,
    pub tracks: Vec<Track>,
    pub chart_offset_secs: f64,
    pub sections: Vec<TimedTick>,
    pub end_events: Vec<TimedTick>,
    /// Time signatures with their times: (tick, ms, numerator, denominator).
    pub timed_time_sigs: Vec<(i64, f64, u32, u32)>,
    pub has_vocals: bool,
    pub has_lyrics: bool,
    pub parse_issues: Vec<&'static str>,
    /// rv-index patch: some fret note was forced (HOPO/strum/unnatural marker) away from its
    /// natural state. Not part of any hash.
    pub has_forced_notes: bool,
}

// rv-index patch: set by resolve_fret_modifiers, read once per parse.
thread_local! {
    static FORCED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn parse_chart_file(data: &[u8], is_mid: bool, mods: &Modifiers) -> Result<ParsedChart, String> {
    let format = if is_mid { Format::Mid } else { Format::Chart };
    FORCED.with(|f| f.set(false));
    let raw = if is_mid { parse_midi(data, mods)? } else { parse_dot_chart(data)? };
    let tempos = timed_tempos(&raw.tempos, raw.resolution);
    let tpb = raw.resolution;

    let drum_tracks: Vec<&RawTrack> = raw.tracks.iter().filter(|t| t.instrument == "drums").collect();
    let drum_type = if drum_tracks.is_empty() {
        None
    } else if mods.five_lane_drums {
        Some(DrumType::FiveLane)
    } else if mods.pro_drums {
        Some(DrumType::FourLanePro)
    } else if drum_tracks.iter().any(|t| t.events.iter().any(|e| is_cymbal_or_tom_marker(e.ty))) {
        Some(DrumType::FourLanePro)
    } else if drum_tracks.iter().any(|t| t.events.iter().any(|e| e.ty == ev::FIVE_GREEN_DRUM)) {
        Some(DrumType::FiveLane)
    } else {
        Some(DrumType::FourLane)
    };

    let mut tracks = Vec::with_capacity(raw.tracks.len());
    for t in raw.tracks {
        let mut events = t.events;
        trim_sustains(&mut events, mods.sustain_cutoff_threshold, tpb, format);
        let grouped = group_by_tick(&events);
        let resolved = if t.instrument == "drums" {
            resolve_drum_modifiers(grouped, drum_type.unwrap(), format)
        } else {
            resolve_fret_modifiers(grouped, mods, tpb, format)
        };
        let mut snapped = snap_chords(resolved, mods.chord_snap_threshold, t.instrument);
        sort_and_fix_invalid_note_overlaps(&mut snapped);
        let note_groups = set_group_ms(snapped, &tempos, tpb);

        let mut sp: Vec<TimedSpan> = t.star_power.iter().map(|s| timed_span(*s, &tempos, tpb)).collect();
        sort_and_fix_invalid_event_overlaps(&mut sp);
        let mut solo: Vec<TimedSpan> = t.solo.iter().map(|s| timed_span(*s, &tempos, tpb)).collect();
        sort_and_fix_invalid_event_overlaps(&mut solo);
        let mut flex: Vec<(TimedSpan, bool)> = t.flex.iter().map(|(s, d)| (timed_span(*s, &tempos, tpb), *d)).collect();
        flex.sort_by(|a, b| a.0.tick.cmp(&b.0.tick).then(a.1.cmp(&b.1)).then(b.0.length.cmp(&a.0.length)));
        flex.dedup_by(|b, a| a.0.tick == b.0.tick && a.1 == b.1);
        let rejected_star_power = t.rejected_star_power.iter().map(|s| timed_span(*s, &tempos, tpb)).collect();
        tracks.push(Track {
            instrument: t.instrument,
            difficulty: t.difficulty,
            star_power: sp,
            solo,
            flex,
            freestyle: t.freestyle,
            rejected_star_power,
            note_groups,
        });
    }

    let timed = |tick: i64| TimedTick { tick, ms: timed_span(Span { tick, length: 0 }, &tempos, tpb).ms };
    let g = raw.global;
    Ok(ParsedChart {
        resolution: raw.resolution,
        tempos: tempos.iter().map(|t| (t.tick, t.bpm)).collect(),
        timed_time_sigs: raw.time_sigs.iter().map(|&(t, n, d)| (t, timed(t).ms, n, d)).collect(),
        time_sigs: raw.time_sigs,
        drum_type,
        tracks,
        chart_offset_secs: raw.chart_offset_secs,
        sections: g.sections.iter().map(|&t| timed(t)).collect(),
        end_events: g.end_events.iter().map(|&t| timed(t)).collect(),
        has_vocals: g.has_vocals,
        has_lyrics: g.has_lyrics,
        parse_issues: g.parse_issues,
        has_forced_notes: FORCED.with(|f| f.get()),
    })
}

// ---------- Timing ----------

struct Tempo {
    tick: i64,
    bpm: f64,
    ms: f64,
}

fn timed_tempos(tempos: &[(i64, f64)], tpb: i64) -> Vec<Tempo> {
    let mut out = vec![Tempo { tick: 0, bpm: 120.0, ms: 0.0 }];
    for &(tick, bpm) in tempos {
        let last = out.last().unwrap();
        let ms = last.ms + ((tick - last.tick) as f64 * 60000.0) / (last.bpm * tpb as f64);
        out.push(Tempo { tick, bpm, ms });
    }
    if out.len() > 1 && out[1].tick == 0 {
        out.remove(0);
    }
    out
}

fn tick_to_ms(tick: i64, tempos: &[Tempo], idx: &mut usize, tpb: i64) -> f64 {
    while *idx + 1 < tempos.len() && tempos[*idx + 1].tick <= tick {
        *idx += 1;
    }
    let t = &tempos[*idx];
    t.ms + ((tick - t.tick) as f64 * 60000.0) / (t.bpm * tpb as f64)
}

fn timed_span(s: Span, tempos: &[Tempo], tpb: i64) -> TimedSpan {
    let mut i = 0;
    let ms = tick_to_ms(s.tick, tempos, &mut i, tpb);
    let ms_length = if s.length != 0 { tick_to_ms(s.tick + s.length, tempos, &mut i, tpb) - ms } else { 0.0 };
    TimedSpan { tick: s.tick, length: s.length, ms, ms_length }
}

fn set_group_ms(groups: Vec<Vec<Untimed>>, tempos: &[Tempo], tpb: i64) -> Vec<Vec<Note>> {
    let mut idx = 0;
    groups
        .into_iter()
        .map(|g| {
            g.into_iter()
                .map(|n| {
                    let ms = tick_to_ms(n.tick, tempos, &mut idx, tpb);
                    let ms_length = if n.length != 0 {
                        let mut end_idx = idx;
                        tick_to_ms(n.tick + n.length, tempos, &mut end_idx, tpb) - ms
                    } else {
                        0.0
                    };
                    Note { tick: n.tick, length: n.length, ms, ms_length, ty: n.ty, flags: n.flags }
                })
                .collect()
        })
        .collect()
}

// ---------- Note resolution (parse-chart-file.ts) ----------

#[derive(Debug, Clone, Copy)]
struct Untimed {
    tick: i64,
    length: i64,
    ty: u8,
    flags: u32,
}

fn is_cymbal_or_tom_marker(t: u8) -> bool {
    matches!(
        t,
        ev::YELLOW_CYMBAL_MARKER
            | ev::BLUE_CYMBAL_MARKER
            | ev::GREEN_CYMBAL_MARKER
            | ev::YELLOW_TOM_MARKER
            | ev::BLUE_TOM_MARKER
            | ev::GREEN_TOM_MARKER
    )
}

fn trim_sustains(events: &mut [TrackEvent], cutoff: i64, tpb: i64, format: Format) {
    let threshold = if cutoff != -1 {
        cutoff
    } else if format == Format::Mid {
        tpb / 3 + 1
    } else {
        0
    };
    if threshold > 0 {
        for e in events.iter_mut() {
            if e.length <= threshold {
                e.length = 0;
            }
        }
    }
}

fn group_by_tick(events: &[TrackEvent]) -> Vec<Vec<TrackEvent>> {
    let mut groups: Vec<Vec<TrackEvent>> = Vec::new();
    for e in events {
        match groups.last_mut() {
            Some(g) if g[0].tick == e.tick => g.push(*e),
            _ => groups.push(vec![*e]),
        }
    }
    groups
}

fn is_kick(t: u8) -> bool {
    t == ev::KICK || t == ev::KICK2X
}
fn is_drum_note(t: u8) -> bool {
    matches!(
        t,
        ev::KICK
            | ev::KICK2X
            | ev::RED_DRUM
            | ev::YELLOW_DRUM
            | ev::BLUE_DRUM
            | ev::FIVE_ORANGE_FOUR_GREEN_DRUM
            | ev::FIVE_GREEN_DRUM
    )
}

fn resolve_drum_modifiers(groups: Vec<Vec<TrackEvent>>, drum_type: DrumType, format: Format) -> Vec<Vec<Untimed>> {
    let mut out = Vec::new();
    let mut active_disco = ev::DISCO_FLIP_OFF;
    for events in groups {
        let mut notes = Vec::new();
        let mut kicks = Vec::new();
        let mut modifiers = Vec::new();
        let mut flam = nf::NONE;
        let (mut has_orange, mut has_green) = (false, false);
        let mut disco_mod: Option<u8> = None;
        for e in &events {
            let t = e.ty;
            if is_kick(t) {
                kicks.push(*e);
            } else if is_drum_note(t) {
                notes.push(*e);
                if t == ev::FIVE_GREEN_DRUM {
                    has_green = true;
                } else if t == ev::FIVE_ORANGE_FOUR_GREEN_DRUM {
                    has_orange = true;
                }
            } else {
                modifiers.push(t);
                if t == ev::FORCE_FLAM {
                    flam = nf::FLAM;
                }
                if matches!(t, ev::DISCO_FLIP_OFF | ev::DISCO_FLIP_ON | ev::DISCO_NO_FLIP_ON)
                    && disco_mod.is_none_or(|d| t < d)
                {
                    disco_mod = Some(t);
                }
            }
        }
        if let Some(d) = disco_mod {
            active_disco = d;
        }
        if notes.len() + kicks.len() == 0 {
            continue;
        }
        let mut group = Vec::new();
        for k in &kicks {
            let kick_flag = if k.ty == ev::KICK { nf::NONE } else { nf::DOUBLE_KICK };
            group.push(Untimed {
                tick: k.tick,
                length: k.length,
                ty: nt::KICK,
                flags: flam | kick_flag | ghost_or_accent(k.ty, &modifiers),
            });
        }
        let orange_and_green = has_orange && has_green;
        for n in &notes {
            let ty = drum_note_type(n.ty, orange_and_green);
            let can_disco = ty == nt::RED_DRUM || ty == nt::YELLOW_DRUM;
            let disco = if !can_disco || active_disco == ev::DISCO_FLIP_OFF {
                nf::NONE
            } else if active_disco == ev::DISCO_FLIP_ON {
                nf::DISCO
            } else {
                nf::DISCO_NOFLIP
            };
            group.push(Untimed {
                tick: n.tick,
                length: n.length,
                ty,
                flags: flam | disco | tom_or_cymbal(n.ty, &modifiers, drum_type, format) | ghost_or_accent(n.ty, &modifiers),
            });
        }
        out.push(group);
    }
    out
}

fn drum_note_type(t: u8, orange_and_green: bool) -> u8 {
    match t {
        ev::RED_DRUM => nt::RED_DRUM,
        ev::YELLOW_DRUM => nt::YELLOW_DRUM,
        ev::BLUE_DRUM => nt::BLUE_DRUM,
        ev::FIVE_ORANGE_FOUR_GREEN_DRUM => nt::GREEN_DRUM,
        ev::FIVE_GREEN_DRUM => {
            if orange_and_green {
                nt::BLUE_DRUM
            } else {
                nt::GREEN_DRUM
            }
        }
        _ => nt::RED_DRUM,
    }
}

fn tom_or_cymbal(t: u8, m: &[u8], drum_type: DrumType, format: Format) -> u32 {
    let has = |x: u8| m.contains(&x);
    match drum_type {
        DrumType::FourLane => nf::TOM,
        DrumType::FourLanePro => match format {
            Format::Mid => match t {
                ev::RED_DRUM => nf::TOM,
                ev::YELLOW_DRUM => if has(ev::YELLOW_TOM_MARKER) { nf::TOM } else { nf::CYMBAL },
                ev::BLUE_DRUM => if has(ev::BLUE_TOM_MARKER) { nf::TOM } else { nf::CYMBAL },
                ev::FIVE_ORANGE_FOUR_GREEN_DRUM => if has(ev::GREEN_TOM_MARKER) { nf::TOM } else { nf::CYMBAL },
                ev::FIVE_GREEN_DRUM => nf::TOM,
                _ => nf::NONE,
            },
            Format::Chart => match t {
                ev::RED_DRUM => nf::TOM,
                ev::YELLOW_DRUM => if has(ev::YELLOW_CYMBAL_MARKER) { nf::CYMBAL } else { nf::TOM },
                ev::BLUE_DRUM => if has(ev::BLUE_CYMBAL_MARKER) { nf::CYMBAL } else { nf::TOM },
                ev::FIVE_ORANGE_FOUR_GREEN_DRUM => if has(ev::GREEN_CYMBAL_MARKER) { nf::CYMBAL } else { nf::TOM },
                ev::FIVE_GREEN_DRUM => nf::TOM,
                _ => nf::NONE,
            },
        },
        DrumType::FiveLane => match t {
            ev::RED_DRUM => nf::TOM,
            ev::YELLOW_DRUM => nf::CYMBAL,
            ev::BLUE_DRUM => nf::TOM,
            ev::FIVE_ORANGE_FOUR_GREEN_DRUM => nf::CYMBAL,
            ev::FIVE_GREEN_DRUM => nf::TOM,
            _ => nf::NONE,
        },
    }
}

fn ghost_or_accent(t: u8, m: &[u8]) -> u32 {
    let pick = |accent: u8, ghost: u8| {
        if m.contains(&accent) {
            nf::ACCENT
        } else if m.contains(&ghost) {
            nf::GHOST
        } else {
            nf::NONE
        }
    };
    match t {
        ev::RED_DRUM => pick(ev::RED_ACCENT, ev::RED_GHOST),
        ev::YELLOW_DRUM => pick(ev::YELLOW_ACCENT, ev::YELLOW_GHOST),
        ev::BLUE_DRUM => pick(ev::BLUE_ACCENT, ev::BLUE_GHOST),
        ev::FIVE_ORANGE_FOUR_GREEN_DRUM => pick(ev::FIVE_ORANGE_FOUR_GREEN_ACCENT, ev::FIVE_ORANGE_FOUR_GREEN_GHOST),
        ev::FIVE_GREEN_DRUM => pick(ev::FIVE_GREEN_ACCENT, ev::FIVE_GREEN_GHOST),
        ev::KICK | ev::KICK2X => pick(ev::KICK_ACCENT, ev::KICK_GHOST),
        _ => nf::NONE,
    }
}

fn is_fret_event(t: u8) -> bool {
    (ev::OPEN..=ev::WHITE3).contains(&t)
}

fn fret_types(events: &[TrackEvent]) -> u32 {
    events.iter().filter(|e| is_fret_event(e.ty)).fold(0u32, |m, e| m | (1 << e.ty))
}
fn is_chord(events: &[TrackEvent]) -> bool {
    fret_types(events).count_ones() > 1
}
fn is_same_fret_note(a: &[TrackEvent], b: &[TrackEvent]) -> bool {
    fret_types(a) == fret_types(b)
}
fn is_in_fret_note(inner: &[TrackEvent], outer: &[TrackEvent]) -> bool {
    fret_types(inner) & !fret_types(outer) == 0
}

fn resolve_fret_modifiers(groups: Vec<Vec<TrackEvent>>, mods: &Modifiers, tpb: i64, format: Format) -> Vec<Vec<Untimed>> {
    let hopo_threshold = if mods.hopo_frequency != 0 {
        mods.hopo_frequency
    } else if mods.eighthnote_hopo {
        1 + tpb / 2
    } else if format == Format::Mid {
        (1.0 + tpb as f64 / 3.0).floor() as i64
    } else {
        ((65.0 / 192.0) * tpb as f64).floor() as i64
    };

    let mut out = Vec::new();
    let mut last_notes: Option<Vec<TrackEvent>> = None;
    for mut events in groups {
        let mut notes = Vec::new();
        let (mut f_open, mut f_tap, mut f_hopo, mut f_strum, mut f_unnatural) = (false, false, false, false, false);
        let mut longest: Option<TrackEvent> = None;
        for e in &events {
            match e.ty {
                t if is_fret_event(t) => {
                    notes.push(*e);
                    if longest.is_none_or(|l| e.length > l.length) {
                        longest = Some(*e);
                    }
                }
                ev::FORCE_OPEN => f_open = true,
                ev::FORCE_TAP => f_tap = true,
                ev::FORCE_HOPO => f_hopo = true,
                ev::FORCE_STRUM => f_strum = true,
                ev::FORCE_UNNATURAL => f_unnatural = true,
                _ => {}
            }
        }
        let effective = match (f_open, longest) {
            (true, Some(mut l)) => {
                l.ty = ev::OPEN;
                events.retain(|e| !is_fret_event(e.ty) && e.ty != ev::FORCE_OPEN);
                events.push(l);
                vec![l]
            }
            _ => notes,
        };
        if effective.is_empty() {
            continue;
        }
        let natural = match &last_notes {
            Some(last) => {
                effective[0].tick - last[0].tick <= hopo_threshold
                    && !is_chord(&effective)
                    && !(!is_chord(last) && is_same_fret_note(&events, last))
                    && !(format == Format::Mid && is_chord(last) && is_in_fret_note(&effective, last))
            }
            None => false,
        };
        let flags = if f_tap {
            nf::TAP
        } else if f_hopo {
            nf::HOPO
        } else if f_strum {
            nf::STRUM
        } else if (f_unnatural && natural) || (!f_unnatural && !natural) {
            nf::STRUM
        } else {
            nf::HOPO
        };
        // rv-index patch: remember forcing that changed the note.
        if !f_tap && (f_hopo || f_strum || f_unnatural) && flags != if natural { nf::HOPO } else { nf::STRUM } {
            FORCED.with(|f| f.set(true));
        }
        out.push(
            effective
                .iter()
                .map(|n| Untimed { tick: n.tick, length: n.length, ty: fret_note_type(n.ty), flags })
                .collect(),
        );
        last_notes = Some(effective);
    }
    out
}

fn fret_note_type(t: u8) -> u8 {
    match t {
        ev::OPEN => nt::OPEN,
        ev::GREEN => nt::GREEN,
        ev::RED => nt::RED,
        ev::YELLOW => nt::YELLOW,
        ev::BLUE => nt::BLUE,
        ev::ORANGE => nt::ORANGE,
        ev::BLACK1 => nt::BLACK1,
        ev::BLACK2 => nt::BLACK2,
        ev::BLACK3 => nt::BLACK3,
        ev::WHITE1 => nt::WHITE1,
        ev::WHITE2 => nt::WHITE2,
        ev::WHITE3 => nt::WHITE3,
        _ => nt::GREEN,
    }
}

fn snap_chords(groups: Vec<Vec<Untimed>>, threshold: i64, instrument: &str) -> Vec<Vec<Untimed>> {
    if threshold <= 0 || groups.is_empty() {
        return groups;
    }
    let mut it = groups.into_iter();
    let mut out = vec![it.next().unwrap()];
    for mut group in it {
        let last = out.last_mut().unwrap();
        if group[0].tick - last[0].tick >= threshold {
            out.push(group);
            continue;
        }
        if instrument == "drums" {
            let flags_of = |g: &[Untimed], t: u8| g.iter().find(|n| n.ty == t).map(|n| n.flags);
            for n in group.iter_mut() {
                if let Some(f) = flags_of(last, n.ty) {
                    n.flags = f;
                }
                if n.ty == nt::RED_DRUM || n.ty == nt::YELLOW_DRUM {
                    let r = flags_of(last, nt::RED_DRUM);
                    let y = flags_of(last, nt::YELLOW_DRUM);
                    if r.is_some() || y.is_some() {
                        let disco = nf::DISCO | nf::DISCO_NOFLIP;
                        let last_disco = (r.unwrap_or(0) | y.unwrap_or(0)) & disco;
                        n.flags = (n.flags & !disco) | last_disco;
                    }
                }
            }
        } else {
            let f = last[0].flags;
            group.iter_mut().for_each(|n| n.flags = f);
        }
        let tick = last[0].tick;
        for mut n in group {
            n.tick = tick;
            last.push(n);
        }
    }
    out
}

fn sort_and_fix_invalid_note_overlaps(groups: &mut [Vec<Untimed>]) {
    for g in groups.iter_mut() {
        if g.len() <= 1 {
            continue;
        }
        g.sort_by(|a, b| a.ty.cmp(&b.ty).then(b.length.cmp(&a.length)).then(b.flags.cmp(&a.flags)));
        g.dedup_by(|b, a| a.ty == b.ty);
    }
    // previous note of each type: (group index, note index)
    let mut prev: [Option<(usize, usize)>; nt::MAX + 1] = [None; nt::MAX + 1];
    for gi in 0..groups.len() {
        for ni in 0..groups[gi].len() {
            let ty = groups[gi][ni].ty as usize;
            if let Some((pg, pn)) = prev[ty] {
                let p = groups[pg][pn];
                let n = groups[gi][ni];
                if p.tick + p.length > n.tick {
                    groups[gi][ni].length = n.length.max(p.length - (n.tick - p.tick));
                    groups[pg][pn].length = n.tick - p.tick;
                }
            }
            prev[ty] = Some((gi, ni));
        }
    }
}

fn sort_and_fix_invalid_event_overlaps(events: &mut Vec<TimedSpan>) {
    if events.len() > 1 {
        events.sort_by(|a, b| a.tick.cmp(&b.tick).then(b.length.cmp(&a.length)));
        events.dedup_by(|b, a| a.tick == b.tick);
    }
    for i in 1..events.len() {
        let (p, e) = (events[i - 1], events[i]);
        if p.tick + p.length > e.tick {
            events[i].length = e.length.max(p.length - (e.tick - p.tick));
            events[i].ms_length = e.ms_length.max(p.ms_length - (e.ms - p.ms));
            events[i - 1].length = e.tick - p.tick;
            events[i - 1].ms_length = e.ms - p.ms;
        }
    }
}

// ---------- .chart parser (chart-file-parser.ts) ----------

const CHART_TRACKS: &[(&str, &str, &str)] = &[
    ("ExpertSingle", "guitar", "expert"),
    ("HardSingle", "guitar", "hard"),
    ("MediumSingle", "guitar", "medium"),
    ("EasySingle", "guitar", "easy"),
    ("ExpertDoubleGuitar", "guitarcoop", "expert"),
    ("HardDoubleGuitar", "guitarcoop", "hard"),
    ("MediumDoubleGuitar", "guitarcoop", "medium"),
    ("EasyDoubleGuitar", "guitarcoop", "easy"),
    ("ExpertDoubleRhythm", "rhythm", "expert"),
    ("HardDoubleRhythm", "rhythm", "hard"),
    ("MediumDoubleRhythm", "rhythm", "medium"),
    ("EasyDoubleRhythm", "rhythm", "easy"),
    ("ExpertDoubleBass", "bass", "expert"),
    ("HardDoubleBass", "bass", "hard"),
    ("MediumDoubleBass", "bass", "medium"),
    ("EasyDoubleBass", "bass", "easy"),
    ("ExpertDrums", "drums", "expert"),
    ("HardDrums", "drums", "hard"),
    ("MediumDrums", "drums", "medium"),
    ("EasyDrums", "drums", "easy"),
    ("ExpertKeyboard", "keys", "expert"),
    ("HardKeyboard", "keys", "hard"),
    ("MediumKeyboard", "keys", "medium"),
    ("EasyKeyboard", "keys", "easy"),
    ("ExpertGHLGuitar", "guitarghl", "expert"),
    ("HardGHLGuitar", "guitarghl", "hard"),
    ("MediumGHLGuitar", "guitarghl", "medium"),
    ("EasyGHLGuitar", "guitarghl", "easy"),
    ("ExpertGHLCoop", "guitarcoopghl", "expert"),
    ("HardGHLCoop", "guitarcoopghl", "hard"),
    ("MediumGHLCoop", "guitarcoopghl", "medium"),
    ("EasyGHLCoop", "guitarcoopghl", "easy"),
    ("ExpertGHLRhythm", "rhythmghl", "expert"),
    ("HardGHLRhythm", "rhythmghl", "hard"),
    ("MediumGHLRhythm", "rhythmghl", "medium"),
    ("EasyGHLRhythm", "rhythmghl", "easy"),
    ("ExpertGHLBass", "bassghl", "expert"),
    ("HardGHLBass", "bassghl", "hard"),
    ("MediumGHLBass", "bassghl", "medium"),
    ("EasyGHLBass", "bassghl", "easy"),
];

fn decode_text(data: &[u8]) -> String {
    if data.len() >= 2 && data[0] == 0xff && data[1] == 0xfe {
        let u: Vec<u16> = data[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    if data.len() >= 2 && data[0] == 0xfe && data[1] == 0xff {
        let u: Vec<u16> = data[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    let s = String::from_utf8_lossy(data);
    s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or_else(|| s.into_owned())
}

fn file_sections(text: &str) -> Result<HashMap<String, Vec<String>>, String> {
    let bytes = text.as_bytes();
    let mut sections = HashMap::new();
    let (mut skip_line, mut reading_section) = (false, false);
    let mut start = 0;
    let mut this_section: Option<String> = None;
    for (i, &c) in bytes.iter().enumerate() {
        if reading_section {
            if c == b']' {
                reading_section = false;
                this_section = Some(text[start..i].to_string());
            } else if c == b'\n' {
                return Err("Invalid .chart file: unexpected new line in section name".into());
            }
            continue;
        }
        if c == b'=' {
            skip_line = true;
        } else if c == b'\n' {
            skip_line = false;
        }
        if skip_line {
            continue;
        }
        match c {
            b'{' => {
                skip_line = true;
                start = i + 1;
            }
            b'}' => {
                let name = this_section.clone().ok_or("Invalid .chart file: section without a name")?;
                let lines = text[start..i]
                    .split('\n')
                    .map(|l| l.trim_matches([' ', '\t', '\r']))
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect();
                sections.insert(name, lines);
            }
            b'[' => {
                start = i + 1;
                reading_section = true;
            }
            _ => {}
        }
    }
    Ok(sections)
}

/// Splits "123 = rest" into (123, "rest").
fn split_tick(line: &str) -> Option<(i64, &str)> {
    let (t, rest) = line.split_once(" = ")?;
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((t.parse().ok()?, rest))
}

/// `^\s*\[?mix[ _]([0-3])[ _]drums([0-5])(d|dnoflip|easy|easynokick|)\]?\s*$`
fn parse_disco(text: &str) -> Option<(usize, &str)> {
    let s = text.trim();
    let s = s.strip_prefix('[').unwrap_or(s);
    let s = s.strip_suffix(']').unwrap_or(s);
    let s = s.strip_prefix("mix")?;
    let b = s.as_bytes();
    if b.len() < 9 || !(b[0] == b' ' || b[0] == b'_') || !(b'0'..=b'3').contains(&b[1]) || !(b[2] == b' ' || b[2] == b'_') {
        return None;
    }
    let rest = s[3..].strip_prefix("drums")?;
    let rb = rest.as_bytes();
    if rb.is_empty() || !(b'0'..=b'5').contains(&rb[0]) {
        return None;
    }
    let flag = &rest[1..];
    matches!(flag, "d" | "dnoflip" | "easy" | "easynokick" | "").then_some(((b[1] - b'0') as usize, flag))
}

fn disco_event(flag: &str) -> Option<u8> {
    match flag {
        "" => Some(ev::DISCO_FLIP_OFF),
        "d" => Some(ev::DISCO_FLIP_ON),
        "dnoflip" => Some(ev::DISCO_NO_FLIP_ON),
        _ => None,
    }
}

fn parse_dot_chart(data: &[u8]) -> Result<RawChart, String> {
    let text = decode_text(data);
    let sections = file_sections(&text)?;
    if sections.is_empty() {
        return Err("Invalid .chart file: no sections were found.".into());
    }
    let mut meta: HashMap<String, String> = HashMap::new();
    for line in sections.get("Song").into_iter().flatten() {
        if let Some((k, v)) = line.split_once(" = ") {
            let v = v.strip_prefix('"').unwrap_or(v);
            let v = v.strip_suffix('"').unwrap_or(v);
            meta.insert(k.to_string(), v.to_string());
        }
    }
    let resolution: i64 = meta
        .get("Resolution")
        .and_then(|r| r.trim().parse::<f64>().ok())
        .filter(|r| *r > 0.0)
        .ok_or("Invalid .chart file: resolution not found.")? as i64;
    let chart_offset_secs = meta.get("Offset").and_then(|o| o.trim().parse::<f64>().ok()).unwrap_or(0.0);

    let mut tempos = Vec::new();
    for line in sections.get("SyncTrack").into_iter().flatten() {
        if let Some((tick, rest)) = split_tick(line)
            && let Some(v) = rest.strip_prefix("B ")
            && let Ok(milli_bpm) = v.parse::<i64>()
        {
            if milli_bpm == 0 {
                return Err(format!("Invalid .chart file: Tempo at tick {tick} was zero."));
            }
            tempos.push((tick, milli_bpm as f64 / 1000.0));
        }
    }
    if tempos.first().is_none_or(|t| t.0 != 0) {
        tempos.insert(0, (0, 120.0));
    }

    let mut time_sigs = Vec::new();
    for line in sections.get("SyncTrack").into_iter().flatten() {
        if let Some((tick, rest)) = split_tick(line)
            && let Some(v) = rest.strip_prefix("TS ")
        {
            let mut p = v.split(' ');
            let num = p.next().and_then(|n| n.parse::<u32>().ok());
            let den = p.next().map(|d| d.parse::<u32>().ok());
            if p.next().is_some() {
                continue;
            }
            let (Some(num), Some(den)) = (num, den.unwrap_or(Some(2))) else { continue };
            if num == 0 {
                return Err(format!("Invalid .chart file: Time signature numerator at tick {tick} was zero."));
            }
            time_sigs.push((tick, num, 2u32.saturating_pow(den)));
        }
    }
    if time_sigs.first().is_none_or(|t| t.0 != 0) {
        time_sigs.insert(0, (0, 4, 4));
    }

    let mut global = GlobalEvents::default();
    for line in sections.get("Events").into_iter().flatten() {
        let Some((tick, rest)) = split_tick(line) else { continue };
        let Some(text) = rest.strip_prefix("E \"").and_then(|t| t.strip_suffix('"')) else { continue };
        classify_event_text(text, tick, &mut global, false);
    }

    // First [coda] event in the Events section.
    let first_coda = sections.get("Events").into_iter().flatten().find_map(|line| {
        let (tick, rest) = split_tick(line)?;
        let text = rest.strip_prefix("E \"")?.strip_suffix('"')?;
        is_coda(text).then_some(tick)
    });

    let mut tracks = Vec::new();
    for &(name, instrument, difficulty) in CHART_TRACKS {
        let Some(lines) = sections.get(name) else { continue };
        let drums = instrument == "drums";
        let ghl = instrument_type(instrument) == InstrumentType::SixFret;
        let mut events: Vec<TrackEvent> = Vec::new();
        for line in lines {
            let Some((tick, rest)) = split_tick(line) else { continue };
            if let Some(nv) = rest.strip_prefix("N ").or_else(|| rest.strip_prefix("S ")) {
                let is_s = rest.starts_with('S');
                let mut parts = nv.split(' ');
                let value = parts.next().unwrap_or("");
                let length: i64 = parts.next().and_then(|l| l.parse().ok()).unwrap_or(0);
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    continue;
                }
                let ty = if is_s {
                    match value {
                        "2" => Some(ev::STAR_POWER),
                        "64" => Some(ev::FREESTYLE_SECTION),
                        "65" => Some(ev::FLEX_LANE_SINGLE),
                        "66" => Some(ev::FLEX_LANE_DOUBLE),
                        _ => None,
                    }
                } else if drums {
                    match value {
                        "0" => Some(ev::KICK),
                        "1" => Some(ev::RED_DRUM),
                        "2" => Some(ev::YELLOW_DRUM),
                        "3" => Some(ev::BLUE_DRUM),
                        "4" => Some(ev::FIVE_ORANGE_FOUR_GREEN_DRUM),
                        "5" => Some(ev::FIVE_GREEN_DRUM),
                        "32" => Some(ev::KICK2X),
                        "34" => Some(ev::RED_ACCENT),
                        "35" => Some(ev::YELLOW_ACCENT),
                        "36" => Some(ev::BLUE_ACCENT),
                        "37" => Some(ev::FIVE_ORANGE_FOUR_GREEN_ACCENT),
                        "38" => Some(ev::FIVE_GREEN_ACCENT),
                        "40" => Some(ev::RED_GHOST),
                        "41" => Some(ev::YELLOW_GHOST),
                        "42" => Some(ev::BLUE_GHOST),
                        "43" => Some(ev::FIVE_ORANGE_FOUR_GREEN_GHOST),
                        "44" => Some(ev::FIVE_GREEN_GHOST),
                        "66" => Some(ev::YELLOW_CYMBAL_MARKER),
                        "67" => Some(ev::BLUE_CYMBAL_MARKER),
                        "68" => Some(ev::GREEN_CYMBAL_MARKER),
                        _ => None,
                    }
                } else if ghl {
                    match value {
                        "0" => Some(ev::WHITE1),
                        "1" => Some(ev::WHITE2),
                        "2" => Some(ev::WHITE3),
                        "3" => Some(ev::BLACK1),
                        "4" => Some(ev::BLACK2),
                        "5" => Some(ev::FORCE_UNNATURAL),
                        "6" => Some(ev::FORCE_TAP),
                        "7" => Some(ev::OPEN),
                        "8" => Some(ev::BLACK3),
                        _ => None,
                    }
                } else {
                    match value {
                        "0" => Some(ev::GREEN),
                        "1" => Some(ev::RED),
                        "2" => Some(ev::YELLOW),
                        "3" => Some(ev::BLUE),
                        "4" => Some(ev::ORANGE),
                        "5" => Some(ev::FORCE_UNNATURAL),
                        "6" => Some(ev::FORCE_TAP),
                        "7" => Some(ev::OPEN),
                        _ => None,
                    }
                };
                if let Some(ty) = ty {
                    events.push(TrackEvent { tick, length, ty });
                }
            } else if let Some(e) = rest.strip_prefix("E ") {
                let value = e.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(e);
                let ty = match value {
                    "solo" => Some(ev::SOLO_SECTION_START),
                    "soloend" => Some(ev::SOLO_SECTION_END),
                    _ => parse_disco(value)
                        .filter(|(d, _)| ["easy", "medium", "hard", "expert"][*d] == difficulty)
                        .and_then(|(_, f)| disco_event(f)),
                };
                if let Some(ty) = ty {
                    events.push(TrackEvent { tick, length: 0, ty });
                }
            }
        }
        events.sort_by_key(|e| e.tick); // stable, like Array.sort
        merge_solo_events(&mut events);

        let mut t = RawTrack::new(instrument, difficulty);
        for e in events {
            t.categorize(e.tick, e.length, e.ty, first_coda);
        }
        tracks.push(t);
    }

    Ok(RawChart { resolution, tempos, time_sigs, tracks, chart_offset_secs, global })
}

fn merge_solo_events(events: &mut Vec<TrackEvent>) {
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..events.len() {
        match events[i].ty {
            ev::SOLO_SECTION_START => starts.push(i),
            ev::SOLO_SECTION_END => {
                if let Some(s) = starts.pop() {
                    events[s].ty = ev::SOLO_SECTION;
                    events[s].length = events[i].tick - events[s].tick + 1;
                }
            }
            _ => {}
        }
    }
    events.retain(|e| e.ty != ev::SOLO_SECTION_START && e.ty != ev::SOLO_SECTION_END);
}

/// `^\s*\[?coda\]?\s*$`
fn is_coda(text: &str) -> bool {
    let s = text.trim();
    let s = s.strip_prefix('[').unwrap_or(s);
    let s = s.strip_suffix(']').unwrap_or(s);
    s == "coda"
}

impl RawTrack {
    fn new(instrument: &'static str, difficulty: &'static str) -> Self {
        Self {
            instrument,
            difficulty,
            star_power: Vec::new(),
            solo: Vec::new(),
            flex: Vec::new(),
            freestyle: Vec::new(),
            rejected_star_power: Vec::new(),
            events: Vec::new(),
        }
    }

    /// Routes a phrase-like event to its list, or keeps it as a track event (returns true).
    fn categorize(&mut self, tick: i64, length: i64, ty: u8, first_coda: Option<i64>) -> bool {
        let span = Span { tick, length };
        match ty {
            ev::STAR_POWER => self.star_power.push(span),
            ev::SOLO_SECTION => self.solo.push(span),
            ev::REJECTED_STAR_POWER => self.rejected_star_power.push(span),
            ev::FLEX_LANE_SINGLE => self.flex.push((span, false)),
            ev::FLEX_LANE_DOUBLE => self.flex.push((span, true)),
            ev::FREESTYLE_SECTION => self.freestyle.push((span, first_coda.is_some_and(|c| tick >= c))),
            _ => {
                self.events.push(TrackEvent { tick, length, ty });
                return true;
            }
        }
        false
    }
}

// ---------- .mid parser (midi-file-parser.ts) ----------

const MIDI_TRACKS: &[(&str, &str)] = &[
    ("T1 GEMS", "guitar"),
    ("PART GUITAR", "guitar"),
    ("PART GUITAR COOP", "guitarcoop"),
    ("PART RHYTHM", "rhythm"),
    ("PART BASS", "bass"),
    ("PART DRUMS", "drums"),
    ("PART KEYS", "keys"),
    ("PART GUITAR GHL", "guitarghl"),
    ("PART GUITAR COOP GHL", "guitarcoopghl"),
    ("PART RHYTHM GHL", "rhythmghl"),
    ("PART BASS GHL", "bassghl"),
];
const OTHER_KNOWN_TRACKS: &[&str] =
    &["PART VOCALS", "HARM1", "HARM2", "HARM3", "PART HARM1", "PART HARM2", "PART HARM3", "EVENTS"];

#[derive(Debug, Clone, Copy)]
struct EventEnd {
    tick: i64,
    ty: u8,
    velocity: u8,
    channel: u8,
    is_start: bool,
}

#[derive(Debug, Clone, Copy)]
struct MidiTrackEvent {
    tick: i64,
    length: i64,
    ty: u8,
    velocity: u8,
    channel: u8,
}

enum MidiEv<'a> {
    Note { tick: i64, on: bool, key: u8, vel: u8, channel: u8 },
    SysEx { tick: i64, data: &'a [u8] },
    Text { tick: i64, text: String },
}

fn parse_midi(data: &[u8], mods: &Modifiers) -> Result<RawChart, String> {
    use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
    let smf = Smf::parse(data).map_err(|e| format!("Invalid .mid file: {e}"))?;
    if smf.header.format != midly::Format::Parallel {
        return Err("Invalid .mid file: unsupported header format".into());
    }
    let resolution = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as i64,
        Timing::Timecode(..) => return Err("Invalid .mid file: SMPTE timing is not supported".into()),
    };
    if resolution == 0 {
        return Err("Invalid .mid file: zero resolution".into());
    }
    if smf.tracks.is_empty() {
        return Err("Invalid .mid file: no tracks detected".into());
    }

    // Tempo map from the first (conductor) track.
    let mut tempos = Vec::new();
    let mut tick = 0i64;
    for e in &smf.tracks[0] {
        tick += e.delta.as_int() as i64;
        if let TrackEventKind::Meta(MetaMessage::Tempo(us)) = e.kind {
            let us = us.as_int();
            if us == 0 {
                return Err(format!("Invalid .mid file: Tempo at tick {tick} was zero."));
            }
            tempos.push((tick, 60_000_000.0 / us as f64));
        }
    }
    if tempos.first().is_none_or(|t| t.0 != 0) {
        tempos.insert(0, (0, 120.0));
    }
    let mut time_sigs = Vec::new();
    let mut tick = 0i64;
    for e in &smf.tracks[0] {
        tick += e.delta.as_int() as i64;
        if let TrackEventKind::Meta(MetaMessage::TimeSignature(num, den_pow, _, _)) = e.kind {
            if num == 0 {
                return Err(format!("Invalid .mid file: Time signature numerator at tick {tick} was zero."));
            }
            time_sigs.push((tick, num as u32, 2u32.saturating_pow(den_pow as u32)));
        }
    }
    if time_sigs.first().is_none_or(|t| t.0 != 0) {
        time_sigs.insert(0, (0, 4, 4));
    }

    let track_name = |track: &[midly::TrackEvent<'_>]| -> Option<String> {
        for e in track.iter() {
            if e.delta.as_int() != 0 {
                break;
            }
            if let TrackEventKind::Meta(MetaMessage::TrackName(n)) = e.kind {
                let n = String::from_utf8_lossy(n);
                if MIDI_TRACKS.iter().any(|(t, _)| *t == n) || OTHER_KNOWN_TRACKS.contains(&n.as_ref()) {
                    return Some(n.into_owned());
                }
            }
        }
        None
    };

    // First [coda] in the EVENTS track (text-like meta events).
    let first_coda = smf.tracks.iter().find(|t| track_name(t).as_deref() == Some("EVENTS")).and_then(|t| {
        let mut tick = 0i64;
        for e in t.iter() {
            tick += e.delta.as_int() as i64;
            if let TrackEventKind::Meta(
                MetaMessage::Text(b) | MetaMessage::Lyric(b) | MetaMessage::Marker(b) | MetaMessage::CuePoint(b),
            ) = e.kind
                && is_coda(&String::from_utf8_lossy(b))
            {
                return Some(tick);
            }
        }
        None
    });

    let mut global = GlobalEvents::default();
    for track in &smf.tracks {
        let name = track_name(track);
        let is_events = name.as_deref() == Some("EVENTS");
        let is_vocals = name.as_deref().is_some_and(|n| n == "PART VOCALS" || n.contains("HARM"));
        if !is_events && !is_vocals {
            continue;
        }
        let mut tick = 0i64;
        for e in track.iter() {
            tick += e.delta.as_int() as i64;
            match e.kind {
                TrackEventKind::Meta(
                    MetaMessage::Text(b) | MetaMessage::Lyric(b) | MetaMessage::Marker(b) | MetaMessage::CuePoint(b),
                ) => {
                    let text = String::from_utf8_lossy(b);
                    if is_events {
                        classify_event_text(&text, tick, &mut global, true);
                    } else if !text.trim().is_empty() && !text.starts_with('[') {
                        global.has_lyrics = true;
                    }
                }
                TrackEventKind::Midi { message: MidiMessage::NoteOn { key, vel }, .. }
                    if is_vocals && vel.as_int() > 0 && (36..=84).contains(&key.as_int()) =>
                {
                    global.has_vocals = true;
                }
                _ => {}
            }
        }
    }
    global.has_lyrics &= global.has_vocals;

    let mut tracks = Vec::new();
    for track in &smf.tracks {
        let Some(instrument) =
            track_name(track).and_then(|n| MIDI_TRACKS.iter().find(|(t, _)| *t == n).map(|(_, i)| *i))
        else {
            continue;
        };
        let mut evs = Vec::with_capacity(track.len());
        let mut tick = 0i64;
        for e in track.iter() {
            tick += e.delta.as_int() as i64;
            match e.kind {
                TrackEventKind::Midi { channel, message } => match message {
                    MidiMessage::NoteOn { key, vel } => evs.push(MidiEv::Note {
                        tick,
                        on: vel.as_int() != 0,
                        key: key.as_int(),
                        vel: vel.as_int(),
                        channel: channel.as_int(),
                    }),
                    MidiMessage::NoteOff { key, vel } => evs.push(MidiEv::Note {
                        tick,
                        on: false,
                        key: key.as_int(),
                        vel: vel.as_int(),
                        channel: channel.as_int(),
                    }),
                    _ => {}
                },
                TrackEventKind::SysEx(d) | TrackEventKind::Escape(d) => evs.push(MidiEv::SysEx { tick, data: d }),
                TrackEventKind::Meta(MetaMessage::Text(t)) => {
                    evs.push(MidiEv::Text { tick, text: String::from_utf8_lossy(t).into_owned() })
                }
                _ => {}
            }
        }
        tracks.extend(build_midi_track(instrument, &evs, mods, first_coda));
    }

    Ok(RawChart { resolution, tempos, time_sigs, tracks, chart_offset_secs: 0.0, global })
}

/// Sorts one EVENTS text event into sections / end events / lyrics (scan-chart's rules).
fn classify_event_text(text: &str, tick: i64, g: &mut GlobalEvents, is_mid: bool) {
    let inner = text.strip_prefix('[').and_then(|t| t.strip_suffix(']'));
    let section_body = |t: &str| {
        ["section ", "section_", "prc ", "prc_"].iter().any(|p| t.starts_with(p))
    };
    if inner.is_some_and(section_body) || section_body(text) {
        g.sections.push(tick);
        return;
    }
    if inner.unwrap_or(text) == "end" {
        g.end_events.push(tick);
        return;
    }
    let lyric = text.trim_start_matches('[').trim_start().starts_with("lyric ")
        || text.trim_start_matches('[').trim_start().starts_with("lyric\t");
    if is_mid {
        if lyric {
            g.parse_issues.push("invalidLyric");
        } else if inner.unwrap_or(text) == "phrase_start" {
            g.parse_issues.push("invalidPhraseStart");
        } else if inner.unwrap_or(text) == "phrase_end" {
            g.parse_issues.push("invalidPhraseEnd");
        }
    } else if lyric {
        // .chart files have lyrics but no sung notes, so they don't count as vocals.
        g.has_lyrics = true;
    }
}

fn diff_index(d: &str) -> usize {
    match d {
        "expert" => 0,
        "hard" => 1,
        "medium" => 2,
        _ => 3,
    }
}

fn build_midi_track(
    instrument: &'static str,
    evs: &[MidiEv<'_>],
    mods: &Modifiers,
    first_coda: Option<i64>,
) -> Vec<RawTrack> {
    let itype = instrument_type(instrument);
    // Index 0..=3: expert, hard, medium, easy; 4: all
    let mut ends: [Vec<EventEnd>; 5] = Default::default();
    let mut enhanced_opens = false;

    for e in evs {
        match e {
            MidiEv::Note { tick, on, key, vel, channel } => {
                let nn = *key;
                if nn == 105 || nn == 106 {
                    continue; // versus phrases
                }
                let (slot, diff) = if nn <= 66 {
                    (3, "easy")
                } else if nn <= 78 {
                    (2, "medium")
                } else if nn <= 90 {
                    (1, "hard")
                } else if nn <= 102 {
                    (0, "expert")
                } else {
                    (4, "all")
                };
                let ty = if slot == 4 {
                    instrument_event_type(nn)
                } else {
                    match itype {
                        InstrumentType::SixFret => six_fret_type(nn, diff),
                        InstrumentType::Drums => drums_type(nn, diff),
                        InstrumentType::FiveFret => five_fret_type(nn, diff, enhanced_opens),
                    }
                };
                if let Some(ty) = ty {
                    ends[slot].push(EventEnd { tick: *tick, ty, velocity: *vel, channel: *channel, is_start: *on });
                }
            }
            MidiEv::SysEx { tick, data } => {
                let ps = data.len() > 6 && data[0] == 0x50 && data[1] == 0x53 && data[2] == 0 && data[3] == 0;
                if !ps {
                    continue;
                }
                let ty = match data[5] {
                    0x01 => ev::FORCE_OPEN,
                    0x04 => ev::FORCE_TAP,
                    _ => continue,
                };
                let slot = match data[4] {
                    0xff => 4,
                    0 => 3,
                    1 => 2,
                    2 => 1,
                    _ => 0,
                };
                ends[slot].push(EventEnd { tick: *tick, ty, velocity: 127, channel: 1, is_start: data[6] == 0x01 });
            }
            MidiEv::Text { tick, text } => {
                if itype == InstrumentType::Drums
                    && let Some((d, flag)) = parse_disco(text)
                    && let Some(ty) = disco_event(flag)
                {
                    let slot = diff_index(["easy", "medium", "hard", "expert"][d]);
                    ends[slot].push(EventEnd { tick: *tick, ty, velocity: 127, channel: 1, is_start: true });
                    ends[slot].push(EventEnd { tick: *tick, ty, velocity: 127, channel: 1, is_start: false });
                }
                if text == "ENHANCED_OPENS" || text == "[ENHANCED_OPENS]" {
                    enhanced_opens = true;
                } else if text == "ENABLE_CHART_DYNAMICS" || text == "[ENABLE_CHART_DYNAMICS]" {
                    for is_start in [true, false] {
                        ends[4].push(EventEnd { tick: *tick, ty: ev::ENABLE_CHART_DYNAMICS, velocity: 127, channel: 1, is_start });
                    }
                }
            }
        }
    }

    // distributeInstrumentEvents
    let all = std::mem::take(&mut ends[4]);
    for slot in ends.iter_mut().take(4) {
        if slot.is_empty() {
            continue;
        }
        slot.extend(all.iter().copied());
        slot.sort_by(|a, b| a.tick.cmp(&b.tick).then(b.ty.cmp(&a.ty)));
    }

    // getTrackEvents: pair note on/off
    let mut paired: [Vec<MidiTrackEvent>; 4] = Default::default();
    for d in 0..4 {
        let mut out: Vec<MidiTrackEvent> = Vec::new();
        let mut partial: HashMap<u8, Vec<usize>> = HashMap::new();
        for e in &ends[d] {
            if e.is_start {
                partial.entry(e.ty).or_default().push(out.len());
                out.push(MidiTrackEvent { tick: e.tick, length: -1, ty: e.ty, velocity: e.velocity, channel: e.channel });
            } else if let Some(list) = partial.get_mut(&e.ty)
                && let Some(pos) = list.iter().rposition(|&i| out[i].channel == e.channel)
            {
                let i = list.remove(pos);
                out[i].length = e.tick - out[i].tick;
            }
        }
        out.retain(|e| e.length != -1);
        paired[d] = out;
    }

    // splitMidiModifierSustains
    let modifier_sustains: &[u8] = if itype == InstrumentType::Drums {
        &[ev::FORCE_FLAM, ev::YELLOW_TOM_MARKER, ev::BLUE_TOM_MARKER, ev::GREEN_TOM_MARKER]
    } else {
        &[ev::FORCE_OPEN, ev::FORCE_TAP, ev::FORCE_STRUM, ev::FORCE_HOPO]
    };
    let modifiable: &[u8] = match itype {
        InstrumentType::FiveFret => &[ev::OPEN, ev::GREEN, ev::RED, ev::YELLOW, ev::BLUE, ev::ORANGE],
        InstrumentType::SixFret => {
            &[ev::OPEN, ev::BLACK3, ev::BLACK2, ev::BLACK1, ev::WHITE3, ev::WHITE2, ev::WHITE1]
        }
        InstrumentType::Drums => &[
            ev::KICK,
            ev::KICK2X,
            ev::RED_DRUM,
            ev::YELLOW_DRUM,
            ev::BLUE_DRUM,
            ev::FIVE_ORANGE_FOUR_GREEN_DRUM,
            ev::FIVE_GREEN_DRUM,
        ],
    };
    let mut enable_dynamics = false;
    let mut split: [Vec<MidiTrackEvent>; 4] = Default::default();
    for d in 0..4 {
        let mut has_notes = false;
        let mut active: Vec<MidiTrackEvent> = Vec::new();
        let mut latest: HashMap<u8, i64> = HashMap::new();
        let mut out = Vec::new();
        for e in &paired[d] {
            if e.ty == ev::ENABLE_CHART_DYNAMICS {
                enable_dynamics = true;
                continue;
            }
            active.retain(|m| if m.length == 0 { m.tick >= e.tick } else { m.tick + m.length > e.tick });
            if modifier_sustains.contains(&e.ty) {
                active.push(*e);
                continue;
            }
            if modifiable.contains(&e.ty) {
                has_notes = true;
                for m in &active {
                    if latest.get(&m.ty).is_none_or(|t| *t < e.tick) {
                        latest.insert(m.ty, e.tick);
                        out.push(MidiTrackEvent { tick: e.tick, length: 0, ty: m.ty, velocity: m.velocity, channel: m.channel });
                    }
                }
                if enable_dynamics && itype == InstrumentType::Drums && (e.velocity == 1 || e.velocity == 127) {
                    let ty = if e.velocity == 1 { drum_ghost_type(e.ty) } else { drum_accent_type(e.ty) };
                    if let Some(ty) = ty {
                        out.push(MidiTrackEvent { tick: e.tick, length: 0, ty, velocity: 127, channel: e.channel });
                    }
                }
            }
            out.push(*e);
        }
        split[d] = if has_notes { out } else { Vec::new() };
    }

    // fixLegacyGhStarPower
    if itype != InstrumentType::Drums && mods.multiplier_note != 116 {
        for events in split.iter_mut() {
            let sp = events.iter().filter(|e| e.ty == ev::STAR_POWER).count();
            let solo = events.iter().filter(|e| e.ty == ev::SOLO_SECTION).count();
            if mods.multiplier_note == 103 || (sp == 0 && solo > 1) {
                for e in events.iter_mut() {
                    if e.ty == ev::SOLO_SECTION {
                        e.ty = ev::STAR_POWER;
                    } else if e.ty == ev::STAR_POWER {
                        e.ty = ev::REJECTED_STAR_POWER;
                    }
                }
            }
        }
    }

    // fixFlexLaneLds: lower difficulties only keep flex lanes within a velocity range.
    for (d, max_vel) in [(1usize, 50u8), (2, 40), (3, 30)] {
        split[d].retain(|e| {
            !((e.ty == ev::FLEX_LANE_SINGLE || e.ty == ev::FLEX_LANE_DOUBLE) && (e.velocity < 21 || e.velocity > max_vel))
        });
    }

    let mut out = Vec::new();
    for (d, difficulty) in DIFFICULTIES.iter().enumerate() {
        let events = &split[d];
        if events.is_empty() {
            continue;
        }
        let mut t = RawTrack::new(instrument, difficulty);
        let mut has_real = false;
        for e in events {
            if t.categorize(e.tick, e.length, e.ty, first_coda) && e.ty != ev::ENABLE_CHART_DYNAMICS {
                has_real = true;
            }
        }
        if has_real || !t.star_power.is_empty() || !t.solo.is_empty() {
            out.push(t);
        }
    }
    out
}

fn instrument_event_type(n: u8) -> Option<u8> {
    Some(match n {
        103 => ev::SOLO_SECTION,
        104 => ev::FORCE_TAP,
        109 => ev::FORCE_FLAM,
        110 => ev::YELLOW_TOM_MARKER,
        111 => ev::BLUE_TOM_MARKER,
        112 => ev::GREEN_TOM_MARKER,
        116 => ev::STAR_POWER,
        120 => ev::FREESTYLE_SECTION,
        126 => ev::FLEX_LANE_SINGLE,
        127 => ev::FLEX_LANE_DOUBLE,
        _ => return None,
    })
}

fn diff_start(diff: &str, starts: [i32; 4]) -> i32 {
    match diff {
        "easy" => starts[0],
        "medium" => starts[1],
        "hard" => starts[2],
        _ => starts[3],
    }
}

fn six_fret_type(n: u8, diff: &str) -> Option<u8> {
    Some(match n as i32 - diff_start(diff, [58, 70, 82, 94]) {
        0 => ev::OPEN,
        1 => ev::WHITE1,
        2 => ev::WHITE2,
        3 => ev::WHITE3,
        4 => ev::BLACK1,
        5 => ev::BLACK2,
        6 => ev::BLACK3,
        7 => ev::FORCE_HOPO,
        8 => ev::FORCE_STRUM,
        _ => return None,
    })
}

fn five_fret_type(n: u8, diff: &str, enhanced_opens: bool) -> Option<u8> {
    Some(match n as i32 - diff_start(diff, [59, 71, 83, 95]) {
        0 if enhanced_opens => ev::OPEN,
        1 => ev::GREEN,
        2 => ev::RED,
        3 => ev::YELLOW,
        4 => ev::BLUE,
        5 => ev::ORANGE,
        6 => ev::FORCE_HOPO,
        7 => ev::FORCE_STRUM,
        _ => return None,
    })
}

fn drums_type(n: u8, diff: &str) -> Option<u8> {
    Some(match n as i32 - diff_start(diff, [60, 72, 84, 96]) {
        -1 => ev::KICK2X,
        0 => ev::KICK,
        1 => ev::RED_DRUM,
        2 => ev::YELLOW_DRUM,
        3 => ev::BLUE_DRUM,
        4 => ev::FIVE_ORANGE_FOUR_GREEN_DRUM,
        5 => ev::FIVE_GREEN_DRUM,
        _ => return None,
    })
}

fn drum_ghost_type(t: u8) -> Option<u8> {
    Some(match t {
        ev::RED_DRUM => ev::RED_GHOST,
        ev::YELLOW_DRUM => ev::YELLOW_GHOST,
        ev::BLUE_DRUM => ev::BLUE_GHOST,
        ev::FIVE_ORANGE_FOUR_GREEN_DRUM => ev::FIVE_ORANGE_FOUR_GREEN_GHOST,
        ev::FIVE_GREEN_DRUM => ev::FIVE_GREEN_GHOST,
        ev::KICK | ev::KICK2X => ev::KICK_GHOST,
        _ => return None,
    })
}

fn drum_accent_type(t: u8) -> Option<u8> {
    Some(match t {
        ev::RED_DRUM => ev::RED_ACCENT,
        ev::YELLOW_DRUM => ev::YELLOW_ACCENT,
        ev::BLUE_DRUM => ev::BLUE_ACCENT,
        ev::FIVE_ORANGE_FOUR_GREEN_DRUM => ev::FIVE_ORANGE_FOUR_GREEN_ACCENT,
        ev::FIVE_GREEN_DRUM => ev::FIVE_GREEN_ACCENT,
        ev::KICK | ev::KICK2X => ev::KICK_ACCENT,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// TRUSS_TEST_SNG=<file.sng>: parse its chart and print a summary.
    #[test]
    fn parse_real_chart() {
        let Ok(path) = std::env::var("TRUSS_TEST_SNG") else { return };
        let data = std::fs::read(path).unwrap();
        let sng = crate::sng::read_sng(&data, |n| n.ends_with(".chart") || n.ends_with(".mid")).unwrap();
        let (name, bytes) = &sng.files[0];
        let t = std::time::Instant::now();
        let parsed = parse_chart_file(bytes, name.ends_with(".mid"), &Modifiers::default()).unwrap();
        println!("parsed {name} in {:?}, drum type {:?}", t.elapsed(), parsed.drum_type);
        for tr in &parsed.tracks {
            let notes: usize = tr.note_groups.iter().map(|g| g.len()).sum();
            let hopos = tr.note_groups.iter().filter(|g| g[0].flags & nf::HOPO != 0).count();
            let taps = tr.note_groups.iter().filter(|g| g[0].flags & nf::TAP != 0).count();
            let opens = tr.note_groups.iter().flatten().filter(|n| n.ty == nt::OPEN).count();
            let sustains = tr.note_groups.iter().flatten().filter(|n| n.ms_length > 0.0).count();
            println!("  sustained notes: {sustains}");
            println!(
                "  {} {}: {} notes, {} groups, {} hopo groups, {} tap groups, {} opens, {} SP, last {:.0}ms",
                tr.instrument,
                tr.difficulty,
                notes,
                tr.note_groups.len(),
                hopos,
                taps,
                opens,
                tr.star_power.len(),
                tr.note_groups.last().map(|g| g[0].ms).unwrap_or(0.0)
            );
        }
    }
}
