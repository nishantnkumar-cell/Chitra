//! The batch row of the Convert and Enhance windows: the progress bar with
//! **Pause** and **Stop** beside it, and after a stop, **Continue** and
//! **Discard**. Pausing and stopping let the files being converted finish, so
//! nothing is half written; Continue carries on with exactly the files that
//! were not started.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chitra::convert::{self, Control, ConvertOptions, Job, Report, State};
use eframe::egui;

struct Running {
    /// Files in the whole batch, including those done before a Continue.
    total: usize,
    /// Done before this run (a batch that was stopped and continued).
    before: usize,
    done: Arc<AtomicUsize>,
    control: Arc<Control>,
    report: Arc<Mutex<Option<Report>>>,
    opts: ConvertOptions,
    earlier: Report,
}

struct Stopped {
    opts: ConvertOptions,
    /// What was done, with the files not started in `remaining`.
    earlier: Report,
}

/// A batch of conversions: idle, running (with Pause and Stop), or stopped with
/// the rest kept for Continue.
#[derive(Default)]
pub struct Batch {
    running: Option<Running>,
    stopped: Option<Stopped>,
    /// A batch the person chose to end after a stop; handed out by `poll`.
    ready: Option<Report>,
}

impl Batch {
    /// A batch is running (the window's settings are locked).
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    pub fn start(&mut self, jobs: Vec<Job>, opts: ConvertOptions) {
        self.stopped = None;
        self.launch(jobs, opts, Report::default());
    }

    fn launch(&mut self, jobs: Vec<Job>, opts: ConvertOptions, earlier: Report) {
        let before = earlier.results.len();
        let total = before + jobs.len();
        let done = Arc::new(AtomicUsize::new(0));
        let control = Arc::new(Control::new());
        let report = Arc::new(Mutex::new(None));
        let (d, c, r, o) = (done.clone(), control.clone(), report.clone(), opts.clone());
        std::thread::spawn(move || {
            let rep = convert::run_with(jobs, &o, &c, |_| {
                d.fetch_add(1, Ordering::Relaxed);
            });
            *r.lock().unwrap_or_else(|e| e.into_inner()) = Some(rep);
        });
        self.running = Some(Running {
            total,
            before,
            done,
            control,
            report,
            opts,
            earlier,
        });
    }

    /// The finished report, once the whole batch is over (not when it was only
    /// stopped: then it waits for Continue or Discard).
    pub fn poll(&mut self) -> Option<Report> {
        if let Some(r) = self.ready.take() {
            return Some(r);
        }
        let finished = self
            .running
            .as_ref()?
            .report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()?;
        let run = self.running.take()?;
        let merged = run.earlier.merged(finished);
        if merged.remaining.is_empty() {
            Some(merged)
        } else {
            self.stopped = Some(Stopped {
                opts: run.opts,
                earlier: merged,
            });
            None
        }
    }

    /// Draws the progress row (or the stopped row). True when it drew one, so
    /// the window should not offer to start another batch.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        if let Some(run) = &self.running {
            let done = run.before + run.done.load(Ordering::Relaxed);
            let state = run.control.state();
            let active = run.control.active();
            let text = match state {
                State::Running => format!("{done} / {}", run.total),
                State::Paused if active > 0 => {
                    format!("{done} / {}: pausing, finishing {active}", run.total)
                }
                State::Paused => format!("{done} / {}: paused", run.total),
                State::Stopped => format!("{done} / {}: stopping", run.total),
            };
            let paused = state == State::Paused;
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 150.0).max(120.0);
                ui.add(
                    egui::ProgressBar::new(done as f32 / run.total.max(1) as f32)
                        .desired_width(width)
                        .text(text)
                        .animate(state == State::Running),
                );
                if state != State::Stopped {
                    let label = if paused { "Resume" } else { "Pause" };
                    if ui
                        .button(label)
                        .on_hover_text(if paused {
                            "Carry on with the next file"
                        } else {
                            "Finishes the files being converted, then waits"
                        })
                        .clicked()
                    {
                        if paused {
                            run.control.resume();
                        } else {
                            run.control.pause();
                        }
                    }
                    if ui
                        .button("Stop")
                        .on_hover_text(
                            "Finishes the files being converted, then stops. \
                             You can continue from there.",
                        )
                        .clicked()
                    {
                        run.control.stop();
                    }
                }
            });
            return true;
        }
        let Some(stopped) = &self.stopped else {
            return false;
        };
        let done = stopped.earlier.results.len();
        let left = stopped.earlier.remaining.len();
        ui.label(format!(
            "Stopped: {done} of {} done, {left} left.",
            done + left
        ));
        let (mut go, mut discard) = (false, false);
        ui.horizontal(|ui| {
            go = ui
                .button("Continue")
                .on_hover_text("Carries on with the next file, where it stopped")
                .clicked();
            discard = ui
                .button("Discard the rest")
                .on_hover_text("Leaves the remaining files unconverted")
                .clicked();
        });
        if go && let Some(st) = self.stopped.take() {
            let mut earlier = st.earlier;
            let jobs = std::mem::take(&mut earlier.remaining);
            self.launch(jobs, st.opts, earlier);
        } else if discard && let Some(st) = self.stopped.take() {
            let mut report = st.earlier;
            report.remaining.clear();
            self.ready = Some(report);
        }
        true
    }
}
