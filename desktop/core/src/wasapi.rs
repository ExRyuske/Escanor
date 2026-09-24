//! Вывод звука в Windows через WASAPI с минимальным периодом звукового движка.
//! `IAudioClient3` позволяет работать с периодом 2–3 мс вместо стандартных 10 мс,
//! если драйвер устройства это поддерживает; иначе — обычный режим с минимальным буфером.

use crate::audio::SharedRing;
use anyhow::{Context, Result, bail};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, IAudioClient, IAudioClient3, IAudioRenderClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Threading::{AvSetMmThreadCharacteristicsW, CreateEventW, WaitForSingleObject};
use windows::core::{HSTRING, w};

pub struct WasapiOutput {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Период звукового движка, мс.
    pub period_ms: f32,
}

impl WasapiOutput {
    /// `endpoint_id` — идентификатор устройства WASAPI; `None` — устройство по умолчанию.
    pub fn start(endpoint_id: Option<String>, ring: SharedRing) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<f32>>();
        let stop_flag = stop.clone();
        let thread = std::thread::Builder::new().name("escanor-wasapi".into()).spawn(move || {
            if let Err(e) = render_loop(endpoint_id, ring, &stop_flag, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        let period_ms = ready_rx.recv().context("поток WASAPI завершился")??;
        Ok(Self { stop, thread: Some(thread), period_ms })
    }
}

impl WasapiOutput {
    /// Поток вывода работает; он завершается с ошибкой, например когда устройство отключили.
    pub fn is_alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for WasapiOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn device(endpoint_id: Option<&str>) -> Result<IMMDevice> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        Ok(match endpoint_id {
            Some(id) => enumerator.GetDevice(&HSTRING::from(id))?,
            None => enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?,
        })
    }
}

fn is_float(format: &WAVEFORMATEX) -> bool {
    if format.wBitsPerSample != 32 {
        return false;
    }
    match format.wFormatTag as u32 {
        WAVE_FORMAT_IEEE_FLOAT => true,
        WAVE_FORMAT_EXTENSIBLE => {
            // Структура упакована — читаем поле без ссылки.
            let ext = format as *const WAVEFORMATEX as *const WAVEFORMATEXTENSIBLE;
            let sub_format = unsafe { std::ptr::addr_of!((*ext).SubFormat).read_unaligned() };
            sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        }
        _ => false,
    }
}

/// Инициализирует клиент с минимальным периодом; возвращает период в кадрах.
unsafe fn initialize(device: &IMMDevice, format: *const WAVEFORMATEX, rate: u32) -> Result<(IAudioClient, u32)> {
    unsafe {
        let client3: Result<IAudioClient3> = device.Activate(CLSCTX_ALL, None).map_err(Into::into);
        if let Ok(client3) = client3 {
            let (mut default, mut fundamental, mut min, mut max) = (0, 0, 0, 0);
            if client3.GetSharedModeEnginePeriod(format, &mut default, &mut fundamental, &mut min, &mut max).is_ok()
                && client3.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, min, format, None).is_ok()
            {
                return Ok((client3.into(), min));
            }
        }
        // Запасной путь: обычный общий режим с периодом по умолчанию.
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 0, 0, format, None)?;
        let mut period = 0i64;
        client.GetDevicePeriod(Some(&mut period), None)?;
        Ok((client, (period * rate as i64 / 10_000_000) as u32))
    }
}

fn render_loop(
    endpoint_id: Option<String>,
    ring: SharedRing,
    stop: &AtomicBool,
    ready: &mpsc::Sender<Result<f32>>,
) -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let device = device(endpoint_id.as_deref())?;
        let probe: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = probe.GetMixFormat()?;
        drop(probe);
        let result = (|| -> Result<()> {
            if !is_float(&*format) {
                bail!("формат микшера не float32");
            }
            let channels = (*format).nChannels as usize;
            let rate = (*format).nSamplesPerSec;
            let (client, period) = initialize(&device, format, rate)?;
            ring.lock().unwrap().set_output_period_ms(period as f32 * 1000.0 / rate as f32);

            let event = CreateEventW(None, false, false, None)?;
            client.SetEventHandle(event)?;
            let render: IAudioRenderClient = client.GetService()?;
            let buffer_frames = client.GetBufferSize()?;
            // Планировщик Windows даёт таким потокам приоритет, как у профессионального звука.
            let mut task = 0u32;
            let _ = AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task);
            client.Start()?;
            let _ = ready.send(Ok(period as f32 * 1000.0 / rate as f32));

            while !stop.load(Ordering::Acquire) {
                if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 {
                    continue;
                }
                let available = buffer_frames - client.GetCurrentPadding()?;
                if available == 0 {
                    continue;
                }
                let data = render.GetBuffer(available)?;
                let samples = std::slice::from_raw_parts_mut(data as *mut f32, available as usize * channels);
                ring.lock().unwrap().render(samples, channels, rate);
                render.ReleaseBuffer(available, 0)?;
            }
            let _ = client.Stop();
            let _ = CloseHandle(event);
            Ok(())
        })();
        CoTaskMemFree(Some(format as *const _));
        result
    }
}
