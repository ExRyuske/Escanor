//! Медиаисточник камеры (IMFMediaSourceEx) с одним видеопотоком.

use crate::log::{Live, vlog};
use crate::stream::MediaStream;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, ERROR_SET_NOT_FOUND, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncResult, IMFAttributes, IMFGetService, IMFGetService_Impl, IMFMediaEvent,
    IMFMediaEventGenerator_Impl, IMFMediaEventQueue, IMFMediaSource_Impl, IMFMediaSourceEx, IMFMediaSourceEx_Impl,
    IMFPresentationDescriptor, IMFSampleAllocatorControl, IMFSampleAllocatorControl_Impl, IMFStreamDescriptor,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MENewStream, MESourceStarted, MESourceStopped, MEUpdatedStream,
    MF_E_INVALID_STATE_TRANSITION, MF_E_INVALIDREQUEST, MF_E_SHUTDOWN, MF_E_UNSUPPORTED_SERVICE, MFCreateAttributes,
    MFCreateEventQueue, MFCreatePresentationDescriptor, MFMEDIASOURCE_IS_LIVE, MFSampleAllocatorUsage,
    MFSampleAllocatorUsage_UsesProvidedAllocator,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{BOOL, ComObject, GUID, HRESULT, IUnknown, Interface, Ref, Result, implement};

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl, IMFSampleAllocatorControl)]
pub struct MediaSource {
    state: Mutex<State>,
    _live: Live,
}

struct State {
    queue: Option<IMFMediaEventQueue>,
    attributes: IMFAttributes,
    descriptor: Option<IMFPresentationDescriptor>,
    streams: Vec<ComObject<MediaStream>>,
}

impl MediaSource {
    pub fn create(activator_attributes: &IMFAttributes) -> Result<IMFMediaSourceEx> {
        let mut attributes = None;
        unsafe { MFCreateAttributes(&mut attributes, 8)? };
        let attributes = attributes.unwrap();
        unsafe { activator_attributes.CopyAllItems(&attributes)? };

        let source = ComObject::new(MediaSource {
            state: Mutex::new(State {
                queue: Some(unsafe { MFCreateEventQueue()? }),
                attributes,
                descriptor: None,
                streams: Vec::new(),
            }),
            _live: Live::new(),
        });

        // Поток ссылается на источник (GetMediaSource), поэтому создаём его после источника.
        let stream = ComObject::new(MediaStream::new(0, source.to_interface::<IMFMediaSourceEx>().into())?);
        let descriptors = [Some(stream.descriptor().clone())];
        let descriptor = unsafe { MFCreatePresentationDescriptor(Some(&descriptors))? };
        unsafe { descriptor.SelectStream(0)? };
        {
            let mut state = source.state.lock().unwrap();
            state.descriptor = Some(descriptor);
            state.streams.push(stream);
        }
        Ok(source.into_interface())
    }

    fn queue(&self) -> Result<IMFMediaEventQueue> {
        self.state.lock().unwrap().queue.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn stream(&self, id: u32) -> Result<ComObject<MediaStream>> {
        let state = self.state.lock().unwrap();
        if state.queue.is_none() {
            return Err(MF_E_SHUTDOWN.into());
        }
        state.streams.get(id as usize).cloned().ok_or_else(|| MF_E_INVALIDREQUEST.into())
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        // Очередь берём вне блокировки: GetEvent может ждать событие сколь угодно долго.
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

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        self.queue()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        let state = self.state.lock().unwrap();
        let descriptor = state.descriptor.as_ref().filter(|_| state.queue.is_some()).ok_or(MF_E_SHUTDOWN)?;
        unsafe { descriptor.Clone() }
    }

    fn Start(
        &self,
        requested: Ref<IMFPresentationDescriptor>,
        _time_format: *const GUID,
        start_position: *const PROPVARIANT,
    ) -> Result<()> {
        let requested = requested.ok().map_err(|_| windows::core::Error::from(E_INVALIDARG))?;
        if start_position.is_null() {
            return Err(E_POINTER.into());
        }
        let (queue, own) = {
            let state = self.state.lock().unwrap();
            (state.queue.clone().ok_or(MF_E_SHUTDOWN)?, state.descriptor.clone().ok_or(MF_E_SHUTDOWN)?)
        };

        let count = unsafe { requested.GetStreamDescriptorCount()? };
        for index in 0..count {
            let mut selected = BOOL::default();
            let mut descriptor: Option<IMFStreamDescriptor> = None;
            unsafe { requested.GetStreamDescriptorByIndex(index, &mut selected, &mut descriptor)? };
            let descriptor = descriptor.ok_or(E_POINTER)?;
            let id = unsafe { descriptor.GetStreamIdentifier()? };
            let stream = self.stream(id)?;
            let was_active = stream.is_active();

            if selected.as_bool() {
                unsafe { own.SelectStream(index)? };
                let media_type = unsafe { descriptor.GetMediaTypeHandler()?.GetCurrentMediaType()? };
                let unknown: IUnknown = stream.to_interface();
                let event = if was_active { MEUpdatedStream } else { MENewStream };
                unsafe { queue.QueueEventParamUnk(event.0 as u32, &GUID::zeroed(), S_OK, &unknown)? };
                stream.start(&media_type).inspect_err(|e| vlog!("поток не запущен: {e}"))?;
            } else if was_active {
                unsafe { own.DeselectStream(index)? };
                stream.stop()?;
            }
        }
        unsafe { queue.QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, start_position) }
    }

    fn Stop(&self) -> Result<()> {
        let (queue, streams) = {
            let state = self.state.lock().unwrap();
            (state.queue.clone().ok_or(MF_E_SHUTDOWN)?, state.streams.clone())
        };
        for stream in streams.iter().filter(|s| s.is_active()) {
            stream.stop()?;
        }
        unsafe { queue.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null()) }
    }

    fn Pause(&self) -> Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> Result<()> {
        vlog!("источник закрыт");
        let (queue, streams) = {
            let mut state = self.state.lock().unwrap();
            (state.queue.take(), std::mem::take(&mut state.streams))
        };
        for stream in &streams {
            stream.shutdown();
        }
        if let Some(queue) = queue {
            unsafe { queue.Shutdown()? };
        }
        Ok(())
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        let state = self.state.lock().unwrap();
        if state.queue.is_none() {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(state.attributes.clone())
    }

    fn GetStreamAttributes(&self, id: u32) -> Result<IMFAttributes> {
        Ok(self.stream(id)?.attributes().clone())
    }

    /// Кадры пишутся в системную память — D3D-менеджер не нужен.
    fn SetD3DManager(&self, _manager: Ref<IUnknown>) -> Result<()> {
        self.queue()?;
        Ok(())
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(&self, _service: *const GUID, _riid: *const GUID, _ppv: *mut *mut c_void) -> Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IKsControl_Impl for MediaSource_Impl {
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

impl IMFSampleAllocatorControl_Impl for MediaSource_Impl {
    fn SetDefaultAllocator(&self, stream_id: u32, allocator: Ref<IUnknown>) -> Result<()> {
        let allocator = allocator.ok()?;
        self.stream(stream_id)?.set_allocator(allocator.cast()?);
        Ok(())
    }

    fn GetAllocatorUsage(&self, stream_id: u32, input_id: *mut u32, usage: *mut MFSampleAllocatorUsage) -> Result<()> {
        if input_id.is_null() || usage.is_null() {
            return Err(E_POINTER.into());
        }
        self.stream(stream_id)?;
        unsafe {
            *input_id = stream_id;
            *usage = MFSampleAllocatorUsage_UsesProvidedAllocator;
        }
        Ok(())
    }
}
