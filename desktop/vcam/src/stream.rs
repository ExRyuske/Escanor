//! Видеопоток камеры (IMFMediaStream2): на каждый запрос отдаёт самый свежий кадр
//! из общей памяти, масштабированный под выбранный потребителем формат.

use crate::log::{Live, vlog};
use crate::scale::{Nv12Target, scale_nv12};
use escanor_shm::FrameInfo;
use escanor_shm::windows::Mapping;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{ERROR_SET_NOT_FOUND, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer2, IMFAsyncCallback, IMFAsyncResult, IMFAttributes, IMFMediaEvent, IMFMediaEventGenerator_Impl,
    IMFMediaEventQueue, IMFMediaSource, IMFMediaStream_Impl, IMFMediaStream2, IMFMediaStream2_Impl, IMFMediaType,
    IMFStreamDescriptor, IMFVideoSampleAllocatorEx, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MEMediaSample,
    MEStreamStarted, MEStreamStopped, MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MF_DEVICESTREAM_FRAMESERVER_SHARED,
    MF_DEVICESTREAM_STREAM_CATEGORY, MF_DEVICESTREAM_STREAM_ID, MF_E_INVALIDREQUEST, MF_E_SHUTDOWN,
    MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE,
    MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_STREAM_STATE, MF_STREAM_STATE_RUNNING, MF_STREAM_STATE_STOPPED,
    MF2DBuffer_LockFlags_Write, MFCreateAttributes, MFCreateEventQueue, MFCreateMediaType, MFCreateStreamDescriptor,
    MFCreateVideoSampleAllocatorEx, MFFrameSourceTypes_Color, MFGetSystemTime, MFMediaType_Video,
    MFNominalRange_16_235, MFSampleExtension_Token, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    MFVideoTransferMatrix_BT709,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, HRESULT, IUnknown, Interface, Ref, Result, implement};

/// Форматы, которые видят приложения. Первый — формат по умолчанию.
const FORMATS: [(u32, u32, u32); 5] =
    [(1920, 1080, 30), (1920, 1080, 60), (1280, 720, 30), (1280, 720, 60), (640, 360, 30)];
const ALLOCATOR_SAMPLES: u32 = 10;
const RETRY_OPEN: Duration = Duration::from_millis(500);

#[implement(IMFMediaStream2, IKsControl)]
pub struct MediaStream {
    attributes: IMFAttributes,
    descriptor: IMFStreamDescriptor,
    state: Mutex<State>,
    _live: Live,
}

struct State {
    queue: Option<IMFMediaEventQueue>,
    source: Option<IMFMediaSource>,
    allocator: Option<IMFVideoSampleAllocatorEx>,
    allocator_ready: bool,
    stream_state: MF_STREAM_STATE,
    /// Ширина, высота и частота кадров текущего формата.
    format: Option<(u32, u32, u32, u32)>,
    frames: Frames,
}

impl MediaStream {
    pub fn new(index: u32, source: IMFMediaSource) -> Result<Self> {
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 8)?;
            let attributes: IMFAttributes = attributes.unwrap();
            attributes.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            attributes.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, index)?;
            attributes.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            attributes.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color.0 as u32)?;

            let types =
                FORMATS.iter().map(|&(w, h, fps)| media_type(w, h, fps).map(Some)).collect::<Result<Vec<_>>>()?;
            let descriptor = MFCreateStreamDescriptor(index, &types)?;
            descriptor.GetMediaTypeHandler()?.SetCurrentMediaType(types[0].as_ref())?;
            attributes.CopyAllItems(&descriptor)?;

            Ok(Self {
                attributes,
                descriptor,
                state: Mutex::new(State {
                    queue: Some(MFCreateEventQueue()?),
                    source: Some(source),
                    allocator: None,
                    allocator_ready: false,
                    stream_state: MF_STREAM_STATE_STOPPED,
                    format: None,
                    frames: Frames::default(),
                }),
                _live: Live::new(),
            })
        }
    }

    pub fn descriptor(&self) -> &IMFStreamDescriptor {
        &self.descriptor
    }

    pub fn attributes(&self) -> &IMFAttributes {
        &self.attributes
    }

    pub fn is_active(&self) -> bool {
        self.state.lock().unwrap().stream_state == MF_STREAM_STATE_RUNNING
    }

    pub fn set_allocator(&self, allocator: IMFVideoSampleAllocatorEx) {
        let mut state = self.state.lock().unwrap();
        state.allocator = Some(allocator);
        state.allocator_ready = false;
    }

    pub fn start(&self, media_type: &IMFMediaType) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let queue = state.queue.clone().ok_or(MF_E_SHUTDOWN)?;
        let (width, height) = split_u64(unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE)? });
        let (fps_num, fps_den) = split_u64(unsafe { media_type.GetUINT64(&MF_MT_FRAME_RATE)? });

        let allocator = match &state.allocator {
            Some(a) => a.clone(),
            None => {
                let mut raw = std::ptr::null_mut();
                unsafe { MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)? };
                let a = unsafe { IMFVideoSampleAllocatorEx::from_raw(raw) };
                state.allocator = Some(a.clone());
                a
            }
        };
        unsafe {
            if state.allocator_ready {
                allocator.UninitializeSampleAllocator()?;
            }
            allocator.InitializeSampleAllocator(ALLOCATOR_SAMPLES, media_type)?;
        }
        state.allocator_ready = true;
        state.format = Some((width, height, fps_num.max(1), fps_den.max(1)));
        state.stream_state = MF_STREAM_STATE_RUNNING;
        state.frames.set_consumer(Some((width, height)));
        vlog!("видео запущено: {width}×{height} @ {}/{}", fps_num, fps_den);
        unsafe { queue.QueueEventParamVar(MEStreamStarted.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null()) }
    }

    pub fn stop(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let queue = state.queue.clone().ok_or(MF_E_SHUTDOWN)?;
        state.stream_state = MF_STREAM_STATE_STOPPED;
        state.frames.set_consumer(None);
        vlog!("видео остановлено");
        unsafe { queue.QueueEventParamVar(MEStreamStopped.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null()) }
    }

    pub fn shutdown(&self) {
        let mut state = self.state.lock().unwrap();
        state.stream_state = MF_STREAM_STATE_STOPPED;
        state.frames.set_consumer(None);
        state.source = None;
        if let Some(allocator) = state.allocator.take() {
            unsafe {
                let _ = allocator.UninitializeSampleAllocator();
            }
        }
        if let Some(queue) = state.queue.take() {
            unsafe {
                let _ = queue.Shutdown();
            }
        }
    }

    fn queue(&self) -> Result<IMFMediaEventQueue> {
        self.state.lock().unwrap().queue.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let queue = self.queue()?;
        unsafe { queue.GetEvent(flags.0) }
    }

    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        let queue = self.queue()?;
        unsafe { queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }

    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        let queue = self.queue()?;
        unsafe { queue.EndGetEvent(result.as_ref()) }
    }

    fn QueueEvent(&self, met: u32, extended: *const GUID, status: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        let queue = self.queue()?;
        unsafe { queue.QueueEventParamVar(met, extended, status, value) }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        self.state.lock().unwrap().source.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        self.queue()?;
        Ok(self.descriptor.clone())
    }

    fn RequestSample(&self, token: Ref<IUnknown>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let queue = state.queue.clone().ok_or(MF_E_SHUTDOWN)?;
        if state.stream_state != MF_STREAM_STATE_RUNNING {
            return Err(MF_E_INVALIDREQUEST.into());
        }
        let (width, height, fps_num, fps_den) = state.format.ok_or(MF_E_INVALIDREQUEST)?;
        let allocator = state.allocator.clone().ok_or(MF_E_INVALIDREQUEST)?;

        unsafe {
            let sample = allocator.AllocateSample()?;
            sample.SetSampleTime(MFGetSystemTime())?;
            sample.SetSampleDuration(10_000_000 * fps_den as i64 / fps_num as i64)?;

            let buffer: IMF2DBuffer2 = sample.GetBufferByIndex(0)?.cast()?;
            let (mut scanline0, mut pitch, mut start, mut length) =
                (std::ptr::null_mut(), 0i32, std::ptr::null_mut(), 0u32);
            buffer.Lock2DSize(MF2DBuffer_LockFlags_Write, &mut scanline0, &mut pitch, &mut start, &mut length)?;
            let needed = pitch.unsigned_abs() as usize * height as usize * 3 / 2;
            let available = length as usize - (scanline0 as usize - start as usize);
            if pitch > 0 && needed <= available {
                let data = std::slice::from_raw_parts_mut(scanline0, needed);
                let mut target =
                    Nv12Target { data, pitch: pitch as usize, width: width as usize, height: height as usize };
                state.frames.render(&mut target);
            }
            buffer.Unlock2D()?;

            if let Some(token) = token.as_ref() {
                sample.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample)
        }
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.queue.is_none() {
            return Err(MF_E_SHUTDOWN.into());
        }
        state.stream_state = value;
        Ok(())
    }

    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        Ok(self.state.lock().unwrap().stream_state)
    }
}

impl IKsControl_Impl for MediaStream_Impl {
    fn KsProperty(&self, _: *const KSIDENTIFIER, _: u32, _: *mut c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(HRESULT::from_win32(ERROR_SET_NOT_FOUND.0).into())
    }
    fn KsMethod(&self, _: *const KSIDENTIFIER, _: u32, _: *mut c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(HRESULT::from_win32(ERROR_SET_NOT_FOUND.0).into())
    }
    fn KsEvent(&self, _: *const KSIDENTIFIER, _: u32, _: *mut c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(HRESULT::from_win32(ERROR_SET_NOT_FOUND.0).into())
    }
}

/// Источник кадров: общая память с приложением Escanor.
#[derive(Default)]
struct Frames {
    mapping: Option<Mapping>,
    last_attempt: Option<Instant>,
    buffer: Vec<u8>,
    last: Option<FrameInfo>,
    /// Разрешение, которое мы объявили в общей памяти как «используется».
    registered: Option<(u32, u32)>,
    wanted: Option<(u32, u32)>,
}

impl Frames {
    fn ensure_mapping(&mut self) {
        if self.mapping.is_none() && self.last_attempt.is_none_or(|t| t.elapsed() >= RETRY_OPEN) {
            let first = self.last_attempt.is_none();
            self.last_attempt = Some(Instant::now());
            self.mapping = match Mapping::create_or_open() {
                Ok(m) if m.frames().is_compatible() => {
                    vlog!("общая память с приложением открыта");
                    Some(m)
                }
                Ok(_) => {
                    vlog!("общая память другой версии — обновите Escanor");
                    None
                }
                Err(e) => {
                    if first {
                        vlog!("общая память недоступна: {e}");
                    }
                    None
                }
            };
        }
        if let Some(mapping) = &self.mapping
            && self.registered != self.wanted
        {
            if self.registered.is_some() {
                mapping.frames().set_consumer(false, 0, 0);
            }
            if let Some((w, h)) = self.wanted {
                mapping.frames().set_consumer(true, w, h);
            }
            self.registered = self.wanted;
        }
    }

    fn set_consumer(&mut self, wanted: Option<(u32, u32)>) {
        self.wanted = wanted;
        self.ensure_mapping();
    }

    fn render(&mut self, target: &mut Nv12Target) {
        self.ensure_mapping();
        let Some(mapping) = &self.mapping else {
            target.fill_placeholder();
            return;
        };
        let frames = mapping.frames();
        let after = self.last.map_or(0, |f| f.counter);
        if let Some(info) = frames.read_latest(after, &mut self.buffer) {
            self.last = Some(info);
        }
        match self.last {
            Some(info) if !frames.is_stale() => {
                scale_nv12(&self.buffer, info.width as usize, info.height as usize, target)
            }
            _ => target.fill_placeholder(),
        }
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        if let (Some(mapping), Some(_)) = (&self.mapping, self.registered) {
            mapping.frames().set_consumer(false, 0, 0);
        }
    }
}

fn media_type(width: u32, height: u32, fps: u32) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, join_u64(width, height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, join_u64(fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, join_u64(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, width * height * 3 / 2)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, width * height * 12 * fps)?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(t)
    }
}

fn join_u64(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn split_u64(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}
