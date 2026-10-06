use crate::{
    config::{Backend, Config},
    logging::Log,
    protocol::{self, Response},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::OpenOptions,
    io::{BufReader, BufWriter},
    os::windows::{io::AsRawHandle, process::CommandExt},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{JobObjects::*, Threading::CREATE_NO_WINDOW},
};

pub fn serve(dir: &Path, gpu: bool) -> Result<()> {
    let mut output = BufWriter::new(std::io::stdout());
    let result = (|| -> Result<()> {
        let config = Config::load(dir)?;
        if gpu {
            ensure!(cfg!(feature = "cuda"), "This binary was built without CUDA");
            // Only native backends linked into this executable are registered here.
            let found = unsafe {
                use whisper_rs::whisper_rs_sys::*;
                (0..ggml_backend_dev_count()).any(|i| {
                    ggml_backend_dev_type(ggml_backend_dev_get(i))
                        == ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU
                })
            };
            ensure!(found, "No CUDA GPU is available");
        }
        let mut options = WhisperContextParameters::default();
        options.use_gpu(gpu).flash_attn(gpu);
        let context = WhisperContext::new_with_params(config.model_path(dir), options)
            .context("Could not load Whisper model")?;
        ensure!(
            context.is_multilingual() || config.language == "en",
            "English-only model requires language = 'en'"
        );
        let mut state = context.create_state()?;
        // Do model/GPU allocation on login, not on the first key release.
        let _ = infer(&mut state, &[0.0; 16_000], &config)?;
        protocol::write_response(
            &mut output,
            &Response::Ready {
                backend: if gpu { "CUDA" } else { "CPU" }.into(),
            },
        )?;
        let mut input = BufReader::new(std::io::stdin());
        while let Some(samples) = protocol::read_audio(&mut input)? {
            let start = Instant::now();
            let text = infer(&mut state, &samples, &config)?;
            protocol::write_response(
                &mut output,
                &Response::Text {
                    text,
                    inference_ms: start.elapsed().as_millis() as u64,
                },
            )?;
        }
        Ok(())
    })();
    if let Err(error) = &result {
        let _ = protocol::write_response(
            &mut output,
            &Response::Error {
                message: format!("{error:#}"),
            },
        );
    }
    result
}

fn infer(state: &mut WhisperState, audio: &[f32], config: &Config) -> Result<String> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(config.threads as i32);
    params.set_language(if config.language == "auto" {
        None
    } else {
        Some(&config.language)
    });
    params.set_translate(false);
    params.set_no_context(true);
    params.set_no_timestamps(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_temperature(0.0);
    params.set_temperature_inc(0.0);
    params.set_no_speech_thold(0.6);
    // All segments remain private until full() has returned successfully.
    state.full(params, audio)?;
    let mut text = String::new();
    for segment in state.as_iter() {
        if segment.no_speech_probability() < 0.6 {
            text.push_str(segment.to_str()?);
        }
    }
    Ok(crate::typing::normalize(&text))
}

struct Job(HANDLE);
impl Job {
    fn attach(child: &Child) -> Result<Self> {
        unsafe {
            let job = Self(CreateJobObjectW(std::ptr::null(), std::ptr::null()));
            ensure!(
                !job.0.is_null(),
                "CreateJobObject: {}",
                std::io::Error::last_os_error()
            );
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            ensure!(
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as _,
                    std::mem::size_of_val(&limits) as u32
                ) != 0,
                "Could not set worker lifetime limit"
            );
            ensure!(
                AssignProcessToJobObject(job.0, child.as_raw_handle() as _) != 0,
                "Could not attach worker job: {}",
                std::io::Error::last_os_error()
            );
            Ok(job)
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                CloseHandle(self.0);
            }
        }
    }
}

struct Process {
    child: Child,
    input: BufWriter<ChildStdin>,
    responses: mpsc::Receiver<Result<Response>>,
    _job: Job,
    gpu: bool,
}

impl Process {
    fn start(dir: &Path, gpu: bool, timeout: Duration) -> Result<Self> {
        let exe = std::env::current_exe()?;
        let binary = if gpu {
            exe.with_file_name("whisper-pause-break-cuda.exe")
        } else {
            exe
        };
        let log_path = dir.join(if gpu { "cuda.log" } else { "cpu.log" });
        if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > 2_000_000) {
            std::fs::File::create(&log_path)?;
        }
        let stderr = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        let mut command = Command::new(binary);
        command.arg("--data-dir").arg(dir).arg("worker");
        if gpu {
            command.arg("--gpu");
        }
        let mut child = command
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()?;
        let job = match Job::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let input = BufWriter::new(child.stdin.take().context("Missing worker stdin")?);
        let mut output = BufReader::new(child.stdout.take().context("Missing worker stdout")?);
        let (tx, responses) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let response = protocol::read_response(&mut output);
                let failed = response.is_err();
                if tx.send(response).is_err() || failed {
                    break;
                }
            }
        });
        let process = Self {
            child,
            input,
            responses,
            _job: job,
            gpu,
        };
        match process.receive(timeout)? {
            Response::Ready { .. } => Ok(process),
            Response::Error { message } => bail!("{message}"),
            _ => bail!("Unexpected worker startup response"),
        }
    }

    fn receive(&self, timeout: Duration) -> Result<Response> {
        self.responses
            .recv_timeout(timeout)
            .context("Whisper worker timed out or disconnected")?
    }

    fn transcribe(&mut self, audio: &[f32], timeout: Duration) -> Result<(String, u64)> {
        protocol::write_audio(&mut self.input, audio)?;
        match self.receive(timeout)? {
            Response::Text { text, inference_ms } => Ok((text, inference_ms)),
            Response::Error { message } => bail!("{message}"),
            _ => bail!("Unexpected worker response"),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Client {
    process: Option<Process>,
    timeout: Duration,
}

impl Client {
    pub fn start(dir: &Path, config: &Config, log: &Log) -> Result<Self> {
        let timeout = Duration::from_secs(config.inference_timeout_seconds);
        let gpu_exists = std::env::current_exe()?
            .with_file_name("whisper-pause-break-cuda.exe")
            .exists();
        let process = if config.backend == Backend::Auto && gpu_exists {
            match Process::start(dir, true, timeout) {
                Ok(process) => process,
                Err(error) => {
                    log.event(format!("CUDA unavailable; using CPU: {error:#}"));
                    Process::start(dir, false, timeout)?
                }
            }
        } else {
            Process::start(dir, false, timeout)?
        };
        log.event(format!(
            "Whisper ready: {}",
            if process.gpu { "CUDA" } else { "CPU" }
        ));
        Ok(Self {
            process: Some(process),
            timeout,
        })
    }
    pub fn backend(&self) -> &'static str {
        if self.process.as_ref().is_some_and(|p| p.gpu) {
            "CUDA"
        } else {
            "CPU"
        }
    }
    pub fn transcribe(&mut self, audio: &[f32], dir: &Path, log: &Log) -> Result<(String, u64)> {
        if self.process.is_none() {
            self.process = Some(Process::start(dir, false, self.timeout)?);
        }
        let result = self
            .process
            .as_mut()
            .unwrap()
            .transcribe(audio, self.timeout);
        if result.is_err() {
            // Destroy the broken process before allocating another model, including after a native abort.
            let was_gpu = self.process.take().is_some_and(|p| p.gpu);
            if was_gpu {
                log.event("CUDA inference failed; retrying this recording once on CPU");
                self.process = Some(Process::start(dir, false, self.timeout)?);
                return self
                    .process
                    .as_mut()
                    .unwrap()
                    .transcribe(audio, self.timeout);
            }
        }
        result
    }
}
