use crate::{config::Config, platform};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_SET_VALUE},
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "WhisperPauseBreak";
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
const MODEL_HASH: &str = "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002";
const MODEL_SIZE: u64 = 147_964_211;

pub fn download_model(dir: &Path) -> Result<PathBuf> {
    let model = dir.join("models/ggml-base.en.bin");
    fs::create_dir_all(model.parent().unwrap())?;
    if model.exists() {
        verify_model(&model)?;
        return Ok(model);
    }
    let partial = model.with_extension("bin.part");
    let mut file = File::options().write(true).create_new(true).open(&partial)
        .context("Cannot create model download; remove a stale .bin.part file if a previous download was interrupted")?;
    let result = (|| -> Result<()> {
        println!("Downloading the public Whisper base.en model (148 MB); no audio is uploaded.");
        let mut response = ureq::get(MODEL_URL).call()?;
        let mut reader = response.body_mut().as_reader();
        let mut hash = Sha256::new();
        let mut bytes = [0_u8; 65536];
        let mut total = 0;
        loop {
            let n = reader.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            ensure!(total <= MODEL_SIZE, "Model download larger than expected");
            hash.update(&bytes[..n]);
            file.write_all(&bytes[..n])?;
        }
        ensure!(total == MODEL_SIZE, "Incomplete model download");
        ensure!(
            format!("{:x}", hash.finalize()) == MODEL_HASH,
            "Model SHA-256 verification failed"
        );
        file.sync_all()?;
        Ok(())
    })();
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(partial);
        return Err(error);
    }
    fs::rename(partial, &model)?;
    Config::default().save_if_missing(dir)?;
    Ok(model)
}

pub fn verify_model(path: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.len() == MODEL_SIZE,
        "Existing base.en model has unexpected size"
    );
    let mut hash = Sha256::new();
    let mut buf = [0_u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    ensure!(
        format!("{:x}", hash.finalize()) == MODEL_HASH,
        "Existing base.en model has an invalid SHA-256"
    );
    Ok(())
}

fn run_value(exe: &Path, dir: &Path) -> String {
    // Windows paths cannot contain quotes. Always quote both paths, including spaces.
    format!("\"{}\" --data-dir \"{}\" run", exe.display(), dir.display())
}

pub fn install(dir: &Path, model: Option<&Path>, start: bool) -> Result<()> {
    ensure!(
        !cfg!(feature = "cuda"),
        "Install using the CPU host executable, not the CUDA worker"
    );
    let mut config = Config::load(dir)?;
    fs::create_dir_all(dir.join("models"))?;
    if let Some(source) = model {
        ensure!(source.is_file(), "Model file does not exist");
        let dest = dir
            .join("models")
            .join(source.file_name().context("Invalid model filename")?);
        if fs::canonicalize(source).ok() != fs::canonicalize(&dest).ok() {
            fs::copy(source, &dest)?;
        }
        config.model = PathBuf::from("models").join(source.file_name().unwrap());
    }
    ensure!(
        config.model_path(dir).is_file(),
        "Model missing. Run download-model first, or install --model <existing GGML model>"
    );
    platform::stop()?;
    // Wait until a previous instance has closed its executable before copying.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(instance) = platform::Instance::acquire() {
            drop(instance);
            break;
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "Previous instance is still shutting down; try install again"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let source_exe = std::env::current_exe()?;
    let source_dir = source_exe.parent().unwrap();
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir)?;
    let installed = bin_dir.join("whisper-pause-break.exe");
    if fs::canonicalize(&source_exe).ok() != fs::canonicalize(&installed).ok() {
        fs::copy(&source_exe, &installed)?;
    }
    for item in fs::read_dir(source_dir)? {
        let path = item?.path();
        let name = path.file_name().unwrap().to_string_lossy();
        if name == "whisper-pause-break-cuda.exe"
            || (name.ends_with(".dll")
                && ["cublas64_", "cublasLt64_", "cudart64_"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix)))
        {
            let dest = bin_dir.join(path.file_name().unwrap());
            if fs::canonicalize(&path).ok() != fs::canonicalize(&dest).ok() {
                fs::copy(path, dest)?;
            }
        }
    }
    fs::write(dir.join("config.toml"), toml::to_string_pretty(&config)?)?;
    let (run, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY)?;
    run.set_value(RUN_NAME, &run_value(&installed, dir))?;
    if start {
        Command::new(&installed)
            .arg("--data-dir")
            .arg(dir)
            .arg("run")
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
    }
    println!(
        "Installed to {}. Windows login startup enabled.",
        installed.display()
    );
    Ok(())
}

pub fn uninstall() -> Result<()> {
    platform::stop()?;
    if let Ok(run) =
        RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
    {
        match run.delete_value(RUN_NAME) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    println!(
        "Stopped and removed Windows login startup. Model, config, logs and binaries are retained; delete the data directory if desired."
    );
    Ok(())
}

pub fn startup_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|key| key.get_value::<String, _>(RUN_NAME))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_command_quotes_paths() {
        assert_eq!(
            run_value(
                Path::new(r"C:\Users\A Person\app.exe"),
                Path::new(r"C:\Users\A Person\data")
            ),
            r#""C:\Users\A Person\app.exe" --data-dir "C:\Users\A Person\data" run"#
        );
    }
}
