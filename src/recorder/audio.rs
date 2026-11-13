//! Sound for recordings, with Windows' own audio API (WASAPI):
//! - game sound only, through process loopback (Windows 10 2004+), given the
//!   game's process id; the process itself is never opened;
//! - all PC sound (loopback of the default output) when that is unavailable;
//! - the default microphone, when asked;
//! - Discord only (process loopback of the Discord app), when asked.
//!
//! With several sources the recording gets one track per source plus a mix,
//! so a clip can keep only some of them.
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
    eCapture, eCommunications, eConsole, eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, DEVICE_STATE_ACTIVE, IMMDevice,
};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::System::Com::STGM_READ;
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
    /// "game", "pc", "mic" or "discord": the track name in the recording.
    pub label: &'static str,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
    queue: VecDeque<f32>,
    gain: f32,
}

impl AudioSource {
    fn open(label: &'static str, client: IAudioClient, loopback: bool, gain: f32) -> Result<Self> {
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
            Ok(Self { label, client, capture, event, queue: VecDeque::new(), gain })
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
    process_sound("game", pid)
}

/// Discord's sound only (voice chat), when Discord runs. Discord plays audio
/// from a child process, so the whole tree of the main Discord process is
/// captured. The processes are only listed, never opened.
pub fn discord_sound() -> Result<AudioSource> {
    process_sound("discord", discord_process().ok_or_else(fail)?)
}

/// The main Discord process (stable, PTB or Canary): a Discord process whose
/// parent is not one.
fn discord_process() -> Option<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
    let mut processes: Vec<(u32, u32, String)> = Vec::new();
    let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        processes.push((entry.th32ProcessID, entry.th32ParentProcessID, String::from_utf16_lossy(&entry.szExeFile[..length]).to_lowercase()));
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    unsafe {
        let _ = CloseHandle(snapshot);
    }
    let is_discord = |name: &str| matches!(name, "discord.exe" | "discordptb.exe" | "discordcanary.exe");
    let discord: Vec<&(u32, u32, String)> = processes.iter().filter(|(_, _, name)| is_discord(name)).collect();
    discord.iter().find(|(_, parent, _)| !discord.iter().any(|(pid, _, _)| pid == parent)).map(|(pid, _, _)| *pid)
}

fn process_sound(label: &'static str, pid: u32) -> Result<AudioSource> {
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
        AudioSource::open(label, client, true, 1.0)
    }
}

/// Everything the PC plays (default output device).
pub fn pc_sound() -> Result<AudioSource> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        AudioSource::open("pc", device.Activate(CLSCTX_ALL, None)?, true, 1.0)
    }
}

/// A microphone (capture device) Windows knows.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CaptureDevice {
    /// Windows' endpoint id (kept in the settings).
    pub id: String,
    pub name: String,
    /// Windows' "Default device".
    pub console: bool,
    /// Windows' "Default communication device" (what Discord and calls use).
    pub communications: bool,
}

fn device_id(device: &IMMDevice) -> String {
    unsafe { device.GetId().ok().and_then(|id| { let text = id.to_string().ok(); windows::Win32::System::Com::CoTaskMemFree(Some(id.0 as *const _)); text }) }.unwrap_or_default()
}

fn device_name(device: &IMMDevice) -> String {
    unsafe {
        let Ok(store) = device.OpenPropertyStore(STGM_READ) else { return String::new() };
        store.GetValue(&PKEY_Device_FriendlyName).map(|value| value.to_string()).unwrap_or_default()
    }
}

/// Every active microphone, with Windows' default roles.
pub fn capture_devices() -> Vec<CaptureDevice> {
    let mut devices = Vec::new();
    unsafe {
        let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return devices };
        let default = |role| enumerator.GetDefaultAudioEndpoint(eCapture, role).map(|device| device_id(&device)).unwrap_or_default();
        let (console, communications) = (default(eConsole), default(eCommunications));
        let Ok(collection) = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) else { return devices };
        for index in 0..collection.GetCount().unwrap_or(0) {
            let Ok(device) = collection.Item(index) else { continue };
            let id = device_id(&device);
            devices.push(CaptureDevice { name: device_name(&device), console: id == console, communications: id == communications, id });
        }
    }
    devices
}

/// The microphone with this id; otherwise Windows' default communication
/// device (the headset people talk into on Discord), then the default device.
pub fn microphone(id: &str) -> Result<AudioSource> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let chosen = if id.is_empty() { None } else { enumerator.GetDevice(&windows::core::HSTRING::from(id)).ok() };
        let device = match chosen {
            Some(device) => device,
            None => enumerator.GetDefaultAudioEndpoint(eCapture, eCommunications).or_else(|_| enumerator.GetDefaultAudioEndpoint(eCapture, eConsole))?,
        };
        AudioSource::open("mic", device.Activate(CLSCTX_ALL, None)?, false, 1.0)
    }
}

/// Loudest sample (0..1) a microphone hears in `seconds` (diagnostics).
pub fn probe_level(id: &str, seconds: f64) -> Option<f32> {
    let mut source = microphone(id).ok()?;
    let started = Instant::now();
    let mut peak = 0f32;
    while started.elapsed().as_secs_f64() < seconds {
        std::thread::sleep(std::time::Duration::from_millis(50));
        source.pump();
        peak = source.queue.drain(..).fold(peak, |peak, value| peak.max(value.abs()));
    }
    Some(peak)
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

    /// The audio tracks `take` returns, in order: one source alone, or the mix
    /// followed by each source.
    pub fn tracks(&self) -> Vec<&'static str> {
        match self.sources.len() {
            0 => Vec::new(),
            1 => vec![self.sources[0].label],
            _ => std::iter::once("mix").chain(self.sources.iter().map(|source| source.label)).collect(),
        }
    }

    /// Track time 0 is now (the first video frame).
    pub fn start(&mut self) {
        for source in &mut self.sources {
            source.pump();
            source.queue.clear();
        }
        self.start = Some(Instant::now());
    }

    /// The samples due since the last call for each track of `tracks()`, and
    /// their start time (100 ns).
    pub fn take(&mut self) -> Option<(Vec<Vec<i16>>, i64)> {
        let start = self.start?;
        for source in &mut self.sources {
            source.pump();
        }
        let due = ((start.elapsed().as_secs_f64() - HOLD_BACK).max(0.0) * f64::from(AUDIO_RATE)) as u64;
        if due <= self.written {
            return None;
        }
        let frames = (due - self.written) as usize;
        let length = frames * AUDIO_CHANNELS as usize;
        let mut mix = vec![0f32; length];
        let mut each = Vec::with_capacity(self.sources.len());
        for source in &mut self.sources {
            let count = length.min(source.queue.len());
            let mut own = vec![0f32; length];
            for ((slot, mixed), value) in own.iter_mut().zip(mix.iter_mut()).zip(source.queue.drain(..count)) {
                *slot = value * source.gain;
                *mixed += *slot;
            }
            each.push(own);
        }
        let time = (self.written * 10_000_000 / u64::from(AUDIO_RATE)) as i64;
        self.written = due;
        let pcm = |values: Vec<f32>| values.into_iter().map(|value| (value.clamp(-1.0, 1.0) * 32767.0) as i16).collect::<Vec<i16>>();
        let tracks = if each.len() == 1 { vec![pcm(mix)] } else { std::iter::once(mix).chain(each).map(pcm).collect() };
        Some((tracks, time))
    }
}
