//! H.264 video encoding with what Windows already has: the GPU's video
//! processor converts and scales captured BGRA frames to NV12, then a Media
//! Foundation sink writer encodes them into an MP4. The hardware path hands the
//! GPU textures straight to the driver's encoder (NVIDIA NVENC, Intel QuickSync,
//! AMD AMF), so frames never come back to the CPU. The software path (Microsoft's
//! H.264 encoder) reads the NV12 frame back once per frame.

use super::capture::Gpu;
use std::mem::ManuallyDrop;
use std::path::Path;
use windows::core::{Interface, Result, GUID, HSTRING};
use windows::Win32::Foundation::{E_FAIL, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Media::MediaFoundation::*;

#[derive(Clone, Debug)]
pub struct EncoderSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Bits per second.
    pub bitrate: u32,
    /// Try the GPU's encoder first.
    pub hardware: bool,
    /// Fragmented MP4: the file stays playable if the app or PC stops mid-game.
    pub fragmented: bool,
    /// Add an AAC track (48 kHz stereo, fed with `write_audio`).
    pub audio: bool,
}

/// Audio format of `write_audio`: 16-bit stereo PCM at 48 kHz.
pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u32 = 2;

/// NV12 frames in flight: the encoder may still read one while the next is drawn.
const RING: usize = 8;

struct Slot {
    texture: ID3D11Texture2D,
    view: Option<ID3D11VideoProcessorOutputView>,
}

struct Input {
    texture: ID3D11Texture2D,
    size: (u32, u32),
    processor: ID3D11VideoProcessor,
    view: ID3D11VideoProcessorInputView,
}

pub struct VideoEncoder {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    writer: IMFSinkWriter,
    stream: u32,
    audio_stream: Option<u32>,
    _manager: Option<IMFDXGIDeviceManager>,
    settings: EncoderSettings,
    input: Option<Input>,
    ring: Vec<Slot>,
    next: usize,
    staging: Option<ID3D11Texture2D>,
    /// The encoder Media Foundation picked, e.g. "NVIDIA H.264 Encoder MFT".
    pub name: String,
    pub hardware: bool,
}

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

fn fail() -> windows::core::Error {
    windows::core::Error::from(E_FAIL)
}

fn attributes(size: u32) -> Result<IMFAttributes> {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, size)? };
    attributes.ok_or_else(fail)
}

fn texture(device: &ID3D11Device, width: u32, height: u32, format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT, bind: u32, staging: bool) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: if staging { D3D11_USAGE_STAGING } else { D3D11_USAGE_DEFAULT },
        BindFlags: bind,
        CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
    texture.ok_or_else(fail)
}

/// Where a `content` picture goes inside the output, keeping its shape (black bars).
fn fit(content: (u32, u32), output: (u32, u32)) -> RECT {
    let (cw, ch) = (content.0.max(1) as u64, content.1.max(1) as u64);
    let (ow, oh) = (output.0 as u64, output.1 as u64);
    let (w, h) = if cw * oh > ch * ow { (ow, (ch * ow / cw) & !1) } else { ((cw * oh / ch) & !1, oh) };
    let x = (ow - w) / 2;
    let y = (oh - h) / 2;
    RECT { left: x as i32, top: y as i32, right: (x + w) as i32, bottom: (y + h) as i32 }
}

impl VideoEncoder {
    /// Hardware encoder when asked and available, otherwise software.
    pub fn new(gpu: &Gpu, path: &Path, settings: EncoderSettings) -> Result<Self> {
        if settings.hardware {
            if let Ok(encoder) = Self::create(gpu, path, &settings, true) {
                if encoder.hardware {
                    return Ok(encoder);
                }
                // Media Foundation fell back to software on its own: use the CPU path properly.
                drop(encoder);
            }
            let _ = std::fs::remove_file(path);
        }
        Self::create(gpu, path, &settings, false)
    }

    fn create(gpu: &Gpu, path: &Path, settings: &EncoderSettings, hardware: bool) -> Result<Self> {
        let (width, height, fps) = (settings.width, settings.height, settings.fps.max(1));
        let writer_attributes = attributes(4)?;
        let manager = unsafe {
            writer_attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            let container = if settings.fragmented { MFTranscodeContainerType_FMPEG4 } else { MFTranscodeContainerType_MPEG4 };
            writer_attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &container)?;
            if hardware {
                let mut token = 0;
                let mut manager = None;
                MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
                let manager = manager.ok_or_else(fail)?;
                manager.ResetDevice(&gpu.device, token)?;
                writer_attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
                writer_attributes.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
                Some(manager)
            } else {
                writer_attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 0)?;
                None
            }
        };
        let writer = unsafe { MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None::<&IMFByteStream>, &writer_attributes)? };

        let stream = unsafe {
            let output = MFCreateMediaType()?;
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate)?;
            output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            output.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            output.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            output.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            output.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            let stream = writer.AddStream(&output)?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
            input.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            input.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;

            // Constant bitrate (predictable file size) and a keyframe every second (fast clip cuts).
            let parameters = attributes(2)?;
            parameters.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32)?;
            parameters.SetUINT32(&CODECAPI_AVEncMPVGOPSize, fps)?;
            if writer.SetInputMediaType(stream, &input, &parameters).is_err() {
                writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>)?;
            }
            stream
        };
        let audio_stream = if settings.audio { Some(unsafe { add_audio(&writer)? }) } else { None };
        unsafe { writer.BeginWriting()? };
        let (name, uses_hardware) = encoder_info(&writer, stream);

        let video_device: ID3D11VideoDevice = gpu.device.cast()?;
        let video_context: ID3D11VideoContext = gpu.context.cast()?;
        let mut ring = Vec::with_capacity(RING);
        for _ in 0..RING {
            ring.push(Slot { texture: texture(&gpu.device, width, height, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET.0 as u32, false)?, view: None });
        }
        let staging = if hardware { None } else { Some(texture(&gpu.device, width, height, DXGI_FORMAT_NV12, 0, true)?) };
        Ok(Self {
            device: gpu.device.clone(),
            context: gpu.context.clone(),
            video_device,
            video_context,
            writer,
            stream,
            audio_stream,
            _manager: manager,
            settings: settings.clone(),
            input: None,
            ring,
            next: 0,
            staging,
            name,
            hardware: hardware && uses_hardware,
        })
    }

    pub fn settings(&self) -> &EncoderSettings {
        &self.settings
    }

    /// Keep a copy of the game's picture (GPU to GPU): `region` of `source`.
    pub fn set_input(&mut self, source: &ID3D11Texture2D, region: RECT) -> Result<()> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        let left = region.left.clamp(0, desc.Width as i32) as u32;
        let top = region.top.clamp(0, desc.Height as i32) as u32;
        let right = (region.right.max(0) as u32).clamp(left, desc.Width);
        let bottom = (region.bottom.max(0) as u32).clamp(top, desc.Height);
        let size = (right - left, bottom - top);
        if size.0 < 2 || size.1 < 2 {
            return Ok(());
        }
        if self.input.as_ref().map(|input| input.size) != Some(size) {
            self.input = None;
            self.input = Some(self.build_input(size)?);
        }
        let input = self.input.as_ref().ok_or_else(fail)?;
        let area = D3D11_BOX { left, top, front: 0, right, bottom, back: 1 };
        unsafe { self.context.CopySubresourceRegion(&input.texture, 0, 0, 0, 0, source, 0, Some(&area)) };
        Ok(())
    }

    pub fn has_input(&self) -> bool {
        self.input.is_some()
    }

    fn build_input(&mut self, size: (u32, u32)) -> Result<Input> {
        let (width, height, fps) = (self.settings.width, self.settings.height, self.settings.fps.max(1));
        let rate = DXGI_RATIONAL { Numerator: fps, Denominator: 1 };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: size.0,
            InputHeight: size.1,
            OutputFrameRate: rate,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let bind = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
        let source = texture(&self.device, size.0, size.1, DXGI_FORMAT_B8G8R8A8_UNORM, bind, false)?;
        unsafe {
            let enumerator = self.video_device.CreateVideoProcessorEnumerator(&desc)?;
            let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;
            let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
            };
            let mut view = None;
            self.video_device.CreateVideoProcessorInputView(&source, &enumerator, &input_desc, Some(&mut view))?;
            let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            for slot in &mut self.ring {
                let mut output = None;
                self.video_device.CreateVideoProcessorOutputView(&slot.texture, &enumerator, &output_desc, Some(&mut output))?;
                slot.view = output;
            }
            let context = &self.video_context;
            context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            if let Ok(context1) = context.cast::<ID3D11VideoContext1>() {
                context1.VideoProcessorSetStreamColorSpace1(&processor, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                context1.VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            }
            let whole = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&whole));
            let black = D3D11_VIDEO_COLOR { Anonymous: D3D11_VIDEO_COLOR_0 { YCbCr: D3D11_VIDEO_COLOR_YCbCrA { Y: 16.0 / 255.0, Cb: 0.5, Cr: 0.5, A: 1.0 } } };
            context.VideoProcessorSetOutputBackgroundColor(&processor, true, &black);
            Ok(Input { texture: source, size, processor, view: view.ok_or_else(fail)? })
        }
    }

    /// Encode the newest picture at `time` (100 ns units from the start).
    /// Called once per frame tick; an unchanged picture is simply encoded again.
    pub fn write_frame(&mut self, time: i64) -> Result<()> {
        let Some(input) = self.input.as_ref() else { return Ok(()) };
        let output = (self.settings.width, self.settings.height);
        let slot = &self.ring[self.next];
        self.next = (self.next + 1) % self.ring.len();
        let view = slot.view.as_ref().ok_or_else(fail)?;
        unsafe {
            let source = RECT { left: 0, top: 0, right: input.size.0 as i32, bottom: input.size.1 as i32 };
            let target = fit(input.size, output);
            self.video_context.VideoProcessorSetStreamSourceRect(&input.processor, 0, true, Some(&source));
            self.video_context.VideoProcessorSetStreamDestRect(&input.processor, 0, true, Some(&target));
            let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(input.view.clone())),
                ..Default::default()
            }];
            let result = self.video_context.VideoProcessorBlt(&input.processor, view, 0, &streams);
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            result?;

            let buffer = match &self.staging {
                None => {
                    let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &slot.texture, 0, false)?;
                    let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
                    buffer.SetCurrentLength(length)?;
                    buffer
                }
                Some(staging) => self.read_back(staging, &slot.texture)?,
            };
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(10_000_000 / i64::from(self.settings.fps.max(1)))?;
            self.writer.WriteSample(self.stream, &sample)?;
        }
        Ok(())
    }

    /// Software path: copy the NV12 frame into system memory for the CPU encoder.
    unsafe fn read_back(&self, staging: &ID3D11Texture2D, frame: &ID3D11Texture2D) -> Result<IMFMediaBuffer> {
        let (width, height) = (self.settings.width as usize, self.settings.height as usize);
        let size = width * height * 3 / 2;
        unsafe {
            self.context.CopyResource(staging, frame);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let buffer = MFCreateMemoryBuffer(size as u32)?;
            let mut target = std::ptr::null_mut();
            let locked = buffer.Lock(&mut target, None, None);
            if locked.is_ok() {
                let pitch = mapped.RowPitch as usize;
                let source = mapped.pData as *const u8;
                for row in 0..height {
                    std::ptr::copy_nonoverlapping(source.add(row * pitch), target.add(row * width), width);
                }
                let chroma = source.add(pitch * height);
                for row in 0..height / 2 {
                    std::ptr::copy_nonoverlapping(chroma.add(row * pitch), target.add(width * height + row * width), width);
                }
                let _ = buffer.Unlock();
            }
            self.context.Unmap(staging, 0);
            locked?;
            buffer.SetCurrentLength(size as u32)?;
            Ok(buffer)
        }
    }

    /// Write interleaved 16-bit stereo samples starting at `time` (100 ns units).
    pub fn write_audio(&mut self, samples: &[i16], time: i64) -> Result<()> {
        let Some(stream) = self.audio_stream else { return Ok(()) };
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
            sample.SetSampleTime(time)?;
            let frames = (samples.len() / AUDIO_CHANNELS as usize) as i64;
            sample.SetSampleDuration(frames * 10_000_000 / i64::from(AUDIO_RATE))?;
            self.writer.WriteSample(stream, &sample)?;
        }
        Ok(())
    }

    pub fn has_audio(&self) -> bool {
        self.audio_stream.is_some()
    }

    /// Finish the file (writes the index so players can seek).
    pub fn finish(self) -> Result<()> {
        unsafe { self.writer.Finalize() }
    }
}

/// AAC 160 kbps from 16-bit stereo PCM at 48 kHz (Windows' own AAC encoder).
unsafe fn add_audio(writer: &IMFSinkWriter) -> Result<u32> {
    unsafe {
        let output = MFCreateMediaType()?;
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        output.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
        output.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        output.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
        output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)?;
        output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 20_000)?;
        let stream = writer.AddStream(&output)?;
        let input = MFCreateMediaType()?;
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
        input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)?;
        input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, AUDIO_CHANNELS * 2)?;
        input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_RATE * AUDIO_CHANNELS * 2)?;
        writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>)?;
        Ok(stream)
    }
}

/// The encoder the sink writer loaded, and whether it is a hardware one.
fn encoder_info(writer: &IMFSinkWriter, stream: u32) -> (String, bool) {
    let Ok(writer) = writer.cast::<IMFSinkWriterEx>() else { return (String::new(), false) };
    for index in 0..8 {
        let mut category = GUID::zeroed();
        let mut transform = None;
        if unsafe { writer.GetTransformForStream(stream, index, Some(&mut category), &mut transform) }.is_err() {
            break;
        }
        if category != MFT_CATEGORY_VIDEO_ENCODER {
            continue;
        }
        let Some(attributes) = transform.and_then(|transform| unsafe { transform.GetAttributes() }.ok()) else { continue };
        let hardware = unsafe { attributes.GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute) }.is_ok();
        let name = unsafe { attributes.GetStringLength(&MFT_FRIENDLY_NAME_Attribute) }
            .ok()
            .and_then(|length| {
                let mut text = vec![0u16; length as usize + 1];
                unsafe { attributes.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut text, None) }.ok()?;
                Some(String::from_utf16_lossy(&text[..length as usize]))
            })
            .unwrap_or_else(|| if hardware { "Hardware H.264".into() } else { "Microsoft H.264 (software)".into() });
        return (name, hardware);
    }
    (String::new(), false)
}
