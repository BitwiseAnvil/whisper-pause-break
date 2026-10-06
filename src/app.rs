use crate::{
    audio::{self, Cue, Cues, Recorder},
    config::Config,
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
enum WorkerEvent {
    Ready(String),
    Failed(String),
    Done(Result<(String, u64)>, String),
}

#[derive(Clone)]
pub struct Status(pub Arc<Mutex<String>>, pub Arc<AtomicBool>);
impl Status {
    pub fn new() -> Self {
        Self(
            Arc::new(Mutex::new("Loading local model…".into())),
            Arc::new(AtomicBool::new(true)),
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
    let (requests, work) = crossbeam_channel::bounded::<Vec<f32>>(1);
    let (results, done) = crossbeam_channel::unbounded();
    let worker_dir = dir.clone();
    let worker_log = log.clone();
    let worker_config = config.clone();
    std::thread::spawn(move || {
        let mut client = match Client::start(&worker_dir, &worker_config, &worker_log) {
            Ok(client) => client,
            Err(e) => {
                let _ = results.send(WorkerEvent::Failed(format!("{e:#}")));
                return;
            }
        };
        let _ = results.send(WorkerEvent::Ready(client.backend().into()));
        while let Ok(samples) = work.recv() {
            let result = client.transcribe(&samples, &worker_dir, &worker_log);
            if results
                .send(WorkerEvent::Done(result, client.backend().into()))
                .is_err()
            {
                break;
            }
        }
    });
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
    let mut state = State {
        busy: true,
        ..State::default()
    };
    let mut backend = String::new();
    let mut ready = false;
    let mut suspended = false;
    let mut suppress_result = false;
    let mut started = Instant::now();
    let mut released = Instant::now();
    let ticks = crossbeam_channel::tick(Duration::from_millis(200));
    let mut next_device_check = Instant::now();
    loop {
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
                    if ready && !state.busy { status.set(format!("Ready · {backend}")); }
                }
                Ok(Event::Key(input)) => {
                    if suspended { continue; }
                    match state.input(input) {
                        Action::Start => {
                            if !ready { state.input(Input::Cancel); continue; }
                            let result = recorder.as_ref().ok_or_else(|| anyhow::anyhow!("No microphone available"))
                                .and_then(|r| r.start());
                            if let Err(error) = result {
                                state.input(Input::Cancel); status.set("Microphone unavailable · see app.log");
                                log.event(format!("Recording failed: {error:#}")); continue;
                            }
                            started = Instant::now();
                            if let Some(cues) = &cues { cues.play(Cue::Start); }
                            status.set("Recording · Escape cancels");
                        }
                        Action::Discard => {
                            if let Some(recorder) = &recorder { recorder.discard(); }
                            if let Some(cues) = &cues { cues.play(Cue::Cancel); }
                            log.event("Recording cancelled; nothing submitted");
                            status.set("Cancelled · release the key");
                        }
                        Action::Submit => {
                            released = Instant::now();
                            let recording = recorder.as_ref().unwrap();
                            let captured = recording.stop();
                            if let Some(cues) = &cues { cues.play(Cue::Stop); }
                            match captured.and_then(|samples| audio::prepare(&samples, recording.rate, &config)) {
                                Ok(Some(samples)) => {
                                    suppress_result = false;
                                    if requests.try_send(samples).is_err() {
                                        state.complete(); ready = false;
                                        status.set("Whisper unavailable · restart the app");
                                    } else { status.set(format!("Transcribing · {backend}")); }
                                }
                                Ok(None) => {
                                    state.complete(); status.set(format!("Ready · {backend} · no speech detected"));
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
                            if input == Input::Up && !state.busy && ready { status.set(format!("Ready · {backend}")); }
                        }
                    }
                }
            },
            recv(done) -> event => match event {
                Ok(WorkerEvent::Ready(name)) => {
                    backend = name; ready = true; state.complete();
                    status.set(if recorder.is_some() { format!("Ready · {backend}") } else { "No default microphone · waiting".into() });
                }
                Ok(WorkerEvent::Failed(error)) => {
                    ready = false; state.busy = true;
                    status.set("Model unavailable · see app.log and restart");
                    log.event(format!("Whisper startup failed: {error}"));
                }
                Ok(WorkerEvent::Done(result, name)) => {
                    backend = name;
                    state.complete();
                    if suppress_result || suspended {
                        log.event("Pending transcription discarded after session lock/suspend");
                        continue;
                    }
                    match result {
                        Ok((text, inference_ms)) if !text.is_empty() => {
                            // Trailing space keeps consecutive takes from running together.
                            match typing::insert(&format!("{text} "), &status.1) {
                                Ok(()) => {
                                    if let Some(cues) = &cues { cues.play(Cue::Complete); }
                                    log.event(format!("Inserted {} characters; inference {inference_ms} ms; release-to-insertion {} ms; {backend}", text.chars().count(), released.elapsed().as_millis()));
                                    status.set(format!("Ready · {backend}"));
                                }
                                Err(error) => { status.set("Text insertion failed · see app.log"); log.event(format!("Insertion failed: {error:#}")); }
                            }
                        }
                        Ok(_) => { status.set(format!("Ready · {backend} · no speech detected")); }
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

pub fn send(tx: &Sender<Event>, event: Event) {
    let _ = tx.send(event);
}
