//! truss-scan: scans RhythmVerse's Clone Hero charts and keeps a compact index of their
//! leaderboard hashes, Fretwork ratings and features for Truss.

// Copied from Truss (see README for the commit); some of their helpers aren't used here.
#[allow(dead_code)]
mod chart;
#[allow(dead_code)]
mod chart_issues;
#[allow(dead_code)]
mod download;
#[allow(dead_code)]
mod fretwork;
#[allow(dead_code)]
mod rhythmverse;
#[allow(dead_code)]
mod sng;
#[allow(dead_code)]
mod trackhash;
#[allow(dead_code)]
mod zip_ranges;

mod api;
mod index;
mod record;
mod store;
mod zip_stream;

use clap::{Args, Parser, Subcommand};
use rhythmverse::RvEntry;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use store::{Status, Store};

#[derive(Parser)]
#[command(name = "truss-scan", version, about = "Scans RhythmVerse's Clone Hero charts into a compact index for Truss")]
struct Cli {
    /// The scanner's working state (SQLite).
    #[arg(long, global = true, default_value = "state/rv-state.sqlite")]
    state: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct ScanOpts {
    /// Seconds between charts, per worker (each chart's fetches count towards it).
    #[arg(long, default_value_t = 3.0)]
    delay: f64,
    /// Charts scanned at once. Starts are spread evenly, so the rate is workers / delay charts
    /// per second.
    #[arg(long, default_value_t = 1)]
    workers: usize,
    /// Stop after scanning this many charts (0 = no limit).
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Charts per RhythmVerse list page (RhythmVerse serves at most ~249 pages, so keep the
    /// whole list within that).
    #[arg(long, default_value_t = 500)]
    page_size: u32,
    /// Where to write the index when done.
    #[arg(long, default_value = "out/rv-index.json.zst")]
    out: PathBuf,
    /// Give up on one chart after this many seconds.
    #[arg(long, default_value_t = 600)]
    chart_timeout: u64,
    /// Stop scanning after this many minutes (0 = no limit); the index is still written.
    #[arg(long, default_value_t = 0)]
    max_minutes: u64,
}

static STARTED: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

#[derive(Subcommand)]
enum Cmd {
    /// Scan the whole list, page by page (newest-changed first: RhythmVerse's oldest-first
    /// order isn't reliable). Resumable: re-run to continue from the saved page.
    Backfill {
        #[command(flatten)]
        opts: ScanOpts,
        /// Start from this page instead of the saved one (already-scanned charts are skipped).
        #[arg(long)]
        start_page: Option<u32>,
    },
    /// Scan new and changed charts (newest first, until reaching a known one), retry earlier
    /// failures, and once a week (or with --full) walk the whole list to drop deleted charts.
    Update {
        #[command(flatten)]
        opts: ScanOpts,
        /// Do the full pass now.
        #[arg(long)]
        full: bool,
        /// Days between full passes.
        #[arg(long, default_value_t = 7)]
        full_every_days: i64,
    },
    /// Scan the charts matching a title (without touching the state) and print their hashes.
    Probe {
        title: String,
        /// Only charts by this charter (case-insensitive substring).
        #[arg(long)]
        charter: Option<String>,
        /// Also save each chart's note files under this folder (to compare with other tools).
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Print scan statistics (success rate, bytes and time per chart, projection).
    Stats {
        /// Only charts scanned in the last N hours.
        #[arg(long)]
        hours: Option<f64>,
        /// Charts in the full list, for the projection.
        #[arg(long, default_value_t = 88_000)]
        catalogue: u64,
    },
    /// Print one chart's entry from a written index file.
    Show {
        /// RhythmVerse file_id.
        id: String,
        #[arg(long, default_value = "out/rv-index.json.zst")]
        index: PathBuf,
    },
    /// Write the index from the state.
    Export {
        #[arg(long, default_value = "out/rv-index.json.zst")]
        out: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    std::sync::LazyLock::force(&STARTED);
    let result = match cli.cmd {
        Cmd::Probe { title, charter, save } => probe(&title, charter.as_deref(), save.as_deref()).await,
        Cmd::Show { id, index } => show(&id, &index),
        cmd => {
            let store = match Store::open(&cli.state) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Can't open {}: {e}", cli.state.display());
                    std::process::exit(1);
                }
            };
            match cmd {
                Cmd::Backfill { opts, start_page } => backfill(&store, &opts, start_page).await.and_then(|_| export(&store, &opts.out)),
                Cmd::Update { opts, full, full_every_days } => {
                    update(&store, &opts, full, full_every_days).await.and_then(|_| export(&store, &opts.out))
                }
                Cmd::Stats { hours, catalogue } => {
                    print_stats(&store, hours, catalogue);
                    Ok(())
                }
                Cmd::Export { out } => export(&store, &out),
                Cmd::Probe { .. } | Cmd::Show { .. } => unreachable!(),
            }
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// The result of scanning one chart.
struct Outcome {
    status: Status,
    reason: Option<String>,
    data: Option<record::ChartData>,
    bytes: u64,
    ms: u64,
}

/// Messages that mean this version of the chart can't be read, now or later.
fn is_permanent(msg: &str) -> bool {
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
        "The host refused",
        "Couldn't find the MediaFire download link",
    ]
    .iter()
    .any(|p| msg.contains(p))
}

async fn scan_one(e: &RvEntry, timeout: Duration) -> Outcome {
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
        if o.reason.as_deref().is_some_and(|r| r.contains("limiting downloads") || r.contains("429")) {
            eprintln!("  a host is rate-limiting: pausing 2 minutes");
            let mut next = self.next.lock().await;
            *next = (*next).max(Instant::now() + Duration::from_secs(120));
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
    println!("[{n}] {} {name}: {what} ({:.0} KB, {:.1} s)", e.file_id, o.bytes as f64 / 1024.0, o.ms as f64 / 1000.0);
}

/// One run's progress: charts scanned so far, and the shared pacing.
struct Run {
    id: i64,
    n: usize,
    pacer: Pacer,
}

impl Run {
    fn new(opts: &ScanOpts) -> Run {
        Run { id: store::now(), n: 0, pacer: Pacer::new(opts) }
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
                pacer.wait().await;
                if !Run::in_time(opts) {
                    return (e, None);
                }
                (e, Some(scan_one(e, timeout).await))
            })
            .buffer_unordered(opts.workers.max(1));
        let mut stopped = take < entries.len();
        while let Some((e, o)) = results.next().await {
            let Some(o) = o else {
                stopped = true;
                continue;
            };
            self.n += 1;
            log_outcome(self.n, e, &o);
            store.save(e, o.status, o.reason.as_deref(), o.data.as_ref(), o.bytes, o.ms, self.id);
            pacer.after(&o).await;
        }
        if !Run::in_time(opts) {
            println!("time budget (--max-minutes) used up");
        }
        !stopped && self.room(opts) > 0
    }
}

async fn list_page(page: u32, size: u32, newest_first: bool) -> Result<(Vec<RvEntry>, usize, u64), String> {
    let mut wait = 10;
    for attempt in 0..5 {
        match rhythmverse::fetch_list_page(page, size, newest_first).await {
            Ok(r) => return Ok(r),
            Err(e) if attempt < 4 => {
                eprintln!("list page {page}: {e}; retrying in {wait} s");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                wait *= 3;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

async fn backfill(store: &Store, opts: &ScanOpts, start_page: Option<u32>) -> Result<(), String> {
    let mut run = Run::new(opts);
    let mut page: u32 = start_page.unwrap_or_else(|| store.get_meta("backfill_page").and_then(|p| p.parse().ok()).unwrap_or(1));
    loop {
        let (entries, raw, total) = list_page(page, opts.page_size, true).await?;
        if raw == 0 {
            println!("backfill: reached the end of the list ({total} charts)");
            store.set_meta("backfill_done", &run.id.to_string());
            return Ok(());
        }
        println!("backfill: page {page} ({} Clone Hero charts of {raw}; {total} in the list)", entries.len());
        let mut todo = Vec::new();
        for e in entries {
            store.mark_seen(&e.file_id, run.id);
            if !store.is_done(&e) {
                todo.push(e);
            }
        }
        if !run.scan_all(store, &todo, opts).await {
            println!("backfill: stopped after {} charts; re-run to continue from page {page}", run.n);
            store.set_meta("backfill_page", &page.to_string());
            return Ok(());
        }
        page += 1;
        store.set_meta("backfill_page", &page.to_string());
    }
}

async fn update(store: &Store, opts: &ScanOpts, full: bool, full_every_days: i64) -> Result<(), String> {
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
            println!("update: stopped after {} charts", run.n);
            return Ok(());
        }
        if let Some(i) = known {
            println!("update: caught up at {} ({})", entries[i].file_id, entries[i].modified);
            break;
        }
        page += 1;
    }

    // 2. Earlier failures whose retry time has come.
    let retries = store.due_retries(500);
    if !retries.is_empty() {
        println!("update: retrying {} earlier failures", retries.len());
        if !run.scan_all(store, &retries, opts).await {
            return Ok(());
        }
    }

    // 3. Weekly: the whole list, to drop deleted charts and pick up anything missed.
    let last_full: i64 = store.get_meta("last_full_pass").and_then(|v| v.parse().ok()).unwrap_or(0);
    if full || run.id - last_full >= full_every_days * 86400 {
        println!("update: full pass");
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
        println!("update: full pass done: {dropped} deleted charts dropped, {} not yet scanned", missed.len());
        store.set_meta("last_full_pass", &run.id.to_string());
        run.scan_all(store, &missed, opts).await;
    }
    Ok(())
}

fn export(store: &Store, out: &std::path::Path) -> Result<(), String> {
    let index = index::build(store);
    let ok = index.charts.iter().filter(|c| c.s == "ok").count();
    let size = index::write(&index, out)?;
    println!("wrote {} ({} charts, {ok} ok, {:.2} MB)", out.display(), index.charts.len(), size as f64 / 1_048_576.0);
    Ok(())
}

fn show(id: &str, path: &std::path::Path) -> Result<(), String> {
    let index = index::read(path)?;
    let c = index.charts.iter().find(|c| c.id == id).ok_or_else(|| format!("{id} isn't in {}", path.display()))?;
    println!("{}", serde_json::to_string_pretty(c).unwrap_or_default());
    Ok(())
}

async fn probe(title: &str, charter: Option<&str>, save: Option<&std::path::Path>) -> Result<(), String> {
    let found = rhythmverse::search_title(title).await?;
    let want = |e: &RvEntry| {
        charter.is_none_or(|c| e.charter.as_deref().is_some_and(|x| x.to_lowercase().contains(&c.to_lowercase())))
            && e.title.as_deref().is_some_and(|t| t.to_lowercase().contains(&title.to_lowercase()))
    };
    let picked: Vec<&RvEntry> = found.iter().filter(|e| want(e)).collect();
    if picked.is_empty() {
        return Err(format!("no RhythmVerse chart matches \"{title}\""));
    }
    for e in picked {
        if let Some(dir) = save
            && let Ok(files) = rhythmverse::fetch_note_files(&e.rv).await
        {
            let dir = dir.join(&e.file_id);
            std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
            for (name, data) in files {
                std::fs::write(dir.join(download::sanitize_filename(&name)), data).map_err(|err| err.to_string())?;
            }
        }
        let o = scan_one(e, Duration::from_secs(600)).await;
        println!(
            "{} | {} - {} by {} | {} | {:.0} KB, {:.1} s",
            e.file_id,
            e.artist.as_deref().unwrap_or("?"),
            e.title.as_deref().unwrap_or("?"),
            e.charter.as_deref().unwrap_or("?"),
            o.status.as_str(),
            o.bytes as f64 / 1024.0,
            o.ms as f64 / 1000.0
        );
        match &o.data {
            Some(d) => println!("{}", serde_json::to_string_pretty(d).unwrap_or_default()),
            None => println!("  {}", o.reason.unwrap_or_default()),
        }
    }
    Ok(())
}

fn print_stats(store: &Store, hours: Option<f64>, catalogue: u64) {
    let since = hours.map(|h| store::now() - (h * 3600.0) as i64).unwrap_or(0);
    let s = store.stats(since);
    if s.total == 0 {
        println!("no charts scanned");
        return;
    }
    let pct = |x: u64| 100.0 * x as f64 / s.total as f64;
    println!("charts scanned: {}", s.total);
    println!("  ok:          {} ({:.1}%)", s.ok, pct(s.ok));
    println!("  unavailable: {} ({:.1}%)", s.unavailable, pct(s.unavailable));
    println!("  failed:      {} ({:.1}%)", s.failed, pct(s.failed));
    let avg_bytes = s.bytes as f64 / s.total as f64;
    let avg_ms = s.ms as f64 / s.total as f64;
    println!("bytes fetched: {:.1} MB total, {:.0} KB per chart on average, {:.1} MB the most", s.bytes as f64 / 1_048_576.0, avg_bytes / 1024.0, s.max_bytes as f64 / 1_048_576.0);
    println!("time per chart (fetch + parse, excluding the pacing delay): {:.2} s on average", avg_ms / 1000.0);
    println!(
        "projection for {catalogue} charts: {:.1} GB downloaded; {:.1} days at 3 s/chart pacing (or fetch time when longer: {:.1} days)",
        avg_bytes * catalogue as f64 / 1e9,
        catalogue as f64 * 3.0f64.max(avg_ms / 1000.0) / 86400.0,
        catalogue as f64 * (avg_ms / 1000.0) / 86400.0,
    );
    println!("reasons:");
    let mut reasons: Vec<_> = s.reasons.into_iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1));
    for (r, c) in reasons.into_iter().take(25) {
        println!("  {c:5}  {r}");
    }
}
