//! Приём H.264 с телефона, аппаратное декодирование и раздача кадров: в виртуальную камеру и в превью.

use crate::clock::{ClockOffsets, PhoneClock};
use crate::decoder::{self, Decoder, Nv12Frame};
use crate::protocol::{FLAG_CONFIG, FLAG_KEYFRAME, read_packet};
use crate::vcam::FrameOutput;
use std::io::BufReader;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Превью в интерфейсе не нужно в полном разрешении и с полной частотой.
const PREVIEW_MAX_WIDTH: usize = 960;
const PREVIEW_INTERVAL: Duration = Duration::from_millis(33);
const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Счётчики за интервал; движок раз в секунду забирает их через `take`.
#[derive(Debug, Default)]
pub struct VideoStats {
    /// Пакеты, пришедшие от телефона (включая те, что не удалось декодировать).
    packets: AtomicU64,
    decode_errors: AtomicU64,
    frames: AtomicU64,
    bytes: AtomicU64,
    latency_sum_us: AtomicI64,
    latency_count: AtomicU64,
    decode_sum_us: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct VideoStatsSnapshot {
    pub packets: u64,
    pub decode_errors: u64,
    pub frames: u64,
    pub bytes: u64,
    pub latency_ms: Option<f32>,
    pub decode_ms: Option<f32>,
}

impl VideoStats {
    pub fn take(&self) -> VideoStatsSnapshot {
        let packets = self.packets.swap(0, Ordering::Relaxed);
        let decode_errors = self.decode_errors.swap(0, Ordering::Relaxed);
        let frames = self.frames.swap(0, Ordering::Relaxed);
        let bytes = self.bytes.swap(0, Ordering::Relaxed);
        let latency_sum = self.latency_sum_us.swap(0, Ordering::Relaxed);
        let latency_count = self.latency_count.swap(0, Ordering::Relaxed);
        let decode_sum = self.decode_sum_us.swap(0, Ordering::Relaxed);
        VideoStatsSnapshot {
            packets,
            decode_errors,
            frames,
            bytes,
            latency_ms: (latency_count > 0).then(|| latency_sum as f32 / latency_count as f32 / 1000.0),
            decode_ms: (frames > 0).then(|| decode_sum as f32 / frames as f32 / 1000.0),
        }
    }
}

/// Состояние, общее для потока приёма видео и движка.
pub struct VideoShared {
    pub stats: VideoStats,
    pub clock: Arc<ClockOffsets>,
    /// Часы, которыми телефон подписывает кадры (см. `timestamp_source`).
    pub realtime_timestamps: AtomicBool,
    pub preview: AtomicBool,
    pub output: Arc<FrameOutput>,
    /// Какой декодер сейчас работает (для интерфейса).
    pub decoder: Mutex<Option<String>>,
}

pub struct VideoCallbacks {
    pub preview: Box<dyn Fn(PreviewFrame) + Send>,
    pub request_keyframe: Box<dyn Fn() + Send>,
    /// Поток завершился (телефон отключён или сокет закрыт).
    pub closed: Box<dyn FnOnce(String) + Send>,
}

pub fn spawn(stream: TcpStream, shared: Arc<VideoShared>, callbacks: VideoCallbacks) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("escanor-video".into())
        .spawn(move || {
            let VideoCallbacks { preview, request_keyframe, closed } = callbacks;
            // Превью строится в отдельном потоке, чтобы не задерживать следующий кадр.
            let (preview_tx, preview_rx) = mpsc::sync_channel::<(usize, usize, Vec<u8>)>(1);
            let preview_thread = std::thread::Builder::new()
                .name("escanor-preview".into())
                .spawn(move || {
                    for (width, height, nv12) in preview_rx {
                        let (y, uv) = nv12.split_at(width * height);
                        let frame = Nv12Frame { width, height, y, y_stride: width, uv, uv_stride: width };
                        preview(frame.to_preview(PREVIEW_MAX_WIDTH));
                    }
                })
                .expect("не удалось создать поток превью");
            let reason = run(stream, &shared, &preview_tx, &*request_keyframe);
            drop(preview_tx);
            let _ = preview_thread.join();
            closed(reason);
        })
        .expect("не удалось создать поток видео")
}

fn run(
    stream: TcpStream,
    shared: &VideoShared,
    preview: &mpsc::SyncSender<(usize, usize, Vec<u8>)>,
    request_keyframe: &dyn Fn(),
) -> String {
    // Декодер создаётся в этом потоке: Media Foundation привязан к COM-потоку.
    let mut decoder: Box<dyn Decoder> = match decoder::create() {
        Ok(d) => d,
        Err(e) => return format!("не удалось создать декодер H.264: {e:#}"),
    };
    let mut description = String::new();

    let mut reader = BufReader::with_capacity(1 << 20, stream);
    let mut packet = Vec::new();
    let mut config: Vec<u8> = Vec::new();
    let mut waiting_keyframe = true;
    let mut last_keyframe_request: Option<Instant> = None;
    let mut last_preview = Instant::now() - PREVIEW_INTERVAL;

    let ask_keyframe = |last: &mut Option<Instant>| {
        if last.is_none_or(|t| t.elapsed() >= KEYFRAME_REQUEST_INTERVAL) {
            *last = Some(Instant::now());
            request_keyframe();
        }
    };

    loop {
        let (flags, pts) = match read_packet(&mut reader, &mut packet) {
            Ok(v) => v,
            Err(e) => return format!("видеоканал закрыт: {e}"),
        };
        shared.stats.packets.fetch_add(1, Ordering::Relaxed);
        shared.stats.bytes.fetch_add(packet.len() as u64 + 13, Ordering::Relaxed);

        if flags & FLAG_CONFIG != 0 {
            // Новая конфигурация (смена камеры или разрешения) — ждём ключевой кадр.
            if packet != config {
                config = packet.clone();
                waiting_keyframe = true;
            }
            if let Err(e) = decoder.decode(&config, pts, &mut |_| {}) {
                log::warn!("конфигурация H.264 не принята: {e:#}");
            }
            continue;
        }
        if waiting_keyframe {
            if flags & FLAG_KEYFRAME == 0 {
                ask_keyframe(&mut last_keyframe_request);
                continue;
            }
            waiting_keyframe = false;
        }

        let clock = if shared.realtime_timestamps.load(Ordering::Relaxed) {
            PhoneClock::Realtime
        } else {
            PhoneClock::Monotonic
        };
        let started = Instant::now();
        let mut decoded = 0;
        // Время обработки готового кадра вычитается, чтобы в статистике было время самого декодера.
        let mut handling = Duration::ZERO;
        let result = decoder.decode(&packet, pts, &mut |frame| {
            let handling_started = Instant::now();
            decoded += 1;
            shared.output.write(frame);
            // Задержка «сенсор → кадр готов для виртуальной камеры».
            if let Some(latency) = shared.clock.latency_us(clock, pts) {
                // Отбрасываем явную ерунду (например, пока смещение часов ещё не устоялось).
                if (0..5_000_000).contains(&latency) {
                    shared.stats.latency_sum_us.fetch_add(latency, Ordering::Relaxed);
                    shared.stats.latency_count.fetch_add(1, Ordering::Relaxed);
                }
            }
            if shared.preview.load(Ordering::Relaxed) && last_preview.elapsed() >= PREVIEW_INTERVAL {
                last_preview = Instant::now();
                let mut nv12 = vec![0; frame.width * frame.height * 3 / 2];
                frame.write_nv12(&mut nv12);
                // Поток превью занят — пропускаем кадр, а не ждём.
                let _ = preview.try_send((frame.width, frame.height, nv12));
            }
            handling += handling_started.elapsed();
        });
        if let Err(e) = result {
            log::warn!("ошибка декодирования: {e:#}");
            shared.stats.decode_errors.fetch_add(1, Ordering::Relaxed);
            waiting_keyframe = true;
            ask_keyframe(&mut last_keyframe_request);
            continue;
        }
        if decoded == 0 {
            continue;
        }
        // Аппаратный ли декодер, становится ясно только после первого SPS.
        let current = decoder.description();
        if current != description {
            *shared.decoder.lock().unwrap() = Some(current.clone());
            description = current;
        }

        let decode_time = started.elapsed().saturating_sub(handling);
        shared.stats.decode_sum_us.fetch_add(decode_time.as_micros() as u64, Ordering::Relaxed);
        shared.stats.frames.fetch_add(decoded, Ordering::Relaxed);
    }
}
