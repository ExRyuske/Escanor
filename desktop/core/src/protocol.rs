//! Протокол обмена с телефоном. Описание — docs/protocol.md.

use serde::{Deserialize, Serialize};
use std::io::{self, Read};

pub const PHONE_PORT: u16 = 27183;
pub const VERSION: u32 = 6;

pub const CHANNEL_CONTROL: u8 = 1;
pub const CHANNEL_VIDEO: u8 = 2;
pub const CHANNEL_AUDIO: u8 = 3;

pub const FLAG_CONFIG: u8 = 1;
pub const FLAG_KEYFRAME: u8 = 2;

const PACKET_HEADER: usize = 13;
/// Защита от мусора в потоке: кадр 4K в Baseline при большом битрейте сильно меньше.
const MAX_PACKET: usize = 32 * 1024 * 1024;

// --- ПК → телефон ---

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        version: u32,
    },
    GetDevices,
    Ping {
        t: i64,
    },
    StartVideo(VideoParams),
    StopVideo,
    RequestKeyframe,
    SetControls(Controls),
    StartAudio(AudioParams),
    StopAudio,
    /// Показать макропад на экране телефона.
    Macropad(MacropadLayout),
    MacropadOff,
    /// Переключатель перешёл в другое состояние.
    MacropadState {
        page: usize,
        id: u32,
        state: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MacropadLayout {
    pub columns: u32,
    pub rows: u32,
    pub orientation: crate::macropad::Orientation,
    pub amoled: crate::macropad::Amoled,
    /// Страница 0 — корневая, остальные — папки. Между страницами телефон ходит сам.
    pub pages: Vec<MacropadPage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MacropadPage {
    pub parent: Option<usize>,
    pub buttons: Vec<MacropadButton>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MacropadButton {
    /// "keys", "folder" (открывает страницу `target`) или "back".
    pub kind: &'static str,
    pub target: Option<usize>,
    pub states: Vec<MacropadState>,
    /// Какое состояние показывать.
    pub state: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MacropadState {
    pub label: String,
    /// PNG в base64.
    pub image: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VideoParams {
    pub camera: String,
    pub physical: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioParams {
    /// `None` — микрофон по умолчанию для выбранного источника.
    pub device: Option<i32>,
    pub source: String,
    pub channels: u8,
}

/// Ручные настройки камеры. Отправляются только заданные поля.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Controls {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ev: Option<i32>,
    /// "continuous" или "manual".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    /// Диоптрии: 0 — бесконечность, `min_focus_distance` — ближе всего.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_distance: Option<f32>,
    /// "auto", "daylight", "cloudy", "incandescent", "fluorescent".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub white_balance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub torch: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stabilization: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ae_lock: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awb_lock: Option<bool>,
}

impl Controls {
    /// Накладывает заданные поля `other` поверх текущих.
    pub fn merge(&mut self, other: &Controls) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f.clone(); } )* };
        }
        take!(zoom, ev, focus, focus_distance, white_balance, torch, stabilization, ae_lock, awb_lock);
    }
}

// --- телефон → ПК ---

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        version: u32,
        phone: PhoneInfo,
    },
    Devices(Devices),
    Pong {
        t: i64,
        realtime_us: i64,
        monotonic_us: i64,
    },
    Channel {
        channel: String,
    },
    VideoStarted(VideoStarted),
    VideoStopped,
    VideoError {
        message: String,
    },
    AudioStarted(AudioStarted),
    AudioStopped,
    AudioError {
        message: String,
    },
    /// Кнопка макропада нажата (`down`) или отпущена: страница и индекс по строкам.
    MacropadPress {
        page: usize,
        id: u32,
        down: bool,
    },
    Error {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct PhoneInfo {
    pub manufacturer: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Devices {
    pub cameras: Vec<CameraInfo>,
    pub microphones: Vec<MicInfo>,
    pub audio_sources: Vec<AudioSource>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CameraInfo {
    pub id: String,
    pub physical_id: Option<String>,
    pub name: String,
    pub facing: String,
    /// `[ширина, высота, максимальный fps]`, от большего к меньшему.
    pub sizes: Vec<[u32; 3]>,
    pub fps: Vec<u32>,
    pub zoom: [f32; 2],
    pub ev: [i32; 2],
    pub ev_step: f32,
    pub manual_focus: bool,
    pub min_focus_distance: f32,
    pub flash: bool,
    pub eis: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MicInfo {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AudioSource {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VideoStarted {
    pub camera: String,
    pub physical: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: String,
    pub timestamp_source: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AudioStarted {
    pub sample_rate: u32,
    pub channels: u16,
    pub device: Option<i32>,
    /// Как телефон захватывает звук: «AAudio, эксклюзивный», «AudioRecord» и т.п.
    pub api: Option<String>,
}

/// Читает медиапакет `[u32 size][u8 flags][i64 pts_us][payload]` в `buf`.
pub fn read_packet(reader: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<(u8, i64)> {
    let mut header = [0u8; PACKET_HEADER];
    reader.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
    let flags = header[4];
    let pts = i64::from_be_bytes(header[5..13].try_into().unwrap());
    if size > MAX_PACKET {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("пакет {size} байт")));
    }
    buf.resize(size, 0);
    reader.read_exact(buf)?;
    Ok((flags, pts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_serialize_flat() {
        let json = serde_json::to_string(&Request::StartAudio(AudioParams {
            device: None,
            source: "camcorder".into(),
            channels: 2,
        }))
        .unwrap();
        assert_eq!(json, r#"{"type":"start_audio","device":null,"source":"camcorder","channels":2}"#);

        let json =
            serde_json::to_string(&Request::SetControls(Controls { zoom: Some(2.0), ..Default::default() })).unwrap();
        assert_eq!(json, r#"{"type":"set_controls","zoom":2.0}"#);
    }

    #[test]
    fn macropad_orientation_names() {
        // Эти имена разбирает телефон (Macropad.orientation).
        let layout = MacropadLayout {
            columns: 1,
            rows: 1,
            orientation: crate::macropad::Orientation::ReverseLandscape,
            amoled: crate::macropad::Amoled { dim_after_secs: 30, dim_brightness: 10 },
            pages: Vec::new(),
        };
        let json = serde_json::to_string(&Request::Macropad(layout)).unwrap();
        assert!(json.contains(r#""orientation":"reverse_landscape""#), "{json}");
        assert!(json.contains(r#""amoled":{"dim_after_secs":30,"dim_brightness":10}"#), "{json}");
    }

    #[test]
    fn responses_parse() {
        let r: Response = serde_json::from_str(r#"{"type":"channel","channel":"video"}"#).unwrap();
        assert!(matches!(r, Response::Channel { channel } if channel == "video"));
        let r: Response = serde_json::from_str(r#"{"type":"something_new","x":1}"#).unwrap();
        assert!(matches!(r, Response::Unknown));
        let r: Response =
            serde_json::from_str(r#"{"type":"audio_started","sample_rate":48000,"channels":1,"device":null}"#).unwrap();
        assert!(matches!(r, Response::AudioStarted(AudioStarted { sample_rate: 48000, .. })));
    }

    #[test]
    fn packet_roundtrip() {
        let mut data = Vec::new();
        data.extend_from_slice(&3u32.to_be_bytes());
        data.push(FLAG_KEYFRAME);
        data.extend_from_slice(&123_456i64.to_be_bytes());
        data.extend_from_slice(&[9, 8, 7]);
        let mut buf = Vec::new();
        let (flags, pts) = read_packet(&mut data.as_slice(), &mut buf).unwrap();
        assert_eq!((flags, pts, buf.as_slice()), (FLAG_KEYFRAME, 123_456, &[9u8, 8, 7][..]));
    }

    #[test]
    fn controls_merge() {
        let mut a = Controls { zoom: Some(1.0), torch: Some(false), ..Default::default() };
        a.merge(&Controls { torch: Some(true), ev: Some(-2), ..Default::default() });
        assert_eq!(a, Controls { zoom: Some(1.0), torch: Some(true), ev: Some(-2), ..Default::default() });
    }
}
