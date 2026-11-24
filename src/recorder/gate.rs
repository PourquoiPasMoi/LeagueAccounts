//! Noise gate for the microphone track: the microphone is silenced while the
//! player is not talking, so keyboard and mouse clicks between words are not
//! recorded. Plain signal processing on the samples we capture ourselves.
//!
//! Each microphone has its own settings (by Windows endpoint id), shared with
//! `set_all` / `current_for`: the recording and the live tuning window read
//! them every block, so a change is heard at once, even in the middle of a game.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

use super::encoder::{AUDIO_CHANNELS, AUDIO_RATE};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GateSettings {
    pub enabled: bool,
    /// The gate opens above this level (dB below full scale, -80..0).
    pub threshold_db: f32,
    /// How long it stays open after the voice drops below the threshold.
    pub hold_ms: u32,
    /// How long it takes to fade out after the hold.
    pub release_ms: u32,
}

impl Default for GateSettings {
    fn default() -> Self {
        Self { enabled: false, threshold_db: -42.0, hold_ms: 200, release_ms: 150 }
    }
}

impl GateSettings {
    pub fn sanitized(self) -> Self {
        let threshold = if self.threshold_db.is_finite() { self.threshold_db } else { -42.0 };
        Self {
            enabled: self.enabled,
            threshold_db: threshold.clamp(-80.0, 0.0),
            hold_ms: self.hold_ms.min(2000),
            release_ms: self.release_ms.clamp(10, 2000),
        }
    }
}

static GATES: Mutex<BTreeMap<String, GateSettings>> = Mutex::new(BTreeMap::new());

/// The gate of this microphone (off when it has none). The app sets them at start and on every change.
pub fn current_for(device: &str) -> GateSettings {
    GATES.lock().ok().and_then(|gates| gates.get(device).copied()).unwrap_or_default()
}

/// Every microphone's gate, by Windows endpoint id.
pub fn set_all(gates: &BTreeMap<String, GateSettings>) {
    if let Ok(mut current) = GATES.lock() {
        *current = gates.iter().map(|(device, gate)| (device.clone(), gate.sanitized())).collect();
    }
}

/// Below the threshold by this much before the gate starts closing, so a
/// voice hovering around the threshold does not flutter.
const HYSTERESIS_DB: f32 = 4.0;
/// Fade-in when the gate opens: fast enough to keep the start of a word.
const ATTACK_MS: f32 = 2.0;
/// Fall time of the level detector.
const DETECTOR_MS: f32 = 12.0;

pub fn db(level: f32) -> f32 {
    if level <= 1e-5 {
        -100.0
    } else {
        20.0 * level.log10()
    }
}

fn coefficient(ms: f32) -> f32 {
    (-1.0 / (ms.max(0.1) / 1000.0 * AUDIO_RATE as f32)).exp()
}

/// What a block did, for the meter: loudest input, loudest output, open at its end.
#[derive(Clone, Copy, Debug, Default)]
pub struct Block {
    pub input_peak: f32,
    pub output_peak: f32,
    pub open: bool,
}

/// One gate (keeps its state between blocks).
pub struct NoiseGate {
    envelope: f32,
    gain: f32,
    hold_left: u32,
    open: bool,
}

impl Default for NoiseGate {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseGate {
    pub fn new() -> Self {
        Self { envelope: 0.0, gain: 1.0, hold_left: 0, open: true }
    }

    /// Gate interleaved stereo samples in place with `settings`. When the gate
    /// is off, samples pass untouched (the meter still gets the levels).
    pub fn process(&mut self, samples: &mut [f32], settings: GateSettings) -> Block {
        let channels = AUDIO_CHANNELS as usize;
        let mut block = Block::default();
        let open_level = 10f32.powf(settings.threshold_db / 20.0);
        let close_level = 10f32.powf((settings.threshold_db - HYSTERESIS_DB) / 20.0);
        let hold = (settings.hold_ms as f32 / 1000.0 * AUDIO_RATE as f32) as u32;
        let (attack, release, detector) = (coefficient(ATTACK_MS), coefficient(settings.release_ms as f32 / 4.0), coefficient(DETECTOR_MS));
        for frame in samples.chunks_mut(channels) {
            let peak = frame.iter().fold(0f32, |peak, value| peak.max(value.abs()));
            block.input_peak = block.input_peak.max(peak);
            if !settings.enabled {
                block.output_peak = block.output_peak.max(peak);
                continue;
            }
            self.envelope = if peak > self.envelope { peak } else { self.envelope * detector + peak * (1.0 - detector) };
            if self.envelope >= open_level {
                self.open = true;
                self.hold_left = hold;
            } else if self.envelope < close_level {
                if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            } else if self.open {
                // Between the two levels: stays as it is, hold refreshed.
                self.hold_left = hold;
            }
            let target = if self.open { 1.0 } else { 0.0 };
            // release_ms / 4 as the time constant: about silent after release_ms.
            let speed = if target > self.gain { attack } else { release };
            self.gain = target + (self.gain - target) * speed;
            for value in frame.iter_mut() {
                *value *= self.gain;
                block.output_peak = block.output_peak.max(value.abs());
            }
        }
        block.open = !settings.enabled || self.open;
        block
    }
}

