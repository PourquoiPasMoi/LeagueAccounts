//! The moving sound wave of the music page: the music's sound (WASAPI
//! process loopback of our WebView2 browser process tree, `audio::music_sound`) cut into frequency bands about 30 times a second.
//! Runs only while the page asks for it (page open, window shown, playing).

use crate::recorder::audio;
use crate::recorder::encoder::AUDIO_RATE;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Bands sent to the page.
pub const BANDS: usize = 48;
const SIZE: usize = 2048;

/// In-place radix-2 FFT (re, im), SIZE points.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * std::f32::consts::PI / len as f32;
        let (w_re, w_im) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut c_re, mut c_im) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let t_re = re[b] * c_re - im[b] * c_im;
                let t_im = re[b] * c_im + im[b] * c_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                let next = c_re * w_re - c_im * w_im;
                c_im = c_re * w_im + c_im * w_re;
                c_re = next;
            }
        }
        len <<= 1;
    }
}

/// Band levels 0..255 of the last SIZE mono samples, bands spread
/// logarithmically from 40 Hz to 16 kHz.
fn bands(samples: &[f32], window: &[f32]) -> Vec<u8> {
    let mut re: Vec<f32> = samples.iter().zip(window).map(|(sample, weight)| sample * weight).collect();
    let mut im = vec![0.0f32; SIZE];
    fft(&mut re, &mut im);
    let bin_hz = AUDIO_RATE as f32 / SIZE as f32;
    let (low, high) = (40.0f32, 16_000.0f32);
    (0..BANDS)
        .map(|band| {
            let from = low * (high / low).powf(band as f32 / BANDS as f32);
            let to = low * (high / low).powf((band + 1) as f32 / BANDS as f32);
            let first = ((from / bin_hz) as usize).max(1);
            let last = ((to / bin_hz) as usize).max(first + 1).min(SIZE / 2);
            let peak = (first..last).map(|bin| (re[bin] * re[bin] + im[bin] * im[bin]).sqrt()).fold(0.0f32, f32::max);
            // Magnitude to dB, -70..-10 dB mapped to 0..255 (window sum ~SIZE/2).
            let db = 20.0 * (peak / (SIZE as f32 / 2.0)).max(1e-7).log10();
            (((db + 70.0) / 60.0).clamp(0.0, 1.0) * 255.0) as u8
        })
        .collect()
}

/// Measure until `stop` is set; `send` gets BANDS levels about 30 times a second.
/// Returns at once when the sound cannot be opened.
pub fn run(stop: Arc<AtomicBool>, send: impl Fn(Vec<u8>)) {
    let _ = crate::recorder::startup();
    let Ok(mut source) = audio::music_sound() else { return };
    let window: Vec<f32> = (0..SIZE).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / SIZE as f32).cos()).collect();
    let mut mono: Vec<f32> = vec![0.0; SIZE];
    let mut quiet = 0u32;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(33));
        let fresh = source.drain();
        let frames: Vec<f32> = fresh.chunks_exact(2).map(|pair| (pair[0] + pair[1]) * 0.5).collect();
        if frames.len() >= SIZE {
            mono.copy_from_slice(&frames[frames.len() - SIZE..]);
        } else {
            mono.drain(..frames.len());
            mono.extend_from_slice(&frames);
        }
        let levels = bands(&mono, &window);
        // Nothing to draw: send one silent frame, then stay quiet.
        if levels.iter().all(|level| *level == 0) {
            quiet += 1;
            if quiet > 1 {
                continue;
            }
        } else {
            quiet = 0;
        }
        send(levels);
    }
    drop(source);
    crate::recorder::shutdown();
}
