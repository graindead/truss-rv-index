//! The scanning loop shared by `truss-scan` and the `RV Cacher` window: pacing, workers,
//! backfill and update, with progress counters and pause / stop controls.

use crate::rhythmverse::{self, RvEntry};
use crate::store::{self, Status, Store};
use crate::{api, index, record};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Where progress messages go: stdout by default, or a window's log.
static SINK: OnceLock<Box<dyn Fn(String) + Send + Sync>> = OnceLock::new();

/// Sends progress messages somewhere other than stdout (call once, before scanning).
pub fn set_sink(f: impl Fn(String) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(f));
}

pub fn say(msg: String) {
    match SINK.get() {
        Some(f) => f(msg),
        None => println!("{msg}"),
    }
}

macro_rules! say {
    ($($t:tt)*) => { say(format!($($t)*)) };
}

/// Pause / stop requests and live progress, read by the window.
#[derive(Default)]
pub struct Progress {
    /// No new charts start while set (charts already downloading finish).
    pub paused: AtomicBool,
    /// Finish the charts in progress, save and stop.
    pub stop: AtomicBool,
    /// Charts in RhythmVerse's Clone Hero list (from the last list page read).
    pub list_total: AtomicU64,
    /// Charts already done (ok or unavailable) when this run started.
    pub done_before: AtomicU64,
    pub ok: AtomicU64,
    pub unavailable: AtomicU64,
    pub failed: AtomicU64,
    pub bytes: AtomicU64,
    /// Charts downloading right now.
    pub active: AtomicU64,
    pub page: AtomicU64,
    /// While reading the song list: the page being read and how many pages there are.
    pub listing_page: AtomicU64,
    pub listing_pages: AtomicU64,
    /// Seconds spent paused, so the speed shown ignores pauses.
    pub paused_ms: AtomicU64,
    pub last: Mutex<Option<String>>,
    /// When the index file was last written.
    pub last_saved: Mutex<Option<Instant>>,
}

impl Progress {
    /// Charts finished for good this run (ok or unavailable).
    pub fn done_now(&self) -> u64 {
        self.ok.load(Relaxed) + self.unavailable.load(Relaxed)
    }
    /// Clears the counters for a new run (done_before / list_total are set by the run).
    pub fn reset(&self) {
        for c in [&self.ok, &self.unavailable, &self.failed, &self.bytes, &self.active, &self.paused_ms] {
            c.store(0, Relaxed);
        }
        self.stop.store(false, Relaxed);
        self.paused.store(false, Relaxed);
        *self.last.lock().unwrap() = None;
    }
    /// Charts left to cache.
    pub fn remaining(&self) -> u64 {
        self.list_total.load(Relaxed).saturating_sub(self.done_before.load(Relaxed) + self.done_now())
    }
}

pub static PROGRESS: std::sync::LazyLock<Progress> = std::sync::LazyLock::new(Progress::default);

#[derive(clap::Args, Clone, Debug)]
pub struct ScanOpts {
    /// Seconds between charts, per worker (each chart's fetches count towards it).
    #[arg(long, default_value_t = 3.0)]
    pub delay: f64,
    /// Charts scanned at once. Starts are spread evenly, so the rate is workers / delay charts
    /// per second.
    #[arg(long, default_value_t = 1)]
    pub workers: usize,
    /// Stop after scanning this many charts (0 = no limit).
    #[arg(long, default_value_t = 0)]
    pub limit: usize,
    /// Charts per RhythmVerse list page (RhythmVerse serves at most ~249 pages, so keep the
    /// whole list within that).
    #[arg(long, default_value_t = 500)]
    pub page_size: u32,
    /// Where to write the index when done.
    #[arg(long, default_value = "out/rv-index.json.zst")]
    pub out: PathBuf,
    /// Give up on one chart after this many seconds.
    #[arg(long, default_value_t = 600)]
    pub chart_timeout: u64,
    /// Write the index every this many seconds while scanning (each chart is saved to the
    /// state database as it finishes either way).
    #[arg(long, default_value_t = 600)]
    pub save_every: u64,
    /// Stop scanning after this many minutes (0 = no limit); the index is still written.
    #[arg(long, default_value_t = 0)]
    pub max_minutes: u64,
}

pub static STARTED: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// The result of scanning one chart.
pub struct Outcome {
    pub status: Status,
    pub reason: Option<String>,
    pub data: Option<record::ChartData>,
    pub bytes: u64,
    pub ms: u64,
}

/// Messages that mean this version of the chart can't be read, now or later.
pub fn is_permanent(msg: &str) -> bool {
    [
        "no longer available",
        "isn't available",
        "which Truss can't read",
        "has no download",
        "No Clone Hero chart was found",
        "not public",
        "has no notes.chart",
        "Couldn't read the Google Drive link",
        "Couldn't find this chart's file",
        "doesn't know how to open",
        "isn't a valid",
        "Damaged",
        // A .7z / .rar that 7-Zip also can't unpack.
        "Couldn't unpack the .",
        "The host refused",
        "Couldn't find the MediaFire download link",
    ]
    .iter()
    .any(|p| msg.contains(p))
}

pub async fn scan_one(e: &RvEntry, timeout: Duration) -> Outcome {
    let started = Instant::now();
    let ((status, reason, data), bytes) = api::counted(scan_inner(e, timeout)).await;
    Outcome {
        status,
        reason: reason.map(|r: String| r.replace("Truss", "the scanner")),
        data,
        bytes,
        ms: started.elapsed().as_millis() as u64,
    }
}

async fn scan_inner(e: &RvEntry, timeout: Duration) -> (Status, Option<String>, Option<record::ChartData>) {
    if !rhythmverse::can_download(&e.rv) {
        let why = match e.rv.external_url.as_deref() {
            Some(u) => format!("hosted on {}", rhythmverse::host_name(u)),
            None => "no download".into(),
        };
        return (Status::Unavailable, Some(why), None);
    }
    let files = match tokio::time::timeout(timeout, rhythmverse::fetch_note_files(&e.rv)).await {
        Err(_) => return (Status::Failed, Some("timed out".into()), None),
        Ok(Err(msg)) => {
            let status = if is_permanent(&msg) { Status::Unavailable } else { Status::Failed };
            return (status, Some(msg), None);
        }
        Ok(Ok(f)) => f,
    };
    match tokio::task::spawn_blocking(move || record::compute(&files)).await {
        Ok(Ok(d)) => (Status::Ok, None, Some(d)),
        // The chart file itself is broken: permanent for this version.
        Ok(Err(msg)) => (Status::Unavailable, Some(format!("bad chart: {msg}")), None),
        Err(_) => (Status::Unavailable, Some("bad chart: the parser crashed".into()), None),
    }
}

/// Spaces chart starts evenly: with `workers` scans at once and `delay` seconds per worker, a
/// chart starts every delay / workers seconds. A host that says to slow down pauses everything.
struct Pacer {
    gap: Duration,
    next: tokio::sync::Mutex<Instant>,
}

impl Pacer {
    fn new(opts: &ScanOpts) -> Pacer {
        Pacer { gap: Duration::from_secs_f64(opts.delay / opts.workers.max(1) as f64), next: tokio::sync::Mutex::new(Instant::now()) }
    }
    /// Waits for this chart's start slot.
    async fn wait(&self) {
        let at = {
            let mut next = self.next.lock().await;
            let at = (*next).max(Instant::now());
            *next = at + self.gap;
            at
        };
        tokio::time::sleep_until(at.into()).await;
    }
    async fn after(&self, o: &Outcome) {
        let r = o.reason.as_deref().unwrap_or("");
        let pause = if r.contains("limiting downloads") || r.contains("429") {
            120
        } else if r.contains("download limit") || r.contains("returned 503") {
            // Google Drive pushing back: ease off for everyone so it doesn't spread.
            30
        } else {
            0
        };
        if pause > 0 {
            say!("  a host is pushing back: pausing {pause} s");
            let mut next = self.next.lock().await;
            *next = (*next).max(Instant::now() + Duration::from_secs(pause));
        }
    }
}

fn log_outcome(n: usize, e: &RvEntry, o: &Outcome) {
    let name = format!("{} - {}", e.artist.as_deref().unwrap_or("?"), e.title.as_deref().unwrap_or("?"));
    let what = match o.status {
        Status::Ok => {
            let d = o.data.as_ref().unwrap();
            format!("ok, {} tracks", d.h.values().map(|m| m.len()).sum::<usize>())
        }
        s => format!("{}: {}", s.as_str(), o.reason.as_deref().unwrap_or("")),
    };
    say!("[{n}] {} {name}: {what} ({:.0} KB, {:.1} s)", e.file_id, o.bytes as f64 / 1024.0, o.ms as f64 / 1000.0);
}

/// One run's progress: charts scanned so far, and the shared pacing.
struct Run {
    id: i64,
    n: usize,
    pacer: Pacer,
    last_save: Instant,
}

impl Run {
    fn new(opts: &ScanOpts) -> Run {
        Run { id: store::now(), n: 0, pacer: Pacer::new(opts), last_save: Instant::now() }
    }
    fn in_time(opts: &ScanOpts) -> bool {
        opts.max_minutes == 0 || STARTED.elapsed() < Duration::from_secs(opts.max_minutes * 60)
    }
    /// How many more charts the limits allow.
    fn room(&self, opts: &ScanOpts) -> usize {
        if !Run::in_time(opts) {
            0
        } else if opts.limit == 0 {
            usize::MAX
        } else {
            opts.limit.saturating_sub(self.n)
        }
    }

    /// Scans `entries`, `opts.workers` at a time, saving each as it finishes. Returns false when
    /// the run's limit or time budget stopped it.
    async fn scan_all(&mut self, store: &Store, entries: &[RvEntry], opts: &ScanOpts) -> bool {
        use futures::StreamExt;
        let take = entries.len().min(self.room(opts));
        let timeout = Duration::from_secs(opts.chart_timeout);
        let pacer = &self.pacer;
        let mut results = futures::stream::iter(&entries[..take])
            .map(|e| async move {
                let p = &*PROGRESS;
                loop {
                    pacer.wait().await;
                    if !p.paused.load(Relaxed) || p.stop.load(Relaxed) {
                        break;
                    }
                    // Paused: wait, then take a fresh start slot.
                    while p.paused.load(Relaxed) && !p.stop.load(Relaxed) {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        p.paused_ms.fetch_add(250, Relaxed);
                    }
                }
                if !Run::in_time(opts) || p.stop.load(Relaxed) {
                    return (e, None);
                }
                p.active.fetch_add(1, Relaxed);
                let o = scan_one(e, timeout).await;
                p.active.fetch_sub(1, Relaxed);
                (e, Some(o))
            })
            .buffer_unordered(opts.workers.max(1));
        let mut stopped = take < entries.len();
        loop {
            // A stop request cancels the songs still downloading (their temporary files are
            // removed; they are scanned again next time).
            let next = tokio::select! {
                r = results.next() => r,
                _ = async {
                    while !PROGRESS.stop.load(Relaxed) {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                } => {
                    stopped = true;
                    break;
                }
            };
            let Some((e, o)) = next else { break };
            let Some(o) = o else {
                stopped = true;
                continue;
            };
            self.n += 1;
            let p = &*PROGRESS;
            match o.status {
                Status::Ok => p.ok.fetch_add(1, Relaxed),
                Status::Unavailable => p.unavailable.fetch_add(1, Relaxed),
                Status::Failed => p.failed.fetch_add(1, Relaxed),
            };
            p.bytes.fetch_add(o.bytes, Relaxed);
            *p.last.lock().unwrap() = Some(format!("{} - {}", e.artist.as_deref().unwrap_or("?"), e.title.as_deref().unwrap_or("?")));
            log_outcome(self.n, e, &o);
            store.save(e, o.status, o.reason.as_deref(), o.data.as_ref(), o.bytes, o.ms, self.id);
            pacer.after(&o).await;
            if self.last_save.elapsed() >= Duration::from_secs(opts.save_every.max(10)) {
                self.last_save = Instant::now();
                if let Err(err) = export(store, &opts.out) {
                    say!("couldn't save the index: {err}");
                }
            }
        }
        drop(results);
        // Cancelled scans never got to count themselves out.
        PROGRESS.active.store(0, Relaxed);
        if !Run::in_time(opts) {
            say!("time budget (--max-minutes) used up");
        }
        !stopped && self.room(opts) > 0
    }
}

pub async fn list_page(page: u32, size: u32, newest_first: bool) -> Result<(Vec<RvEntry>, usize, u64), String> {
    let mut wait = 10;
    for attempt in 0..5 {
        match rhythmverse::fetch_list_page(page, size, newest_first).await {
            Ok(r) => return Ok(r),
            Err(e) if attempt < 4 => {
                say!("list page {page}: {e}; retrying in {wait} s");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                wait *= 3;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Reads RhythmVerse's whole song list (with file sizes) into the state, unless it was read in
/// the last `max_age_hours` (0 = always). Returns false if it was stopped part-way.
pub async fn refresh_list(store: &Store, page_size: u32, max_age_hours: i64) -> Result<bool, String> {
    let now = store::now();
    let last: i64 = store.get_meta("list_read_at").and_then(|v| v.parse().ok()).unwrap_or(0);
    if store.queue_counts().0 > 0 && max_age_hours > 0 && now - last < max_age_hours * 3600 {
        return Ok(true);
    }
    // Truss keeps the same list; use it when it synced more recently than this one.
    if let Some(path) = truss_cache()
        && let Ok(synced) = truss_synced(&path)
        && synced > last
        && (max_age_hours == 0 || now - synced < max_age_hours * 3600)
    {
        match import_truss_list(store, &path) {
            Ok(_) => return Ok(true),
            Err(e) => say!("couldn't use Truss's song list ({e}); reading RhythmVerse's instead"),
        }
    }
    say!("reading RhythmVerse's song list…");
    let mut page = 1u32;
    loop {
        if PROGRESS.stop.load(Relaxed) {
            PROGRESS.listing_page.store(0, Relaxed);
            return Ok(false);
        }
        PROGRESS.listing_page.store(page as u64, Relaxed);
        let (entries, raw, total) = list_page(page, page_size, true).await?;
        if total > 0 {
            PROGRESS.listing_pages.store(total.div_ceil(page_size as u64), Relaxed);
        }
        if raw == 0 {
            break;
        }
        for e in &entries {
            store.mark_seen(&e.file_id, now);
        }
        store.queue_put(&entries, now);
        page += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    PROGRESS.listing_page.store(0, Relaxed);
    if page <= 2 {
        return Err("RhythmVerse sent an empty song list".into());
    }
    let dropped = store.queue_drop_unseen(now);
    store.set_meta("list_read_at", &now.to_string());
    let (total, done) = store.queue_counts();
    say!("song list read: {total} songs ({done} already cached){}", if dropped > 0 { format!(", {dropped} no longer listed") } else { String::new() });
    Ok(true)
}

/// Truss's RhythmVerse cache, if Truss is installed (%LOCALAPPDATA%\truss\cache).
pub fn truss_cache() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    let p = PathBuf::from(base).join("truss").join("cache").join("rhythmverse.sqlite");
    p.exists().then_some(p)
}

/// When Truss last synced its RhythmVerse cache (unix seconds).
pub fn truss_synced(path: &std::path::Path) -> Result<i64, String> {
    let con = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let v: String = con.query_row("select value from meta where key = 'last_sync'", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    v.parse().map_err(|_| "no sync time".to_string())
}

/// Uses Truss's RhythmVerse cache as the song list (only read, never changed).
pub fn import_truss_list(store: &Store, path: &std::path::Path) -> Result<usize, String> {
    let now = store::now();
    let (n, synced) = store.import_truss(path, now)?;
    if n < 1000 {
        return Err(format!("Truss's cache only has {n} songs"));
    }
    let dropped = store.queue_drop_unseen(now);
    // The list is as fresh as Truss's last sync.
    store.set_meta("list_read_at", &synced.min(now).to_string());
    let (total, done) = store.queue_counts();
    say!(
        "song list taken from Truss's cache: {total} songs ({done} already cached){}; songs without a known file size are ordered by length, unknown ones last",
        if dropped > 0 { format!(", {dropped} no longer listed") } else { String::new() }
    );
    Ok(n)
}

/// Caches every song in the list, smallest first. Resumable: stopping keeps everything scanned
/// so far, and the next run carries on with the smallest songs not done yet. The list is read
/// again when it's over `list_max_age_hours` old.
pub async fn backfill(store: &Store, opts: &ScanOpts, list_max_age_hours: i64) -> Result<(), String> {
    if !refresh_list(store, opts.page_size, list_max_age_hours).await? {
        say!("backfill: stopped while reading the song list");
        return Ok(());
    }
    let mut run = Run::new(opts);
    let counts = |run: &Run| {
        let (total, done) = store.queue_counts();
        PROGRESS.list_total.store(total, Relaxed);
        // The window counts this run's songs on top of what was done before it.
        let this_run = PROGRESS.done_now();
        PROGRESS.done_before.store(done.saturating_sub(this_run), Relaxed);
        let _ = run;
    };
    loop {
        counts(&run);
        if PROGRESS.stop.load(Relaxed) {
            say!("backfill: stopped; it carries on with the smallest songs left next time");
            return Ok(());
        }
        let todo = store.queue_todo(500);
        if todo.is_empty() {
            say!("backfill: every song in the list is cached");
            store.set_meta("backfill_done", &run.id.to_string());
            return Ok(());
        }
        say!(
            "backfill: next {} songs, {:.1}–{:.1} MB each",
            todo.len(),
            todo.first().map(|e| e.size).unwrap_or(0) as f64 / 1_048_576.0,
            todo.last().map(|e| e.size).unwrap_or(0) as f64 / 1_048_576.0
        );
        if !run.scan_all(store, &todo, opts).await {
            counts(&run);
            say!("backfill: stopped after {} songs; it carries on with the smallest songs left next time", run.n);
            return Ok(());
        }
    }
}

pub async fn update(store: &Store, opts: &ScanOpts, full: bool, full_every_days: i64) -> Result<(), String> {
    let mut run = Run::new(opts);

    // 1. New and changed charts, newest first, until a version we already have.
    let mut page = 1;
    loop {
        let (entries, raw, _) = list_page(page, opts.page_size, true).await?;
        if raw == 0 {
            break;
        }
        let known = entries.iter().position(|e| store.known(&e.file_id).is_some_and(|(m, _)| m == e.modified));
        let todo = &entries[..known.unwrap_or(entries.len())];
        if !run.scan_all(store, todo, opts).await {
            say!("update: stopped after {} charts", run.n);
            return Ok(());
        }
        if let Some(i) = known {
            say!("update: caught up at {} ({})", entries[i].file_id, entries[i].modified);
            break;
        }
        page += 1;
    }

    // 2. Earlier failures whose retry time has come.
    let retries = store.due_retries(500);
    if !retries.is_empty() {
        say!("update: retrying {} earlier failures", retries.len());
        if !run.scan_all(store, &retries, opts).await {
            return Ok(());
        }
    }

    // 3. Weekly: the whole list, to drop deleted charts and pick up anything missed.
    let last_full: i64 = store.get_meta("last_full_pass").and_then(|v| v.parse().ok()).unwrap_or(0);
    if full || run.id - last_full >= full_every_days * 86400 {
        say!("update: full pass");
        let mut page = 1;
        let mut missed = Vec::new();
        loop {
            let (entries, raw, _) = list_page(page, opts.page_size.max(500), true).await?;
            if raw == 0 {
                break;
            }
            for e in entries {
                store.mark_seen(&e.file_id, run.id);
                if store.known(&e.file_id).is_none_or(|(m, _)| m != e.modified) {
                    missed.push(e);
                }
            }
            page += 1;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        if page <= 2 {
            return Err("the full pass got an empty list; not dropping anything".into());
        }
        let dropped = store.drop_unseen(run.id);
        say!("update: full pass done: {dropped} deleted charts dropped, {} not yet scanned", missed.len());
        store.set_meta("last_full_pass", &run.id.to_string());
        run.scan_all(store, &missed, opts).await;
    }
    Ok(())
}

pub fn export(store: &Store, out: &std::path::Path) -> Result<(), String> {
    *PROGRESS.last_saved.lock().unwrap() = Some(Instant::now());
    let index = index::build(store);
    let ok = index.charts.iter().filter(|c| c.s == "ok").count();
    let size = index::write(&index, out)?;
    say!("wrote {} ({} charts, {ok} ok, {:.2} MB)", out.display(), index.charts.len(), size as f64 / 1_048_576.0);
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Network: stop a backfill part-way (as closing RV Cacher does), then resume it and check
    /// nothing already saved is downloaded again. `cargo test -- --ignored stop_and_resume`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "network"]
    async fn stop_and_resume() {
        let path = std::env::temp_dir().join(format!("rv-stop-resume-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        // One page of the real list stands in for the whole list.
        let (entries, _, _) = list_page(30, 500, true).await.unwrap();
        store.queue_put(&entries, store::now());
        store.set_meta("list_read_at", &store::now().to_string());
        let opts = ScanOpts {
            delay: 1.0,
            workers: 4,
            limit: 0,
            page_size: 500,
            out: std::env::temp_dir().join("rv-stop-resume.json.zst"),
            chart_timeout: 120,
            save_every: 600,
            max_minutes: 0,
        };
        let stopper = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(20)).await;
            PROGRESS.stop.store(true, Relaxed);
            Instant::now()
        });
        backfill(&store, &opts, 24).await.unwrap();
        let stopped_at = stopper.await.unwrap();
        let first = store.scanned_times();
        println!("first run saved {} songs; stopped {:?} after the request", first.len(), stopped_at.elapsed());
        assert!(!first.is_empty());
        assert!(stopped_at.elapsed() < Duration::from_secs(2));
        // Smallest first: everything scanned is no bigger than anything left.
        let biggest_done = first.iter().filter_map(|(id, _)| entries.iter().find(|e| &e.file_id == id)).map(|e| e.size).max().unwrap();
        // Only songs that were still downloading when it stopped (one per worker) can be
        // smaller than the biggest one done.
        let smaller_left = store.queue_todo(50).iter().filter(|e| e.size < biggest_done).count();
        println!("biggest scanned {biggest_done} bytes; {smaller_left} smaller songs left (cancelled mid-download)");
        assert!(smaller_left <= opts.workers, "{smaller_left}");

        // Reopen: carry on for a while more.
        PROGRESS.reset();
        let stopper = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(15)).await;
            PROGRESS.stop.store(true, Relaxed);
        });
        backfill(&store, &opts, 24).await.unwrap();
        stopper.await.unwrap();
        let after = store.scanned_times();
        let rescanned = first.iter().filter(|(id, t)| after.iter().any(|(a, u)| a == id && u != t)).count();
        println!("second run: {} songs in total, {rescanned} scanned again", after.len());
        assert!(after.len() > first.len());
        assert_eq!(rescanned, 0);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }
}
