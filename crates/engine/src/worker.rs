//! Inference worker thread.
//!
//! All llama.cpp state lives on one dedicated thread. Callers send [`Command`]s through a
//! [`Worker`] handle and receive [`Event`]s through a callback.
//!
//! Jobs supersede each other: submitting a translation cancels the one in flight, and queued
//! jobs that are already stale are skipped. That keeps auto-translate responsive while the
//! user types. Every job ends with one [`Event::Finished`] or [`Event::Failed`], a skipped one
//! with [`StopReason::Cancelled`].

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use pfml::TranslationOptions;

use crate::document::{DocumentOptions, Output, Progress, translate_document, translate_segments};
use crate::llama::{
    Device, GpuOffload, Model, ModelOptions, Runtime, RuntimeOptions, SessionOptions,
};
use crate::{StopReason, TranslateStats};

/// Identifies a translation job. Ids increase monotonically per worker.
pub type JobId = u64;

#[derive(Debug, Clone)]
pub struct LoadRequest {
    pub path: PathBuf,
    pub model: ModelOptions,
    pub session: SessionOptions,
}

/// Compares how fast `devices` translate with the model at `path` (see [`Worker::measure`]).
#[derive(Debug, Clone)]
pub struct MeasureRequest {
    pub path: PathBuf,
    /// ggml device indices, or `None` for the CPU.
    pub devices: Vec<Option<usize>>,
}

/// How fast one device translated the measurement text.
#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    /// ggml device index, or `None` for the CPU.
    pub device: Option<usize>,
    /// The speed, or why the model could not run there.
    pub result: Result<Speed, String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Speed {
    pub prompt_tokens_per_second: f64,
    pub generation_tokens_per_second: f64,
}

impl Speed {
    /// Seconds for a typical piece of a long text: 200 prompt tokens (the piece and some
    /// history) and 100 generated ones.
    pub fn typical_seconds(&self) -> f64 {
        200.0 / self.prompt_tokens_per_second + 100.0 / self.generation_tokens_per_second
    }
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub path: PathBuf,
    pub n_params: u64,
    pub size: u64,
    pub n_ctx: usize,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// llama.cpp could not be initialized, or the worker failed unexpectedly. The worker has
    /// exited.
    RuntimeFailed(String),
    /// The compute devices available, sent once after llama.cpp starts.
    Devices(Vec<Device>),
    LoadProgress(f32),
    Loaded(ModelInfo),
    LoadFailed(String),
    /// The model was released after an unload request; its file is no longer open.
    Unloaded,
    /// A measurement started on device `index` of `count` (see [`Worker::measure`]).
    Measuring {
        index: usize,
        count: usize,
    },
    /// The measurement ended, with one result per device in the order requested. No model is
    /// loaded afterwards.
    Measured(Vec<Measurement>),
    /// A long text was split; chunk `index` of `count` is starting. `source` is its byte range
    /// in the text.
    Chunk {
        job: JobId,
        index: usize,
        count: usize,
        source: (usize, usize),
    },
    /// The translation of segment `index` of a [`Worker::translate_segments`] job.
    Segment {
        job: JobId,
        index: usize,
        text: String,
    },
    /// Generated text for a job, in order.
    Token {
        job: JobId,
        text: String,
    },
    /// Keep only the first `len` bytes of the job's output: output that ran away is being
    /// translated again.
    Rewind {
        job: JobId,
        len: usize,
    },
    /// The output for chunk `index` (or a piece of it) ran away and is translated again in
    /// smaller pieces.
    Retry {
        job: JobId,
        index: usize,
        reason: StopReason,
    },
    /// Output for a piece of chunk `index` (usually a sentence) kept running away and was cut.
    /// `source` is the piece's byte range in the text, and `output` what was kept of its
    /// translation, including the cut marker.
    Cut {
        job: JobId,
        index: usize,
        reason: StopReason,
        source: (usize, usize),
        output: (usize, usize),
    },
    /// The job ended: finished, cancelled (also before it started), or cut short.
    Finished {
        job: JobId,
        stats: TranslateStats,
    },
    Failed {
        job: JobId,
        error: String,
    },
}

enum Command {
    Load(LoadRequest),
    Measure(MeasureRequest),
    Unload,
    Translate {
        job: JobId,
        options: TranslationOptions,
        document: DocumentOptions,
        text: String,
    },
    TranslateSegments {
        job: JobId,
        options: TranslationOptions,
        document: DocumentOptions,
        segments: Vec<String>,
    },
}

type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// Handle to the inference worker. Dropping it stops the current job, a model load or a
/// measurement included, and shuts the worker down.
pub struct Worker {
    commands: Option<mpsc::Sender<Command>>,
    thread: Option<thread::JoinHandle<()>>,
    stopping: Arc<AtomicBool>,
    /// The newest job id. Any job with a different id is cancelled or skipped.
    latest: Arc<AtomicU64>,
    /// Set when the handle is dropped: stops a model load or a measurement in progress, which
    /// job ids do not reach.
    closing: Arc<AtomicBool>,
    /// Waited for on drop (see [`Worker::wait_for_thread`]).
    #[cfg(target_os = "macos")]
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        self.cancel();
        // Replacing the only sender closes the channel: the thread frees the model and exits.
        self.commands = mpsc::channel().0;
        #[cfg(target_os = "macos")]
        self.wait_for_thread();
    }
}

impl Worker {
    pub fn spawn(options: RuntimeOptions, events: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let (commands, rx) = mpsc::channel();
        let latest = Arc::new(AtomicU64::new(0));
        let events: EventSink = Arc::new(events);
        let closing = Arc::new(AtomicBool::new(false));
        let (thread_latest, thread_closing) = (latest.clone(), closing.clone());
        let thread = thread::Builder::new()
            .name("inference".into())
            .spawn(move || {
                let report = events.clone();
                let result = panic::catch_unwind(AssertUnwindSafe(|| {
                    run(options, rx, events, thread_latest, thread_closing)
                }));
                // Without this the UI would wait for the dead thread forever.
                if let Err(payload) = result {
                    let message = payload
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_owned())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_default();
                    report(Event::RuntimeFailed(format!("internal error: {message}")));
                }
            })
            .expect("failed to spawn the inference thread");
        #[cfg(not(target_os = "macos"))]
        drop(thread);
        Self {
            commands,
            latest,
            closing,
            #[cfg(target_os = "macos")]
            thread: Some(thread),
        }
    }

    /// ggml's Metal backend asserts when the process exits that all its buffers were freed, and
    /// aborts otherwise (`ggml_metal_rsets_free`, from a static destructor). The thread frees them
    /// once the command channel closes, so let it finish before the process can exit.
    #[cfg(target_os = "macos")]
    fn wait_for_thread(&mut self) {
        // Enough to finish the current token, or to stop a load or a measurement, and free the
        // model.
        const LIMIT: Duration = Duration::from_secs(10);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let start = std::time::Instant::now();
        while !thread.is_finished() && start.elapsed() < LIMIT {
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Loads a model, replacing the current one. Cancels any running job.
    pub fn load(&self, request: LoadRequest) {
        self.cancel();
        self.send(Command::Load(request));
    }

    pub fn unload(&self) {
        self.cancel();
        self.send(Command::Unload);
    }

    /// Unloads the model and translates a fixed text with it on each of the request's devices
    /// in turn, reporting [`Event::Measuring`] for each and then [`Event::Measured`]. Cancels
    /// any running job; load a model afterwards to translate again.
    pub fn measure(&self, request: MeasureRequest) {
        self.cancel();
        self.send(Command::Measure(request));
    }

    /// Starts a translation, cancelling any earlier one. Returns the new job id.
    pub fn translate(
        &self,
        options: TranslationOptions,
        document: DocumentOptions,
        text: String,
    ) -> JobId {
        let job = self.latest.fetch_add(1, Ordering::SeqCst) + 1;
        self.send(Command::Translate {
            job,
            options,
            document,
            text,
        });
        job
    }

    /// Translates `segments` in order, with the earlier ones as history, reporting each one as
    /// an [`Event::Segment`]. Cancels any earlier job, like [`Worker::translate`].
    pub fn translate_segments(
        &self,
        options: TranslationOptions,
        document: DocumentOptions,
        segments: Vec<String>,
    ) -> JobId {
        let job = self.latest.fetch_add(1, Ordering::SeqCst) + 1;
        self.send(Command::TranslateSegments {
            job,
            options,
            document,
            segments,
        });
        job
    }

    /// Cancels the running job, if any.
    pub fn cancel(&self) {
        self.latest.fetch_add(1, Ordering::SeqCst);
    }

    /// Cancel work and join the inference thread. This must finish before native
    /// process-exit destructors run (not in a time-limited asynchronous quit task).
    /// Event sinks must not wait for the calling thread while shutdown is joining.
    pub fn shutdown(&mut self) {
        self.cancel();
        self.stopping.store(true, Ordering::SeqCst);
        self.commands.take(); // Wake a worker waiting for its next command.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn send(&self, command: Command) {
        // The worker only exits after the runtime failed, which was already reported.
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(
    options: RuntimeOptions,
    commands: mpsc::Receiver<Command>,
    events: EventSink,
    latest: Arc<AtomicU64>,
    closing: Arc<AtomicBool>,
) {
    let runtime = match Runtime::init(options) {
        Ok(r) => r,
        Err(e) => {
            events(Event::RuntimeFailed(e.to_string()));
            return;
        }
    };
    events(Event::Devices(runtime.devices()));

    let mut next = None;
    loop {
        let Some(command) = next.take().or_else(|| commands.recv().ok()) else {
            return;
        };
        if stopping.load(Ordering::SeqCst) {
            return;
        }
        match command {
            Command::Load(request) => {
                if let Some(model) = load(&runtime, &request, &events, &closing) {
                    // Serve requests until the next load or unload.
                    next = serve(&runtime, &model, &request, &commands, &events, &latest);
                    drop(model);
                    if next.is_none() {
                        events(Event::Unloaded);
                    }
                }
            }
            Command::Measure(request) => events(Event::Measured(measure(
                &runtime, &request, &events, &closing,
            ))),
            Command::Unload => events(Event::Unloaded),
            Command::Translate { job, .. } | Command::TranslateSegments { job, .. } => {
                events(Event::Failed {
                    job,
                    error: "no model is loaded".into(),
                })
            }
        }
    }
}

fn load(
    runtime: &Runtime,
    request: &LoadRequest,
    events: &EventSink,
    closing: &Arc<AtomicBool>,
) -> Option<Model> {
    let progress_events = events.clone();
    let closing = closing.clone();
    let result = runtime.load_model(&request.path, &request.model, move |p| {
        progress_events(Event::LoadProgress(p));
        // Returning false makes llama.cpp stop loading and free what it allocated.
        !closing.load(Ordering::SeqCst)
    });
    match result {
        Ok(model) => Some(model),
        Err(e) => {
            events(Event::LoadFailed(e.to_string()));
            None
        }
    }
}

/// Runs translation jobs on `model`. Returns the command that ended the session
/// (a new load), or `None` when the session was unloaded or the channel closed.
fn serve(
    runtime: &Runtime,
    model: &Model,
    request: &LoadRequest,
    commands: &mpsc::Receiver<Command>,
    events: &EventSink,
    latest: &AtomicU64,
) -> Option<Command> {
    let mut session = match model.session(runtime, &request.session) {
        Ok(s) => s,
        Err(e) => {
            events(Event::LoadFailed(e.to_string()));
            return None;
        }
    };
    events(Event::Loaded(ModelInfo {
        path: request.path.clone(),
        n_params: model.n_params(),
        size: model.size(),
        n_ctx: session.n_ctx(),
    }));

    loop {
        let command = commands.recv().ok()?;
        let job = match &command {
            Command::Load(_) | Command::Measure(_) => return Some(command),
            Command::Unload => return None,
            Command::Translate { job, .. } | Command::TranslateSegments { job, .. } => *job,
        };
        if latest.load(Ordering::SeqCst) != job {
            // Cancelled or superseded before it started. Reported all the same, so that a
            // caller waiting for this job (the browser host) learns that it ended.
            events(Event::Finished {
                job,
                stats: TranslateStats::cancelled(),
            });
            continue;
        }
        let n_ctx = session.n_ctx();
        let count_tokens = |s: &str| model.count_tokens(s);
        let is_cancelled = || latest.load(Ordering::Relaxed) != job;
        let result = match command {
            Command::Translate {
                options,
                document,
                text,
                ..
            } => translate_document(
                &mut session,
                &count_tokens,
                n_ctx,
                &document,
                &options,
                &text,
                &mut |output| match output {
                    Output::Text(text) => events(Event::Token {
                        job,
                        text: text.to_owned(),
                    }),
                    Output::Truncate(len) => events(Event::Rewind { job, len }),
                },
                &mut |progress| match progress {
                    Progress::Chunk {
                        index,
                        count,
                        source,
                    } => events(Event::Chunk {
                        job,
                        index,
                        count,
                        source,
                    }),
                    Progress::Retry { index, reason, .. } => {
                        events(Event::Retry { job, index, reason })
                    }
                    Progress::Cut {
                        index,
                        reason,
                        source,
                        output,
                    } => events(Event::Cut {
                        job,
                        index,
                        reason,
                        source,
                        output,
                    }),
                },
                &is_cancelled,
            ),
            Command::TranslateSegments {
                options,
                document,
                segments,
                ..
            } => translate_segments(
                &mut session,
                &count_tokens,
                n_ctx,
                &document,
                &options,
                &segments,
                &mut |index, text| events(Event::Segment { job, index, text }),
                &is_cancelled,
            ),
            Command::Load(_) | Command::Measure(_) | Command::Unload => {
                unreachable!("handled above")
            }
        };
        events(match result {
            Ok(stats) => Event::Finished { job, stats },
            Err(e) => Event::Failed {
                job,
                error: e.to_string(),
            },
        });
    }
}

/// The text translated to measure a device: a short paragraph, as typed or pasted.
const MEASURE_TEXT: &str = "The meeting has been moved to next Monday afternoon. Please let the other \
members of the team know, and bring the revised budget and the schedule for the spring release \
with you, so that we can agree on the priorities before the end of the month.";

/// Measures each device of `request` (see [`Worker::measure`]). Stops early when the worker is
/// closing.
fn measure(
    runtime: &Runtime,
    request: &MeasureRequest,
    events: &EventSink,
    closing: &Arc<AtomicBool>,
) -> Vec<Measurement> {
    let count = request.devices.len();
    request
        .devices
        .iter()
        .enumerate()
        .take_while(|_| !closing.load(Ordering::SeqCst))
        .map(|(index, &device)| {
            events(Event::Measuring { index, count });
            Measurement {
                device,
                result: measure_device(runtime, &request.path, device, closing),
            }
        })
        .collect()
}

fn measure_device(
    runtime: &Runtime,
    path: &std::path::Path,
    device: Option<usize>,
    closing: &Arc<AtomicBool>,
) -> Result<Speed, String> {
    let options = ModelOptions {
        gpu: device.map(|device| GpuOffload {
            device,
            layers: None,
        }),
    };
    let load_closing = closing.clone();
    let model = runtime
        .load_model(path, &options, move |_| {
            !load_closing.load(Ordering::SeqCst)
        })
        .map_err(|e| e.to_string())?;
    // A short context is enough for the text and is quicker to set up.
    let session_options = SessionOptions {
        n_ctx: 4096,
        ..SessionOptions::default().with_auto_threads()
    };
    let mut session = model
        .session(runtime, &session_options)
        .map_err(|e| e.to_string())?;
    let options = TranslationOptions::new(pfml::LangCode::new("ja").expect("a valid code"));
    let mut translate = || {
        translate_document(
            &mut session,
            &|s: &str| model.count_tokens(s),
            4096,
            &DocumentOptions::default(),
            &options,
            MEASURE_TEXT,
            &mut |_| {},
            &mut |_| {},
            &|| closing.load(Ordering::SeqCst),
        )
        .map_err(|e| e.to_string())
    };
    // On a GPU the first run includes one-time work, such as compiling kernels.
    if device.is_some() {
        translate()?;
    }
    let stats = translate()?;
    let rate = |tokens: usize, duration: Duration| tokens as f64 / duration.as_secs_f64().max(1e-6);
    Ok(Speed {
        prompt_tokens_per_second: rate(stats.prompt_tokens, stats.prompt_duration),
        generation_tokens_per_second: rate(stats.generated_tokens, stats.generation_duration),
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn shutdown_waits_for_cleanup_and_is_idempotent() {
        let (commands, receiver) = mpsc::channel();
        let latest = Arc::new(AtomicU64::new(1));
        let stopping = Arc::new(AtomicBool::new(false));
        let cleaned_up = Arc::new(AtomicBool::new(false));
        let cleaned = cleaned_up.clone();
        let thread = thread::spawn(move || {
            // Simulate an idle native worker: cleanup happens after its sender closes.
            assert!(receiver.recv().is_err());
            cleaned.store(true, Ordering::SeqCst);
        });
        let mut worker = Worker {
            commands: Some(commands),
            thread: Some(thread),
            latest: latest.clone(),
            stopping: stopping.clone(),
        };
        worker.shutdown();
        assert!(cleaned_up.load(Ordering::SeqCst));
        assert!(stopping.load(Ordering::SeqCst));
        assert_ne!(latest.load(Ordering::SeqCst), 1);
        worker.shutdown();
        worker.unload(); // Late UI notifications cannot restart a stopped worker.
    }
}
