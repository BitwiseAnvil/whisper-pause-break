//! Tray icon drawn in code: color shows activity, a filled dot means the model is loaded
//! and a hollow ring means it is unloaded (it reloads on the next press).
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Activity {
    Idle = 0,
    Recording = 1,
    Transcribing = 2,
    Loading = 3,
    Error = 4,
    Paused = 5,
}

impl Activity {
    const ALL: [Self; 6] = [
        Self::Idle,
        Self::Recording,
        Self::Transcribing,
        Self::Loading,
        Self::Error,
        Self::Paused,
    ];
    fn rgb(self) -> [u8; 3] {
        match self {
            Self::Idle => [0x9a, 0xa0, 0xa6],         // gray
            Self::Recording => [0x43, 0xc0, 0x4a],    // green: listening
            Self::Transcribing => [0x1e, 0x88, 0xe5], // blue
            Self::Loading => [0xfb, 0xc0, 0x2d],      // amber
            Self::Error => [0x8e, 0x24, 0xaa],        // purple
            Self::Paused => [0x5f, 0x63, 0x68],       // dark gray: Windows locked
        }
    }
}

/// Lock-free hand-off from the controller thread to the tray (UI) thread.
#[derive(Default)]
pub struct Indicator(AtomicU8);

impl Indicator {
    pub fn set(&self, activity: Activity, loaded: bool) {
        self.0
            .store(activity as u8 | if loaded { 0x80 } else { 0 }, Ordering::Relaxed);
    }
    pub fn get(&self) -> (Activity, bool) {
        let value = self.0.load(Ordering::Relaxed);
        let activity = Activity::ALL
            .into_iter()
            .find(|a| *a as u8 == value & 0x7f)
            .unwrap_or(Activity::Idle);
        (activity, value & 0x80 != 0)
    }
}

/// Premultiplied BGRA pixels, top-down, `size` x `size`, anti-aliased by 4x4 supersampling.
pub fn pixels(size: usize, activity: Activity, filled: bool) -> Vec<u8> {
    let [r, g, b] = activity.rgb();
    let center = size as f32 / 2.0;
    let outer = center - 0.5;
    let inner = if filled {
        0.0
    } else {
        outer - (size as f32 / 5.0).max(2.0)
    };
    // A darker 1px rim keeps the shape visible on both light and dark taskbars.
    let rim = 1.0_f32.max(size as f32 / 16.0);
    let mut out = vec![0_u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let (mut cover, mut edge) = (0.0_f32, 0.0_f32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let dx = x as f32 + (sx as f32 + 0.5) / 4.0 - center;
                    let dy = y as f32 + (sy as f32 + 0.5) / 4.0 - center;
                    let d = (dx * dx + dy * dy).sqrt();
                    if d <= outer && d >= inner {
                        cover += 1.0 / 16.0;
                        if d > outer - rim || (!filled && d < inner + rim) {
                            edge += 1.0 / 16.0;
                        }
                    }
                }
            }
            if cover > 0.0 {
                let shade = 1.0 - 0.45 * (edge / cover);
                let alpha = cover;
                let i = (y * size + x) * 4;
                out[i] = (b as f32 * shade * alpha) as u8;
                out[i + 1] = (g as f32 * shade * alpha) as u8;
                out[i + 2] = (r as f32 * shade * alpha) as u8;
                out[i + 3] = (alpha * 255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indicator_round_trips() {
        let indicator = Indicator::default();
        assert_eq!(indicator.get(), (Activity::Idle, false));
        for activity in Activity::ALL {
            for loaded in [false, true] {
                indicator.set(activity, loaded);
                assert_eq!(indicator.get(), (activity, loaded));
            }
        }
    }
    #[test]
    fn hollow_ring_has_a_transparent_center() {
        for size in [16, 24, 32] {
            let mid = (size / 2 * size + size / 2) * 4 + 3;
            assert_eq!(pixels(size, Activity::Recording, false)[mid], 0);
            assert_eq!(pixels(size, Activity::Recording, true)[mid], 255);
            // Corners are always transparent.
            assert_eq!(pixels(size, Activity::Idle, true)[3], 0);
        }
    }
}
