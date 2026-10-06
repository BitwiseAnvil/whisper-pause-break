//! Framed anonymous-pipe IPC, no sockets or audio files.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const MAX_SAMPLES: usize = 301 * 16_000;
const MAX_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ready { backend: String },
    Text { text: String, inference_ms: u64 },
    Error { message: String },
}

fn read_length(reader: &mut impl Read) -> Result<Option<usize>> {
    let mut bytes = [0_u8; 4];
    if reader.read(&mut bytes[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut bytes[1..])?;
    Ok(Some(u32::from_le_bytes(bytes) as usize))
}

pub fn write_audio(writer: &mut impl Write, samples: &[f32]) -> Result<()> {
    ensure!(
        !samples.is_empty() && samples.len() <= MAX_SAMPLES,
        "Invalid audio frame size"
    );
    writer.write_all(&(samples.len() as u32).to_le_bytes())?;
    let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_audio(reader: &mut impl Read) -> Result<Option<Vec<f32>>> {
    let Some(n) = read_length(reader)? else {
        return Ok(None);
    };
    ensure!(n > 0 && n <= MAX_SAMPLES, "Invalid audio frame size");
    let mut bytes = vec![0_u8; n * 4];
    reader.read_exact(&mut bytes)?;
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect();
    ensure!(
        samples.iter().all(|v| v.is_finite()),
        "Audio contains non-finite values"
    );
    Ok(Some(samples))
}

pub fn write_response(writer: &mut impl Write, response: &Response) -> Result<()> {
    let bytes = serde_json::to_vec(response)?;
    ensure!(bytes.len() <= MAX_RESPONSE_BYTES, "Response too large");
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_response(reader: &mut impl Read) -> Result<Response> {
    let n = read_length(reader)?
        .ok_or_else(|| anyhow::anyhow!("Worker exited or its CUDA runtime could not load"))?;
    ensure!(n <= MAX_RESPONSE_BYTES, "Invalid worker response length");
    let mut bytes = vec![0; n];
    reader.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_round_trip() {
        let mut bytes = Vec::new();
        write_audio(&mut bytes, &[0.0, -0.25, 0.75]).unwrap();
        assert_eq!(
            read_audio(&mut bytes.as_slice()).unwrap().unwrap(),
            [0.0, -0.25, 0.75]
        );
    }
    #[test]
    fn oversized_and_truncated_frames_are_rejected() {
        assert!(read_audio(&mut u32::MAX.to_le_bytes().as_slice()).is_err());
        assert!(read_audio(&mut [1, 0].as_slice()).is_err());
        assert!(read_audio(&mut [1, 0, 0, 0].as_slice()).is_err());
        assert!(read_response(&mut u32::MAX.to_le_bytes().as_slice()).is_err());
        assert!(read_audio(&mut [].as_slice()).unwrap().is_none());
    }
    #[test]
    fn unicode_response_round_trip() {
        let mut bytes = Vec::new();
        write_response(
            &mut bytes,
            &Response::Text {
                text: "Hello 世界 🦀".into(),
                inference_ms: 12,
            },
        )
        .unwrap();
        assert!(
            matches!(read_response(&mut bytes.as_slice()).unwrap(), Response::Text { text, .. } if text == "Hello 世界 🦀")
        );
    }
}
