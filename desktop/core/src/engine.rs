//! Движок: подключение к телефону по USB, управление потоками и виртуальной камерой.
//!
//! Интерфейс шлёт `Command` и получает `Event`. Движок помнит желаемое состояние
//! (видео, звук, настройки камеры) и восстанавливает его после переподключения кабеля.

use crate::adb::{self, Adb, AdbDevice, find_apk};
use crate::audio::{self, AudioOutput, AudioOutputInfo, AudioRing, SharedRing};
use crate::clock::{ClockOffsets, ClockSync, now_us};
use crate::keys;
use crate::macropad::{PadKind, PadLayout};
use crate::protocol::{
    AudioParams, AudioStarted, CHANNEL_AUDIO, CHANNEL_CONTROL, CHANNEL_VIDEO, Controls, Devices, MacropadButton,
    MacropadLayout, MacropadPage, MacropadState, PHONE_PORT, PhoneInfo, Request, Response, VERSION, VideoParams,
    VideoStarted,
};
use crate::vcam::{self, FrameOutput, VcamStatus, VirtualCamera};
use crate::video::{self, PreviewFrame, VideoCallbacks, VideoShared, VideoStats};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(200);
const PING_INTERVAL: Duration = Duration::from_secs(1);
const ADB_POLL_INTERVAL: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub enum Command {
    /// Подключиться к устройству (или к первому готовому) и переподключаться автоматически.
    Connect(Option<String>),
    /// Отключиться и не переподключаться, пока не попросят.
    Disconnect,
    /// Желаемое видео; `None` — остановить.
    SetVideo(Option<VideoParams>),
    /// Желаемый звук; `None` — остановить.
    SetAudio(Option<AudioParams>),
    /// Изменения настроек камеры (накладываются на текущие).
    SetControls(Controls),
    /// Устройство вывода звука (id из `AudioOutputs`); `None` — по умолчанию.
    SetAudioOutput(Option<String>),
    SetPreview(bool),
    InstallVirtualCamera,
    /// Включить или выключить виртуальную камеру (выключатель «Видео»).
    SetVirtualCamera(bool),
    RefreshAudioOutputs,
    /// Раскладка макропада; `None` — выключить.
    SetMacropad(Option<PadLayout>),
}

#[derive(Debug, Clone)]
pub enum Event {
    AdbDevices(Vec<AdbDevice>),
    AdbError(String),
    Connecting {
        stage: String,
    },
    Connected {
        serial: String,
        phone: PhoneInfo,
        devices: Devices,
    },
    Disconnected {
        reason: Option<String>,
    },
    VideoStarted(VideoStarted),
    VideoStopped,
    AudioStarted(AudioStarted),
    AudioStopped,
    AudioOutputs(Vec<AudioOutputInfo>),
    /// Последняя запись журнала DLL виртуальной камеры.
    VirtualCameraLog(String),
    /// Переключатель макропада сменил состояние по нажатию на телефоне.
    MacropadState {
        page: usize,
        id: usize,
        state: usize,
    },
    Error(String),
    Preview(PreviewFrame),
    Stats(Stats),
    VirtualCamera(VcamStatus),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    /// Пакетов видео в секунду от телефона — если кадров нет, а пакеты идут, проблема в декодере.
    pub packets_per_sec: f32,
    pub decode_errors: u64,
    pub decoder: Option<String>,
    pub fps: f32,
    pub bitrate_mbps: f32,
    /// От сенсора телефона до декодированного кадра на ПК.
    pub latency_ms: Option<f32>,
    pub decode_ms: Option<f32>,
    /// Время пути команды до телефона и обратно (USB + adb).
    pub rtt_ms: Option<f32>,
    pub audio_buffer_ms: Option<f32>,
    /// Период устройства вывода звука.
    pub audio_output_ms: Option<f32>,
    pub audio_underruns: u64,
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<Msg>,
}

impl EngineHandle {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(Msg::Command(command));
    }
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EngineHandle")
    }
}

pub fn spawn(events: EventSink) -> EngineHandle {
    let (tx, rx) = unbounded();
    let handle = EngineHandle { tx: tx.clone() };
    std::thread::Builder::new()
        .name("escanor-engine".into())
        .spawn(move || Engine::new(events, tx, rx).run())
        .expect("не удалось создать поток движка");
    handle
}

enum Msg {
    Command(Command),
    ConnectProgress { epoch: u64, stage: String },
    Connected { epoch: u64, result: Result<Connection> },
    Phone { epoch: u64, response: Response },
    Closed { epoch: u64, reason: String },
    KeyframeNeeded { epoch: u64 },
    VcamInstalled(Result<()>),
}

/// Результат рукопожатия: сокеты всех трёх каналов.
struct Connection {
    serial: String,
    control: TcpStream,
    reader: BufReader<TcpStream>,
    video: TcpStream,
    audio: TcpStream,
    phone: PhoneInfo,
    devices: Devices,
}

struct Active {
    serial: String,
    control: TcpStream,
    sockets: Vec<TcpStream>,
    devices: Devices,
    audio_running: bool,
}

struct Engine {
    events: EventSink,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    adb: Adb,

    /// Номер попытки подключения; сообщения от старых потоков отбрасываются.
    epoch: u64,
    connecting: bool,
    active: Option<Active>,
    auto_connect: bool,
    preferred_serial: Option<String>,
    adb_devices: Option<Vec<AdbDevice>>,
    adb_error: bool,

    desired_video: Option<VideoParams>,
    desired_audio: Option<AudioParams>,
    controls: Controls,
    audio_output_id: Option<String>,

    clock: Arc<ClockOffsets>,
    clock_sync: ClockSync,
    video: Arc<VideoShared>,
    ring: SharedRing,
    audio_output: Option<AudioOutput>,

    vcam: Option<VirtualCamera>,
    /// Нужна ли камера пользователю; пока видео выключено, она убрана из системы.
    vcam_wanted: bool,
    vcam_status: VcamStatus,
    vcam_log: Option<String>,

    last_ping: Instant,
    last_adb_poll: Option<Instant>,
    last_stats: Instant,

    macropad: Option<PadLayout>,
    /// Кнопки макропада, которые сейчас удерживаются (их клавиши нажаты на ПК).
    pad_held: Vec<(usize, usize)>,
}

impl Engine {
    fn new(events: EventSink, tx: Sender<Msg>, rx: Receiver<Msg>) -> Self {
        let clock = Arc::new(ClockOffsets::default());
        let video = Arc::new(VideoShared {
            stats: VideoStats::default(),
            clock: clock.clone(),
            realtime_timestamps: AtomicBool::new(true),
            preview: AtomicBool::new(true),
            output: Arc::new(FrameOutput::default()),
            decoder: Mutex::new(None),
        });
        Self {
            events,
            tx,
            rx,
            adb: Adb::locate(),
            epoch: 0,
            connecting: false,
            active: None,
            auto_connect: true,
            preferred_serial: None,
            adb_devices: None,
            adb_error: false,
            desired_video: None,
            desired_audio: None,
            controls: Controls::default(),
            audio_output_id: None,
            clock,
            clock_sync: ClockSync::default(),
            video,
            ring: Arc::new(Mutex::new(AudioRing::default())),
            audio_output: None,
            vcam: None,
            vcam_wanted: true,
            vcam_status: vcam::initial_status(),
            vcam_log: None,
            last_ping: Instant::now(),
            last_adb_poll: None,
            last_stats: Instant::now(),
            macropad: None,
            pad_held: Vec::new(),
        }
    }

    fn emit(&self, event: Event) {
        (self.events)(event);
    }

    fn run(mut self) {
        self.start_virtual_camera();
        // set_vcam_status шлёт только изменения, а интерфейс ещё не знает начальное состояние.
        self.emit(Event::VirtualCamera(self.vcam_status.clone()));
        self.emit(Event::AudioOutputs(audio::list_outputs()));
        loop {
            match self.rx.recv_timeout(TICK) {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
        }
        self.disconnect(None);
    }

    fn tick(&mut self) {
        if self.last_adb_poll.is_none_or(|t| t.elapsed() >= ADB_POLL_INTERVAL) {
            self.last_adb_poll = Some(Instant::now());
            self.poll_adb();
        }
        if self.active.is_some() && self.last_ping.elapsed() >= PING_INTERVAL {
            self.last_ping = Instant::now();
            self.send(Request::Ping { t: now_us() });
        }
        if self.last_stats.elapsed() >= Duration::from_secs(1) {
            self.report_stats();
            self.update_vcam_status();
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Command(command) => self.handle_command(command),
            Msg::ConnectProgress { epoch, stage } if epoch == self.epoch => {
                self.emit(Event::Connecting { stage });
            }
            Msg::Connected { epoch, result } if epoch == self.epoch => {
                self.connecting = false;
                match result {
                    Ok(connection) => self.on_connected(connection),
                    Err(e) => {
                        self.emit(Event::Disconnected { reason: Some(format!("{e:#}")) });
                        // Не долбим телефон повторами: следующая попытка — по кнопке или при новом подключении.
                        self.auto_connect = false;
                    }
                }
            }
            Msg::Phone { epoch, response } if epoch == self.epoch => self.on_phone(response),
            Msg::Closed { epoch, reason } if epoch == self.epoch && self.active.is_some() => {
                self.disconnect(Some(reason));
            }
            Msg::KeyframeNeeded { epoch } if epoch == self.epoch => self.send(Request::RequestKeyframe),
            Msg::VcamInstalled(result) => match result {
                Ok(()) => self.start_virtual_camera(),
                Err(e) => self.set_vcam_status(VcamStatus::Failed(format!("Установка не удалась: {e:#}"))),
            },
            _ => {}
        }
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Connect(serial) => {
                self.auto_connect = true;
                self.preferred_serial = serial;
                if self.active.is_none() && !self.connecting {
                    self.poll_adb();
                    self.try_auto_connect();
                }
            }
            Command::Disconnect => {
                self.auto_connect = false;
                self.disconnect(None);
            }
            Command::SetVideo(params) => {
                if params == self.desired_video {
                    return;
                }
                self.desired_video = params.clone();
                match params {
                    Some(p) => self.send(Request::StartVideo(p)),
                    None => self.send(Request::StopVideo),
                }
            }
            Command::SetAudio(params) => {
                if params == self.desired_audio {
                    return;
                }
                self.desired_audio = params.clone();
                match params {
                    Some(p) => {
                        self.ensure_audio_output();
                        self.send(Request::StartAudio(p));
                    }
                    None => {
                        self.send(Request::StopAudio);
                        self.audio_output = None;
                    }
                }
            }
            Command::SetControls(changes) => {
                self.controls.merge(&changes);
                self.send(Request::SetControls(changes));
            }
            Command::SetAudioOutput(id) => {
                if id != self.audio_output_id {
                    self.audio_output_id = id;
                    if self.audio_output.is_some() {
                        self.audio_output = None;
                        self.ensure_audio_output();
                    }
                }
            }
            Command::SetPreview(on) => self.video.preview.store(on, Ordering::Relaxed),
            Command::InstallVirtualCamera => {
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::VcamInstalled(vcam::install()));
                });
            }
            Command::SetVirtualCamera(wanted) => {
                if wanted != self.vcam_wanted {
                    self.vcam_wanted = wanted;
                    self.apply_vcam_wanted();
                }
            }
            Command::RefreshAudioOutputs => self.emit(Event::AudioOutputs(audio::list_outputs())),
            Command::SetMacropad(layout) => {
                if layout == self.macropad {
                    return;
                }
                self.release_pad_keys();
                self.macropad = layout;
                self.send_macropad();
            }
        }
    }

    // --- ADB и подключение ---

    fn poll_adb(&mut self) {
        match self.adb.devices() {
            Ok(devices) => {
                self.adb_error = false;
                if self.adb_devices.as_ref() != Some(&devices) {
                    // Новое устройство в списке — повод снова попробовать автоподключение.
                    let appeared = devices.iter().any(|d| {
                        d.is_ready()
                            && !self.adb_devices.iter().flatten().any(|old| old.serial == d.serial && old.is_ready())
                    });
                    if appeared && self.active.is_none() {
                        self.auto_connect = true;
                    }
                    self.adb_devices = Some(devices.clone());
                    self.emit(Event::AdbDevices(devices));
                }
                self.try_auto_connect();
            }
            Err(e) => {
                if !self.adb_error {
                    self.adb_error = true;
                    self.emit(Event::AdbError(format!("{e:#}")));
                }
            }
        }
    }

    fn try_auto_connect(&mut self) {
        if !self.auto_connect || self.active.is_some() || self.connecting {
            return;
        }
        let devices = self.adb_devices.clone().unwrap_or_default();
        let target = match &self.preferred_serial {
            Some(serial) => devices.iter().find(|d| &d.serial == serial && d.is_ready()),
            None => devices.iter().find(|d| d.is_ready()),
        };
        let Some(device) = target else { return };
        let serial = device.serial.clone();

        self.epoch += 1;
        self.connecting = true;
        self.preferred_serial = Some(serial.clone());
        let (epoch, adb, tx) = (self.epoch, self.adb.clone(), self.tx.clone());
        std::thread::Builder::new()
            .name("escanor-connect".into())
            .spawn(move || {
                let progress = |stage: &str| {
                    let _ = tx.send(Msg::ConnectProgress { epoch, stage: stage.to_string() });
                };
                let result = connect(&adb, &serial, &progress);
                let _ = tx.send(Msg::Connected { epoch, result });
            })
            .expect("не удалось создать поток подключения");
    }

    fn on_connected(&mut self, c: Connection) {
        let epoch = self.epoch;
        let sockets = [&c.control, &c.video, &c.audio].into_iter().filter_map(|s| s.try_clone().ok()).collect();

        let tx = self.tx.clone();
        let mut reader = c.reader;
        std::thread::Builder::new()
            .name("escanor-control".into())
            .spawn(move || {
                let mut line = String::new();
                let reason = loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break "телефон закрыл соединение".to_string(),
                        Ok(_) => match serde_json::from_str::<Response>(&line) {
                            Ok(response) => {
                                let _ = tx.send(Msg::Phone { epoch, response });
                            }
                            Err(e) => log::warn!("непонятное сообщение {line:?}: {e}"),
                        },
                        Err(e) => break format!("управляющий канал: {e}"),
                    }
                };
                let _ = tx.send(Msg::Closed { epoch, reason });
            })
            .expect("не удалось создать поток управления");

        self.ring.lock().unwrap().reset(48_000, 1);
        let tx = self.tx.clone();
        audio::spawn_receiver(
            c.audio,
            self.ring.clone(),
            Box::new(move |reason| {
                let _ = tx.send(Msg::Closed { epoch, reason });
            }),
        );

        let (tx_key, tx_closed, events) = (self.tx.clone(), self.tx.clone(), self.events.clone());
        video::spawn(
            c.video,
            self.video.clone(),
            VideoCallbacks {
                preview: Box::new(move |frame| events(Event::Preview(frame))),
                request_keyframe: Box::new(move || {
                    let _ = tx_key.send(Msg::KeyframeNeeded { epoch });
                }),
                closed: Box::new(move |reason| {
                    let _ = tx_closed.send(Msg::Closed { epoch, reason });
                }),
            },
        );

        self.clock_sync.reset(&self.clock);
        self.active = Some(Active {
            serial: c.serial.clone(),
            control: c.control,
            sockets,
            devices: c.devices.clone(),
            audio_running: false,
        });
        self.emit(Event::Connected { serial: c.serial, phone: c.phone, devices: c.devices });
        self.send(Request::Ping { t: now_us() });
        self.last_ping = Instant::now();
        self.restore_desired_state();
        self.send_macropad();
    }

    /// После переподключения запускаем то, что было выбрано, если такая камера/микрофон есть.
    fn restore_desired_state(&mut self) {
        let Some(active) = &self.active else { return };
        let has_camera =
            |p: &VideoParams| active.devices.cameras.iter().any(|c| c.id == p.camera && c.physical_id == p.physical);
        let video = self.desired_video.clone().filter(has_camera);
        let has_mic = |p: &AudioParams| p.device.is_none_or(|id| active.devices.microphones.iter().any(|m| m.id == id));
        let audio = self.desired_audio.clone().filter(has_mic);
        if let Some(p) = video {
            self.send(Request::StartVideo(p));
            if self.controls != Controls::default() {
                self.send(Request::SetControls(self.controls.clone()));
            }
        }
        if let Some(p) = audio {
            self.ensure_audio_output();
            self.send(Request::StartAudio(p));
        }
    }

    fn on_phone(&mut self, response: Response) {
        match response {
            Response::Pong { t, realtime_us, monotonic_us } => {
                self.clock_sync.on_pong(t, realtime_us, monotonic_us, &self.clock)
            }
            Response::VideoStarted(info) => {
                self.video.realtime_timestamps.store(info.timestamp_source == "realtime", Ordering::Relaxed);
                self.emit(Event::VideoStarted(info));
            }
            Response::VideoStopped => self.emit(Event::VideoStopped),
            Response::VideoError { message } => {
                // Не пытаемся бесконечно запускать то, что телефон не может.
                self.desired_video = None;
                self.emit(Event::VideoStopped);
                self.emit(Event::Error(format!("Видео: {message}")));
            }
            Response::AudioStarted(info) => {
                self.ring.lock().unwrap().reset(info.sample_rate, info.channels);
                if let Some(a) = &mut self.active {
                    a.audio_running = true;
                }
                self.emit(Event::AudioStarted(info));
            }
            Response::AudioStopped => {
                if let Some(a) = &mut self.active {
                    a.audio_running = false;
                }
                self.emit(Event::AudioStopped);
            }
            Response::AudioError { message } => {
                if let Some(a) = &mut self.active {
                    a.audio_running = false;
                }
                self.desired_audio = None;
                self.audio_output = None;
                self.emit(Event::AudioStopped);
                self.emit(Event::Error(format!("Звук: {message}")));
            }
            Response::Error { message } => self.emit(Event::Error(message)),
            Response::MacropadPress { page, id, down } => self.press(page, id as usize, down),
            Response::Devices(devices) => {
                if let Some(a) = &mut self.active {
                    a.devices = devices;
                }
            }
            Response::Hello { .. } | Response::Channel { .. } | Response::Unknown => {}
        }
    }

    fn send(&mut self, request: Request) {
        let Some(active) = &mut self.active else { return };
        let mut line = serde_json::to_string(&request).expect("сериализация запроса");
        line.push('\n');
        if let Err(e) = active.control.write_all(line.as_bytes()) {
            let reason = format!("не удалось отправить команду: {e}");
            self.disconnect(Some(reason));
        }
    }

    fn disconnect(&mut self, reason: Option<String>) {
        self.epoch += 1;
        self.connecting = false;
        self.release_pad_keys();
        let Some(active) = self.active.take() else { return };
        for socket in &active.sockets {
            let _ = socket.shutdown(Shutdown::Both);
        }
        self.adb.remove_forward(&active.serial, PHONE_PORT);
        self.clock_sync.reset(&self.clock);
        self.emit(Event::Disconnected { reason });
    }

    // --- Макропад ---

    fn send_macropad(&mut self) {
        let Some(layout) = &self.macropad else {
            self.send(Request::MacropadOff);
            return;
        };
        let encode = |png: &Vec<u8>| base64::engine::general_purpose::STANDARD.encode(png);
        let message = MacropadLayout {
            columns: layout.columns,
            rows: layout.rows,
            orientation: layout.orientation,
            amoled: layout.amoled,
            pages: layout
                .pages
                .iter()
                .map(|page| MacropadPage {
                    parent: page.parent,
                    buttons: page
                        .buttons
                        .iter()
                        .map(|b| {
                            let (kind, target) = match b.kind {
                                PadKind::Keys => ("keys", None),
                                PadKind::Folder(page) => ("folder", Some(page)),
                                PadKind::Back => ("back", None),
                            };
                            MacropadButton {
                                kind,
                                target,
                                states: b
                                    .states
                                    .iter()
                                    .map(|s| MacropadState {
                                        label: s.label.clone(),
                                        image: s.image_png.as_ref().map(encode),
                                    })
                                    .collect(),
                                state: b.state,
                            }
                        })
                        .collect(),
                })
                .collect(),
        };
        self.send(Request::Macropad(message));
    }

    /// Нажатие или отпускание кнопки на странице `page`. Папки и «Назад» телефон
    /// открывает сам — сюда приходят только кнопки с клавишами.
    fn press(&mut self, page: usize, index: usize, down: bool) {
        let Some(button) = self.macropad.as_mut().and_then(|l| l.button_mut(page, index)) else { return };
        if button.kind != PadKind::Keys {
            return;
        }
        if let Some(target) = button.launch.clone() {
            if down && let Err(e) = crate::launcher::launch(&target) {
                self.emit(Event::Error(format!("Макропад: {e:#}")));
            }
            return;
        }
        if button.toggle {
            // Переключатель срабатывает на касание: короткое нажатие сочетания и смена состояния.
            if !down {
                return;
            }
            let combo = button.keys;
            button.state = (button.state + 1) % button.states.len().max(1);
            let state = button.state;
            let result = if combo.is_empty() {
                Ok(())
            } else {
                keys::send(&combo, true).and_then(|_| keys::send(&combo, false))
            };
            self.send(Request::MacropadState { page, id: index as u32, state });
            self.emit(Event::MacropadState { page, id: index, state });
            if let Err(e) = result {
                self.emit(Event::Error(format!("Макропад: {e:#}")));
            }
            return;
        }

        let combo = button.keys;
        if combo.is_empty() {
            return;
        }
        if down {
            self.pad_held.push((page, index));
        } else if let Some(pos) = self.pad_held.iter().position(|&held| held == (page, index)) {
            self.pad_held.remove(pos);
        } else {
            return;
        }
        if let Err(e) = keys::send(&combo, down) {
            self.emit(Event::Error(format!("Макропад: {e:#}")));
        }
    }

    /// Отпускает всё, что удерживается: при отключении телефона клавиши не должны «залипнуть».
    fn release_pad_keys(&mut self) {
        for (page, index) in std::mem::take(&mut self.pad_held) {
            if let Some(button) = self.macropad.as_ref().and_then(|l| l.button(page, index)) {
                let _ = keys::send(&button.keys, false);
            }
        }
    }

    // --- Звук ---

    fn ensure_audio_output(&mut self) {
        if self.audio_output.is_some() {
            return;
        }
        match AudioOutput::start(self.audio_output_id.as_deref(), self.ring.clone()) {
            Ok(output) => self.audio_output = Some(output),
            Err(e) => self.emit(Event::Error(format!("Не удалось открыть устройство вывода: {e:#}"))),
        }
    }

    // --- Статистика и виртуальная камера ---

    fn report_stats(&mut self) {
        let elapsed = self.last_stats.elapsed().as_secs_f32();
        self.last_stats = Instant::now();
        let video = self.video.stats.take();
        let Some(active) = &self.active else { return };
        let ring = self.ring.lock().unwrap();
        self.emit(Event::Stats(Stats {
            packets_per_sec: video.packets as f32 / elapsed,
            decode_errors: video.decode_errors,
            decoder: self.video.decoder.lock().unwrap().clone(),
            fps: video.frames as f32 / elapsed,
            bitrate_mbps: video.bytes as f32 * 8.0 / elapsed / 1_000_000.0,
            latency_ms: video.latency_ms,
            decode_ms: video.decode_ms,
            rtt_ms: self.clock.rtt_us().map(|us| us as f32 / 1000.0),
            audio_buffer_ms: active.audio_running.then(|| ring.buffered_ms()),
            audio_output_ms: self.audio_output.as_ref().and_then(|o| o.period_ms),
            audio_underruns: ring.underruns(),
        }));
    }

    fn start_virtual_camera(&mut self) {
        if !cfg!(windows) {
            self.set_vcam_status(VcamStatus::Unsupported);
            return;
        }
        match vcam::installation() {
            vcam::Installation::Missing => return self.set_vcam_status(VcamStatus::NotInstalled),
            vcam::Installation::Outdated => return self.set_vcam_status(VcamStatus::Outdated),
            vcam::Installation::Current => {}
        }
        if self.vcam.is_some() {
            return;
        }
        match VirtualCamera::start() {
            Ok(camera) => {
                self.vcam = Some(camera);
                self.set_vcam_status(VcamStatus::Ready);
                if !self.vcam_wanted {
                    self.apply_vcam_wanted();
                }
            }
            Err(e) => self.set_vcam_status(VcamStatus::Failed(format!("{e:#}"))),
        }
    }

    fn apply_vcam_wanted(&mut self) {
        let Some(camera) = &self.vcam else { return };
        match camera.set_enabled(self.vcam_wanted) {
            Ok(()) => self.set_vcam_status(if self.vcam_wanted { VcamStatus::Ready } else { VcamStatus::Off }),
            Err(e) => self.set_vcam_status(VcamStatus::Failed(format!("{e:#}"))),
        }
    }

    fn update_vcam_status(&mut self) {
        if let Some(line) = vcam::last_log_line().filter(|l| Some(l) != self.vcam_log.as_ref()) {
            self.vcam_log = Some(line.clone());
            self.emit(Event::VirtualCameraLog(line));
        }
        if self.vcam.is_none() || !self.vcam_wanted {
            return;
        }
        let status = match self.video.output.consumer() {
            Some((width, height)) => VcamStatus::InUse { width, height },
            None => VcamStatus::Ready,
        };
        self.set_vcam_status(status);
    }

    fn set_vcam_status(&mut self, status: VcamStatus) {
        if status != self.vcam_status || matches!(status, VcamStatus::Failed(_) | VcamStatus::Unsupported) {
            self.vcam_status = status.clone();
            self.emit(Event::VirtualCamera(status));
        }
    }
}

// --- Рукопожатие (в отдельном потоке) ---

fn connect(adb: &Adb, serial: &str, progress: &dyn Fn(&str)) -> Result<Connection> {
    progress("Проверяю приложение на телефоне…");
    // Обновляем приложение на телефоне, если оно отличается от APK рядом с программой.
    let apk = find_apk();
    let installed = adb.is_installed(serial)?;
    let outdated = installed
        && apk.as_ref().is_some_and(|apk| {
            let local = adb::file_hash(apk).ok();
            let remote = adb.installed_apk_hash(serial);
            local.is_some() && remote.is_some() && local != remote
        });
    if !installed || outdated {
        let apk = apk.ok_or_else(|| anyhow!("На телефоне нет Escanor, а escanor.apk рядом с программой не найден"))?;
        progress(if installed {
            "Обновляю Escanor на телефоне…"
        } else {
            "Устанавливаю Escanor на телефон…"
        });
        adb.install(serial, &apk)?;
    }
    progress("Запускаю Escanor на телефоне…");
    adb.launch(serial)?;
    adb.forward(serial, PHONE_PORT, PHONE_PORT)?;

    progress("Жду ответа телефона…");
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut reinstalled = false;
    let (control, mut reader, phone) = loop {
        match handshake() {
            Ok((_, _, version, _)) if version != VERSION => {
                let apk = find_apk().filter(|_| !reinstalled).ok_or_else(|| {
                    anyhow!("Версия Escanor на телефоне ({version}) не совпадает с программой ({VERSION})")
                })?;
                progress("Обновляю Escanor на телефоне…");
                adb.install(serial, &apk)?;
                adb.launch(serial)?;
                reinstalled = true;
            }
            Ok((control, reader, _, phone)) => break (control, reader, phone),
            Err(e) if Instant::now() >= deadline => {
                return Err(e.context("Телефон не отвечает: разблокируйте экран и проверьте, что Escanor запущен"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    };

    let video = open_channel(CHANNEL_VIDEO)?;
    let audio = open_channel(CHANNEL_AUDIO)?;
    let mut writer = control.try_clone()?;
    writeln!(writer, "{}", serde_json::to_string(&Request::GetDevices)?)?;

    // Ждём подтверждения обоих медиаканалов и список устройств.
    let (mut video_ok, mut audio_ok, mut devices) = (false, false, None);
    let mut line = String::new();
    while !(video_ok && audio_ok && devices.is_some()) {
        line.clear();
        if reader.read_line(&mut line).context("чтение ответа телефона")? == 0 {
            bail!("телефон закрыл соединение во время подключения");
        }
        match serde_json::from_str::<Response>(&line) {
            Ok(Response::Channel { channel }) if channel == "video" => video_ok = true,
            Ok(Response::Channel { channel }) if channel == "audio" => audio_ok = true,
            Ok(Response::Devices(d)) => devices = Some(d),
            Ok(Response::Error { message }) => bail!("телефон: {message}"),
            _ => {}
        }
    }
    reader.get_ref().set_read_timeout(None)?;

    Ok(Connection { serial: serial.to_string(), control, reader, video, audio, phone, devices: devices.unwrap() })
}

fn phone_addr() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, PHONE_PORT))
}

fn open_channel(channel: u8) -> Result<TcpStream> {
    let mut stream = TcpStream::connect_timeout(&phone_addr(), Duration::from_secs(2))?;
    stream.set_nodelay(true)?;
    stream.write_all(&[channel])?;
    Ok(stream)
}

/// Открывает управляющий канал и обменивается hello.
/// adb принимает соединение даже когда на телефоне никто не слушает и сразу его закрывает,
/// поэтому успехом считается только полученный ответ.
fn handshake() -> Result<(TcpStream, BufReader<TcpStream>, u32, PhoneInfo)> {
    let mut control = open_channel(CHANNEL_CONTROL)?;
    control.set_read_timeout(Some(Duration::from_secs(3)))?;
    writeln!(control, "{}", serde_json::to_string(&Request::Hello { version: VERSION })?)?;
    let mut reader = BufReader::new(control.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        bail!("сервер на телефоне ещё не запущен");
    }
    match serde_json::from_str::<Response>(&line)? {
        Response::Hello { version, phone } => Ok((control, reader, version, phone)),
        other => bail!("неожиданный ответ: {other:?}"),
    }
}
