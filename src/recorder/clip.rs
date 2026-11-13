//! Cutting a part of a recording into a new MP4 with Media Foundation.
//! Fast: stream copy (no re-encoding), starting at the keyframe at or before
//! the chosen start (recordings have a keyframe every second). Exact: decode
//! and re-encode (on the GPU when possible) for a frame-exact start.

use super::capture::Gpu;
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

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Video,
    Audio,
}

struct Track {
    reader: u32,
    writer: u32,
    kind: Kind,
    done: bool,
}

fn native_tracks(reader: &IMFSourceReader) -> Vec<(u32, Kind, IMFMediaType)> {
    let mut tracks = Vec::new();
    for index in 0..8u32 {
        let Ok(native) = (unsafe { reader.GetNativeMediaType(index, 0) }) else { break };
        let Ok(major) = (unsafe { native.GetMajorType() }) else { continue };
        let kind = if major == MFMediaType_Video {
            Kind::Video
        } else if major == MFMediaType_Audio {
            Kind::Audio
        } else {
            continue;
        };
        if tracks.iter().any(|(_, existing, _)| *existing == kind) {
            continue;
        }
        tracks.push((index, kind, native));
    }
    tracks
}

/// Where a part landed, in seconds: start and end in the source, start in the clip.
#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub source_start: f64,
    pub source_end: f64,
    pub clip_start: f64,
}

/// Cut the `parts` (start, end seconds, in order) of `source` and join them
/// with plain cuts into `target`. Returns where each part really landed (a
/// fast cut starts on the keyframe at or before the chosen start).
pub fn cut(source: &Path, target: &Path, parts: &[(f64, f64)], exact: bool) -> Result<Vec<Part>> {
    let result = if exact { reencode(source, target, parts, true, None).or_else(|_| reencode(source, target, parts, false, None)) } else { copy(source, target, parts) };
    if result.is_err() {
        let _ = std::fs::remove_file(target);
    }
    result
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

/// AAC 96 kbps for shared clips.
const SHARE_AUDIO_BYTES: u32 = 12_000;

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
pub fn cut_for_size(source: &Path, target: &Path, parts: &[(f64, f64)], max_bytes: u64) -> Result<Vec<Part>> {
    let length: f64 = parts.iter().map(|(start, end)| (end - start).max(0.0)).sum::<f64>().max(1.0);
    // Room for the MP4 index and the encoder overshooting its target.
    let mut budget = max_bytes as f64 * 8.0 * 0.9 / length;
    let mut last = Err(fail());
    for _ in 0..3 {
        let shrink = shrink_for(budget);
        last = reencode(source, target, parts, true, Some(shrink)).or_else(|_| reencode(source, target, parts, false, Some(shrink)));
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

/// Every track selected again for the next part.
unsafe fn select_all(reader: &IMFSourceReader, tracks: &mut [Track]) -> Result<()> {
    for track in tracks.iter_mut() {
        track.done = false;
        unsafe { reader.SetStreamSelection(track.reader, true)? };
    }
    Ok(())
}

const UNITS: f64 = 10_000_000.0;

/// Stream copy, each part from the keyframe at or before its start.
fn copy(source: &Path, target: &Path, parts: &[(f64, f64)]) -> Result<Vec<Part>> {
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(source.as_os_str()), None::<&IMFAttributes>)?;
        let writer = mp4_writer(target, None)?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        let mut tracks = Vec::new();
        for (index, kind, native) in native_tracks(&reader) {
            reader.SetStreamSelection(index, true)?;
            reader.SetCurrentMediaType(index, None, &native)?;
            let stream = writer.AddStream(&native)?;
            writer.SetInputMediaType(stream, &native, None::<&IMFAttributes>)?;
            tracks.push(Track { reader: index, writer: stream, kind, done: false });
        }
        if !tracks.iter().any(|track| track.kind == Kind::Video) {
            return Err(fail());
        }
        writer.BeginWriting()?;
        let mut offset = 0i64;
        let mut landed = Vec::new();
        for &(start, end) in parts {
            select_all(&reader, &mut tracks)?;
            reader.SetCurrentPosition(&GUID::zeroed(), &position(start))?;
            let end = (end * UNITS) as i64;
            let mut base: Option<i64> = None;
            let mut length = 0i64;
            pump(&reader, &mut tracks, |track, sample, time| {
                match track.kind {
                    Kind::Video => {
                        if base.is_none() {
                            // Each part must open on a keyframe.
                            if sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 0 {
                                return Ok(false);
                            }
                            base = Some(time);
                        }
                    }
                    Kind::Audio if base.is_none_or(|base| time < base) => return Ok(false),
                    Kind::Audio => {}
                }
                if time > end {
                    track.done = true;
                    return Ok(false);
                }
                let relative = time - base.unwrap_or(time);
                length = length.max(relative + sample.GetSampleDuration().unwrap_or(0));
                sample.SetSampleTime(offset + relative)?;
                writer.WriteSample(track.writer, sample)?;
                Ok(true)
            })?;
            if let Some(base) = base {
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

/// Read samples until every track is past the end. `handle` returns whether
/// the sample was used; it marks a track done when it is past the end.
unsafe fn pump(reader: &IMFSourceReader, tracks: &mut [Track], mut handle: impl FnMut(&mut Track, &IMFSample, i64) -> Result<bool>) -> Result<()> {
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

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Decode and encode again: every part starts on the exact frame.
fn reencode(source: &Path, target: &Path, parts: &[(f64, f64)], hardware: bool, shrink: Option<Shrink>) -> Result<Vec<Part>> {
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
        if shrink.is_some() {
            // Lets the reader scale the picture and change the frame rate.
            options.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        }
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(source.as_os_str()), &options)?;
        let writer = mp4_writer(target, manager.as_ref())?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        let mut tracks = Vec::new();
        for (index, kind, native) in native_tracks(&reader) {
            reader.SetStreamSelection(index, true)?;
            let output = MFCreateMediaType()?;
            match kind {
                Kind::Video => {
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
                    output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
                    output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
                    output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                    output.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
                    output.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
                    output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
                    output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
                }
                Kind::Audio => {
                    let decoded = MFCreateMediaType()?;
                    decoded.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    decoded.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                    decoded.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    reader.SetCurrentMediaType(index, None, &decoded)?;
                    let current = reader.GetCurrentMediaType(index)?;
                    output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    output.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                    output.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    output.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, current.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?)?;
                    output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, current.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?)?;
                    output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, if shrink.is_some() { SHARE_AUDIO_BYTES } else { 20_000 })?;
                }
            }
            let stream = writer.AddStream(&output)?;
            let input = reader.GetCurrentMediaType(index)?;
            if kind == Kind::Video && shrink.is_some() {
                // Constant bitrate keeps the size predictable.
                let parameters = attributes(1)?;
                parameters.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32)?;
                if writer.SetInputMediaType(stream, &input, &parameters).is_err() {
                    writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>)?;
                }
            } else {
                writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>)?;
            }
            tracks.push(Track { reader: index, writer: stream, kind, done: false });
        }
        if !tracks.iter().any(|track| track.kind == Kind::Video) {
            return Err(fail());
        }
        writer.BeginWriting()?;
        let mut offset = 0i64;
        let mut landed = Vec::new();
        for &(start, end) in parts {
            select_all(&reader, &mut tracks)?;
            reader.SetCurrentPosition(&GUID::zeroed(), &position(start))?;
            let start = (start * UNITS) as i64;
            let end = (end * UNITS) as i64;
            let mut length = 0i64;
            let mut wrote = false;
            pump(&reader, &mut tracks, |track, sample, time| {
                if time < start {
                    return Ok(false);
                }
                if time > end {
                    track.done = true;
                    return Ok(false);
                }
                let relative = time - start;
                length = length.max(relative + sample.GetSampleDuration().unwrap_or(0));
                sample.SetSampleTime(offset + relative)?;
                writer.WriteSample(track.writer, sample)?;
                wrote = true;
                Ok(true)
            })?;
            if wrote {
                landed.push(Part { source_start: start as f64 / UNITS, source_end: (start + length) as f64 / UNITS, clip_start: offset as f64 / UNITS });
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
