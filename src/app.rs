use crate::{
    audio::{self, Cue, Cues, Recorder},
    config::Config,
    icon::{Activity, Indicator},
    logging::Log,
    state::{Action, Input, State},
    typing,
    worker::Client,
};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum Event {
    Key(Input),
    Suspend,
    Resume,
    Quit,
}
/// Commands run strictly in order on the worker thread, so a transcription queued
/// behind a load simply waits for the model instead of failing.
enum Command {
    Load,
    Transcribe(Vec<f32>),
    Unload,
}
enum WorkerEvent {
    Loaded {
        backend: String,
        ms: u128,
    },
    LoadFailed(String),
    Unloaded,
    Done {
        result: Result<(String, u64)>,
        backend: String,
        loaded: bool,
    },
}

/// The model is released only when nothing is recording, queued, or loading.
fn should_unload(
    idle_unload_seconds: u64,
    loaded: bool,
    loading: bool,
    busy: bool,
    recording: bool,
    idle: Duration,
) -> bool {
    idle_unload_seconds > 0
        && loaded
        && !loading
        && !busy
        && !recording
        && idle >= Duration::from_secs(idle_unload_seconds)
}

#[derive(Clone)]
pub struct Status(
    pub Arc<Mutex<String>>,
    pub Arc<AtomicBool>,
    pub Arc<Indicator>,
);
impl Status {
    pub fn new() -> Self {
        let indicator = Indicator::default();
        indicator.set(Activity::Loading, false);
        Self(
            Arc::new(Mutex::new("Loading local model…".into())),
            Arc::new(AtomicBool::new(true)),
            Arc::new(indicator),
        )
    }
    pub fn get(&self) -> String {
        self.0.lock().unwrap().clone()
    }
    pub fn set(&self, text: impl Into<String>) {
        *self.0.lock().unwrap() = text.into();
    }
    pub fn allow_insertion(&self, allowed: bool) {
        self.1.store(allowed, Ordering::SeqCst);
    }
}

pub fn control(config: Config, dir: PathBuf, log: Log, events: Receiver<Event>, status: Status) {
    let (commands, work) = crossbeam_channel::unbounded::<Command>();
    let (results, done) = crossbeam_channel::unbounded();
    let worker_dir = dir.clone();
    let worker_log = log.clone();
    let worker_config = config.clone();
    std::thread::spawn(move || {
        let mut client: Option<Client> = None;
        let load = |client: &mut Option<Client>| -> WorkerEvent {
            let started = Instant::now();
            match Client::start(&worker_dir, &worker_config, &worker_log) {
                Ok(new_client) => {
                    let backend = new_client.backend().into();
                    *client = Some(new_client);
                    WorkerEvent::Loaded {
                        backend,
                        ms: started.elapsed().as_millis(),
                    }
                }
                Err(error) => WorkerEvent::LoadFailed(format!("{error:#}")),
            }
        };
        while let Ok(command) = work.recv() {
            let event = match command {
                Command::Load if client.is_some() => continue,
                Command::Load => load(&mut client),
                Command::Unload => match client.take() {
                    // Dropping the client kills the worker process, which frees all of its GPU memory.
                    Some(_) => WorkerEvent::Unloaded,
                    None => continue,
                },
                Command::Transcribe(samples) => {
                    let mut load_error = None;
                    if client.is_none() {
                        match load(&mut client) {
                            WorkerEvent::LoadFailed(error) => load_error = Some(error),
                            loaded_event => {
                                if results.send(loaded_event).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    match client.as_mut() {
                        Some(client) => transcribe(client, &samples, &worker_dir, &worker_log),
                        None => WorkerEvent::Done {
                            result: Err(anyhow::anyhow!(load_error.unwrap_or_default())),
                            backend: String::new(),
                            loaded: false,
                        },
                    }
                }
            };
            if results.send(event).is_err() {
                break;
            }
        }
    });
    // Load at startup to surface configuration errors early; idle unload releases it later.
    let _ = commands.send(Command::Load);
    let mut recorder = match Recorder::open(&config, &log) {
        Ok(recorder) => Some(recorder),
        Err(error) => {
            log.event(format!("Microphone: {error:#}"));
            None
        }
    };
    let mut cues = match Cues::open(config.cue_volume) {
        Ok(cues) => Some(cues),
        Err(error) => {
            log.event(format!("Audio cues unavailable: {error:#}"));
            None
        }
    };
    let mut state = State::default();
    let mut backend = String::new();
    let mut loaded = false;
    let mut loading = true;
    let mut last_used = Instant::now();
    let mut suspended = false;
    let mut model_error = false;
    let mut suppress_result = false;
    let mut started = Instant::now();
    let mut released = Instant::now();
    let ticks = crossbeam_channel::tick(Duration::from_millis(200));
    let mut next_device_check = Instant::now();
    loop {
        let activity = if suspended {
            Activity::Paused
        } else if state.recording {
            Activity::Recording
        } else if state.busy {
            Activity::Transcribing
        } else if loading {
            Activity::Loading
        } else if model_error || recorder.is_none() {
            Activity::Error
        } else {
            Activity::Idle
        };
        status.2.set(activity, loaded);
        crossbeam_channel::select! {
            recv(events) -> event => match event {
                Ok(Event::Quit) | Err(_) => {
                    if let Some(recorder) = &recorder { recorder.discard(); }
                    log.event("Application stopped"); break;
                }
                Ok(Event::Suspend) => {
                    suspended = true; suppress_result = true;
                    state.input(Input::Cancel); state.held = false;
                    if let Some(recorder) = &recorder { recorder.discard(); }
                    status.set("Paused while Windows is locked");
                }
                Ok(Event::Resume) => {
                    suspended = false;
                    if !state.busy { status.set(idle_status(&backend, loaded, loading)); }
                }
                Ok(Event::Key(input)) => {
                    if suspended { continue; }
                    match state.input(input) {
                        Action::Start => {
                            last_used = Instant::now();
                            // Load in parallel with recording; the microphone stream is already open.
                            if !loaded && !loading {
                                loading = commands.send(Command::Load).is_ok();
                            }
                            let result = recorder.as_ref().ok_or_else(|| anyhow::anyhow!("No microphone available"))
                                .and_then(|r| r.start());
                            if let Err(error) = result {
                                state.input(Input::Cancel); status.set("Microphone unavailable · see app.log");
                                log.event(format!("Recording failed: {error:#}")); continue;
                            }
                            started = Instant::now();
                            if let Some(cues) = &cues { cues.play(Cue::Start); }
                            status.set(if loaded { "Recording · Escape cancels" } else { "Recording · loading model · Escape cancels" });
                        }
                        Action::Discard => {
                            if let Some(recorder) = &recorder { recorder.discard(); }
                            if let Some(cues) = &cues { cues.play(Cue::Cancel); }
                            log.event("Recording cancelled; nothing submitted");
                            status.set("Cancelled · release the key");
                        }
                        Action::Submit => {
                            released = Instant::now();
                            last_used = released;
                            let recording = recorder.as_ref().unwrap();
                            let captured = recording.stop();
                            if let Some(cues) = &cues { cues.play(Cue::Stop); }
                            match captured.and_then(|samples| audio::prepare(&samples, recording.rate, &config)) {
                                Ok(Some(samples)) => {
                                    suppress_result = false;
                                    if commands.send(Command::Transcribe(samples)).is_err() {
                                        state.complete();
                                        status.set("Whisper unavailable · restart the app");
                                    } else if loaded {
                                        status.set(format!("Transcribing · {backend}"));
                                    } else {
                                        status.set("Transcribing · loading model");
                                    }
                                }
                                Ok(None) => {
                                    state.complete(); status.set(format!("{} · no speech detected", idle_status(&backend, loaded, loading)));
                                    log.event("Empty, very short, or quiet recording discarded");
                                }
                                Err(error) => {
                                    state.complete(); status.set("Recording discarded · see app.log");
                                    log.event(format!("Recording discarded: {error:#}"));
                                }
                            }
                        }
                        Action::None => {
                            if input == Input::Down && state.busy { status.set("Busy · wait for completion before recording"); }
                            if input == Input::Up && !state.busy { status.set(idle_status(&backend, loaded, loading)); }
                        }
                    }
                }
            },
            recv(done) -> event => match event {
                Ok(WorkerEvent::Loaded { backend: name, ms }) => {
                    backend = name; loaded = true; loading = false; model_error = false;
                    log.event(format!("Whisper model loaded in {ms} ms ({backend})"));
                    if !state.busy && !state.recording {
                        status.set(if recorder.is_some() { idle_status(&backend, loaded, loading) } else { "No default microphone · waiting".into() });
                    }
                }
                Ok(WorkerEvent::LoadFailed(error)) => {
                    // Not permanent: the next press (or queued recording) tries to load again.
                    loaded = false; loading = false; model_error = true;
                    log.event(format!("Whisper model load failed: {error}"));
                    if !state.busy && !state.recording { status.set("Model failed to load · see app.log"); }
                }
                Ok(WorkerEvent::Unloaded) => {
                    log.event(format!("Whisper model unloaded after {} s idle; GPU memory released", config.idle_unload_seconds));
                    if !state.busy && !state.recording { status.set(idle_status(&backend, loaded, loading)); }
                }
                Ok(WorkerEvent::Done { result, backend: name, loaded: still_loaded }) => {
                    if !name.is_empty() { backend = name; }
                    loaded = still_loaded; loading = false;
                    last_used = Instant::now();
                    state.complete();
                    if suppress_result || suspended {
                        log.event("Pending transcription discarded after session lock/suspend");
                        continue;
                    }
                    match result {
                        Ok((text, inference_ms)) if !text.is_empty() => {
                            // Trailing space keeps consecutive takes from running together.
                            match typing::insert(&format!("{text} "), config.output_mode, &status.1) {
                                Ok(method) => {
                                    if let Some(cues) = &cues { cues.play(Cue::Complete); }
                                    log.event(format!("Inserted {} characters ({method}); inference {inference_ms} ms; release-to-insertion {} ms; {backend}", text.chars().count(), released.elapsed().as_millis()));
                                    status.set(idle_status(&backend, loaded, loading));
                                }
                                Err(error) => { status.set("Text insertion failed · see app.log"); log.event(format!("Insertion failed: {error:#}")); }
                            }
                        }
                        Ok(_) => { status.set(format!("{} · no speech detected", idle_status(&backend, loaded, loading))); }
                        Err(error) => { status.set("Transcription failed · see app.log"); log.event(format!("Transcription failed: {error:#}")); }
                    }
                }
                Err(_) => {
                    // A disconnected receiver is always ready in select; stop selecting it.
                    while let Ok(event) = events.recv() { if matches!(event, Event::Quit) { return; } }
                    return;
                }
            },
            recv(ticks) -> _ => {
                if should_unload(config.idle_unload_seconds, loaded, loading, state.busy, state.recording, last_used.elapsed())
                    && commands.send(Command::Unload).is_ok()
                {
                    loaded = false;
                }
                if state.recording && (started.elapsed().as_secs() >= config.max_recording_seconds as u64 || recorder.as_ref().is_some_and(Recorder::failed)) {
                    state.input(Input::Cancel);
                    if let Some(recorder) = &recorder { recorder.discard(); }
                    status.set("Recording cancelled · time limit or microphone disconnected");
                    log.event("Capture limit/device failure: discarded audio, waiting for key release");
                }
                if !state.recording && Instant::now() >= next_device_check {
                    next_device_check = Instant::now() + Duration::from_secs(2);
                    if recorder.as_ref().is_none_or(Recorder::needs_reopen) {
                        recorder = None; // release the old WASAPI endpoint before reopening
                        match Recorder::open(&config, &log) {
                            Ok(new_recorder) => { recorder = Some(new_recorder); log.event("Default microphone ready"); }
                            Err(_) => { if !state.busy { status.set("No default microphone · waiting"); } }
                        }
                    }
                    if cues.as_ref().is_none_or(Cues::needs_reopen) {
                        drop(cues.take());
                        cues = Cues::open(config.cue_volume).ok();
                    }
                }
            }
        }
    }
}

fn transcribe(
    client: &mut Client,
    samples: &[f32],
    dir: &std::path::Path,
    log: &Log,
) -> WorkerEvent {
    let result = client.transcribe(samples, dir, log);
    WorkerEvent::Done {
        result,
        backend: client.backend().into(),
        loaded: client.is_loaded(),
    }
}

fn idle_status(backend: &str, loaded: bool, loading: bool) -> String {
    if loading {
        "Loading model · you can start speaking".into()
    } else if loaded {
        format!("Ready · {backend}")
    } else {
        "Ready · model unloaded, loads when you press".into()
    }
}

pub fn send(tx: &Sender<Event>, event: Event) {
    let _ = tx.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    const IDLE: Duration = Duration::from_secs(120);
    #[test]
    fn unloads_only_when_fully_idle() {
        assert!(should_unload(60, true, false, false, false, IDLE));
        assert!(
            !should_unload(0, true, false, false, false, IDLE),
            "0 keeps the model loaded"
        );
        assert!(
            !should_unload(60, false, false, false, false, IDLE),
            "nothing to unload"
        );
        assert!(
            !should_unload(60, true, true, false, false, IDLE),
            "never while loading"
        );
        assert!(
            !should_unload(60, true, false, true, false, IDLE),
            "never with a queued transcription"
        );
        assert!(
            !should_unload(60, true, false, false, true, IDLE),
            "never while recording"
        );
        assert!(
            !should_unload(600, true, false, false, false, IDLE),
            "not before the timeout"
        );
    }
}
