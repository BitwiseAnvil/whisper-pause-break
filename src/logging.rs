use anyhow::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Log(Arc<Mutex<File>>);

impl Log {
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join("app.log");
        if fs::metadata(&path).is_ok_and(|m| m.len() > 2_000_000) {
            fs::copy(&path, dir.join("app.previous.log"))?;
            File::create(&path)?;
        }
        Ok(Self(Arc::new(Mutex::new(
            OpenOptions::new().create(true).append(true).open(path)?,
        ))))
    }
    pub fn event(&self, message: impl AsRef<str>) {
        if let Ok(mut file) = self.0.lock() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            let _ = writeln!(
                file,
                "{}.{:03} {}",
                now.as_secs(),
                now.subsec_millis(),
                message.as_ref()
            );
        }
    }
}
