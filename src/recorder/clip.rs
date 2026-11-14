//! Cutting parts of a recording into a new MP4 with Media Foundation.
//! Video: Fast is a stream copy (each part starts at the keyframe at or before
//! its start; recordings have a keyframe every second); Exact decodes and
//! re-encodes (on the GPU when possible) for a frame-exact start. Sound: the
//! recording's first audio track as it is, none, or only some of its sources
//! (game, mic, Discord) mixed into one track.

use super::capture::Gpu;
use super::encoder::{AUDIO_CHANNELS, AUDIO_RATE};
use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::path::Path;
use windows::core::{Result, GUID, HSTRING};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Variant::VT_I8;

fn fail() -> windows::core::Error {
    windows::core::Error::from(E_FAIL)
}

fn attributes(size: u32) -> Result<IMFAttributes> {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, size)? };
    attributes.ok_or_else(fail)
}

fn position(seconds: f64) -> PROPVARIANT {
    PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_I8,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { hVal: (seconds.max(0.0) * 10_000_000.0) as i64 },
            }),
        },
    }
}

/// Duration of an MP4 in seconds (0 when unknown).
pub fn duration(path: &Path) -> f64 {
    unsafe {
        let Ok(reader) = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None::<&IMFAttributes>) else { return 0.0 };
        let Ok(value) = reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION) else { return 0.0 };
        value.Anonymous.Anonymous.Anonymous.uhVal as f64 / 10_000_000.0
    }
}

/// Which sound goes into the clip (audio tracks counted from 0 in the file).
#[derive(Clone, Debug, PartialEq)]
pub enum AudioPlan {
    None,
    /// One track as it is (track 0 is the mix of everything, or the only source).
    Track(usize),
    /// These tracks mixed into one, each at its own level (1.0 = as recorded).
    Mix(Vec<(usize, f32)>),
}

/// Where a part landed, in seconds: start and end in the source, start in the clip.
#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub source_start: f64,
    pub source_end: f64,
    pub clip_start: f64,
}

/// Smaller output for sharing: lower bitrate, and a lower resolution and frame
/// rate when the bitrate gets low.
#[derive(Clone, Copy, Debug)]
struct Shrink {
    video_bitrate: u32,
    /// Largest height (the width follows the picture's shape).
    height: u32,
    /// Frames per second cap (0 = keep).
    fps: u32,
}

#[derive(Clone, Copy, Debug)]
enum Video {
    Copy,
    Encode { hardware: bool, shrink: Option<Shrink> },
}

/// AAC 160 kbps normally, 96 kbps for shared clips.
const AUDIO_BYTES: u32 = 20_000;
const SHARE_AUDIO_BYTES: u32 = 12_000;
const UNITS: f64 = 10_000_000.0;

/// Cut the `parts` (start, end seconds, in order) of `source` and join them
/// with plain cuts into `target`. Returns where each part really landed (a
/// fast cut starts on the keyframe at or before the chosen start).
pub fn cut(source: &Path, target: &Path, parts: &[(f64, f64)], exact: bool, audio: &AudioPlan) -> Result<Vec<Part>> {
    let result = if exact {
        transcode(source, target, parts, Video::Encode { hardware: true, shrink: None }, audio)
            .or_else(|_| transcode(source, target, parts, Video::Encode { hardware: false, shrink: None }, audio))
    } else {
        transcode(source, target, parts, Video::Copy, audio)
    };
    if result.is_err() {
        let _ = std::fs::remove_file(target);
    }
    result
}

fn shrink_for(bits_per_second: f64) -> Shrink {
    let video = (bits_per_second - f64::from(SHARE_AUDIO_BYTES * 8)).max(150_000.0) as u32;
    let (height, fps) = match video {
        v if v >= 4_500_000 => (1080, 0),
        v if v >= 2_200_000 => (720, 0),
        v if v >= 1_200_000 => (720, 30),
        v if v >= 600_000 => (480, 30),
        _ => (360, 30),
    };
    Shrink { video_bitrate: video, height, fps }
}

/// Like `cut` with an exact cut, re-encoded so the file stays under
/// `max_bytes` (Discord's upload limit for normal accounts). The bitrate comes
/// from the clip's length; if the result is still too big it is tried again
/// with less.
pub fn cut_for_size(source: &Path, target: &Path, parts: &[(f64, f64)], max_bytes: u64, audio: &AudioPlan) -> Result<Vec<Part>> {
    let length: f64 = parts.iter().map(|(start, end)| (end - start).max(0.0)).sum::<f64>().max(1.0);
    // Room for the MP4 index and the encoder overshooting its target.
    let mut budget = max_bytes as f64 * 8.0 * 0.9 / length;
    let mut last = Err(fail());
    for _ in 0..3 {
        let shrink = Some(shrink_for(budget));
        last = transcode(source, target, parts, Video::Encode { hardware: true, shrink }, audio)
            .or_else(|_| transcode(source, target, parts, Video::Encode { hardware: false, shrink }, audio));
        match &last {
            Ok(_) if std::fs::metadata(target).is_ok_and(|metadata| metadata.len() <= max_bytes) => return last,
            Ok(_) => {
                let size = std::fs::metadata(target).map(|metadata| metadata.len()).unwrap_or(max_bytes * 2) as f64;
                budget *= (max_bytes as f64 / size).min(0.8);
            }
            Err(_) => break,
        }
    }
    let _ = std::fs::remove_file(target);
    last.and(Err(fail()))
}

fn mp4_writer(target: &Path, manager: Option<&IMFDXGIDeviceManager>) -> Result<IMFSinkWriter> {
    let _ = std::fs::remove_file(target);
    let options = attributes(3)?;
    unsafe {
        options.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        if let Some(manager) = manager {
            options.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            options.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, manager)?;
        }
        MFCreateSinkWriterFromURL(&HSTRING::from(target.as_os_str()), None::<&IMFByteStream>, &options)
    }
}

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// 16-bit stereo PCM at 48 kHz (the recordings' sound, decoded).
fn pcm_type() -> Result<IMFMediaType> {
    unsafe {
        let pcm = MFCreateMediaType()?;
        pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        pcm.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
        pcm.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)?;
        pcm.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, AUDIO_CHANNELS * 2)?;
        pcm.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_RATE * AUDIO_CHANNELS * 2)?;
        Ok(pcm)
    }
}

fn aac_type(bytes_per_second: u32) -> Result<IMFMediaType> {
    unsafe {
        let aac = MFCreateMediaType()?;
        aac.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        aac.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
        aac.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        aac.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
        aac.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)?;
        aac.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_second)?;
        Ok(aac)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Video,
    /// Written to the clip (copied or re-encoded).
    Audio,
    /// Decoded and added into the mixed track (slot number).
    MixIn(usize),
}

struct Track {
    reader: u32,
    writer: Option<u32>,
    role: Role,
    done: bool,
}

/// The file's video stream and its audio streams in order.
/// A stream number in the file and its native media type.
type Stream = (u32, IMFMediaType);

/// The video stream and the audio streams in file order. Media Foundation's
/// MP4 reader lists a file's tracks in reverse (the video, first in our files,
/// comes last): then the audio list is turned around so track 0 is the file's
/// first audio track (the mix).
fn streams(reader: &IMFSourceReader) -> (Option<Stream>, Vec<Stream>) {
    let mut video = None;
    let mut audio = Vec::new();
    for index in 0..16u32 {
        let Ok(native) = (unsafe { reader.GetNativeMediaType(index, 0) }) else { break };
        let Ok(major) = (unsafe { native.GetMajorType() }) else { continue };
        if major == MFMediaType_Video && video.is_none() {
            video = Some((index, native));
        } else if major == MFMediaType_Audio {
            audio.push((index, native));
        }
    }
    let video_last = video.as_ref().is_some_and(|(video, _)| audio.iter().all(|(index, _)| index < video));
    if video_last {
        audio.reverse();
    }
    (video, audio)
}

/// Adds decoded tracks sample by sample on the clip's timeline and hands out
/// the part every track has reached.
struct Mixer {
    /// Interleaved sums, starting at frame `emitted`.
    sums: VecDeque<i32>,
    emitted: i64,
    /// Frame each slot has written up to.
    filled: Vec<i64>,
    /// Level of each slot (1.0 = as recorded).
    gains: Vec<f32>,
}

/// Loud sums (levels above 100 %) are bent smoothly under the maximum instead
/// of being cut flat, which would crackle.
fn soft_limit(value: i32) -> i16 {
    const KNEE: i32 = 24_576;
    const ROOM: f32 = (32_767 - KNEE) as f32;
    let size = value.abs();
    if size <= KNEE {
        return value as i16;
    }
    let over = (size - KNEE) as f32;
    let bent = KNEE + (over * ROOM / (over + ROOM)) as i32;
    (bent.min(32_767) * value.signum()) as i16
}

impl Mixer {
    fn new(gains: Vec<f32>) -> Self {
        Self { sums: VecDeque::new(), emitted: 0, filled: vec![0; gains.len()], gains }
    }

    fn add(&mut self, slot: usize, frame: i64, samples: &[i16]) {
        let gain = self.gains.get(slot).copied().unwrap_or(1.0);
        let channels = AUDIO_CHANNELS as i64;
        let skip = (self.emitted - frame).max(0);
        let samples = samples.get((skip * channels) as usize..).unwrap_or_default();
        let frame = frame.max(self.emitted);
        let start = ((frame - self.emitted) * channels) as usize;
        let needed = start + samples.len();
        if self.sums.len() < needed {
            self.sums.resize(needed, 0);
        }
        for (index, value) in samples.iter().enumerate() {
            self.sums[start + index] += (f32::from(*value) * gain) as i32;
        }
        self.filled[slot] = self.filled[slot].max(frame + samples.len() as i64 / channels);
    }

    /// Everything all tracks reached (or, at the end of a part, everything).
    fn take(&mut self, all: bool) -> Option<(Vec<i16>, i64)> {
        let upto = if all { self.filled.iter().copied().max() } else { self.filled.iter().copied().min() }.unwrap_or(0);
        if upto <= self.emitted {
            return None;
        }
        let count = (((upto - self.emitted) * AUDIO_CHANNELS as i64) as usize).min(self.sums.len());
        let samples: Vec<i16> = self.sums.drain(..count).map(soft_limit).collect();
        let frame = self.emitted;
        self.emitted = upto;
        if all {
            self.sums.clear();
            for filled in &mut self.filled {
                *filled = upto;
            }
        }
        Some((samples, frame))
    }

    /// The next part starts at `frame` (after the previous one ends).
    fn restart(&mut self, frame: i64) {
        self.sums.clear();
        self.emitted = frame.max(self.emitted);
        for filled in &mut self.filled {
            *filled = self.emitted;
        }
    }
}

unsafe fn write_pcm(writer: &IMFSinkWriter, stream: u32, samples: &[i16], frame: i64) -> Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    let bytes = std::mem::size_of_val(samples);
    unsafe {
        let buffer = MFCreateMemoryBuffer(bytes as u32)?;
        let mut target = std::ptr::null_mut();
        buffer.Lock(&mut target, None, None)?;
        std::ptr::copy_nonoverlapping(samples.as_ptr() as *const u8, target, bytes);
        buffer.Unlock()?;
        buffer.SetCurrentLength(bytes as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(frame * 10_000_000 / i64::from(AUDIO_RATE))?;
        sample.SetSampleDuration((samples.len() as i64 / i64::from(AUDIO_CHANNELS)) * 10_000_000 / i64::from(AUDIO_RATE))?;
        writer.WriteSample(stream, &sample)
    }
}

/// The 16-bit samples of a decoded sample.
unsafe fn pcm_of(sample: &IMFSample) -> Result<Vec<i16>> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut data = std::ptr::null_mut();
        let mut length = 0u32;
        buffer.Lock(&mut data, None, Some(&mut length))?;
        let samples = std::slice::from_raw_parts(data as *const i16, length as usize / 2).to_vec();
        buffer.Unlock()?;
        Ok(samples)
    }
}

/// Every chosen track selected again for the next part.
unsafe fn select_all(reader: &IMFSourceReader, tracks: &mut [Track]) -> Result<()> {
    for track in tracks.iter_mut() {
        track.done = false;
        unsafe { reader.SetStreamSelection(track.reader, true)? };
    }
    Ok(())
}

/// Read samples until every track is past the end. `handle` marks a track
/// done when it is past the end.
unsafe fn pump(reader: &IMFSourceReader, tracks: &mut [Track], mut handle: impl FnMut(&mut Track, &IMFSample, i64) -> Result<()>) -> Result<()> {
    while tracks.iter().any(|track| !track.done) {
        let mut index = 0u32;
        let mut flags = 0u32;
        let mut time = 0i64;
        let mut sample = None;
        unsafe { reader.ReadSample(MF_SOURCE_READER_ANY_STREAM.0 as u32, 0, Some(&mut index), Some(&mut flags), Some(&mut time), Some(&mut sample))? };
        let Some(track) = tracks.iter_mut().find(|track| track.reader == index) else { continue };
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            track.done = true;
        }
        if let Some(sample) = sample {
            if !track.done {
                handle(track, &sample, time)?;
            }
        }
        if track.done {
            unsafe {
                let _ = reader.SetStreamSelection(index, false);
            }
        }
    }
    Ok(())
}

/// Set up the video stream: copied as it is, or decoded and encoded again
/// (smaller for sharing).
unsafe fn add_video(reader: &IMFSourceReader, writer: &IMFSinkWriter, index: u32, native: &IMFMediaType, video: Video) -> Result<u32> {
    unsafe {
        let Video::Encode { shrink, .. } = video else {
            reader.SetCurrentMediaType(index, None, native)?;
            let stream = writer.AddStream(native)?;
            writer.SetInputMediaType(stream, native, None::<&IMFAttributes>)?;
            return Ok(stream);
        };
        let mut size = native.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let mut rate = native.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(pack(30, 1));
        let mut bitrate = native.GetUINT32(&MF_MT_AVG_BITRATE).unwrap_or(8_000_000).max(1_000_000);
        let decoded = MFCreateMediaType()?;
        decoded.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        decoded.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        if let Some(shrink) = shrink {
            bitrate = shrink.video_bitrate;
            let (width, height) = ((size >> 32) as u32, size as u32);
            if height > shrink.height {
                let scaled = ((u64::from(width) * u64::from(shrink.height) / u64::from(height.max(1))) as u32) & !1;
                size = pack(scaled.max(2), shrink.height);
            }
            let (numerator, denominator) = ((rate >> 32) as u32, (rate as u32).max(1));
            if shrink.fps > 0 && numerator / denominator > shrink.fps {
                rate = pack(shrink.fps, 1);
            }
            decoded.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            decoded.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
            decoded.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            decoded.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            if reader.SetCurrentMediaType(index, None, &decoded).is_err() {
                // No frame rate change available: keep the source's.
                rate = native.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(pack(30, 1));
                decoded.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
                reader.SetCurrentMediaType(index, None, &decoded)?;
            }
        } else {
            reader.SetCurrentMediaType(index, None, &decoded)?;
        }
        let output = MFCreateMediaType()?;
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        output.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
        output.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
        output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
        let stream = writer.AddStream(&output)?;
        let input = reader.GetCurrentMediaType(index)?;
        if shrink.is_some() {
            // Constant bitrate keeps the size predictable.
            let parameters = attributes(1)?;
            parameters.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32)?;
            if writer.SetInputMediaType(stream, &input, &parameters).is_err() {
                writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>)?;
            }
        } else {
            writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>)?;
        }
        Ok(stream)
    }
}

fn transcode(source: &Path, target: &Path, parts: &[(f64, f64)], video: Video, audio: &AudioPlan) -> Result<Vec<Part>> {
    let hardware = matches!(video, Video::Encode { hardware: true, .. });
    let shrink = matches!(video, Video::Encode { shrink: Some(_), .. });
    let audio_bytes = if shrink { SHARE_AUDIO_BYTES } else { AUDIO_BYTES };
    let gpu = if hardware { Some(Gpu::new()?) } else { None };
    unsafe {
        let manager = match &gpu {
            Some(gpu) => {
                let mut token = 0;
                let mut manager = None;
                MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
                let manager = manager.ok_or_else(fail)?;
                manager.ResetDevice(&gpu.device, token)?;
                Some(manager)
            }
            None => None,
        };
        let options = attributes(3)?;
        if let Some(manager) = &manager {
            options.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            options.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, manager)?;
        }
        if shrink {
            // Lets the reader scale the picture and change the frame rate.
            options.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        }
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(source.as_os_str()), &options)?;
        let writer = mp4_writer(target, manager.as_ref())?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        let (video_stream, audio_streams) = streams(&reader);
        let (video_index, video_native) = video_stream.ok_or_else(fail)?;
        let mut tracks = Vec::new();
        reader.SetStreamSelection(video_index, true)?;
        let stream = add_video(&reader, &writer, video_index, &video_native, video)?;
        tracks.push(Track { reader: video_index, writer: Some(stream), role: Role::Video, done: false });

        let mut mix_stream = None;
        let mut mixer = None;
        match audio {
            AudioPlan::None => {}
            AudioPlan::Track(number) => {
                if let Some((index, native)) = audio_streams.get(*number) {
                    reader.SetStreamSelection(*index, true)?;
                    let stream = if matches!(video, Video::Copy) {
                        reader.SetCurrentMediaType(*index, None, native)?;
                        let stream = writer.AddStream(native)?;
                        writer.SetInputMediaType(stream, native, None::<&IMFAttributes>)?;
                        stream
                    } else {
                        reader.SetCurrentMediaType(*index, None, &pcm_type()?)?;
                        let stream = writer.AddStream(&aac_type(audio_bytes)?)?;
                        writer.SetInputMediaType(stream, &reader.GetCurrentMediaType(*index)?, None::<&IMFAttributes>)?;
                        stream
                    };
                    tracks.push(Track { reader: *index, writer: Some(stream), role: Role::Audio, done: false });
                }
            }
            AudioPlan::Mix(numbers) => {
                let chosen: Vec<(&(u32, IMFMediaType), f32)> = numbers.iter().filter_map(|(number, gain)| Some((audio_streams.get(*number)?, *gain))).collect();
                if !chosen.is_empty() {
                    let stream = writer.AddStream(&aac_type(audio_bytes)?)?;
                    writer.SetInputMediaType(stream, &pcm_type()?, None::<&IMFAttributes>)?;
                    for (slot, ((index, _), _)) in chosen.iter().enumerate() {
                        reader.SetStreamSelection(*index, true)?;
                        reader.SetCurrentMediaType(*index, None, &pcm_type()?)?;
                        tracks.push(Track { reader: *index, writer: None, role: Role::MixIn(slot), done: false });
                    }
                    mix_stream = Some(stream);
                    mixer = Some(Mixer::new(chosen.iter().map(|(_, gain)| *gain).collect()));
                }
            }
        }
        writer.BeginWriting()?;

        let copy = matches!(video, Video::Copy);
        let mut offset = 0i64;
        let mut landed = Vec::new();
        for &(start, end) in parts {
            select_all(&reader, &mut tracks)?;
            reader.SetCurrentPosition(&GUID::zeroed(), &position(start))?;
            let start = (start * UNITS) as i64;
            let end = (end * UNITS) as i64;
            if let Some(mixer) = mixer.as_mut() {
                mixer.restart(offset * i64::from(AUDIO_RATE) / 10_000_000);
            }
            // A fast cut starts on the first keyframe; an exact one on `start`.
            let mut base: Option<i64> = if copy { None } else { Some(start) };
            let mut length = 0i64;
            pump(&reader, &mut tracks, |track, sample, time| {
                if track.role == Role::Video && base.is_none() {
                    if sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 0 {
                        return Ok(());
                    }
                    base = Some(time);
                }
                let Some(base) = base.filter(|base| time >= *base) else { return Ok(()) };
                if time > end {
                    track.done = true;
                    return Ok(());
                }
                let relative = time - base;
                length = length.max(relative + sample.GetSampleDuration().unwrap_or(0));
                match track.role {
                    Role::Video | Role::Audio => {
                        sample.SetSampleTime(offset + relative)?;
                        writer.WriteSample(track.writer.ok_or_else(fail)?, sample)?;
                    }
                    Role::MixIn(slot) => {
                        let (Some(mixer), Some(stream)) = (mixer.as_mut(), mix_stream) else { return Ok(()) };
                        let frame = (offset + relative) * i64::from(AUDIO_RATE) / 10_000_000;
                        mixer.add(slot, frame, &pcm_of(sample)?);
                        while let Some((samples, frame)) = mixer.take(false) {
                            write_pcm(&writer, stream, &samples, frame)?;
                        }
                    }
                }
                Ok(())
            })?;
            if let (Some(mixer), Some(stream)) = (mixer.as_mut(), mix_stream) {
                if let Some((samples, frame)) = mixer.take(true) {
                    write_pcm(&writer, stream, &samples, frame)?;
                }
            }
            if let Some(base) = base.filter(|_| length > 0) {
                landed.push(Part { source_start: base as f64 / UNITS, source_end: (base + length) as f64 / UNITS, clip_start: offset as f64 / UNITS });
                offset += length;
            }
        }
        if landed.is_empty() {
            return Err(fail());
        }
        writer.Finalize()?;
        Ok(landed)
    }
}

/// Copy audio tracks (numbered from 0 in file order) each into its own
/// audio-only MP4, in one pass over the recording, without re-encoding. The
/// player plays them next to the muted video to give every source its own
/// volume. Files are written under a temporary name and renamed when complete.
pub fn extract_tracks(source: &Path, wanted: &[(usize, std::path::PathBuf)]) -> Result<()> {
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(source.as_os_str()), None::<&IMFAttributes>)?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        let (_, audio) = streams(&reader);
        let mut outputs = Vec::new();
        let mut tracks = Vec::new();
        for (number, target) in wanted {
            let Some((index, native)) = audio.get(*number) else { continue };
            let temporary = target.with_extension("part");
            let writer = mp4_writer(&temporary, None)?;
            reader.SetStreamSelection(*index, true)?;
            reader.SetCurrentMediaType(*index, None, native)?;
            let stream = writer.AddStream(native)?;
            writer.SetInputMediaType(stream, native, None::<&IMFAttributes>)?;
            writer.BeginWriting()?;
            tracks.push(Track { reader: *index, writer: Some(outputs.len() as u32), role: Role::Audio, done: false });
            outputs.push((writer, stream, temporary, target.clone()));
        }
        if outputs.is_empty() {
            return Err(fail());
        }
        let result = pump(&reader, &mut tracks, |track, sample, _| {
            let (writer, stream, ..) = &outputs[track.writer.unwrap_or(0) as usize];
            writer.WriteSample(*stream, sample)
        });
        for (writer, _, temporary, target) in &outputs {
            if result.is_ok() && writer.Finalize().is_ok() && std::fs::rename(temporary, target).is_ok() {
                continue;
            }
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}

/// Loudness (RMS, 0..1) of each audio track, in file order (diagnostics).
pub fn track_levels(path: &Path) -> Vec<f64> {
    let mut levels = Vec::new();
    unsafe {
        let Ok(reader) = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None::<&IMFAttributes>) else { return levels };
        let (_, audio) = streams(&reader);
        for (index, _) in audio {
            let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
            let _ = reader.SetStreamSelection(index, true);
            let _ = reader.SetCurrentPosition(&GUID::zeroed(), &position(0.0));
            let Ok(pcm) = pcm_type() else { break };
            if reader.SetCurrentMediaType(index, None, &pcm).is_err() {
                levels.push(-1.0);
                continue;
            }
            let (mut sum, mut count) = (0f64, 0u64);
            loop {
                let mut flags = 0u32;
                let mut sample = None;
                if reader.ReadSample(index, 0, None, Some(&mut flags), None, Some(&mut sample)).is_err() || flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
                if let Some(sample) = sample {
                    if let Ok(values) = pcm_of(&sample) {
                        for value in values {
                            let value = f64::from(value) / 32768.0;
                            sum += value * value;
                            count += 1;
                        }
                    }
                }
            }
            levels.push(if count > 0 { (sum / count as f64).sqrt() } else { 0.0 });
        }
    }
    levels
}

/// Loudness of every audio track (file order), `per_second` values a second,
/// each 0..255 (square root of the peak, so quiet voices still show).
pub fn peaks(path: &Path, per_second: u32) -> Result<Vec<Vec<u8>>> {
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None::<&IMFAttributes>)?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        let (_, audio) = streams(&reader);
        let mut tracks = Vec::new();
        for (index, _) in &audio {
            reader.SetStreamSelection(*index, true)?;
            reader.SetCurrentMediaType(*index, None, &pcm_type()?)?;
            tracks.push(Track { reader: *index, writer: None, role: Role::Audio, done: false });
        }
        let mut out: Vec<Vec<u8>> = vec![Vec::new(); audio.len()];
        let channels = AUDIO_CHANNELS as usize;
        let per_frame = f64::from(per_second) / f64::from(AUDIO_RATE);
        pump(&reader, &mut tracks, |track, sample, time| {
            let Some(slot) = audio.iter().position(|(index, _)| *index == track.reader) else { return Ok(()) };
            let pcm = pcm_of(sample)?;
            let first = time as f64 / UNITS * f64::from(per_second);
            let levels = &mut out[slot];
            for (frame, values) in pcm.chunks(channels).enumerate() {
                let bucket = (first + frame as f64 * per_frame) as usize;
                if levels.len() <= bucket {
                    levels.resize(bucket + 1, 0);
                }
                let peak = values.iter().map(|value| value.unsigned_abs()).max().unwrap_or(0);
                let level = ((f64::from(peak) / 32768.0).sqrt() * 255.0).min(255.0) as u8;
                levels[bucket] = levels[bucket].max(level);
            }
            Ok(())
        })?;
        Ok(out)
    }
}
