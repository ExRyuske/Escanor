//! Саундпад: кнопки макропада проигрывают звуки на ПК в отдельно выбранное устройство вывода
//! (например, в виртуальный кабель, чтобы звуки слышали в Discord).
//!
//! Выравнивание громкости: у каждого звука измеряется средняя громкость без тишины, и звук
//! усиливается или ослабляется до общего уровня — насколько позволяет пик, чтобы не было перегрузки.

use crate::engine::{Event, EventSink};
use crate::macropad::SoundRef;
use anyhow::{Context, Result, anyhow, ensure};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;

/// Общий уровень громкости после выравнивания (RMS, −20 dBFS).
const TARGET_RMS: f32 = 0.1;
/// Тише этого — тишина, в громкость не входит (−60 dBFS).
const SILENCE_RMS: f32 = 0.001;
/// Выше пик не поднимаем.
const PEAK_LIMIT: f32 = 0.98;
/// Сколько памяти занимают загруженные звуки; давно не игравшие вытесняются.
const CACHE_BYTES: usize = 128 << 20;
/// Длиннее звук обрезается: саундпад — для коротких звуков, а час музыки занял бы сотни мегабайт.
const MAX_SECONDS: usize = 600;
/// Сколько звуков играет одновременно; следующий вытесняет самый старый.
const MAX_VOICES: usize = 16;
/// Тише этого пика (−45 dBFS) край звука считается тишиной.
const SILENCE_PEAK: f32 = 0.0056;
/// Запас при обрезке тишины: перед атакой и после затухания.
const TRIM_PAD_BEFORE_MS: u32 = 10;
const TRIM_PAD_AFTER_MS: u32 = 50;
/// Сколько устройство вывода остаётся открытым после последнего звука.
const IDLE_CLOSE: Duration = Duration::from_secs(10);

/// Звук целиком в памяти. 16 бит — вдвое меньше памяти, чем float, а на слух не отличить.
pub struct Clip {
    /// Чередующиеся каналы.
    samples: Vec<i16>,
    channels: usize,
    rate: u32,
    /// Множитель, приводящий звук к общему уровню громкости.
    gain: f32,
}

impl Clip {
    fn new(samples: Vec<i16>, channels: usize, rate: u32) -> Self {
        let gain = loudness_gain(&samples, channels, rate);
        Self { samples, channels, rate, gain }
    }

    fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    fn bytes(&self) -> usize {
        self.samples.len() * size_of::<i16>()
    }

    /// Кадры отрезка `[start, end)` звука кнопки, в пределах файла.
    fn range(&self, sound: &SoundRef) -> (usize, usize) {
        let frame = |ms: u32| (ms as u64 * self.rate as u64 / 1000) as usize;
        let end = sound.end_ms.map_or(self.frames(), frame).min(self.frames());
        (frame(sound.start_ms).min(end), end)
    }

    fn info(&self, buckets: usize) -> SoundInfo {
        let ms = |frame: usize| (frame as u64 * 1000 / self.rate as u64) as u32;
        let frames = self.frames();
        let peak = |from: usize, to: usize| {
            self.samples[from * self.channels..to * self.channels].iter().map(|s| s.unsigned_abs()).max().unwrap_or(0)
        };
        let peaks =
            (0..buckets).map(|i| peak(frames * i / buckets, frames * (i + 1) / buckets) as f32 / 32768.0).collect();
        // Края без звука — по окнам в 10 мс, тише порога. Немного запаса, чтобы не срезать атаку и затухание.
        let window = (self.rate as usize / 100).max(1);
        let loud = |w: usize| peak(w * window, ((w + 1) * window).min(frames)) as f32 / 32768.0 > SILENCE_PEAK;
        let windows = frames.div_ceil(window);
        let content = match ((0..windows).find(|&w| loud(w)), (0..windows).rev().find(|&w| loud(w))) {
            (Some(first), Some(last)) => (
                ms(first * window).saturating_sub(TRIM_PAD_BEFORE_MS),
                (ms(((last + 1) * window).min(frames)) + TRIM_PAD_AFTER_MS).min(ms(frames)),
            ),
            _ => (0, ms(frames)),
        };
        SoundInfo { duration_ms: ms(frames), peaks, content_ms: content }
    }
}

/// Звук для редактора кнопки: длительность, волна и где он начинается и заканчивается без тишины.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundInfo {
    pub duration_ms: u32,
    /// Пик каждого из равных отрезков, 0..1.
    pub peaks: Vec<f32>,
    /// Начало и конец звука без тишины по краям, мс.
    pub content_ms: (u32, u32),
}

/// Декодирует файл и описывает его для редактора: волна из `buckets` столбиков.
pub fn analyze(path: &Path, buckets: usize) -> Result<SoundInfo> {
    Ok(decode(path)?.info(buckets))
}

/// Файл mp3, wav, ogg или flac → звук в памяти.
pub fn decode(path: &Path) -> Result<Clip> {
    let file = File::open(path).with_context(|| format!("не удалось открыть {}", path.display()))?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, source, Default::default(), Default::default())
        .context("формат файла не поддерживается")?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| anyhow!("в файле нет звука"))?;
    let track_id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or_else(|| anyhow!("в файле нет звука"))?;
    let mut decoder = symphonia::default::get_codecs().make_audio_decoder(params, &AudioDecoderOptions::default())?;

    let (mut samples, mut chunk, mut channels, mut rate) = (Vec::new(), Vec::new(), 0, 0);
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(DecodeError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buffer) => {
                let spec = (buffer.spec().channels().count(), buffer.spec().rate());
                // Формат сменился посреди файла (склеенный ogg) — продолжение не сложить с началом.
                if channels > 0 && spec != (channels, rate) {
                    break;
                }
                (channels, rate) = spec;
                chunk.resize(buffer.samples_interleaved(), 0i16);
                buffer.copy_to_slice_interleaved(&mut chunk);
                samples.extend_from_slice(&chunk);
                let limit = MAX_SECONDS * rate as usize * channels;
                if samples.len() >= limit {
                    samples.truncate(limit);
                    break;
                }
            }
            // Битый кадр пропускаем, как делают плееры.
            Err(DecodeError::DecodeError(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    ensure!(channels > 0 && rate > 0 && !samples.is_empty(), "в файле нет звука");
    samples.shrink_to_fit();
    Ok(Clip::new(samples, channels, rate))
}

/// Во сколько раз изменить звук, чтобы его громкость стала общей.
fn loudness_gain(samples: &[i16], channels: usize, rate: u32) -> f32 {
    let silence = SILENCE_RMS as f64 * 32768.0;
    // Громкость — среднее по блокам в 50 мс, тишина между фразами её не занижает.
    let block = (rate as usize / 20).max(1) * channels;
    let (mut sum, mut blocks) = (0.0f64, 0);
    for chunk in samples.chunks(block) {
        let mean_square = chunk.iter().map(|&s| s as f64 * s as f64).sum::<f64>() / chunk.len() as f64;
        if mean_square > silence * silence {
            sum += mean_square;
            blocks += 1;
        }
    }
    if blocks == 0 {
        return 1.0;
    }
    let rms = ((sum / blocks as f64).sqrt() / 32768.0) as f32;
    let peak = samples.iter().map(|&s| s.unsigned_abs()).max().unwrap_or(0) as f32 / 32768.0;
    (TARGET_RMS / rms).min(PEAK_LIMIT / peak)
}

/// Загруженные звуки с ограничением по памяти.
#[derive(Default)]
struct Cache {
    clips: HashMap<String, Cached>,
    bytes: usize,
    clock: u64,
}

struct Cached {
    clip: Arc<Clip>,
    used: u64,
}

impl Cache {
    fn get(&mut self, path: &str) -> Option<Arc<Clip>> {
        self.clock += 1;
        let cached = self.clips.get_mut(path)?;
        cached.used = self.clock;
        Some(cached.clip.clone())
    }

    fn fits(&self, clip: &Clip) -> bool {
        self.bytes + clip.bytes() <= CACHE_BYTES
    }

    /// Кладёт звук, вытесняя давно не игравшие, пока не хватит места.
    fn insert(&mut self, path: &str, clip: Arc<Clip>) {
        if self.clips.contains_key(path) {
            return;
        }
        while !self.fits(&clip) {
            let Some(oldest) = self.clips.iter().min_by_key(|(_, c)| c.used).map(|(p, _)| p.clone()) else { break };
            self.remove(&oldest);
        }
        self.clock += 1;
        self.bytes += clip.bytes();
        self.clips.insert(path.to_string(), Cached { clip, used: self.clock });
    }

    fn remove(&mut self, path: &str) {
        if let Some(cached) = self.clips.remove(path) {
            self.bytes -= cached.clip.bytes();
        }
    }

    fn retain(&mut self, paths: &[String]) {
        let gone: Vec<String> = self.clips.keys().filter(|p| !paths.contains(p)).cloned().collect();
        for path in gone {
            self.remove(&path);
        }
    }
}

type SharedCache = Arc<Mutex<Cache>>;

/// Звук, который сейчас играет.
struct Voice {
    path: String,
    clip: Arc<Clip>,
    /// Позиция в кадрах звука (дробная — для передискретизации).
    pos: f64,
    /// Кадр, на котором звук заканчивается (конец выбранного отрезка).
    end: usize,
    /// Уже с переводом из 16 бит в диапазон ±1.
    gain: f32,
}

impl Voice {
    fn new(sound: &SoundRef, clip: Arc<Clip>, normalize: bool) -> Self {
        let (start, end) = clip.range(sound);
        let gain = match normalize {
            false => 1.0,
            // Громкость — по тому, что играет: вырезанный громкий щелчок не должен приглушать остальное.
            true if (start, end) == (0, clip.frames()) => clip.gain,
            true => loudness_gain(&clip.samples[start * clip.channels..end * clip.channels], clip.channels, clip.rate),
        };
        Self { path: sound.path.clone(), clip, pos: start as f64, end, gain: gain / 32768.0 }
    }

    /// Добавляет себя в `out`; `false` — звук закончился.
    fn mix(&mut self, out: &mut [f32], out_channels: usize, out_rate: u32) -> bool {
        let clip = &self.clip;
        let step = clip.rate as f64 / out_rate as f64;
        for frame in out.chunks_exact_mut(out_channels) {
            let index = self.pos as usize;
            if index + 1 >= self.end {
                return false;
            }
            let frac = (self.pos - index as f64) as f32;
            for (c, sample) in frame.iter_mut().enumerate() {
                let src = c.min(clip.channels - 1);
                let a = clip.samples[index * clip.channels + src] as f32;
                let b = clip.samples[(index + 1) * clip.channels + src] as f32;
                *sample += (a + (b - a) * frac) * self.gain;
            }
            self.pos += step;
        }
        true
    }
}

struct Mixer {
    voices: Vec<Voice>,
    buffer: Vec<f32>,
    /// Общая громкость саундпада.
    volume: f32,
}

impl Default for Mixer {
    fn default() -> Self {
        Self { voices: Vec::new(), buffer: Vec::new(), volume: 1.0 }
    }
}

impl Mixer {
    fn play(&mut self, voice: Voice) {
        if self.voices.len() >= MAX_VOICES {
            self.voices.remove(0);
        }
        self.voices.push(voice);
    }

    fn render<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T], out_channels: usize, out_rate: u32) {
        if self.voices.is_empty() {
            out.fill(T::from_sample(0.0f32));
            return;
        }
        self.buffer.clear();
        self.buffer.resize(out.len(), 0.0);
        let buffer = &mut self.buffer;
        self.voices.retain_mut(|v| v.mix(buffer, out_channels, out_rate));
        for (o, &s) in out.iter_mut().zip(buffer.iter()) {
            *o = T::from_sample((s * self.volume).clamp(-1.0, 1.0));
        }
    }
}

type SharedMixer = Arc<Mutex<Mixer>>;

pub struct Soundpad {
    events: EventSink,
    device: Option<String>,
    normalize: bool,
    mixer: SharedMixer,
    cache: SharedCache,
    /// Звуки, которые загружаются после нажатия: повторное нажатие за это время их отменяет.
    loading: Arc<Mutex<HashSet<String>>>,
    /// Фоновая загрузка звуков раскладки и последний отправленный ей список.
    preloader: mpsc::Sender<Vec<String>>,
    preloaded: Vec<String>,
    /// Открывается при первом звуке и закрывается, когда звуков долго нет.
    output: Option<Output>,
    idle_since: Option<Instant>,
}

impl Soundpad {
    pub fn new(events: EventSink) -> Self {
        let cache = SharedCache::default();
        Self {
            events,
            device: None,
            normalize: true,
            mixer: SharedMixer::default(),
            preloader: spawn_preloader(cache.clone()),
            cache,
            loading: Default::default(),
            preloaded: Vec::new(),
            output: None,
            idle_since: None,
        }
    }

    /// Устройство вывода (`None` — системное), выравнивание громкости для следующих звуков
    /// и общая громкость (0..1) — она меняется сразу, и у уже играющих звуков.
    pub fn configure(&mut self, device: Option<String>, normalize: bool, volume: f32) {
        if device != self.device {
            self.output = None;
            self.device = device;
            // Иначе звуки застыли бы и доиграли с середины при следующем нажатии.
            self.mixer.lock().unwrap().voices.clear();
        }
        self.normalize = normalize;
        self.mixer.lock().unwrap().volume = volume;
    }

    /// Заранее загружает звуки раскладки, чтобы нажатие звучало сразу; остальные забывает.
    pub fn preload(&mut self, paths: Vec<String>) {
        if paths == self.preloaded {
            return;
        }
        self.cache.lock().unwrap().retain(&paths);
        self.preloaded = paths.clone();
        let _ = self.preloader.send(paths);
    }

    /// Запускает звук с начала; если он уже играет — останавливает.
    pub fn toggle(&mut self, sound: &SoundRef) {
        let path = sound.path.as_str();
        {
            let mut mixer = self.mixer.lock().unwrap();
            let before = mixer.voices.len();
            mixer.voices.retain(|v| v.path != path);
            if mixer.voices.len() != before {
                return;
            }
        }
        if self.loading.lock().unwrap().remove(path) {
            return;
        }
        if self.output.as_ref().is_some_and(Output::failed) {
            self.output = None;
        }
        if self.output.is_none() {
            match Output::start(self.device.as_deref(), self.mixer.clone()) {
                Ok(output) => self.output = Some(output),
                Err(e) => return (self.events)(Event::Error(format!("Саундпад: {e:#}"))),
            }
        }
        self.idle_since = None;
        let cached = self.cache.lock().unwrap().get(path);
        if let Some(clip) = cached {
            self.mixer.lock().unwrap().play(Voice::new(sound, clip, self.normalize));
            return;
        }
        self.loading.lock().unwrap().insert(path.to_string());
        let (cache, loading, mixer, events, sound, normalize) = (
            self.cache.clone(),
            self.loading.clone(),
            self.mixer.clone(),
            self.events.clone(),
            sound.clone(),
            self.normalize,
        );
        // Незагруженный файл декодируется в фоне, чтобы не задерживать остальные нажатия.
        std::thread::spawn(move || {
            let path = sound.path.as_str();
            let result = decode(Path::new(path));
            let wanted = loading.lock().unwrap().remove(path);
            match result {
                Ok(clip) => {
                    let clip = Arc::new(clip);
                    cache.lock().unwrap().insert(path, clip.clone());
                    if wanted {
                        mixer.lock().unwrap().play(Voice::new(&sound, clip, normalize));
                    }
                }
                Err(e) => events(Event::Error(format!("Саундпад: {e:#}"))),
            }
        });
    }

    /// Вызывается движком регулярно: закрывает устройство вывода, если звуков давно нет —
    /// открытый звуковой поток тратит процессор и не даёт Windows уснуть.
    pub fn tick(&mut self) {
        let Some(output) = &self.output else { return };
        if output.failed() {
            // Устройство отключили: колбэк больше не вызывается, и звуки никогда бы не доиграли.
            self.output = None;
            self.idle_since = None;
            self.mixer.lock().unwrap().voices.clear();
            return (self.events)(Event::Error("Саундпад: устройство вывода звуков отключено".into()));
        }
        if !self.mixer.lock().unwrap().voices.is_empty() {
            self.idle_since = None;
        } else if self.idle_since.get_or_insert_with(Instant::now).elapsed() >= IDLE_CLOSE {
            self.output = None;
            self.idle_since = None;
        }
    }
}

/// Один поток по очереди загружает звуки раскладки, пока хватает памяти. Пришёл новый список —
/// берётся он, старый бросается.
fn spawn_preloader(cache: SharedCache) -> mpsc::Sender<Vec<String>> {
    let (tx, rx) = mpsc::channel::<Vec<String>>();
    std::thread::Builder::new()
        .name("escanor-soundpad-load".into())
        .spawn(move || {
            let mut next = None;
            loop {
                let paths = match next.take() {
                    Some(paths) => paths,
                    None => match rx.recv() {
                        Ok(paths) => paths,
                        Err(_) => return,
                    },
                };
                for path in paths {
                    if let Ok(newer) = rx.try_recv() {
                        next = Some(newer);
                        break;
                    }
                    if cache.lock().unwrap().clips.contains_key(&path) {
                        continue;
                    }
                    // Ошибку покажем при нажатии.
                    let Ok(clip) = decode(Path::new(&path)) else { continue };
                    let mut cache = cache.lock().unwrap();
                    // Предзагрузка не вытесняет: память кончилась — остальное загрузится при нажатии,
                    // а не будет декодироваться впустую при каждой смене раскладки.
                    if !cache.fits(&clip) {
                        break;
                    }
                    cache.insert(&path, Arc::new(clip));
                }
            }
        })
        .expect("не удалось создать поток саундпада");
    tx
}

/// `cpal::Stream` нельзя передавать между потоками, поэтому он живёт в своём потоке.
struct Output {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Поток сообщил о неисправимой ошибке (устройство отключили).
    failed: Arc<AtomicBool>,
}

impl Output {
    fn start(device_id: Option<&str>, mixer: SharedMixer) -> Result<Self> {
        let device = crate::audio::find_output(device_id)?;
        let failed = Arc::new(AtomicBool::new(false));
        let failed_flag = failed.clone();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let thread = std::thread::Builder::new().name("escanor-soundpad".into()).spawn(move || {
            match build_stream(&device, mixer, failed_flag) {
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
        ready_rx.recv().context("поток саундпада завершился")??;
        Ok(Self { stop: Some(stop_tx), thread: Some(thread), failed })
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn build_stream(device: &cpal::Device, mixer: SharedMixer, failed: Arc<AtomicBool>) -> Result<cpal::Stream> {
    let supported = device.default_output_config()?;
    let config: StreamConfig = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => make_stream::<f32>(device, config, mixer, failed)?,
        SampleFormat::I16 => make_stream::<i16>(device, config, mixer, failed)?,
        SampleFormat::I32 => make_stream::<i32>(device, config, mixer, failed)?,
        SampleFormat::U16 => make_stream::<u16>(device, config, mixer, failed)?,
        other => return Err(anyhow!("формат {other:?} не поддерживается")),
    };
    stream.play()?;
    Ok(stream)
}

fn make_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: StreamConfig,
    mixer: SharedMixer,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream> {
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _| mixer.lock().unwrap().render(out, channels, rate),
        move |e| {
            log::warn!("ошибка вывода саундпада: {e}");
            if crate::audio::is_fatal(&e) {
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

    fn tone(amplitude: f32, frames: usize) -> Vec<i16> {
        (0..frames).map(|i| (amplitude * (i as f32 * 0.05).sin() * 32767.0) as i16).collect()
    }

    fn clip(samples: Vec<i16>) -> Clip {
        Clip::new(samples, 1, 48_000)
    }

    fn whole(path: &str) -> SoundRef {
        SoundRef { path: path.into(), start_ms: 0, end_ms: None }
    }

    #[test]
    fn loud_and_quiet_sounds_meet_at_one_level() {
        let loud = clip(tone(0.9, 48_000));
        let quiet = clip(tone(0.05, 48_000));
        let level = |c: &Clip| {
            let scaled: Vec<i16> = c.samples.iter().map(|&s| (s as f32 * c.gain) as i16).collect();
            loudness_gain(&scaled, 1, 48_000)
        };
        assert!(loud.gain < 1.0 && quiet.gain > 1.0);
        assert!((level(&loud) - 1.0).abs() < 0.01, "после выравнивания громкость уже общая");
        assert!((level(&quiet) - 1.0).abs() < 0.01);
    }

    #[test]
    fn silence_is_ignored_and_peaks_are_limited() {
        let mut speech = tone(0.3, 4_800);
        speech.extend(vec![0; 48_000]);
        assert!((loudness_gain(&speech, 1, 48_000) - loudness_gain(&tone(0.3, 4_800), 1, 48_000)).abs() < 0.01);

        // Тихий фон с одним громким щелчком: усиление упирается в пик.
        let mut click = tone(0.01, 48_000);
        click[100] = 29_491;
        assert!((loudness_gain(&click, 1, 48_000) - PEAK_LIMIT / 0.9).abs() < 1e-3);
        assert_eq!(loudness_gain(&[0; 1000], 1, 48_000), 1.0);
    }

    #[test]
    fn decodes_wav() {
        let pcm: Vec<u8> = (0..4800i16).flat_map(|i| [i, -i]).flat_map(|s| s.to_le_bytes()).collect();
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((36 + pcm.len() as u32).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        for field in [16u32.to_le_bytes(), [1, 0, 2, 0], 44_100u32.to_le_bytes(), (44_100u32 * 4).to_le_bytes()] {
            wav.extend(field);
        }
        wav.extend([4, 0, 16, 0]);
        wav.extend(b"data");
        wav.extend((pcm.len() as u32).to_le_bytes());
        wav.extend(&pcm);
        let path = std::env::temp_dir().join("escanor-soundpad-test.wav");
        std::fs::write(&path, wav).unwrap();

        let clip = decode(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((clip.channels, clip.rate, clip.frames()), (2, 44_100, 4800));
        assert_eq!(clip.samples[2..4], [1, -1]);
        assert!(decode(Path::new("/нет/такого.mp3")).is_err());
    }

    #[test]
    fn mixer_plays_mono_to_stereo_with_volume_and_drops_finished() {
        let mut mixer = Mixer { volume: 0.5, ..Default::default() };
        mixer.play(Voice::new(&whole("a"), Arc::new(clip(vec![16_384; 10])), false));
        let mut out = vec![0.0f32; 8];
        mixer.render(&mut out, 2, 48_000);
        assert!(out.iter().all(|&s| (s - 0.25).abs() < 1e-6));
        mixer.render(&mut out, 2, 48_000);
        mixer.render(&mut out, 2, 48_000);
        assert!(mixer.voices.is_empty(), "доигравший звук убирается");
        assert!(out.iter().skip(2).all(|&s| s == 0.0));
    }

    #[test]
    fn trimmed_sound_plays_only_its_part() {
        // 1 с тишины, 1 с звука, 1 с тишины при 1 кГц — миллисекунда на кадр.
        let mut samples = vec![0i16; 1000];
        samples.extend(vec![8_000; 1000]);
        samples.extend(vec![0; 1000]);
        let clip = Arc::new(Clip::new(samples, 1, 1000));
        let sound = SoundRef { path: "a".into(), start_ms: 1000, end_ms: Some(2000) };
        let mut mixer = Mixer::default();
        mixer.play(Voice::new(&sound, clip.clone(), false));
        let mut out = vec![0.0f32; 999];
        mixer.render(&mut out, 1, 1000);
        assert!(out.iter().all(|&s| s > 0.2), "начинается сразу со звука, тишина пропущена");
        mixer.render(&mut out, 1, 1000);
        assert!(mixer.voices.is_empty(), "заканчивается на конце отрезка, не доигрывая тишину");

        // Отрезок за пределами файла не выходит за него.
        let beyond = SoundRef { path: "a".into(), start_ms: 5000, end_ms: Some(9000) };
        assert_eq!(clip.range(&beyond), (3000, 3000));
    }

    #[test]
    fn finds_silent_edges_and_normalizes_by_played_part() {
        let mut samples = vec![0i16; 48_000];
        samples.extend(tone(0.2, 24_000));
        samples.extend(vec![0; 48_000]);
        let clip = Arc::new(clip(samples));
        let info = clip.info(50);
        assert_eq!(info.duration_ms, 2500);
        assert_eq!(info.peaks.len(), 50);
        assert!(info.peaks[0] == 0.0 && info.peaks[25] > 0.1);
        let (start, end) = info.content_ms;
        assert!((985..=1000).contains(&start), "звук с 1000 мс, небольшой запас до атаки: {start}");
        assert!((1500..=1560).contains(&end), "звук до 1500 мс, запас на затухание: {end}");

        // Вырезанный громкий щелчок не приглушает оставшийся звук.
        let mut with_click = tone(0.05, 48_000);
        with_click.extend(vec![32_000; 10]);
        let click = Arc::new(super::Clip::new(with_click, 1, 48_000));
        let cut = SoundRef { path: "c".into(), start_ms: 0, end_ms: Some(1000) };
        assert!(Voice::new(&cut, click.clone(), true).gain > Voice::new(&whole("c"), click, true).gain);
    }

    #[test]
    fn voices_are_capped() {
        let mut mixer = Mixer::default();
        for i in 0..MAX_VOICES + 3 {
            mixer.play(Voice::new(&whole(&i.to_string()), Arc::new(clip(vec![0; 10])), true));
        }
        assert_eq!(mixer.voices.len(), MAX_VOICES);
        assert_eq!(mixer.voices[0].path, "3", "вытесняются самые старые");
    }

    #[test]
    fn cache_evicts_least_recently_used() {
        let big = || Arc::new(clip(vec![0; CACHE_BYTES / 2 / 2 - 8]));
        let mut cache = Cache::default();
        cache.insert("a", big());
        cache.insert("b", big());
        assert!(cache.get("a").is_some());
        cache.insert("c", big());
        assert!(cache.get("b").is_none(), "вытеснен давно не игравший");
        assert!(cache.get("a").is_some() && cache.get("c").is_some());
        assert!(cache.bytes <= CACHE_BYTES);
        cache.retain(&["c".into()]);
        assert_eq!(cache.clips.len(), 1);
        assert_eq!(cache.bytes, cache.clips["c"].clip.bytes());
    }
}
