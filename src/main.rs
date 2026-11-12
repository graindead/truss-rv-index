//! truss-scan: scans RhythmVerse's Clone Hero charts and keeps a compact index of their
//! leaderboard hashes, Fretwork ratings and features for Truss.

use truss_rv_index::scan::{self, ScanOpts, backfill, export, scan_one, update};
use truss_rv_index::{download, index, rhythmverse, store};
use clap::{Parser, Subcommand};
use rhythmverse::RvEntry;
use std::path::PathBuf;
use std::time::Duration;
use store::Store;

#[derive(Parser)]
#[command(name = "truss-scan", version, about = "Scans RhythmVerse's Clone Hero charts into a compact index for Truss")]
struct Cli {
    /// The scanner's working state (SQLite).
    #[arg(long, global = true, default_value = "state/rv-state.sqlite")]
    state: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan the whole list, page by page (newest-changed first: RhythmVerse's oldest-first
    /// order isn't reliable). Resumable: re-run to continue from the saved page.
    Backfill {
        #[command(flatten)]
        opts: ScanOpts,
        /// Read RhythmVerse's song list again when the saved one is older than this (0 = always).
        #[arg(long, default_value_t = 24)]
        list_max_age_hours: i64,
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
    /// Use Truss's RhythmVerse cache as the song list (it's only read, never changed).
    ImportTruss {
        /// Truss's rhythmverse.sqlite (default: %LOCALAPPDATA%\truss\cache\rhythmverse.sqlite).
        #[arg(long)]
        from: Option<PathBuf>,
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
    std::sync::LazyLock::force(&scan::STARTED);
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
                Cmd::Backfill { opts, list_max_age_hours } => backfill(&store, &opts, list_max_age_hours).await.and_then(|_| export(&store, &opts.out)),
                Cmd::Update { opts, full, full_every_days } => {
                    update(&store, &opts, full, full_every_days).await.and_then(|_| export(&store, &opts.out))
                }
                Cmd::Stats { hours, catalogue } => {
                    print_stats(&store, hours, catalogue);
                    Ok(())
                }
                Cmd::Export { out } => export(&store, &out),
                Cmd::ImportTruss { from } => match from.or_else(scan::truss_cache) {
                    Some(path) => scan::import_truss_list(&store, &path).map(|_| ()),
                    None => Err("Truss's RhythmVerse cache wasn't found; pass --from".into()),
                },
                Cmd::Probe { .. } | Cmd::Show { .. } => unreachable!(),
            }
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
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
