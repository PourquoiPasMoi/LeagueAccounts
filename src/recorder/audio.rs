//! Sound for recordings, with Windows' own audio API (WASAPI):
//! - game sound only, through process loopback (Windows 10 2004+), given the
//!   game's process id; the process itself is never opened;
//! - all PC sound (loopback of the default output) when that is unavailable;
//! - the default microphone, when asked.
//!
//! Sources are read on the recording thread and mixed against the wall clock,
//! so silence (no packets) still advances the track and stays in sync.

use super::encoder::{AUDIO_CHANNELS, AUDIO_RATE};
use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::time::Instant;
use windows::core::{implement, Interface, Ref, Result, HRESULT, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{CoCreateInstance, BLOB, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

fn fail() -> windows::core::Error {
    windows::core::Error::from(E_FAIL)
}

/// 32-bit float stereo at 48 kHz; Windows converts whatever the device uses.
fn format() -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
        nChannels: AUDIO_CHANNELS as u16,
        nSamplesPerSec: AUDIO_RATE,
        nAvgBytesPerSec: AUDIO_RATE * AUDIO_CHANNELS * 4,
        nBlockAlign: (AUDIO_CHANNELS * 4) as u16,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

/// Most audio a source may hold before the oldest is dropped (clock drift).
const MAX_QUEUED: usize = (AUDIO_RATE * AUDIO_CHANNELS) as usize * 3 / 10;

pub struct AudioSource {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
    queue: VecDeque<f32>,
    gain: f32,
}

impl AudioSource {
    fn open(client: IAudioClient, loopback: bool, gain: f32) -> Result<Self> {
        let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
            | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | if loopback { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
        let format = format();
        unsafe {
            // 200 ms buffer; it is read every frame (33 ms at 30 fps).
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &format, None)?;
            let event = CreateEventW(None, false, false, PCWSTR::null())?;
            client.SetEventHandle(event)?;
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok(Self { client, capture, event, queue: VecDeque::new(), gain })
        }
    }

    /// Move everything Windows has captured into the queue.
    fn pump(&mut self) {
        loop {
            let Ok(size) = (unsafe { self.capture.GetNextPacketSize() }) else { break };
            if size == 0 {
                break;
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            if unsafe { self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }.is_err() {
                break;
            }
            let count = frames as usize * AUDIO_CHANNELS as usize;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                self.queue.extend(std::iter::repeat_n(0.0, count));
            } else {
                let samples = unsafe { std::slice::from_raw_parts(data as *const f32, count) };
                self.queue.extend(samples.iter().copied());
            }
            unsafe {
                let _ = self.capture.ReleaseBuffer(frames);
            }
        }
        if self.queue.len() > MAX_QUEUED {
            let excess = self.queue.len() - MAX_QUEUED;
            self.queue.drain(..excess - excess % AUDIO_CHANNELS as usize);
        }
    }
}

impl Drop for AudioSource {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated {
    event: isize,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _operation: Ref<IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        unsafe { SetEvent(HANDLE(self.event as *mut core::ffi::c_void)) }
    }
}

/// Only the sound of `pid` and its child processes (the game).
pub fn game_sound(pid: u32) -> Result<AudioSource> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // The parameters are borrowed, not owned: nothing may free them.
    let variant = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: &mut params as *mut AUDIOCLIENT_ACTIVATION_PARAMS as *mut u8,
                    },
                },
            }),
        },
    });
    unsafe {
        let event = CreateEventW(None, true, false, PCWSTR::null())?;
        let handler: IActivateAudioInterfaceCompletionHandler = Activated { event: event.0 as isize }.into();
        let operation = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*variant), &handler);
        let operation = match operation {
            Ok(operation) => operation,
            Err(error) => {
                let _ = CloseHandle(event);
                return Err(error);
            }
        };
        WaitForSingleObject(event, 5000);
        let _ = CloseHandle(event);
        let mut result = HRESULT(0);
        let mut unknown = None;
        operation.GetActivateResult(&mut result, &mut unknown)?;
        result.ok()?;
        let client: IAudioClient = unknown.ok_or_else(fail)?.cast()?;
        AudioSource::open(client, true, 1.0)
    }
}

/// Everything the PC plays (default output device).
pub fn pc_sound() -> Result<AudioSource> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        AudioSource::open(device.Activate(CLSCTX_ALL, None)?, true, 1.0)
    }
}

/// The default microphone.
pub fn microphone() -> Result<AudioSource> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?;
        AudioSource::open(device.Activate(CLSCTX_ALL, None)?, false, 1.0)
    }
}

/// Audio is written this far behind real time, so packets have arrived.
const HOLD_BACK: f64 = 0.15;

/// Mixes the sources into one 16-bit stereo track that follows the wall clock.
pub struct AudioMixer {
    sources: Vec<AudioSource>,
    start: Option<Instant>,
    written: u64,
}

impl AudioMixer {
    pub fn new(sources: Vec<AudioSource>) -> Self {
        Self { sources, start: None, written: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Track time 0 is now (the first video frame).
    pub fn start(&mut self) {
        for source in &mut self.sources {
            source.pump();
            source.queue.clear();
        }
        self.start = Some(Instant::now());
    }

    /// The mixed samples due since the last call and their start time (100 ns).
    pub fn take(&mut self) -> Option<(Vec<i16>, i64)> {
        let start = self.start?;
        for source in &mut self.sources {
            source.pump();
        }
        let due = ((start.elapsed().as_secs_f64() - HOLD_BACK).max(0.0) * f64::from(AUDIO_RATE)) as u64;
        if due <= self.written {
            return None;
        }
        let frames = (due - self.written) as usize;
        let mut mix = vec![0f32; frames * AUDIO_CHANNELS as usize];
        for source in &mut self.sources {
            let count = mix.len().min(source.queue.len());
            for (slot, value) in mix.iter_mut().zip(source.queue.drain(..count)) {
                *slot += value * source.gain;
            }
        }
        let time = (self.written * 10_000_000 / u64::from(AUDIO_RATE)) as i64;
        self.written = due;
        Some((mix.into_iter().map(|value| (value.clamp(-1.0, 1.0) * 32767.0) as i16).collect(), time))
    }
}
