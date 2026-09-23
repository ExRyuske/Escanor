//! Сквозная проверка приёма видео: поток H.264 Baseline (как у телефона) → пакеты
//! протокола через TCP → аппаратный декодер → превью. Поток генерирует ffmpeg; без него тест пропускается.

use escanor_core::clock::ClockOffsets;
use escanor_core::protocol::{FLAG_CONFIG, FLAG_KEYFRAME};
use escanor_core::vcam::FrameOutput;
use escanor_core::video::{self, VideoCallbacks, VideoShared, VideoStats};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const FRAMES: usize = 20;

fn generate_h264() -> Option<Vec<u8>> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30"])
        .args(["-frames:v", &FRAMES.to_string(), "-c:v", "libx264", "-profile:v", "baseline", "-bf", "0"])
        .args(["-g", "600", "-bsf:v", "h264_metadata=aud=insert", "-f", "h264", "-"])
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// NAL-единицы Annex B вместе со стартовыми кодами: (тип, байты).
fn nal_units(data: &[u8]) -> Vec<(u8, &[u8])> {
    let starts: Vec<usize> = (0..data.len().saturating_sub(3)).filter(|&i| data[i..i + 3] == [0, 0, 1]).collect();
    let mut units = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        // Четырёхбайтовый стартовый код: захватываем ведущий ноль.
        let begin = if s > 0 && data[s - 1] == 0 { s - 1 } else { s };
        let end = starts.get(n + 1).map_or(data.len(), |&next| if data[next - 1] == 0 { next - 1 } else { next });
        units.push((data[s + 3] & 0x1F, &data[begin..end]));
    }
    units
}

fn packet(flags: u8, pts: i64, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(13 + payload.len());
    p.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    p.push(flags);
    p.extend_from_slice(&pts.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

#[test]
fn decodes_phone_like_stream() {
    let Some(stream) = generate_h264() else {
        eprintln!("ffmpeg с libx264 не найден — тест пропущен");
        return;
    };

    // Разбиваем на кадры по разделителям AUD (тип 9), как MediaCodec отдаёт по одному кадру.
    let units = nal_units(&stream);
    let mut frames: Vec<Vec<u8>> = Vec::new();
    for (kind, bytes) in &units {
        if *kind == 9 || frames.is_empty() {
            frames.push(Vec::new());
        }
        if *kind != 9 {
            frames.last_mut().unwrap().extend_from_slice(bytes);
        }
    }
    frames.retain(|f| !f.is_empty());
    assert_eq!(frames.len(), FRAMES);
    let config: Vec<u8> =
        units.iter().filter(|(k, _)| *k == 7 || *k == 8).take(2).flat_map(|(_, b)| b.iter().copied()).collect();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut phone = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (pc, _) = listener.accept().unwrap();

    let shared = Arc::new(VideoShared {
        stats: VideoStats::default(),
        clock: Arc::new(ClockOffsets::default()),
        realtime_timestamps: AtomicBool::new(true),
        preview: AtomicBool::new(true),
        output: Arc::new(FrameOutput::default()),
        decoder: Mutex::new(None),
    });
    let (preview_tx, preview_rx) = mpsc::channel();
    let (closed_tx, closed_rx) = mpsc::channel();
    let handle = video::spawn(
        pc,
        shared.clone(),
        VideoCallbacks {
            preview: Box::new(move |frame| {
                let _ = preview_tx.send((frame.width, frame.height, frame.rgba.len()));
            }),
            request_keyframe: Box::new(|| {}),
            closed: Box::new(move |reason| {
                let _ = closed_tx.send(reason);
            }),
        },
    );

    phone.write_all(&packet(FLAG_CONFIG, 0, &config)).unwrap();
    for (i, frame) in frames.iter().enumerate() {
        let flags = if i == 0 { FLAG_KEYFRAME } else { 0 };
        phone.write_all(&packet(flags, i as i64 * 33_333, frame)).unwrap();
        // Превью ограничено ~30 к/с; даём потоку время, как при живой съёмке.
        std::thread::sleep(Duration::from_millis(35));
    }

    let (w, h, len) = preview_rx.recv_timeout(Duration::from_secs(5)).expect("нет ни одного кадра превью");
    // 1280 шире 960 — превью уменьшается целым шагом 2.
    assert_eq!((w, h), (640, 360));
    assert_eq!(len, 640 * 360 * 4);

    drop(phone);
    let reason = closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(reason.contains("закрыт"), "{reason}");
    handle.join().unwrap();

    let stats = shared.stats.take();
    assert_eq!(stats.frames, FRAMES as u64, "декодированы все кадры");
    assert_eq!(stats.decode_errors, 0);
    let decoder = shared.decoder.lock().unwrap().clone().unwrap();
    // На серверах CI бывает Windows без видеокарты — там Media Foundation честно
    // переходит на программный режим. На Mac аппаратный VideoToolbox есть всегда.
    if cfg!(target_os = "macos") {
        assert!(decoder.contains("аппаратный"), "{decoder}");
    }
}
