//! Состояние интерфейса и реакция на действия пользователя и события движка.

use crate::settings::{self, BACK_SLOT, ButtonSettings, MacropadSettings, Settings, StateSettings, TOGGLE_STATES};
use crate::system;
use crate::tray::{Tray, TrayCommand};
use crate::update::{self, Release};
use escanor_core::adb::AdbDevice;
use escanor_core::audio::{AudioOutputInfo, looks_virtual};
use escanor_core::keys::{Key, KeyCombo};
use escanor_core::macropad::{Orientation, PadButton, PadKind, PadLayout, PadPage, PadState};
use escanor_core::protocol::{
    AudioParams, AudioStarted, CameraInfo, Controls, Devices, PhoneInfo, VideoParams, VideoStarted,
};
use escanor_core::vcam::VcamStatus;
use escanor_core::{Command, EngineHandle, Event, Stats};
use iced::widget::image;
use iced::{Task, window};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

pub const BITRATES: [u32; 6] = [6, 10, 16, 24, 32, 48];
/// 60 к/с вдвое сокращают время кадра на всём пути: сенсор, конвейер камеры, кодировщик.
const DEFAULT_FPS: u32 = 60;
const PREFERRED_SIZE: (u32, u32) = (1920, 1080);

#[derive(Debug, Clone)]
pub enum Message {
    EngineReady(EngineHandle),
    Engine(Event),

    SelectDevice(DeviceChoice),
    Connect,
    Disconnect,

    VideoEnabled(bool),
    SelectCamera(CameraChoice),
    SelectResolution(ResolutionChoice),
    SelectFps(FpsChoice),
    SelectBitrate(BitrateChoice),

    Zoom(f32),
    Ev(i32),
    ManualFocus(bool),
    FocusDistance(f32),
    WhiteBalance(WbChoice),
    Torch(bool),
    Stabilization(bool),
    AeLock(bool),
    AwbLock(bool),

    AudioEnabled(bool),
    SelectMic(MicChoice),
    SelectSource(SourceChoice),
    Stereo(bool),
    SelectOutput(OutputChoice),
    RefreshOutputs,

    PreviewEnabled(bool),
    InstallVcam,
    DismissError,

    SelectTab(Tab),

    WindowOpened(window::Id),
    CloseRequested(window::Id),
    Tray(TrayCommand),
    SetAutostart(bool),
    SetCloseToTray(bool),
    SetCheckUpdates(bool),
    OpenSettingsFolder,
    CheckUpdates,
    UpdateChecked(Result<Option<Release>, String>),
    InstallUpdate,
    UpdateInstalled(Result<(), String>),
    /// Раскрыть/свернуть дополнительный блок.
    Toggle(Panel),
    PadEnabled(bool),
    PadColumns(u32),
    PadRows(u32),
    PadOrientation(Orientation),
    PadLabel(String),
    /// Обычная кнопка, переключатель или папка.
    PadKind(ButtonKind),
    /// Открыть выбранную папку / вернуться на уровень выше.
    PadOpen(usize),
    PadUp,
    /// Перетаскивание: нажали на ячейку, навели на другую, отпустили.
    PadDragStart(usize),
    PadDragEnter(usize),
    PadDragExit(usize),
    PadDrop(usize),
    PadDragCancel,
    PadDim(DimChoice),
    /// Яркость телефона в затемнении: ползунок двигается, отпустили — отправляем на телефон.
    PadDimBrightness(u32),
    PadDimCommit,
    /// Иконка из самой программы кнопки.
    PadAppIcon,
    /// Путь или адрес для кнопки-программы.
    PadTarget(String),
    PadPickApp,
    PadAppPicked(Option<PathBuf>),
    /// Файл, перетащенный на окно из Проводника: становится кнопкой-программой.
    FileDropped(PathBuf),
    /// Выбрать состояние переключателя: оно сразу становится текущим (без нажатия клавиш).
    PadState(usize),
    PadTint(Option<[u8; 3]>),
    PadTintHex(String),
    /// Начать/отменить запись сочетания с клавиатуры.
    PadRecord(bool),
    /// Клавиатура или мышь во время записи сочетания.
    PadInput(iced::Event),
    PadModifier(Modifier, bool),
    PadKey(Key),
    PadClearKeys,
    PadPickImage,
    PadImagePicked(Option<PathBuf>),
    PadClearImage,
}

/// Сворачиваемые блоки интерфейса: второстепенные настройки скрыты по умолчанию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    VideoQuality,
    Image,
    AudioProcessing,
    ManualKeys,
    StatsDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Normal,
    Toggle,
    Folder,
    App,
}

/// Через сколько приглушать экран телефона в режиме AMOLED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DimChoice(pub u32);

impl DimChoice {
    pub const ALL: [DimChoice; 6] =
        [DimChoice(0), DimChoice(15), DimChoice(30), DimChoice(60), DimChoice(120), DimChoice(300)];
}

impl std::fmt::Display for DimChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            0 => f.write_str("Не затемнять"),
            s if s < 60 => write!(f, "Затемнять через {s} с"),
            s => write!(f, "Затемнять через {} мин", s / 60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Win,
}

/// Картинка кнопки: имя файла и цвет перекрашивания.
type ImageKey = (String, Option<[u8; 3]>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Stream,
    Macropad,
    Settings,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum Connection {
    Idle { reason: Option<String> },
    Connecting { stage: String },
    Connected { phone: PhoneInfo },
}

pub struct App {
    engine: Option<EngineHandle>,

    pub adb_devices: Vec<AdbDevice>,
    pub adb_error: Option<String>,
    pub device: Option<DeviceChoice>,
    pub connection: Connection,
    pub devices: Devices,

    pub video_enabled: bool,
    pub camera: Option<CameraChoice>,
    pub resolution: Option<ResolutionChoice>,
    pub fps: FpsChoice,
    pub bitrate: BitrateChoice,
    pub video: Option<VideoStarted>,

    pub zoom: f32,
    pub ev: i32,
    pub manual_focus: bool,
    pub focus_distance: f32,
    pub white_balance: WbChoice,
    pub torch: bool,
    pub stabilization: bool,
    pub ae_lock: bool,
    pub awb_lock: bool,

    pub audio_enabled: bool,
    pub mic: MicChoice,
    pub source: Option<SourceChoice>,
    pub stereo: bool,
    pub outputs: Vec<AudioOutputInfo>,
    pub output: Option<OutputChoice>,
    pub audio: Option<AudioStarted>,

    pub preview_enabled: bool,
    pub preview: Option<image::Handle>,
    pub stats: Option<Stats>,
    /// `None` — движок ещё не сообщил статус.
    pub vcam: Option<VcamStatus>,
    pub vcam_log: Option<String>,
    pub error: Option<String>,

    pub tab: Tab,
    pub pad: MacropadSettings,
    /// Открытая папка: номера папок от корня; пустой путь — корневая страница.
    pub pad_path: Vec<usize>,
    /// Выбранная кнопка на открытой странице.
    pub pad_selected: usize,
    /// Перетаскиваемая кнопка и ячейка под курсором.
    pub pad_drag: Option<usize>,
    pub pad_hover: Option<usize>,
    /// Номер страницы у телефона → путь к ней (собирается в `apply_pad`).
    pad_pages: Vec<Vec<usize>>,
    /// Ждём нажатия сочетания для выбранной кнопки.
    pub recording: bool,
    /// Модификаторы, зажатые во время записи (у событий мыши их нет).
    record_modifiers: iced::keyboard::Modifiers,
    open_panels: Vec<Panel>,
    /// Текст поля HEX-цвета (пока вводится, может быть неполным).
    pub tint_input: String,
    /// Готовые картинки: (файл, цвет) → PNG и дескриптор для превью; `None` — файла нет.
    pad_images: HashMap<ImageKey, Option<(Vec<u8>, image::Handle)>>,
    /// Звук ещё не включали вручную — включим сами, если найдётся виртуальный кабель.
    audio_auto: bool,
    /// Последнее сохранённое состояние, чтобы писать файл только при изменениях.
    saved: Settings,

    pub close_to_tray: bool,
    pub check_updates: bool,
    pub autostart: bool,
    pub update: UpdateState,
    tray: Option<Tray>,
    window: Option<window::Id>,
    /// Окно спрятано в трей.
    hidden: bool,
}

impl App {
    /// Запуск: `start_hidden` — Escanor стартовал вместе с Windows и должен сразу уйти в трей.
    pub fn boot(saved: Settings, start_hidden: bool) -> (Self, Task<Message>) {
        let mut app = Self::new(saved);
        app.tray = Tray::new();
        app.hidden = start_hidden && app.tray.is_some();
        if let Some(tray) = &app.tray {
            tray.set_hidden(app.hidden);
        }
        app.autostart = system::autostart_enabled();
        app.refresh_pad_images();
        let task = if app.check_updates { app.start_update_check() } else { Task::none() };
        (app, task)
    }

    pub fn new(saved: Settings) -> Self {
        let camera = saved.camera.clone().map(|(id, physical)| CameraChoice {
            index: usize::MAX,
            name: "…".into(),
            id,
            physical,
        });
        let resolution = saved.resolution.map(|(width, height)| ResolutionChoice { width, height, max_fps: 0 });
        Self {
            engine: None,
            adb_devices: Vec::new(),
            adb_error: None,
            device: None,
            connection: Connection::Idle { reason: None },
            devices: Devices::default(),
            video_enabled: saved.video_enabled,
            camera,
            resolution,
            fps: FpsChoice(saved.fps),
            bitrate: BitrateChoice(saved.bitrate),
            video: None,
            zoom: saved.zoom,
            ev: saved.ev,
            manual_focus: saved.manual_focus,
            focus_distance: saved.focus_distance,
            white_balance: WbChoice::from_id(&saved.white_balance),
            torch: false,
            stabilization: saved.stabilization,
            ae_lock: saved.ae_lock,
            awb_lock: saved.awb_lock,
            audio_enabled: saved.audio_enabled.unwrap_or(false),
            mic: saved
                .mic
                .map_or_else(MicChoice::auto, |id| MicChoice { id: Some(id), name: format!("Микрофон {id}") }),
            source: saved.source.clone().map(|id| SourceChoice { name: id.clone(), id }),
            stereo: saved.stereo,
            outputs: Vec::new(),
            output: saved.output.clone().map(|id| OutputChoice { name: "…".into(), id: Some(id) }),
            audio: None,
            preview_enabled: saved.preview_enabled,
            preview: None,
            stats: None,
            vcam: None,
            vcam_log: None,
            error: None,
            tab: Tab::Stream,
            pad: saved.macropad.clone(),
            pad_path: Vec::new(),
            pad_selected: 0,
            pad_drag: None,
            pad_hover: None,
            pad_pages: Vec::new(),
            recording: false,
            record_modifiers: iced::keyboard::Modifiers::default(),
            open_panels: Vec::new(),
            tint_input: String::new(),
            pad_images: HashMap::new(),
            audio_auto: saved.audio_enabled.is_none(),
            close_to_tray: saved.close_to_tray,
            check_updates: saved.check_updates,
            autostart: false,
            update: UpdateState::Idle,
            tray: None,
            window: None,
            hidden: false,
            saved,
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = match message {
            Message::PadPickApp => Task::perform(
                async {
                    let dialog = rfd::AsyncFileDialog::new().set_title("Программа для кнопки");
                    let dialog = if cfg!(windows) {
                        dialog.add_filter("Программы и ярлыки", &["exe", "lnk", "bat", "cmd", "url"])
                    } else {
                        dialog
                    };
                    dialog.add_filter("Все файлы", &["*"]).pick_file().await.map(|f| f.path().to_path_buf())
                },
                Message::PadAppPicked,
            ),
            Message::PadPickImage => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Картинка для кнопки")
                        .add_filter("Изображения", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
                        .pick_file()
                        .await
                        .map(|f| f.path().to_path_buf())
                },
                Message::PadImagePicked,
            ),
            Message::WindowOpened(id) => {
                self.window = Some(id);
                // Трей не создался — прятать окно некуда, иначе программа стала бы невидимой.
                if self.hidden { window::set_mode(id, window::Mode::Hidden) } else { Task::none() }
            }
            Message::CloseRequested(id) => {
                if self.close_to_tray && self.tray.is_some() {
                    self.set_hidden(true);
                    window::set_mode(id, window::Mode::Hidden)
                } else {
                    self.quit()
                }
            }
            Message::Tray(TrayCommand::Toggle) => {
                let hide = !self.hidden;
                self.set_hidden(hide);
                match self.window {
                    Some(id) if hide => window::set_mode(id, window::Mode::Hidden),
                    Some(id) => Task::batch([window::set_mode(id, window::Mode::Windowed), window::gain_focus(id)]),
                    None => Task::none(),
                }
            }
            Message::Tray(TrayCommand::Quit) => self.quit(),
            Message::CheckUpdates => self.start_update_check(),
            Message::InstallUpdate => {
                let UpdateState::Available(release) = self.update.clone() else { return Task::none() };
                self.update = UpdateState::Installing(release.clone());
                background(move || update::install(&release).map_err(|e| format!("{e:#}")), Message::UpdateInstalled)
            }
            Message::UpdateInstalled(Ok(())) => {
                // Значок трея убирается до выхода, иначе он «висит» до наведения мыши.
                self.tray = None;
                update::restart()
            }
            other => {
                self.apply(other);
                Task::none()
            }
        };
        self.persist();
        task
    }

    fn apply(&mut self, message: Message) {
        match message {
            Message::EngineReady(handle) => {
                self.engine = Some(handle);
                self.send_initial_state();
            }
            Message::Engine(event) => self.on_event(event),

            Message::SelectDevice(choice) => {
                let serial = choice.serial.clone();
                self.device = Some(choice);
                self.send(Command::Disconnect);
                self.send(Command::Connect(Some(serial)));
            }
            Message::Connect => self.send(Command::Connect(self.device.as_ref().map(|d| d.serial.clone()))),
            Message::Disconnect => self.send(Command::Disconnect),

            Message::VideoEnabled(on) => {
                self.video_enabled = on;
                self.send(Command::SetVirtualCamera(on));
                self.apply_video();
            }
            Message::SelectCamera(choice) => {
                self.camera = Some(choice);
                self.fit_video_settings();
                self.apply_video();
            }
            Message::SelectResolution(choice) => {
                self.resolution = Some(choice);
                self.fit_video_settings();
                self.apply_video();
            }
            Message::SelectFps(choice) => {
                self.fps = choice;
                self.apply_video();
            }
            Message::SelectBitrate(choice) => {
                self.bitrate = choice;
                self.apply_video();
            }

            Message::Zoom(v) => {
                self.zoom = (v * 10.0).round() / 10.0;
                self.controls(Controls { zoom: Some(self.zoom), ..Default::default() });
            }
            Message::Ev(v) => {
                self.ev = v;
                self.controls(Controls { ev: Some(v), ..Default::default() });
            }
            Message::ManualFocus(on) => {
                self.manual_focus = on;
                let focus = if on { "manual" } else { "continuous" };
                self.controls(Controls {
                    focus: Some(focus.into()),
                    focus_distance: Some(self.focus_distance),
                    ..Default::default()
                });
            }
            Message::FocusDistance(v) => {
                self.focus_distance = v;
                self.controls(Controls { focus_distance: Some(v), ..Default::default() });
            }
            Message::WhiteBalance(wb) => {
                self.white_balance = wb;
                self.controls(Controls { white_balance: Some(wb.id().into()), ..Default::default() });
            }
            Message::Torch(on) => {
                self.torch = on;
                self.controls(Controls { torch: Some(on), ..Default::default() });
            }
            Message::Stabilization(on) => {
                self.stabilization = on;
                self.controls(Controls { stabilization: Some(on), ..Default::default() });
            }
            Message::AeLock(on) => {
                self.ae_lock = on;
                self.controls(Controls { ae_lock: Some(on), ..Default::default() });
            }
            Message::AwbLock(on) => {
                self.awb_lock = on;
                self.controls(Controls { awb_lock: Some(on), ..Default::default() });
            }

            Message::AudioEnabled(on) => {
                self.audio_enabled = on;
                self.audio_auto = false;
                self.apply_audio();
            }
            Message::SelectMic(choice) => {
                self.mic = choice;
                self.apply_audio();
            }
            Message::SelectSource(choice) => {
                self.source = Some(choice);
                self.apply_audio();
            }
            Message::Stereo(on) => {
                self.stereo = on;
                self.apply_audio();
            }
            Message::SelectOutput(choice) => {
                self.send(Command::SetAudioOutput(choice.id.clone()));
                self.output = Some(choice);
            }
            Message::RefreshOutputs => self.send(Command::RefreshAudioOutputs),

            Message::PreviewEnabled(on) => {
                self.preview_enabled = on;
                if !on {
                    self.preview = None;
                }
                self.send(Command::SetPreview(on));
            }
            Message::InstallVcam => self.send(Command::InstallVirtualCamera),
            Message::DismissError => self.error = None,

            Message::SelectTab(tab) => {
                self.tab = tab;
                // Запись сочетания относится к макропаду; на другой вкладке она ловила бы всё подряд.
                self.recording = false;
            }
            Message::SetAutostart(on) => match system::set_autostart(on) {
                Ok(()) => self.autostart = system::autostart_enabled(),
                Err(e) => self.error = Some(format!("Автозапуск: {e:#}")),
            },
            Message::SetCloseToTray(on) => self.close_to_tray = on,
            Message::SetCheckUpdates(on) => self.check_updates = on,
            Message::OpenSettingsFolder => system::open_folder(&settings::config_dir()),
            Message::UpdateChecked(result) => {
                self.update = match result {
                    Ok(Some(release)) => UpdateState::Available(release),
                    Ok(None) => UpdateState::UpToDate,
                    Err(e) => UpdateState::Failed(format!("Не удалось проверить обновления: {e}")),
                }
            }
            Message::UpdateInstalled(Err(e)) => self.update = UpdateState::Failed(format!("Установка не удалась: {e}")),
            Message::WindowOpened(_)
            | Message::CloseRequested(_)
            | Message::Tray(_)
            | Message::CheckUpdates
            | Message::InstallUpdate
            | Message::UpdateInstalled(Ok(())) => {}
            Message::Toggle(panel) => {
                if let Some(i) = self.open_panels.iter().position(|p| *p == panel) {
                    self.open_panels.remove(i);
                } else {
                    self.open_panels.push(panel);
                }
            }
            Message::PadEnabled(on) => {
                self.pad.enabled = on;
                self.apply_pad();
            }
            Message::PadColumns(n) => {
                self.pad.columns = n;
                self.resize_pad();
                self.apply_pad();
            }
            Message::PadRows(n) => {
                self.pad.rows = n;
                self.resize_pad();
                self.apply_pad();
            }
            Message::PadOrientation(orientation) => {
                self.pad.orientation = orientation;
                self.apply_pad();
            }
            Message::PadDim(choice) => {
                self.pad.amoled.dim_after_secs = choice.0;
                self.apply_pad();
            }
            Message::PadDimBrightness(percent) => self.pad.amoled.dim_brightness = percent,
            Message::PadDimCommit => self.apply_pad(),
            Message::PadAppIcon => {
                let icon =
                    self.selected_button().and_then(|b| crate::appicon::extract(std::path::Path::new(&b.target)));
                match icon.map(settings::save_image) {
                    Some(Ok(file)) => self.set_image(Some(file)),
                    Some(Err(e)) => self.error = Some(format!("Иконка: {e:#}")),
                    None => self.error = Some("Не удалось взять иконку у этой программы".into()),
                }
            }
            Message::PadOpen(index) => {
                if self.pad_page().get(index).is_some_and(|b| b.folder) && !self.is_back(index) {
                    self.pad_path.push(index);
                    self.select(if self.pad_page().len() > 1 { 1 } else { 0 });
                }
            }
            Message::PadUp => {
                if let Some(folder) = self.pad_path.pop() {
                    self.select(folder);
                }
            }
            Message::PadDragStart(index) => {
                if self.is_back(index) {
                    // «Назад» не перетаскивается — нажатие просто возвращает на уровень выше.
                    self.pad_drag = None;
                    if let Some(folder) = self.pad_path.pop() {
                        self.select(folder);
                    }
                } else {
                    self.pad_drag = Some(index);
                    self.select(index);
                }
            }
            Message::PadDragEnter(index) => self.pad_hover = Some(index),
            Message::PadDragExit(index) => {
                if self.pad_hover == Some(index) {
                    self.pad_hover = None;
                }
            }
            Message::PadDrop(target) => {
                if let Some(source) = self.pad_drag.take()
                    && source != target
                {
                    self.drop_button(source, target);
                }
            }
            Message::PadTarget(target) => {
                if let Some(b) = self.selected_button_mut() {
                    b.target = target;
                }
                self.apply_pad();
            }
            Message::PadAppPicked(Some(path)) => {
                let index = self.pad_selected;
                self.set_app(index, &path);
            }
            Message::PadAppPicked(None) | Message::PadPickApp => {}
            Message::FileDropped(path) => {
                if self.tab == Tab::Macropad {
                    // Бросили на ячейку — туда; иначе в первую свободную на открытой странице.
                    let first = if self.pad_path.is_empty() { 0 } else { BACK_SLOT + 1 };
                    let target = self
                        .pad_hover
                        .filter(|&i| !self.is_back(i))
                        .or_else(|| (first..self.pad_page().len()).find(|&i| self.pad_page()[i].is_empty()));
                    match target {
                        Some(index) => self.set_app(index, &path),
                        None => self.error = Some("Макропад: на странице нет свободных ячеек".into()),
                    }
                }
            }
            Message::PadDragCancel => {
                self.pad_drag = None;
                self.pad_hover = None;
            }
            Message::PadLabel(label) => self.edit_state(|s| s.label = label),
            Message::PadKind(kind) => {
                let count = self.pad_page().len();
                if let Some(b) = self.selected_button_mut() {
                    b.toggle = kind == ButtonKind::Toggle;
                    b.folder = kind == ButtonKind::Folder;
                    b.launch = kind == ButtonKind::App;
                    b.state = 0;
                    // Содержимое папки не теряется, если она на время станет обычной кнопкой.
                    if b.folder {
                        b.children.resize(count, ButtonSettings::default());
                    }
                }
                self.pad.resize();
                self.sync_tint_input();
                self.apply_pad();
            }
            Message::PadState(index) => {
                if let Some(b) = self.selected_button_mut() {
                    b.state = index.min(TOGGLE_STATES - 1);
                }
                self.sync_tint_input();
                self.apply_pad();
            }
            Message::PadTint(tint) => {
                self.edit_state(|s| s.tint = tint);
                self.sync_tint_input();
            }
            Message::PadTintHex(value) => {
                if let Some(tint) = parse_hex(&value) {
                    self.edit_state(|s| s.tint = Some(tint));
                }
                self.tint_input = value;
            }
            Message::PadRecord(on) => {
                self.recording = on;
                self.record_modifiers = iced::keyboard::Modifiers::default();
            }
            Message::PadInput(event) => {
                if let Some(combo) = self.recorded_combo(event) {
                    self.recording = false;
                    if let Some(combo) = combo {
                        self.set_keys(|keys| *keys = combo);
                    }
                }
            }
            Message::PadModifier(modifier, on) => self.set_keys(|keys| match modifier {
                Modifier::Ctrl => keys.ctrl = on,
                Modifier::Shift => keys.shift = on,
                Modifier::Alt => keys.alt = on,
                Modifier::Win => keys.win = on,
            }),
            Message::PadKey(key) => self.set_keys(|keys| keys.key = Some(key)),
            Message::PadClearKeys => self.set_keys(|keys| *keys = KeyCombo::default()),
            Message::PadImagePicked(Some(path)) => match settings::import_image(&path) {
                Ok(name) => self.set_image(Some(name)),
                Err(e) => self.error = Some(format!("Картинка: {e:#}")),
            },
            Message::PadImagePicked(None) | Message::PadPickImage => {}
            Message::PadClearImage => self.set_image(None),
        }
    }

    /// Движок только что запущен: передаём ему сохранённые настройки.
    fn send_initial_state(&mut self) {
        self.send(Command::SetPreview(self.preview_enabled));
        self.send(Command::SetVirtualCamera(self.video_enabled));
        if let Some(output) = &self.output {
            self.send(Command::SetAudioOutput(output.id.clone()));
        }
        self.send(Command::SetControls(Controls {
            zoom: Some(self.zoom),
            ev: Some(self.ev),
            focus: Some(if self.manual_focus { "manual" } else { "continuous" }.into()),
            focus_distance: Some(self.focus_distance),
            white_balance: Some(self.white_balance.id().into()),
            torch: None,
            stabilization: Some(self.stabilization),
            ae_lock: Some(self.ae_lock),
            awb_lock: Some(self.awb_lock),
        }));
        self.apply_pad();
    }

    fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
        if let Some(tray) = &self.tray {
            tray.set_hidden(hidden);
        }
    }

    fn quit(&mut self) -> Task<Message> {
        self.persist();
        self.tray = None;
        iced::exit()
    }

    fn start_update_check(&mut self) -> Task<Message> {
        self.update = UpdateState::Checking;
        background(|| update::check().map_err(|e| format!("{e:#}")), Message::UpdateChecked)
    }

    fn snapshot(&self) -> Settings {
        Settings {
            video_enabled: self.video_enabled,
            camera: self.camera.as_ref().map(|c| (c.id.clone(), c.physical.clone())),
            resolution: self.resolution.as_ref().map(|r| (r.width, r.height)),
            fps: self.fps.0,
            bitrate: self.bitrate.0,
            zoom: self.zoom,
            ev: self.ev,
            manual_focus: self.manual_focus,
            focus_distance: self.focus_distance,
            white_balance: self.white_balance.id().into(),
            stabilization: self.stabilization,
            ae_lock: self.ae_lock,
            awb_lock: self.awb_lock,
            audio_enabled: (!self.audio_auto).then_some(self.audio_enabled),
            mic: self.mic.id,
            source: self.source.as_ref().map(|s| s.id.clone()),
            stereo: self.stereo,
            output: self.output.as_ref().and_then(|o| o.id.clone()),
            preview_enabled: self.preview_enabled,
            macropad: self.pad.clone(),
            close_to_tray: self.close_to_tray,
            check_updates: self.check_updates,
        }
    }

    fn persist(&mut self) {
        let current = self.snapshot();
        if current != self.saved {
            if let Err(e) = settings::save(&current) {
                log::warn!("не удалось сохранить настройки: {e:#}");
            }
            self.saved = current;
        }
    }

    // --- Макропад ---

    fn set_keys(&mut self, change: impl FnOnce(&mut KeyCombo)) {
        if let Some(b) = self.selected_button_mut() {
            change(&mut b.keys);
        }
        self.apply_pad();
    }

    /// Подгоняет страницы под сетку. Открытая папка могла оказаться за её пределами —
    /// тогда поднимаемся до ближайшей уцелевшей страницы.
    fn resize_pad(&mut self) {
        self.pad.resize();
        let mut page = &self.pad.buttons;
        let mut valid = 0;
        for &i in &self.pad_path {
            match page.get(i) {
                Some(b) if b.folder => page = &b.children,
                _ => break,
            }
            valid += 1;
        }
        if valid < self.pad_path.len() {
            self.pad_path.truncate(valid);
            self.pad_selected = 0;
        }
        self.select(self.pad_selected.min(self.pad_page().len() - 1));
    }

    /// Кнопки открытой страницы.
    pub fn pad_page(&self) -> &Vec<ButtonSettings> {
        self.pad.page(&self.pad_path)
    }

    /// Ячейка «Назад» — первая в каждой папке.
    pub fn is_back(&self, index: usize) -> bool {
        !self.pad_path.is_empty() && index == BACK_SLOT
    }

    pub fn selected_button(&self) -> Option<&ButtonSettings> {
        if self.is_back(self.pad_selected) { None } else { self.pad_page().get(self.pad_selected) }
    }

    fn selected_button_mut(&mut self) -> Option<&mut ButtonSettings> {
        if self.is_back(self.pad_selected) {
            return None;
        }
        let index = self.pad_selected;
        self.pad.page_mut(&self.pad_path).get_mut(index)
    }

    fn select(&mut self, index: usize) {
        self.pad_selected = index;
        self.recording = false;
        self.sync_tint_input();
    }

    /// Делает кнопку `index` открытой страницы кнопкой программы: подпись — имя файла,
    /// картинка — иконка из Проводника (если свою ещё не выбрали).
    fn set_app(&mut self, index: usize, path: &std::path::Path) {
        if self.is_back(index) {
            return;
        }
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let has_image = self.pad_page().get(index).is_some_and(|b| b.states[0].image.is_some());
        let icon =
            if has_image { None } else { crate::appicon::extract(path).and_then(|i| settings::save_image(i).ok()) };
        let path_page = self.pad_path.clone();
        let Some(b) = self.pad.page_mut(&path_page).get_mut(index) else { return };
        b.launch = true;
        b.folder = false;
        b.toggle = false;
        b.state = 0;
        b.target = path.to_string_lossy().into_owned();
        if b.states[0].label.is_empty() {
            b.states[0].label = name;
        }
        if let Some(icon) = icon {
            b.states[0].image = Some(icon);
            b.states[0].tint = None;
        }
        self.select(index);
        self.apply_pad();
    }

    /// Бросили кнопку `source` на ячейку `target` открытой страницы.
    fn drop_button(&mut self, source: usize, target: usize) {
        let path = self.pad_path.clone();
        let into = if self.is_back(target) {
            // На «Назад» — на уровень выше.
            Some(path[..path.len() - 1].to_vec())
        } else if self.pad_page().get(target).is_some_and(|b| b.folder) {
            // На папку — внутрь неё.
            Some([path.as_slice(), &[target]].concat())
        } else {
            None
        };
        match into {
            Some(destination) => {
                let taken = std::mem::take(&mut self.pad.page_mut(&path)[source]);
                let first = if destination.is_empty() { 0 } else { BACK_SLOT + 1 };
                let page = self.pad.page_mut(&destination);
                match (first..page.len()).find(|&i| page[i].is_empty()) {
                    Some(free) => page[free] = taken,
                    None => {
                        // Места нет — возвращаем кнопку на место.
                        self.pad.page_mut(&path)[source] = taken;
                        self.error = Some("Макропад: в папке нет свободных ячеек".into());
                        return;
                    }
                }
                self.pad.resize();
            }
            None => {
                self.pad.page_mut(&path).swap(source, target);
                self.pad_selected = target;
            }
        }
        self.sync_tint_input();
        self.apply_pad();
    }

    /// Меняет картинку редактируемого состояния; перекраска прежней картинки сбрасывается.
    fn set_image(&mut self, image: Option<String>) {
        self.edit_state(|s| {
            s.image = image;
            s.tint = None;
        });
        self.sync_tint_input();
        settings::remove_unused_images(&self.pad);
    }

    /// Какое состояние выбранной кнопки сейчас редактируется: у переключателя — текущее.
    pub fn edited_state_index(&self) -> usize {
        self.selected_button().map_or(0, |b| if b.toggle { b.state } else { 0 })
    }

    pub fn edited_state(&self) -> Option<&StateSettings> {
        self.selected_button().map(|b| &b.states[self.edited_state_index()])
    }

    fn edit_state(&mut self, change: impl FnOnce(&mut StateSettings)) {
        let index = self.edited_state_index();
        if let Some(b) = self.selected_button_mut() {
            change(&mut b.states[index]);
        }
        self.apply_pad();
    }

    fn sync_tint_input(&mut self) {
        self.tint_input = self
            .edited_state()
            .and_then(|s| s.tint)
            .map(|[r, g, b]| format!("#{r:02X}{g:02X}{b:02X}"))
            .unwrap_or_default();
    }

    /// Картинка состояния для превью: `Some(None)` — файл удалён, `None` — картинки нет.
    pub fn pad_image(&self, state: &StateSettings) -> Option<Option<&image::Handle>> {
        let name = state.image.as_ref()?;
        Some(self.pad_images.get(&(name.clone(), state.tint)).and_then(|e| e.as_ref()).map(|(_, h)| h))
    }

    /// Готовит перекрашенные картинки для всех состояний; удалённые файлы помечаются.
    pub(crate) fn refresh_pad_images(&mut self) {
        let used: Vec<ImageKey> = self
            .pad
            .all_buttons()
            .into_iter()
            .flat_map(|b| &b.states)
            .filter_map(|s| s.image.clone().map(|name| (name, s.tint)))
            .collect();
        self.pad_images.retain(|key, entry| {
            used.contains(key) && (entry.is_none() || settings::images_dir().join(&key.0).is_file())
        });
        for key in used {
            self.pad_images.entry(key.clone()).or_insert_with(|| {
                settings::render_image(&key.0, key.1).map(|png| (png.clone(), image::Handle::from_bytes(png)))
            });
        }
    }

    /// Разбирает событие во время записи: `None` — продолжаем ждать,
    /// `Some(None)` — запись отменена (Esc), `Some(Some(..))` — сочетание готово.
    fn recorded_combo(&mut self, event: iced::Event) -> Option<Option<KeyCombo>> {
        use iced::keyboard::{self, key::Physical};
        use iced::mouse::{self, Button, ScrollDelta};
        let key = match event {
            iced::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                self.record_modifiers = m;
                return None;
            }
            iced::Event::Keyboard(keyboard::Event::KeyPressed { physical_key, modifiers, repeat: false, .. }) => {
                self.record_modifiers = modifiers;
                let Physical::Code(code) = physical_key else { return None };
                let code = format!("{code:?}");
                // Одиночные модификаторы — ждём основную клавишу.
                if ["Control", "Shift", "Alt", "Super", "Meta"].iter().any(|m| code.starts_with(m)) {
                    return None;
                }
                let key = Key::from_code(&code)?;
                if key == Key::Escape && modifiers.is_empty() {
                    return Some(None);
                }
                key
            }
            // Левая и правая кнопки нужны, чтобы нажимать на интерфейс; их выбирают в списке.
            iced::Event::Mouse(mouse::Event::ButtonPressed(button)) => match button {
                Button::Middle => Key::MouseMiddle,
                Button::Back => Key::Mouse4,
                Button::Forward => Key::Mouse5,
                _ => return None,
            },
            iced::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let y = match delta {
                    ScrollDelta::Lines { y, .. } | ScrollDelta::Pixels { y, .. } => y,
                };
                if y > 0.0 {
                    Key::WheelUp
                } else if y < 0.0 {
                    Key::WheelDown
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        let m = self.record_modifiers;
        Some(Some(KeyCombo { ctrl: m.control(), shift: m.shift(), alt: m.alt(), win: m.logo(), key: Some(key) }))
    }

    pub fn is_open(&self, panel: Panel) -> bool {
        self.open_panels.contains(&panel)
    }

    fn apply_pad(&mut self) {
        self.refresh_pad_images();
        // Страницы нумеруются обходом папок в глубину: 0 — корень.
        let mut pages = Vec::new();
        let mut paths = Vec::new();
        self.build_page(&[], None, &mut pages, &mut paths);
        self.pad_pages = paths;
        let layout = self.pad.enabled.then_some(PadLayout {
            columns: self.pad.columns,
            rows: self.pad.rows,
            orientation: self.pad.orientation,
            amoled: self.pad.amoled,
            pages,
        });
        self.send(Command::SetMacropad(layout));
    }

    /// Добавляет страницу `path` и вложенные папки; возвращает номер страницы.
    fn build_page(
        &self,
        path: &[usize],
        parent: Option<usize>,
        pages: &mut Vec<PadPage>,
        paths: &mut Vec<Vec<usize>>,
    ) -> usize {
        let number = pages.len();
        pages.push(PadPage { parent, buttons: Vec::new() });
        paths.push(path.to_vec());
        let mut buttons = Vec::new();
        for (i, b) in self.pad.page(path).iter().enumerate() {
            let kind = if !path.is_empty() && i == BACK_SLOT {
                PadKind::Back
            } else if b.folder {
                PadKind::Folder(self.build_page(&[path, &[i]].concat(), Some(number), pages, paths))
            } else {
                PadKind::Keys
            };
            let count = if b.toggle { TOGGLE_STATES } else { 1 };
            let states = if kind == PadKind::Back {
                vec![PadState { label: "← Назад".into(), image_png: None }]
            } else {
                b.states[..count]
                    .iter()
                    .map(|s| PadState {
                        label: s.label.clone(),
                        image_png: s
                            .image
                            .as_ref()
                            .and_then(|name| self.pad_images.get(&(name.clone(), s.tint)))
                            .and_then(|e| e.as_ref())
                            .map(|(png, _)| png.clone()),
                    })
                    .collect()
            };
            let launch = (kind == PadKind::Keys && b.launch && !b.target.trim().is_empty()).then(|| b.target.clone());
            buttons.push(PadButton {
                kind,
                toggle: b.toggle && kind == PadKind::Keys && launch.is_none(),
                keys: b.keys,
                launch,
                states,
                state: if b.toggle { b.state } else { 0 },
            });
        }
        pages[number].buttons = buttons;
        number
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::AdbDevices(devices) => {
                self.adb_error = None;
                if self.device.as_ref().is_none_or(|d| !devices.iter().any(|x| x.serial == d.serial)) {
                    self.device = devices.iter().find(|d| d.is_ready()).map(DeviceChoice::from);
                }
                self.adb_devices = devices;
            }
            Event::AdbError(e) => self.adb_error = Some(e),
            Event::Connecting { stage } => self.connection = Connection::Connecting { stage },
            Event::Connected { serial, phone, devices } => {
                if let Some(d) = self.adb_devices.iter().find(|d| d.serial == serial) {
                    self.device = Some(DeviceChoice::from(d));
                }
                self.connection = Connection::Connected { phone };
                self.devices = devices;
                self.choose_defaults();
                self.apply_video();
                self.apply_audio();
            }
            Event::Disconnected { reason } => {
                self.connection = Connection::Idle { reason };
                self.video = None;
                self.audio = None;
                self.stats = None;
                self.preview = None;
            }
            Event::VideoStarted(info) => self.video = Some(info),
            Event::VideoStopped => {
                self.video = None;
                self.preview = None;
            }
            Event::AudioStarted(info) => self.audio = Some(info),
            Event::AudioStopped => self.audio = None,
            Event::AudioOutputs(outputs) => {
                self.outputs = outputs;
                // Сохранённое устройство: подставляем его имя.
                if let Some(found) =
                    self.output.as_ref().and_then(|o| self.outputs.iter().find(|x| Some(&x.id) == o.id.as_ref()))
                {
                    self.output = Some(OutputChoice::from(found));
                }
                if self.output.is_none() && self.audio_auto {
                    // Виртуальный кабель — лучший выбор по умолчанию: в колонки звук с микрофона не нужен.
                    if let Some(virt) = self.outputs.iter().find(|o| looks_virtual(&o.name)) {
                        let choice = OutputChoice::from(virt);
                        self.send(Command::SetAudioOutput(choice.id.clone()));
                        self.output = Some(choice);
                        self.audio_enabled = true;
                        self.apply_audio();
                    }
                }
            }
            Event::Error(message) => {
                // Телефон отказался от формата — выключаем переключатель, чтобы было видно, что не работает.
                if message.starts_with("Видео:") {
                    self.video_enabled = false;
                    self.send(Command::SetVirtualCamera(false));
                } else if message.starts_with("Звук:") {
                    self.audio_enabled = false;
                }
                self.error = Some(message);
            }
            Event::Preview(frame) => {
                if self.preview_enabled {
                    self.preview = Some(image::Handle::from_rgba(frame.width, frame.height, frame.rgba));
                }
            }
            Event::Stats(stats) => self.stats = Some(stats),
            Event::VirtualCamera(status) => self.vcam = Some(status),
            Event::VirtualCameraLog(line) => self.vcam_log = Some(line),
            Event::MacropadState { page, id, state } => {
                let Some(path) = self.pad_pages.get(page).cloned() else { return };
                if let Some(b) = self.pad.page_mut(&path).get_mut(id) {
                    b.state = state;
                }
                if path == self.pad_path && id == self.pad_selected {
                    self.sync_tint_input();
                }
                // Движок уже знает новое состояние; пересобираем раскладку, чтобы копии совпадали.
                self.apply_pad();
            }
        }
    }

    fn send(&self, command: Command) {
        if let Some(engine) = &self.engine {
            engine.send(command);
        }
    }

    fn controls(&self, controls: Controls) {
        self.send(Command::SetControls(controls));
    }

    // --- Камера ---

    pub fn camera_info(&self) -> Option<&CameraInfo> {
        self.camera.as_ref().and_then(|c| self.devices.cameras.get(c.index))
    }

    pub fn camera_choices(&self) -> Vec<CameraChoice> {
        self.devices
            .cameras
            .iter()
            .enumerate()
            .map(|(index, c)| CameraChoice {
                index,
                name: c.name.clone(),
                id: c.id.clone(),
                physical: c.physical_id.clone(),
            })
            .collect()
    }

    pub fn resolution_choices(&self) -> Vec<ResolutionChoice> {
        self.camera_info()
            .map(|c| {
                c.sizes.iter().map(|&[width, height, max_fps]| ResolutionChoice { width, height, max_fps }).collect()
            })
            .unwrap_or_default()
    }

    pub fn fps_choices(&self) -> Vec<FpsChoice> {
        let max = self.resolution.as_ref().map_or(30, |r| r.max_fps);
        let mut fps: Vec<u32> = self
            .camera_info()
            .map(|c| c.fps.iter().copied().filter(|&f| f >= 15 && f <= max).collect())
            .unwrap_or_default();
        if fps.is_empty() {
            fps.push(max.min(30));
        }
        fps.dedup();
        fps.into_iter().map(FpsChoice).collect()
    }

    /// При первом подключении выбираем основную заднюю камеру, 1080p и 60 к/с, если есть.
    fn choose_defaults(&mut self) {
        let cameras = self.camera_choices();
        let still_valid = self.camera.as_ref().is_some_and(|c| cameras.iter().any(|x| x.same_camera(c)));
        if still_valid {
            // Индексы могли сместиться — переносим выбор по id.
            let current = self.camera.clone().unwrap();
            self.camera = cameras.into_iter().find(|x| x.same_camera(&current));
        } else {
            let index =
                self.devices.cameras.iter().position(|c| c.facing == "back" && c.physical_id.is_none()).unwrap_or(0);
            self.camera = cameras.into_iter().nth(index);
        }
        self.fit_video_settings();

        let sources = self.source_choices();
        self.source = self
            .source
            .as_ref()
            .and_then(|s| sources.iter().find(|x| x.id == s.id))
            .or_else(|| sources.iter().find(|x| x.id == "camcorder"))
            .or_else(|| sources.first())
            .cloned();
        self.mic = self.mic_choices().into_iter().find(|m| m.id == self.mic.id).unwrap_or_else(MicChoice::auto);
    }

    /// Подгоняет разрешение и частоту под возможности выбранной камеры.
    fn fit_video_settings(&mut self) {
        let sizes = self.resolution_choices();
        // Сравниваем по размеру: у сохранённого разрешения нет данных о максимальной частоте.
        let same =
            self.resolution.as_ref().and_then(|r| sizes.iter().find(|s| (s.width, s.height) == (r.width, r.height)));
        if let Some(found) = same {
            self.resolution = Some(found.clone());
        } else {
            self.resolution = sizes
                .iter()
                .find(|r| (r.width, r.height) == PREFERRED_SIZE)
                .or_else(|| sizes.iter().find(|r| r.width <= PREFERRED_SIZE.0 && r.width * 9 == r.height * 16))
                .or_else(|| sizes.first())
                .cloned();
        }
        let fps = self.fps_choices();
        if !fps.contains(&self.fps) {
            self.fps = fps
                .iter()
                .copied()
                .filter(|f| f.0 <= DEFAULT_FPS)
                .max_by_key(|f| f.0)
                .or_else(|| fps.first().copied())
                .unwrap_or(FpsChoice(DEFAULT_FPS));
        }
        if let Some((zoom, ev, focus)) = self.camera_info().map(|c| (c.zoom, c.ev, c.min_focus_distance)) {
            self.zoom = self.zoom.clamp(zoom[0], zoom[1]);
            self.ev = self.ev.clamp(ev[0], ev[1]);
            self.focus_distance = self.focus_distance.clamp(0.0, focus);
        }
    }

    fn video_params(&self) -> Option<VideoParams> {
        let camera = self.camera.as_ref()?;
        let resolution = self.resolution.as_ref()?;
        Some(VideoParams {
            camera: camera.id.clone(),
            physical: camera.physical.clone(),
            width: resolution.width,
            height: resolution.height,
            fps: self.fps.0,
            bitrate: self.bitrate.0 * 1_000_000,
        })
    }

    fn apply_video(&self) {
        let params = if self.video_enabled { self.video_params() } else { None };
        self.send(Command::SetVideo(params));
    }

    // --- Звук ---

    pub fn mic_choices(&self) -> Vec<MicChoice> {
        std::iter::once(MicChoice::auto())
            .chain(self.devices.microphones.iter().map(|m| MicChoice { id: Some(m.id), name: m.name.clone() }))
            .collect()
    }

    pub fn source_choices(&self) -> Vec<SourceChoice> {
        self.devices.audio_sources.iter().map(|s| SourceChoice { id: s.id.clone(), name: s.name.clone() }).collect()
    }

    pub fn output_choices(&self) -> Vec<OutputChoice> {
        std::iter::once(OutputChoice {
            id: None, name: "Системное устройство по умолчанию".into()
        })
        .chain(self.outputs.iter().map(OutputChoice::from))
        .collect()
    }

    fn apply_audio(&self) {
        let params = self.audio_enabled.then(|| AudioParams {
            device: self.mic.id,
            source: self.source.as_ref().map_or("camcorder".into(), |s| s.id.clone()),
            channels: if self.stereo { 2 } else { 1 },
        });
        self.send(Command::SetAudio(params));
    }
}

// --- Элементы выпадающих списков ---

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceChoice {
    pub serial: String,
    pub label: String,
}

impl From<&AdbDevice> for DeviceChoice {
    fn from(d: &AdbDevice) -> Self {
        let label = match d.state.as_str() {
            "device" => d.label(),
            "unauthorized" => format!("{} — подтвердите отладку на телефоне", d.label()),
            other => format!("{} — {other}", d.label()),
        };
        Self { serial: d.serial.clone(), label }
    }
}

impl fmt::Display for DeviceChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CameraChoice {
    pub index: usize,
    pub name: String,
    pub id: String,
    pub physical: Option<String>,
}

impl CameraChoice {
    fn same_camera(&self, other: &CameraChoice) -> bool {
        self.id == other.id && self.physical == other.physical
    }
}

impl fmt::Display for CameraChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolutionChoice {
    pub width: u32,
    pub height: u32,
    pub max_fps: u32,
}

impl fmt::Display for ResolutionChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} × {}", self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FpsChoice(pub u32);

impl fmt::Display for FpsChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} к/с", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BitrateChoice(pub u32);

impl fmt::Display for BitrateChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Мбит/с", self.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MicChoice {
    pub id: Option<i32>,
    pub name: String,
}

impl MicChoice {
    fn auto() -> Self {
        Self { id: None, name: "Автоматически".into() }
    }
}

impl fmt::Display for MicChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceChoice {
    pub id: String,
    pub name: String,
}

impl fmt::Display for SourceChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputChoice {
    pub id: Option<String>,
    pub name: String,
}

impl From<&AudioOutputInfo> for OutputChoice {
    fn from(o: &AudioOutputInfo) -> Self {
        Self { id: Some(o.id.clone()), name: o.name.clone() }
    }
}

impl fmt::Display for OutputChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WbChoice {
    Auto,
    Daylight,
    Cloudy,
    Incandescent,
    Fluorescent,
}

impl WbChoice {
    pub const ALL: [WbChoice; 5] = [Self::Auto, Self::Daylight, Self::Cloudy, Self::Incandescent, Self::Fluorescent];

    fn from_id(id: &str) -> Self {
        Self::ALL.into_iter().find(|wb| wb.id() == id).unwrap_or(Self::Auto)
    }

    fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Daylight => "daylight",
            Self::Cloudy => "cloudy",
            Self::Incandescent => "incandescent",
            Self::Fluorescent => "fluorescent",
        }
    }
}

impl fmt::Display for WbChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "Автоматически",
            Self::Daylight => "Дневной свет",
            Self::Cloudy => "Облачно",
            Self::Incandescent => "Лампа накаливания",
            Self::Fluorescent => "Люминесцентная лампа",
        })
    }
}

/// Выполняет блокирующую работу (сеть, диск) в отдельном потоке, не задерживая интерфейс.
fn background<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    done: impl FnOnce(Result<T, String>) -> Message + Send + 'static,
) -> Task<Message> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    Task::perform(async move { rx.await.unwrap_or_else(|_| Err("фоновая задача прервана".into())) }, done)
}

/// «#RRGGBB» или «RRGGBB».
fn parse_hex(value: &str) -> Option<[u8; 3]> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_labels(labels: &[&str]) -> App {
        let mut app = App::new(Settings::default());
        for (b, label) in app.pad.buttons.iter_mut().zip(labels) {
            b.states[0].label = label.to_string();
        }
        app
    }

    fn labels(page: &[ButtonSettings]) -> Vec<&str> {
        page.iter().map(|b| b.states[0].label.as_str()).collect()
    }

    #[test]
    fn drag_swaps_buttons() {
        let mut app = app_with_labels(&["A", "B", "C"]);
        let _ = app.update(Message::PadDragStart(0));
        let _ = app.update(Message::PadDrop(2));
        assert_eq!(labels(&app.pad.buttons)[..3], ["C", "B", "A"]);
        assert_eq!(app.pad_selected, 2, "выбор переезжает вместе с кнопкой");
    }

    #[test]
    fn drag_into_folder_and_back_out() {
        let mut app = app_with_labels(&["A", "", "Folder"]);
        let _ = app.update(Message::PadDragStart(2));
        let _ = app.update(Message::PadKind(ButtonKind::Folder));
        let _ = app.update(Message::PadDragStart(0));
        let _ = app.update(Message::PadDrop(2));
        assert!(app.pad.buttons[0].is_empty(), "кнопка ушла из корня");
        let folder = &app.pad.buttons[2].children;
        assert_eq!(folder[BACK_SLOT + 1].states[0].label, "A", "легла в первую свободную ячейку после «Назад»");

        let _ = app.update(Message::PadOpen(2));
        assert_eq!(app.pad_path, [2]);
        let _ = app.update(Message::PadDragStart(BACK_SLOT + 1));
        let _ = app.update(Message::PadDrop(BACK_SLOT));
        assert!(app.pad.buttons[2].children[BACK_SLOT + 1].is_empty());
        assert_eq!(app.pad.buttons[0].states[0].label, "A", "вернулась на уровень выше");
    }

    #[test]
    fn dropped_file_becomes_app_button() {
        let mut app = app_with_labels(&["A"]);
        app.tab = Tab::Macropad;
        let _ = app.update(Message::FileDropped(PathBuf::from("/Games/Game Launcher.exe")));
        let b = &app.pad.buttons[1];
        assert!(b.launch, "легла в первую свободную ячейку");
        assert_eq!(b.target, "/Games/Game Launcher.exe");
        assert_eq!(b.states[0].label, "Game Launcher", "подпись — имя программы");
        assert_eq!(app.pad_selected, 1);
        // На других вкладках файлы не ловим.
        app.tab = Tab::Stream;
        let _ = app.update(Message::FileDropped(PathBuf::from("/x.exe")));
        assert!(!app.pad.buttons[2].launch);
    }

    #[test]
    fn full_folder_keeps_button() {
        let mut app = app_with_labels(&["A", "B", "F", "C", "D", "E"]);
        app.pad.buttons[2].folder = true;
        app.pad.resize();
        for b in app.pad.buttons[2].children.iter_mut().skip(1) {
            b.states[0].label = "x".into();
        }
        let _ = app.update(Message::PadDragStart(0));
        let _ = app.update(Message::PadDrop(2));
        assert_eq!(app.pad.buttons[0].states[0].label, "A", "места нет — кнопка остаётся");
        assert!(app.error.is_some());
    }

    #[test]
    fn shrinking_grid_closes_removed_folder() {
        let mut app = app_with_labels(&[]);
        app.pad.buttons[5].folder = true;
        app.pad.resize();
        let _ = app.update(Message::PadOpen(5));
        assert_eq!(app.pad_path, [5]);
        let _ = app.update(Message::PadRows(1));
        assert!(app.pad_path.is_empty(), "папки больше нет — показываем корень");
        assert_eq!(app.pad_page().len(), 3);
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex("#E53935"), Some([0xE5, 0x39, 0x35]));
        assert_eq!(parse_hex("ffffff"), Some([255, 255, 255]));
        assert_eq!(parse_hex("#FFF"), None);
        assert_eq!(parse_hex("#GG0000"), None);
    }
}
