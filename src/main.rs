#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(not(windows))]
compile_error!("Whisper Pause/Break requires Windows 10/11 x64.");

mod app;
mod audio;
mod config;
mod logging;
mod platform;
mod protocol;
mod setup;
mod shortcuts;
mod state;
mod typing;
mod worker;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use logging::Log;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Local push-to-talk dictation: hold Pause/Break, speak, release. Escape cancels."
)]
struct Cli {
    /// Configuration/models/log directory (default: %LOCALAPPDATA%\WhisperPauseBreak).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run silently in the notification area (the default command).
    Run,
    /// Install per-user, enable Windows login startup, and start now.
    Install {
        /// Copy an existing GGML Whisper model into the installation.
        #[arg(long)]
        model: Option<PathBuf>,
        #[arg(long)]
        no_start: bool,
    },
    /// Stop and remove automatic startup, retaining files and models.
    Uninstall,
    /// Stop the running background instance.
    Stop,
    /// One-time, checksum-verified download of the official 148 MB English base model.
    DownloadModel,
    /// Check local configuration, default audio devices, and startup registration.
    Doctor,
    /// Observe this keyboard's real press/release events for 30 seconds. No recording.
    KeyTest,
    /// Transcribe an existing WAV locally, without typing into another application.
    Transcribe {
        input: PathBuf,
        #[arg(long)]
        cpu: bool,
        /// Save the completed transcript as UTF-8 instead of writing it to stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Play the four interaction sounds in sequence without recording.
    CueTest,
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        gpu: bool,
    },
}

fn main() {
    // Child workers inherit this: missing CUDA DLLs/native faults must fall back, not open a dialog.
    unsafe {
        use windows_sys::Win32::System::Diagnostics::Debug::*;
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    }
    // Release GUI binaries can still print CLI diagnostics when invoked from a terminal.
    let is_worker = std::env::args().any(|arg| arg == "worker");
    let interactive = std::env::args().len() > 1 && !is_worker;
    if interactive {
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
    }
    let cli = Cli::parse();
    if let Err(error) = execute(cli) {
        eprintln!("{error:#}");
        if !interactive && !is_worker {
            platform::error_dialog(&format!("{error:#}"));
        }
        std::process::exit(1);
    }
}

fn execute(cli: Cli) -> Result<()> {
    let dir = config::data_dir(cli.data_dir)?;
    match cli.command.unwrap_or(Commands::Run) {
        Commands::Worker { gpu } => worker::serve(&dir, gpu),
        Commands::DownloadModel => {
            println!("Verified model: {}", setup::download_model(&dir)?.display());
            Ok(())
        }
        Commands::Install { model, no_start } => setup::install(&dir, model.as_deref(), !no_start),
        Commands::Uninstall => setup::uninstall(),
        Commands::Stop => platform::stop(),
        Commands::Doctor => {
            use cpal::traits::{DeviceTrait, HostTrait};
            let config = Config::load(&dir)?;
            println!(
                "Data directory: {}\nModel: {}\nModel present: {}\nHotkey: {:?}\nBackend preference: {:?}\nLogin startup: {}",
                dir.display(),
                config.model_path(&dir).display(),
                config.model_path(&dir).is_file(),
                config.hotkey,
                config.backend,
                setup::startup_enabled()
            );
            let host = cpal::default_host();
            for (kind, device) in [
                ("Microphone", host.default_input_device()),
                ("Speakers", host.default_output_device()),
            ] {
                println!(
                    "{kind}: {}",
                    device
                        .and_then(|d| d.description().ok())
                        .map(|d| d.name().to_owned())
                        .unwrap_or_else(|| "UNAVAILABLE".into())
                );
            }
            println!(
                "Run key-test and physically hold/release Pause to verify real release events on this keyboard."
            );
            Ok(())
        }
        Commands::CueTest => {
            let config = Config::load(&dir)?;
            let cues = audio::Cues::open(config.cue_volume)?;
            for (name, cue) in [
                ("Press", audio::Cue::Start),
                ("Release", audio::Cue::Stop),
                ("Completion", audio::Cue::Complete),
                ("Cancellation", audio::Cue::Cancel),
            ] {
                println!("{name}");
                cues.play(cue);
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
            Ok(())
        }
        Commands::KeyTest => {
            let _instance = platform::Instance::acquire()?;
            let config = Config::load(&dir)?;
            let (tx, rx) = crossbeam_channel::unbounded();
            let started = std::time::Instant::now();
            println!(
                "For 30 seconds: hold {:?} for 2 seconds, then release. DOWN and UP should be 2 seconds apart. No microphone is opened.",
                config.hotkey
            );
            let receiver = std::thread::spawn(move || {
                while let Ok(event) = rx.recv() {
                    if matches!(event, app::Event::Quit) {
                        break;
                    }
                    println!("{:>6} ms: {event:?}", started.elapsed().as_millis());
                }
            });
            let result = platform::run(tx.clone(), app::Status::new(), dir, config.hotkey, config.block_win_h, true);
            app::send(&tx, app::Event::Quit);
            let _ = receiver.join();
            result
        }
        Commands::Transcribe { input, cpu, output } => {
            let mut config = Config::load(&dir)?;
            if cpu {
                config.backend = config::Backend::Cpu;
            }
            let log = Log::open(&dir)?;
            let (samples, rate) = audio::read_wav(&input)?;
            let samples = audio::resample(&samples, rate)?;
            let mut client = worker::Client::start(&dir, &config, &log)?;
            let (text, time) = client.transcribe(&samples, &dir, &log)?;
            eprintln!("{}: {time} ms inference", client.backend());
            if let Some(path) = output {
                std::fs::write(path, text)?;
            } else {
                println!("{text}");
            }
            Ok(())
        }
        Commands::Run => {
            let _instance = platform::Instance::acquire()?;
            let config = Config::load(&dir)?;
            config.save_if_missing(&dir)?;
            let log = Log::open(&dir)?;
            log.event("Starting Whisper Pause/Break; audio and transcript contents are not logged");
            let status = app::Status::new();
            let (tx, rx) = crossbeam_channel::unbounded();
            let controller_status = status.clone();
            let controller_dir = dir.clone();
            let controller_config = config.clone();
            let controller = std::thread::spawn(move || {
                app::control(
                    controller_config,
                    controller_dir,
                    log,
                    rx,
                    controller_status,
                )
            });
            let result = platform::run(tx.clone(), status, dir, config.hotkey, config.block_win_h, false);
            app::send(&tx, app::Event::Quit);
            let _ = controller.join();
            result
        }
    }
}
