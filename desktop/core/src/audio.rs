//! Звук: приём PCM с телефона и вывод в выбранное устройство ПК
//! (обычно — воспроизводящая сторона виртуального кабеля, другая сторона которого
//! выбирается как микрофон в Zoom/Discord/…).
//!
//! Между приёмом и выводом — буфер против неравномерности доставки. Его размер подстраивается
//! сам: начинается с малого, растёт после каждого опустошения и медленно сжимается, пока звук
//! идёт ровно. Часы телефона и звуковой карты немного расходятся, поэтому скорость чтения
//! чуть подстраивается под уровень буфера.

use crate::protocol::read_packet;
use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use std::collections::VecDeque;
use std::io::BufReader;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Начальный запас в буфере.
const START_TARGET_MS: f32 = 12.0;
/// Меньше не опускаемся, даже если устройство вывода очень быстрое.
const MIN_TARGET_MS: f32 = 6.0;
const MAX_TARGET_MS: f32 = 80.0;
/// На сколько увеличить запас после опустошения буфера.
const GROW_MS: f32 = 4.0;
/// Как часто уменьшать запас на 1 мс, если опустошений не было.
const SHRINK_EVERY: Duration = Duration::from_secs(5);
/// Выше «запас + это» лишнее отбрасывается сразу (например, после подвисания).
const OVERFLOW_MS: f32 = 60.0;
/// Максимальная подстройка скорости (0,5 % — на слух незаметно).
const MAX_RATE_ADJUST: f64 = 0.005;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutputInfo {
    pub id: String,
    pub name: String,
}

pub fn list_outputs() -> Vec<AudioOutputInfo> {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else { return Vec::new() };
    devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = d.description().ok()?.name().to_string();
            Some(AudioOutputInfo { id, name })
        })
        .collect()
}

/// Устройство вывода по id из `list_outputs`; `None` — системное по умолчанию.
pub(crate) fn find_output(device_id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match device_id {
        Some(id) => host
            .output_devices()?
            .find(|d| d.id().map(|i| i.to_string()).as_deref() == Ok(id))
            .ok_or_else(|| anyhow!("устройство вывода не найдено")),
        None => host.default_output_device().ok_or_else(|| anyhow!("нет устройства вывода")),
    }
}

/// Ошибка, после которой поток вывода уже не заработает: устройство отключили или поток сброшен
/// системой. Опустошения буфера и прочие временные сбои сюда не относятся.
pub(crate) fn is_fatal(error: &cpal::Error) -> bool {
    matches!(
        error.kind(),
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::HostUnavailable
    )
}

/// Похоже ли устройство на виртуальный кабель — такие выбираются по умолчанию.
pub fn looks_virtual(name: &str) -> bool {
    let name = name.to_lowercase();
    ["cable", "vb-audio", "voicemeeter", "blackhole", "virtual", "loopback"].iter().any(|k| name.contains(k))
}

/// Буфер между потоком приёма и звуковым колбэком.
#[derive(Debug)]
pub struct AudioRing {
    samples: VecDeque<f32>,
    channels: usize,
    sample_rate: u32,
    /// Дробная позиция чтения между кадрами (для передискретизации).
    phase: f64,
    /// Ждём накопления запаса перед началом (или после опустошения).
    priming: bool,
    underruns: u64,
    target_ms: f32,
    /// Нижняя граница запаса: не меньше периода устройства вывода.
    min_target_ms: f32,
    last_adjust: Instant,
}

impl Default for AudioRing {
    fn default() -> Self {
        Self {
            samples: VecDeque::new(),
            channels: 1,
            sample_rate: 48_000,
            phase: 0.0,
            priming: true,
            underruns: 0,
            target_ms: START_TARGET_MS,
            min_target_ms: MIN_TARGET_MS,
            last_adjust: Instant::now(),
        }
    }
}

impl AudioRing {
    pub fn reset(&mut self, sample_rate: u32, channels: u16) {
        let min_target_ms = self.min_target_ms;
        *self = Self {
            sample_rate,
            channels: channels.max(1) as usize,
            min_target_ms,
            target_ms: START_TARGET_MS.max(min_target_ms),
            ..Self::default()
        };
    }

    /// Устройство вывода забирает звук порциями по периоду — запас должен его покрывать.
    pub fn set_output_period_ms(&mut self, period_ms: f32) {
        self.min_target_ms = (period_ms + 3.0).max(MIN_TARGET_MS);
        self.target_ms = self.target_ms.max(self.min_target_ms);
    }

    fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    fn frames_for_ms(&self, ms: f32) -> usize {
        (self.sample_rate as f32 * ms / 1000.0) as usize
    }

    pub fn buffered_ms(&self) -> f32 {
        self.frames() as f32 * 1000.0 / self.sample_rate as f32
    }

    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    fn push_s16le(&mut self, data: &[u8]) {
        self.samples.extend(data.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b) as f32 / 32768.0));
        let frames = self.frames();
        if frames > self.frames_for_ms(self.target_ms + OVERFLOW_MS) {
            let drop = frames - self.frames_for_ms(self.target_ms);
            self.samples.drain(..drop * self.channels);
        }
    }

    fn on_underrun(&mut self) {
        self.underruns += 1;
        self.priming = true;
        self.target_ms = (self.target_ms + GROW_MS).min(MAX_TARGET_MS);
        self.last_adjust = Instant::now();
    }

    /// Заполняет `out` (чередующиеся каналы `out_channels`) с передискретизацией до `out_rate`.
    pub(crate) fn render<T: SizedSample + FromSample<f32>>(
        &mut self,
        out: &mut [T],
        out_channels: usize,
        out_rate: u32,
    ) {
        if self.last_adjust.elapsed() >= SHRINK_EVERY {
            self.last_adjust = Instant::now();
            self.target_ms = (self.target_ms - 1.0).max(self.min_target_ms);
        }
        let silence = T::from_sample(0.0f32);
        let target = self.frames_for_ms(self.target_ms);
        if self.priming {
            if self.frames() < target {
                out.fill(silence);
                return;
            }
            self.priming = false;
        }

        // Пропорциональная подстройка: буфер растёт — читаем чуть быстрее, и наоборот.
        let deviation = (self.frames() as f64 - target as f64) / target.max(1) as f64;
        let adjust = (deviation * 0.01).clamp(-MAX_RATE_ADJUST, MAX_RATE_ADJUST);
        let step = self.sample_rate as f64 / out_rate as f64 * (1.0 + adjust);

        let in_ch = self.channels;
        let mut written = 0;
        let mut underrun = false;
        for frame in out.chunks_exact_mut(out_channels) {
            let index = self.phase as usize;
            if index + 1 >= self.frames() {
                underrun = true;
                break;
            }
            let frac = (self.phase - index as f64) as f32;
            for (c, sample) in frame.iter_mut().enumerate() {
                let value = if in_ch == 1 || c < in_ch {
                    let src = c.min(in_ch - 1);
                    let a = self.samples[index * in_ch + src];
                    let b = self.samples[(index + 1) * in_ch + src];
                    a + (b - a) * frac
                } else {
                    0.0
                };
                *sample = T::from_sample(value);
            }
            self.phase += step;
            written += 1;
        }
        for frame in out.chunks_exact_mut(out_channels).skip(written) {
            frame.fill(silence);
        }
        let consumed = (self.phase as usize).min(self.frames());
        self.samples.drain(..consumed * in_ch);
        self.phase -= consumed as f64;
        if underrun {
            self.on_underrun();
        }
    }
}

pub type SharedRing = Arc<Mutex<AudioRing>>;

/// Поток приёма звука: пакеты PCM s16le → кольцевой буфер.
pub fn spawn_receiver(
    stream: TcpStream,
    ring: SharedRing,
    closed: Box<dyn FnOnce(String) + Send>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("escanor-audio".into())
        .spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut packet = Vec::new();
            let reason = loop {
                match read_packet(&mut reader, &mut packet) {
                    Ok(_) => ring.lock().unwrap().push_s16le(&packet),
                    Err(e) => break format!("аудиоканал закрыт: {e}"),
                }
            };
            closed(reason);
        })
        .expect("не удалось создать поток звука")
}

/// Вывод в звуковое устройство. На Windows — напрямую через WASAPI с минимальным периодом,
/// на остальных ОС — через cpal. `cpal::Stream` нельзя передавать между потоками,
/// поэтому он живёт в своём потоке, пока жив этот объект.
pub struct AudioOutput {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    _wasapi: Option<crate::wasapi::WasapiOutput>,
    /// Поток cpal сообщил о неисправимой ошибке.
    failed: Arc<AtomicBool>,
    /// Период устройства вывода, мс (если известен).
    pub period_ms: Option<f32>,
}

impl AudioOutput {
    pub fn start(device_id: Option<&str>, ring: SharedRing) -> Result<Self> {
        let device = find_output(device_id)?;
        #[cfg(windows)]
        {
            let endpoint = device.id().ok().map(|id| id.id().to_string());
            match crate::wasapi::WasapiOutput::start(endpoint, ring.clone()) {
                Ok(output) => {
                    let period_ms = Some(output.period_ms);
                    return Ok(Self {
                        stop: None,
                        thread: None,
                        _wasapi: Some(output),
                        failed: Arc::default(),
                        period_ms,
                    });
                }
                Err(e) => log::warn!("WASAPI с малым периодом недоступен, использую cpal: {e:#}"),
            }
        }

        let failed = Arc::new(AtomicBool::new(false));
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let failed_flag = failed.clone();
        let thread =
            std::thread::Builder::new().name("escanor-audio-out".into()).spawn(move || {
                match build_stream(&device, ring, failed_flag) {
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        let _ = stop_rx.recv();
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })?;
        ready_rx.recv().context("поток вывода звука завершился")??;
        Ok(Self {
            stop: Some(stop_tx),
            thread: Some(thread),
            #[cfg(windows)]
            _wasapi: None,
            failed,
            period_ms: None,
        })
    }

    /// Звук ещё выводится: устройство не отключили и поток не остановился с ошибкой.
    pub fn is_alive(&self) -> bool {
        #[cfg(windows)]
        if let Some(wasapi) = &self._wasapi {
            return wasapi.is_alive();
        }
        !self.failed.load(Ordering::Relaxed)
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn build_stream(device: &cpal::Device, ring: SharedRing, failed: Arc<AtomicBool>) -> Result<cpal::Stream> {
    // Предпочитаем 48 кГц float — тогда передискретизация не нужна.
    let preferred =
        device.supported_output_configs()?.filter(|c| c.sample_format() == SampleFormat::F32).find_map(|c| {
            (c.min_sample_rate() <= 48_000 && c.max_sample_rate() >= 48_000).then(|| c.with_sample_rate(48_000))
        });
    let supported = match preferred {
        Some(c) => c,
        None => device.default_output_config()?,
    };
    let format = supported.sample_format();
    let mut config: StreamConfig = supported.config();
    // Буфер около 5 мс вместо стандартного, если устройство допускает.
    if let cpal::SupportedBufferSize::Range { min, max } = supported.buffer_size() {
        let wanted = (config.sample_rate / 200).clamp(*min, *max);
        config.buffer_size = cpal::BufferSize::Fixed(wanted);
        ring.lock().unwrap().set_output_period_ms(wanted as f32 * 1000.0 / config.sample_rate as f32);
    }
    let stream = match format {
        SampleFormat::F32 => make_stream::<f32>(device, config, ring, failed)?,
        SampleFormat::I16 => make_stream::<i16>(device, config, ring, failed)?,
        SampleFormat::I32 => make_stream::<i32>(device, config, ring, failed)?,
        SampleFormat::U16 => make_stream::<u16>(device, config, ring, failed)?,
        other => return Err(anyhow!("формат {other:?} не поддерживается")),
    };
    stream.play()?;
    Ok(stream)
}

fn make_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: StreamConfig,
    ring: SharedRing,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream> {
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _| ring.lock().unwrap().render(out, channels, rate),
        move |e| {
            log::warn!("ошибка вывода звука: {e}");
            if is_fatal(&e) {
                failed.store(true, Ordering::Relaxed);
            }
        },
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(values: impl IntoIterator<Item = i16>) -> Vec<u8> {
        values.into_iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn primes_before_playing_then_plays() {
        let mut ring = AudioRing::default();
        ring.reset(48_000, 1);
        let mut out = vec![1.0f32; 96];
        ring.push_s16le(&pcm(vec![16384; 240]));
        ring.render(&mut out, 2, 48_000);
        assert!(out.iter().all(|&s| s == 0.0), "до накопления запаса — тишина");

        ring.push_s16le(&pcm(vec![16384; 48 * 20]));
        ring.render(&mut out, 2, 48_000);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-3), "моно дублируется в оба канала");
    }

    #[test]
    fn overflow_is_trimmed_to_target() {
        let mut ring = AudioRing::default();
        ring.reset(48_000, 2);
        ring.push_s16le(&pcm(vec![0; 48 * 200 * 2]));
        assert!((ring.buffered_ms() - ring.target_ms).abs() < 1.0);
    }

    #[test]
    fn underrun_grows_target_and_reprimes() {
        let mut ring = AudioRing::default();
        ring.reset(48_000, 1);
        let before = ring.target_ms;
        ring.push_s16le(&pcm(vec![1000; 48 * 13]));
        let mut out = vec![1i16; 48 * 100];
        ring.render(&mut out, 1, 48_000);
        assert_eq!(ring.underruns(), 1);
        assert_eq!(*out.last().unwrap(), 0);
        assert!(ring.priming);
        assert!(ring.target_ms > before, "после опустошения запас растёт");
    }

    #[test]
    fn output_period_sets_minimum() {
        let mut ring = AudioRing::default();
        ring.set_output_period_ms(10.0);
        ring.reset(48_000, 1);
        assert!(ring.target_ms >= 13.0);
        ring.set_output_period_ms(2.67);
        assert!(ring.min_target_ms < 7.0);
    }

    #[test]
    fn detects_virtual_cables() {
        assert!(looks_virtual("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(looks_virtual("BlackHole 2ch"));
        assert!(!looks_virtual("Speakers (Realtek(R) Audio)"));
    }
}
