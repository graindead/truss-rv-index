# rv-index format (version 1)

`rv-index.json.zst` is one JSON document compressed with [zstd](https://facebook.github.io/zstd/)
(level 19). It describes every Clone Hero chart in RhythmVerse's list that the scanner has seen,
keyed by RhythmVerse `file_id`. It holds derived data only: no chart files, audio or art.

## Top level

| Key           | Type   | Meaning |
|---------------|--------|---------|
| `format`      | int    | Format version, currently `1`. A reader should refuse a version it doesn't know; fields may be added without a version bump. |
| `generated`   | string | When the file was written, RFC 3339 UTC. |
| `hash_layout` | string | `"scan-chart-2025"`: track hashes use scan-chart's current layout (Aug 2025: renumbered note types, 64-bit lengths), as Clone Hero's leaderboards and Truss's `trackhash::CURRENT` do. |
| `charts`      | array  | One entry per chart, sorted by `id`. |

## Chart entry

| Key  | Type   | Present | Meaning |
|------|--------|---------|---------|
| `id` | string | always  | RhythmVerse `file_id`. |
| `m`  | string | always  | RhythmVerse `update_date` (`YYYY-MM-DD HH:MM:SS`) of the version the data was read from. If RhythmVerse lists a different date, the entry is stale and will be rescanned. |
| `s`  | string | always  | `ok`, `failed` (could not be read this time; retried with backoff) or `unavailable` (dead link, unsupported host, no or broken chart: permanent for this version). |
| `r`  | string | not ok  | Why, as a short message. |
| `h`  | object | ok      | Leaderboard track hashes: `{instrument: {difficulty: hash}}`. Instruments use scan-chart/Encore names (`guitar`, `guitarcoop`, `rhythm`, `bass`, `drums`, `keys`, `guitarghl`, `guitarcoopghl`, `rhythmghl`, `bassghl`). Difficulties: `x` expert, `h` hard, `m` medium, `e` easy. Each hash is 43 characters of unpadded base64url (blake3, 32 bytes). |
| `w`  | object | ok, when rated | Fretwork ratings of each instrument's Expert track: `{instrument: rating}`. Fretwork rates 5-fret instruments and drums only. |
| `f`  | int    | ok      | Feature bits, below. |
| `an` | number | ok      | Average notes per second (note groups / time from first to last note) of the densest instrument's Expert track, 2 decimals. |
| `mn` | number | ok      | The most note groups in any 1-second window of any Expert track. |
| `i`  | array  | ok, when any | scan-chart chart issue types found (e.g. `noStarPower`, `brokenNote`, `smallLeadingSilence`); see scan-chart's `findChartIssues`. Folder/metadata issues (audio, art, ini values) aren't checked. |

### Rating (`w` values)

| Key  | Type   | Meaning (Fretwork v3, as in Truss's `fretwork::Rating`) |
|------|--------|---------|
| `t`  | int    | Open-ended tier (very hard charts pass 6). |
| `r`  | int    | 0–6 tier spread like official tiers (remap). |
| `d`  | number | Difficulty score, 3 decimals. |
| `mv` | number | Hand movement per second, 3 decimals. |
| `sp` | number | Note-rate spread (coefficient of variation), 3 decimals. |
| `k`  | number | Drums only: single-pedal kicks per second. |
| `t2` | int    | Drums only, when the chart has 2x kick: open-ended tier counting 2x kick notes. |

### Feature bits (`f`)

| Bit | Value | Feature |
|-----|-------|---------|
| 0 | 1   | Solo sections |
| 1 | 2   | Forced notes: a HOPO/strum force marker changes some fret note from its natural state |
| 2 | 4   | Open notes (fret instruments) |
| 3 | 8   | Tap notes |
| 4 | 16  | Lyrics |
| 5 | 32  | Vocals |
| 6 | 64  | Roll lanes (drum flex lanes) |
| 7 | 128 | 2x kick |
| 8 | 256 | Pro drums (4-lane drums with cymbals) |
| 9 | 512 | 5-lane drums |

## Example

```json
{"format":1,"generated":"2026-11-12T15:30:00Z","hash_layout":"scan-chart-2025","charts":[
 {"id":"9218d86a93f34f0da7bfe72a33539728","m":"2025-06-01 10:00:00","s":"ok",
  "h":{"guitar":{"x":"l_zt2Xyf4HKaBaSXYtTO4OytHpGUwNO4WtOjTCmgmYk"}},
  "w":{"guitar":{"t":7,"r":6,"d":23.904,"mv":7.976,"sp":0.181}},
  "f":14,"an":8.24,"mn":14.0,"i":["smallLeadingSilence"]},
 {"id":"…","m":"…","s":"unavailable","r":"hosted on mega.nz"}
]}
```

## Reading it

Download the `rv-index.json.zst` asset of the `rv-index` release, decompress with any zstd
library, parse as JSON and join on RhythmVerse `file_id`. Compare `m` with the chart's current
`update_date`: a mismatch means the chart changed since it was scanned.
