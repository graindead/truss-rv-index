//! RV Cacher: a small window around the backfill. Shows how many RhythmVerse songs are left to
//! cache, with pause / resume. Closing it saves; opening it again carries on where it stopped.
//! Its state and index live next to the exe (state\ and out\), shared with `truss-scan`.

#![windows_subsystem = "windows"]

use eframe::egui;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use truss_rv_index::scan::{self, PROGRESS, ScanOpts};
use truss_rv_index::store::Store;

#[derive(PartialEq)]
enum Mode {
    Idle,
    Running,
    Stopping,
    Done,
}

struct App {
    dir: PathBuf,
    mode: Mode,
    workers: usize,
    delay: f64,
    log: Arc<Mutex<VecDeque<String>>>,
    result: Arc<Mutex<Option<Result<(), String>>>>,
    started: Option<Instant>,
    done_before: u64,
    close_after_stop: bool,
}

/// How often the index file is written during a run.
const SAVE_MINUTES: u64 = 10;

fn data_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| PathBuf::from("."))
}

impl App {
    fn new() -> App {
        let dir = data_dir();
        let log = Arc::new(Mutex::new(VecDeque::new()));
        let sink = log.clone();
        scan::set_sink(move |msg| {
            let mut l = sink.lock().unwrap();
            if l.len() >= 300 {
                l.pop_front();
            }
            l.push_back(msg);
        });
        // What's already cached, before starting.
        let done_before = Store::open(&dir.join("state/rv-state.sqlite")).map(|s| s.count_done()).unwrap_or(0);
        let saved_total = Store::open(&dir.join("state/rv-state.sqlite")).ok().and_then(|s| s.get_meta("list_total")).and_then(|v| v.parse().ok()).unwrap_or(0);
        PROGRESS.done_before.store(done_before, Relaxed);
        PROGRESS.list_total.store(saved_total, Relaxed);
        App { dir, mode: Mode::Idle, workers: 4, delay: 3.0, log, result: Arc::new(Mutex::new(None)), started: None, done_before, close_after_stop: false }
    }

    fn start(&mut self) {
        // Also used to start again after a stop: fresh counters for this run.
        PROGRESS.reset();
        *self.result.lock().unwrap() = None;
        self.close_after_stop = false;
        let opts = ScanOpts {
            delay: self.delay,
            workers: self.workers,
            limit: 0,
            page_size: 500,
            out: self.dir.join("out/rv-index.json.zst"),
            chart_timeout: 600,
            save_every: SAVE_MINUTES * 60,
            max_minutes: 0,
        };
        let state = self.dir.join("state/rv-state.sqlite");
        let result = self.result.clone();
        std::sync::LazyLock::force(&scan::STARTED);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let r = rt.block_on(async {
                let store = Store::open(&state)?;
                let r = scan::backfill(&store, &opts, None).await;
                store.set_meta("list_total", &PROGRESS.list_total.load(Relaxed).to_string());
                // Always leave a current index behind, finished or not.
                scan::export(&store, &opts.out)?;
                r
            });
            *result.lock().unwrap() = Some(r);
        });
        self.started = Some(Instant::now());
        self.mode = Mode::Running;
    }
}

fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (d, h, m) = (s / 86400, s % 86400 / 3600, s % 3600 / 60);
    if d > 0 {
        format!("{d} d {h} h")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{m} min")
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(250));
        let p = &*PROGRESS;

        // The scan finished (or stopped).
        if matches!(self.mode, Mode::Running | Mode::Stopping) && self.result.lock().unwrap().is_some() {
            self.mode = Mode::Done;
            if self.close_after_stop {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        // Closing the window while scanning: finish the songs in progress and save first.
        if ctx.input(|i| i.viewport().close_requested()) && matches!(self.mode, Mode::Running | Mode::Stopping) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            p.stop.store(true, Relaxed);
            p.paused.store(false, Relaxed);
            self.mode = Mode::Stopping;
            self.close_after_stop = true;
        }

        egui::CentralPanel::default().show(root, |ui| {
            let total = p.list_total.load(Relaxed);
            let remaining = p.remaining();
            let done = total.saturating_sub(remaining);
            ui.vertical_centered(|ui| {
                ui.add_space(12.0);
                let big = if total == 0 { "…".to_string() } else { fmt_num(remaining) };
                ui.label(egui::RichText::new(big).size(64.0).strong());
                ui.label(egui::RichText::new("songs left to cache").size(18.0));
                ui.add_space(10.0);
                let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(frac).show_percentage().desired_width(ui.available_width() - 40.0));
                if total > 0 {
                    ui.label(format!("{} of {} cached", fmt_num(done), fmt_num(total)));
                } else {
                    ui.label(format!("{} cached so far (the total shows once the scan starts)", fmt_num(self.done_before)));
                }
            });
            ui.add_space(12.0);

            // Session numbers.
            let session = p.ok.load(Relaxed) + p.unavailable.load(Relaxed) + p.failed.load(Relaxed);
            if let Some(start) = self.started {
                let active = start.elapsed().as_secs_f64() - p.paused_ms.load(Relaxed) as f64 / 1000.0;
                let per_sec = if active > 5.0 { session as f64 / active } else { 0.0 };
                egui::Grid::new("stats").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                    ui.label("This session");
                    ui.label(format!(
                        "{} songs: {} ok, {} unavailable, {} failed (retried later)",
                        session,
                        p.ok.load(Relaxed),
                        p.unavailable.load(Relaxed),
                        p.failed.load(Relaxed)
                    ));
                    ui.end_row();
                    ui.label("Speed");
                    ui.label(if per_sec > 0.0 { format!("{:.0} songs / min", per_sec * 60.0) } else { "…".into() });
                    ui.end_row();
                    ui.label("Time left");
                    ui.label(if per_sec > 0.0 && remaining > 0 { format!("about {}", fmt_dur(remaining as f64 / per_sec)) } else { "…".into() });
                    ui.end_row();
                    ui.label("Downloaded");
                    ui.label(format!("{:.0} MB (only the notes are kept; the rest is deleted)", p.bytes.load(Relaxed) as f64 / 1_048_576.0));
                    ui.end_row();
                    ui.label("Last saved");
                    ui.label(match *p.last_saved.lock().unwrap() {
                        Some(t) => format!(
                            "{} ago (every song is saved as it finishes; the index file every {} min)",
                            fmt_dur(t.elapsed().as_secs_f64().max(60.0)),
                            SAVE_MINUTES
                        ),
                        None => format!("every song as it finishes; the index file every {} min", SAVE_MINUTES),
                    });
                    ui.end_row();
                    ui.label("Last song");
                    ui.label(p.last.lock().unwrap().clone().unwrap_or_else(|| "…".into()));
                    ui.end_row();
                });
                ui.add_space(12.0);
            }

            // Controls.
            ui.horizontal(|ui| match self.mode {
                Mode::Idle => {
                    ui.label("Songs at once:");
                    ui.add(egui::Slider::new(&mut self.workers, 1..=4));
                    ui.add_space(12.0);
                    let label = if self.done_before > 0 { "Resume caching" } else { "Start caching" };
                    if ui.add(egui::Button::new(egui::RichText::new(label).size(16.0)).min_size([160.0, 32.0].into())).clicked() {
                        self.start();
                    }
                }
                Mode::Running => {
                    let paused = p.paused.load(Relaxed);
                    let label = if paused { "Resume" } else { "Pause" };
                    if ui.add(egui::Button::new(egui::RichText::new(label).size(16.0)).min_size([120.0, 32.0].into())).clicked() {
                        p.paused.store(!paused, Relaxed);
                    }
                    if ui.add(egui::Button::new("Save && stop").min_size([120.0, 32.0].into())).clicked() {
                        p.stop.store(true, Relaxed);
                        p.paused.store(false, Relaxed);
                        self.mode = Mode::Stopping;
                    }
                    let active = p.active.load(Relaxed);
                    ui.label(if paused {
                        if active > 0 { format!("Pausing: finishing {active} song(s)…") } else { "Paused. You can also close the window; it resumes next time.".into() }
                    } else {
                        format!("Caching, {} at once (page {})", self.workers, p.page.load(Relaxed))
                    });
                }
                Mode::Stopping => {
                    ui.spinner();
                    ui.label(format!("Finishing {} song(s) and saving…", p.active.load(Relaxed)));
                }
                Mode::Done => {
                    let all_done = remaining == 0 && total > 0;
                    if !all_done {
                        ui.label("Songs at once:");
                        ui.add(egui::Slider::new(&mut self.workers, 1..=4));
                        ui.add_space(12.0);
                        if ui.add(egui::Button::new(egui::RichText::new("Resume caching").size(16.0)).min_size([160.0, 32.0].into())).clicked() {
                            self.start();
                            return;
                        }
                    }
                    match self.result.lock().unwrap().as_ref() {
                        Some(Err(e)) => ui.colored_label(egui::Color32::LIGHT_RED, format!("Stopped with an error: {e}")),
                        _ if all_done => ui.label("All songs cached. The index is saved."),
                        _ => ui.label("Saved."),
                    };
                }
            });

            ui.add_space(8.0);
            egui::CollapsingHeader::new("Details").default_open(false).show(ui, |ui| {
                ui.label(format!("Data folder: {}", self.dir.display()));
                egui::ScrollArea::vertical().max_height(220.0).stick_to_bottom(true).show(ui, |ui| {
                    for line in self.log.lock().unwrap().iter() {
                        ui.label(egui::RichText::new(line).monospace().size(11.0));
                    }
                });
            });
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("RV Cacher").with_inner_size([620.0, 520.0]).with_min_inner_size([480.0, 380.0]),
        ..Default::default()
    };
    eframe::run_native("RV Cacher", options, Box::new(|_cc| Ok(Box::new(App::new()))))
}
