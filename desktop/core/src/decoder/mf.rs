//! Аппаратный декодер Windows: H.264 MFT от Microsoft с D3D11 (DXVA).
//! Готовые кадры лежат в видеопамяти; Lock2D копирует их в системную память для
//! превью и общей памяти виртуальной камеры.

use super::{Decoder, Nv12Frame};
use anyhow::{Context, Result, bail};
use std::mem::ManuallyDrop;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, IMF2DBuffer, IMFDXGIDeviceManager, IMFMediaType, IMFSample, IMFTransform,
    MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE,
    MF_SA_D3D11_AWARE, MF_VERSION, MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSTARTUP_NOSOCKET, MFStartup, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoArea, MFVideoFormat_H264,
    MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::Interface;

pub struct MfDecoder {
    transform: IMFTransform,
    /// Держим менеджер D3D, пока жив декодер.
    _manager: Option<IMFDXGIDeviceManager>,
    hardware: bool,
    provides_samples: bool,
    output_size: u32,
    /// Размер буфера (высота может быть выровнена до 16, например 1088).
    buffer_width: usize,
    buffer_height: usize,
    /// Видимая область кадра: x, y, ширина, высота.
    crop: (usize, usize, usize, usize),
    default_stride: usize,
}

impl MfDecoder {
    pub fn new() -> Result<Self> {
        unsafe {
            // S_FALSE/RPC_E_CHANGED_MODE — COM на потоке уже инициализирован.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).context("MFStartup")?;
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).context("H.264 MFT")?;
            let attributes = transform.GetAttributes()?;
            // Без переупорядочивания и накопления: кадр отдаётся сразу после декодирования.
            attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;

            let mut manager = None;
            if attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0 {
                match d3d_manager() {
                    Ok(m) => match transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, m.as_raw() as usize) {
                        Ok(()) => manager = Some(m),
                        Err(e) => log::warn!("декодер отказался от D3D11: {e}"),
                    },
                    Err(e) => log::warn!("D3D11 недоступен: {e:#}"),
                }
            }

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &input, 0).context("SetInputType")?;

            let mut decoder = Self {
                transform,
                hardware: manager.is_some(),
                _manager: manager,
                provides_samples: false,
                output_size: 0,
                buffer_width: 0,
                buffer_height: 0,
                crop: (0, 0, 0, 0),
                default_stride: 0,
            };
            // До первого SPS формат выхода может быть ещё неизвестен — тогда его сообщит STREAM_CHANGE.
            if let Err(e) = decoder.select_output() {
                log::info!("выход декодера будет выбран после первого кадра: {e:#}");
            }
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }

    /// Выбирает выход NV12 и запоминает геометрию кадра (после смены потока — заново).
    fn select_output(&mut self) -> Result<()> {
        unsafe {
            let mut index = 0;
            loop {
                let t: IMFMediaType = self.transform.GetOutputAvailableType(0, index).context("нет выхода NV12")?;
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.transform.SetOutputType(0, &t, 0)?;
                    break;
                }
                index += 1;
            }
            let current = self.transform.GetOutputCurrentType(0)?;
            let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
            self.buffer_width = (size >> 32) as usize;
            self.buffer_height = (size & 0xFFFF_FFFF) as usize;
            self.default_stride =
                current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or(self.buffer_width, |s| s as i32 as usize);

            let mut area = MFVideoArea::default();
            let blob =
                std::slice::from_raw_parts_mut(&mut area as *mut MFVideoArea as *mut u8, size_of::<MFVideoArea>());
            self.crop = match current.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, blob, None) {
                Ok(()) if area.Area.cx > 0 && area.Area.cy > 0 => (
                    area.OffsetX.value.max(0) as usize,
                    area.OffsetY.value.max(0) as usize,
                    area.Area.cx as usize,
                    area.Area.cy as usize,
                ),
                _ => (0, 0, self.buffer_width, self.buffer_height),
            };

            let info = self.transform.GetOutputStreamInfo(0)?;
            let provides = MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0;
            self.provides_samples = info.dwFlags & provides as u32 != 0;
            self.output_size = info.cbSize;
        }
        Ok(())
    }

    /// Забирает все готовые кадры.
    fn drain(&mut self, on_frame: &mut dyn FnMut(&Nv12Frame)) -> Result<()> {
        loop {
            let sample = if self.provides_samples {
                None
            } else {
                unsafe {
                    let s = MFCreateSample()?;
                    s.AddBuffer(&MFCreateMemoryBuffer(self.output_size)?)?;
                    Some(s)
                }
            };
            let mut output = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let result = unsafe { self.transform.ProcessOutput(0, &mut output, &mut status) };
            let sample = unsafe { ManuallyDrop::take(&mut output[0].pSample) };
            drop(unsafe { ManuallyDrop::take(&mut output[0].pEvents) });
            match result {
                Ok(()) => {
                    if let Some(sample) = sample {
                        self.emit(&sample, on_frame)?;
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.select_output()?,
                Err(e) => return Err(e).context("ProcessOutput"),
            }
        }
    }

    fn emit(&self, sample: &IMFSample, on_frame: &mut dyn FnMut(&Nv12Frame)) -> Result<()> {
        let (x, y, w, h) = self.crop;
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            // Буфер D3D11 поддерживает Lock2D: копия из видеопамяти во внутренний системный буфер.
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer>() {
                let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
                buffer2d.Lock2D(&mut scan0, &mut pitch)?;
                if pitch > 0 {
                    let pitch = pitch as usize;
                    let data = std::slice::from_raw_parts(scan0, pitch * self.buffer_height * 3 / 2);
                    emit_planes(data, pitch, self.buffer_height, (x, y, w, h), on_frame);
                }
                buffer2d.Unlock2D()?;
            } else {
                let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
                buffer.Lock(&mut ptr, None, Some(&mut len))?;
                let pitch = self.default_stride.max(self.buffer_width);
                if len as usize >= pitch * self.buffer_height * 3 / 2 {
                    let data = std::slice::from_raw_parts(ptr, len as usize);
                    emit_planes(data, pitch, self.buffer_height, (x, y, w, h), on_frame);
                }
                buffer.Unlock()?;
            }
        }
        Ok(())
    }
}

fn emit_planes(
    data: &[u8],
    pitch: usize,
    buffer_height: usize,
    (x, y, w, h): (usize, usize, usize, usize),
    on_frame: &mut dyn FnMut(&Nv12Frame),
) {
    let uv_start = pitch * buffer_height;
    on_frame(&Nv12Frame {
        width: w & !1,
        height: h & !1,
        y: &data[y * pitch + x..uv_start],
        y_stride: pitch,
        uv: &data[uv_start + (y / 2) * pitch + (x & !1)..],
        uv_stride: pitch,
    });
}

impl Decoder for MfDecoder {
    fn decode(&mut self, data: &[u8], pts_us: i64, on_frame: &mut dyn FnMut(&Nv12Frame)) -> Result<()> {
        let sample = unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts_us * 10)?;
            sample
        };
        for _ in 0..3 {
            match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => return self.drain(on_frame),
                // Декодер полон — забираем готовое и пробуем снова.
                Err(e) if e.code() == MF_E_NOTACCEPTING => self.drain(on_frame)?,
                Err(e) => return Err(e).context("ProcessInput"),
            }
        }
        bail!("декодер не принимает данные")
    }

    fn description(&self) -> String {
        if self.hardware {
            "Media Foundation + D3D11 (аппаратный)".into()
        } else {
            "Media Foundation (программный — нет DXVA)".into()
        }
    }
}

fn d3d_manager() -> Result<IMFDXGIDeviceManager> {
    unsafe {
        let mut device = None;
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .context("D3D11CreateDevice")?;
        let device = device.context("нет устройства D3D11")?;
        // Декодер и Lock2D обращаются к устройству из разных потоков.
        let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);

        let (mut token, mut manager) = (0u32, None);
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.context("нет менеджера DXGI")?;
        manager.ResetDevice(&device, token)?;
        Ok(manager)
    }
}
