//! Аппаратный декодер macOS через VideoToolbox (прямые вызовы C API).

use super::{Decoder, Nv12Frame, nal_units};
use anyhow::{Result, anyhow, bail};
use std::ffi::c_void;
use std::ptr::null;

type OSStatus = i32;
type CFTypeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
struct CMSampleTimingInfo {
    duration: CMTime,
    pts: CMTime,
    dts: CMTime,
}

type OutputCallback = extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, CFTypeRef, CMTime, CMTime);

#[repr(C)]
struct OutputCallbackRecord {
    callback: OutputCallback,
    ref_con: *mut c_void,
}

#[repr(C)]
struct Opaque {
    _p: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: Opaque;
    static kCFTypeDictionaryValueCallBacks: Opaque;
    static kCFBooleanTrue: CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const Opaque,
        value_callbacks: *const Opaque,
    ) -> CFTypeRef;
    fn CFNumberCreate(allocator: CFTypeRef, number_type: isize, value: *const c_void) -> CFTypeRef;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFTypeRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_length: i32,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFTypeRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFTypeRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn CMBlockBufferReplaceDataBytes(
        source: *const c_void,
        destination: CFTypeRef,
        offset: usize,
        length: usize,
    ) -> OSStatus;
    fn CMSampleBufferCreateReady(
        allocator: CFTypeRef,
        data_buffer: CFTypeRef,
        format: CFTypeRef,
        num_samples: isize,
        num_timing: isize,
        timing: *const CMSampleTimingInfo,
        num_sizes: isize,
        sizes: *const usize,
        out: *mut CFTypeRef,
    ) -> OSStatus;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFTypeRef;
    fn CVPixelBufferLockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(buffer: CFTypeRef, plane: usize) -> *const u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CFTypeRef, plane: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(buffer: CFTypeRef, plane: usize) -> usize;
    fn CVPixelBufferGetWidth(buffer: CFTypeRef) -> usize;
    fn CVPixelBufferGetHeight(buffer: CFTypeRef) -> usize;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    static kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: CFTypeRef;
    static kVTDecompressionPropertyKey_RealTime: CFTypeRef;
    fn VTDecompressionSessionCreate(
        allocator: CFTypeRef,
        format: CFTypeRef,
        decoder_specification: CFTypeRef,
        image_buffer_attributes: CFTypeRef,
        callback: *const OutputCallbackRecord,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: CFTypeRef,
        sample: CFTypeRef,
        flags: u32,
        frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: CFTypeRef) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: CFTypeRef);
    fn VTSessionSetProperty(session: CFTypeRef, key: CFTypeRef, value: CFTypeRef) -> OSStatus;
}

const K_CF_NUMBER_SINT32: isize = 3;
const K_CM_BLOCK_BUFFER_ASSURE_MEMORY_NOW: u32 = 1;
const K_CV_PIXEL_BUFFER_LOCK_READ_ONLY: u64 = 1;
/// '420v' — NV12, video range.
const PIXEL_FORMAT_NV12: i32 = 0x3432_3076;

/// Кадр, скопированный в колбэке декодера.
#[derive(Default)]
struct Output {
    width: usize,
    height: usize,
    y: Vec<u8>,
    y_stride: usize,
    uv: Vec<u8>,
    uv_stride: usize,
    ready: bool,
    status: OSStatus,
}

pub struct VtDecoder {
    sps: Vec<u8>,
    pps: Vec<u8>,
    format: CFTypeRef,
    session: CFTypeRef,
    hardware: bool,
    output: Box<Output>,
    avcc: Vec<u8>,
}

impl VtDecoder {
    pub fn new() -> Self {
        Self {
            sps: Vec::new(),
            pps: Vec::new(),
            format: null(),
            session: null(),
            hardware: false,
            output: Box::default(),
            avcc: Vec::new(),
        }
    }

    fn release(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
            }
            if !self.format.is_null() {
                CFRelease(self.format);
            }
        }
        self.session = null();
        self.format = null();
    }

    /// Пересоздаёт сессию под новые SPS/PPS (смена камеры или разрешения).
    fn configure(&mut self) -> Result<()> {
        self.release();
        let pointers = [self.sps.as_ptr(), self.pps.as_ptr()];
        let sizes = [self.sps.len(), self.pps.len()];
        let mut format = null();
        let status = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                null(),
                2,
                pointers.as_ptr(),
                sizes.as_ptr(),
                4,
                &mut format,
            )
        };
        if status != 0 {
            bail!("CMVideoFormatDescriptionCreateFromH264ParameterSets: {status}");
        }
        self.format = format;

        unsafe {
            let pixel_format =
                CFNumberCreate(null(), K_CF_NUMBER_SINT32, &PIXEL_FORMAT_NV12 as *const i32 as *const c_void);
            let attributes = dictionary(&[kCVPixelBufferPixelFormatTypeKey], &[pixel_format]);
            CFRelease(pixel_format);
            let hardware_only =
                dictionary(&[kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder], &[kCFBooleanTrue]);
            let callback =
                OutputCallbackRecord { callback: on_output, ref_con: &mut *self.output as *mut Output as *mut c_void };

            let mut session = null();
            let mut status =
                VTDecompressionSessionCreate(null(), format, hardware_only, attributes, &callback, &mut session);
            self.hardware = status == 0;
            if status != 0 {
                log::warn!("аппаратный H.264 недоступен ({status}), пробую программный");
                status = VTDecompressionSessionCreate(null(), format, null(), attributes, &callback, &mut session);
            }
            CFRelease(hardware_only);
            CFRelease(attributes);
            if status != 0 {
                bail!("VTDecompressionSessionCreate: {status}");
            }
            VTSessionSetProperty(session, kVTDecompressionPropertyKey_RealTime, kCFBooleanTrue);
            self.session = session;
        }
        Ok(())
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        self.release();
    }
}

impl Decoder for VtDecoder {
    fn decode(&mut self, data: &[u8], pts_us: i64, on_frame: &mut dyn FnMut(&Nv12Frame)) -> Result<()> {
        // Параметры набора — отдельно, остальные NAL-блоки — в формат AVCC (длина + данные).
        let (mut sps, mut pps) = (None, None);
        self.avcc.clear();
        for nal in nal_units(data) {
            match nal[0] & 0x1F {
                7 => sps = Some(nal),
                8 => pps = Some(nal),
                9 => {}
                _ => {
                    self.avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    self.avcc.extend_from_slice(nal);
                }
            }
        }
        let changed = sps.is_some_and(|s| s != self.sps) || pps.is_some_and(|p| p != self.pps);
        if let Some(s) = sps {
            self.sps = s.to_vec();
        }
        if let Some(p) = pps {
            self.pps = p.to_vec();
        }
        if changed && !self.sps.is_empty() && !self.pps.is_empty() {
            self.configure()?;
        }
        if self.avcc.is_empty() {
            return Ok(());
        }
        if self.session.is_null() {
            bail!("нет SPS/PPS — жду ключевой кадр");
        }

        unsafe {
            let len = self.avcc.len();
            let mut block = null();
            let status = CMBlockBufferCreateWithMemoryBlock(
                null(),
                std::ptr::null_mut(),
                len,
                null(),
                null(),
                0,
                len,
                K_CM_BLOCK_BUFFER_ASSURE_MEMORY_NOW,
                &mut block,
            );
            if status != 0 {
                bail!("CMBlockBufferCreateWithMemoryBlock: {status}");
            }
            CMBlockBufferReplaceDataBytes(self.avcc.as_ptr() as *const c_void, block, 0, len);

            let time = CMTime { value: pts_us, timescale: 1_000_000, flags: 1, epoch: 0 };
            let invalid = CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 };
            let timing = CMSampleTimingInfo { duration: invalid, pts: time, dts: invalid };
            let mut sample = null();
            let status = CMSampleBufferCreateReady(null(), block, self.format, 1, 1, &timing, 1, &len, &mut sample);
            CFRelease(block);
            if status != 0 {
                bail!("CMSampleBufferCreateReady: {status}");
            }

            self.output.ready = false;
            // Без флага асинхронности колбэк вызывается до возврата из DecodeFrame.
            let status =
                VTDecompressionSessionDecodeFrame(self.session, sample, 0, std::ptr::null_mut(), std::ptr::null_mut());
            VTDecompressionSessionWaitForAsynchronousFrames(self.session);
            CFRelease(sample);
            if status != 0 {
                return Err(anyhow!("VTDecompressionSessionDecodeFrame: {status}"));
            }
        }

        let out = &self.output;
        if out.status != 0 {
            bail!("ошибка декодирования VideoToolbox: {}", out.status);
        }
        if out.ready {
            on_frame(&Nv12Frame {
                width: out.width,
                height: out.height,
                y: &out.y,
                y_stride: out.y_stride,
                uv: &out.uv,
                uv_stride: out.uv_stride,
            });
        }
        Ok(())
    }

    fn description(&self) -> String {
        if self.hardware {
            "VideoToolbox (аппаратный)".into()
        } else {
            "VideoToolbox (программный)".into()
        }
    }
}

unsafe fn dictionary(keys: &[CFTypeRef], values: &[CFTypeRef]) -> CFTypeRef {
    unsafe {
        CFDictionaryCreate(
            null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as isize,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
    }
}

extern "C" fn on_output(
    ref_con: *mut c_void,
    _frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    image: CFTypeRef,
    _pts: CMTime,
    _duration: CMTime,
) {
    let out = unsafe { &mut *(ref_con as *mut Output) };
    out.status = status;
    if status != 0 || image.is_null() {
        return;
    }
    unsafe {
        if CVPixelBufferLockBaseAddress(image, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY) != 0 {
            return;
        }
        out.width = CVPixelBufferGetWidth(image);
        out.height = CVPixelBufferGetHeight(image);
        for plane in 0..2 {
            let stride = CVPixelBufferGetBytesPerRowOfPlane(image, plane);
            let rows = CVPixelBufferGetHeightOfPlane(image, plane);
            let base = CVPixelBufferGetBaseAddressOfPlane(image, plane);
            let dst = if plane == 0 { &mut out.y } else { &mut out.uv };
            dst.clear();
            dst.extend_from_slice(std::slice::from_raw_parts(base, stride * rows));
            if plane == 0 {
                out.y_stride = stride;
            } else {
                out.uv_stride = stride;
            }
        }
        CVPixelBufferUnlockBaseAddress(image, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
    }
    out.ready = true;
}
