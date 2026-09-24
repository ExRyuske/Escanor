//! Настройки, которые переживают перезапуск: выбранные камера, микрофон, параметры и макропад.
//! Файл settings.json и папка macropad лежат рядом с программой.

use anyhow::{Context, Result};
use escanor_core::keys::KeyCombo;
use escanor_core::macropad::{Amoled, Orientation};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Размер картинок кнопок макропада: достаточно для экрана телефона и не раздувает протокол.
const IMAGE_SIZE: u32 = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub video_enabled: bool,
    /// id камеры и физического объектива.
    pub camera: Option<(String, Option<String>)>,
    pub resolution: Option<(u32, u32)>,
    pub fps: u32,
    pub bitrate: u32,

    pub zoom: f32,
    pub ev: i32,
    pub manual_focus: bool,
    pub focus_distance: f32,
    pub white_balance: String,
    pub stabilization: bool,
    pub ae_lock: bool,
    pub awb_lock: bool,

    /// `None` — включить автоматически, если найден виртуальный кабель.
    pub audio_enabled: Option<bool>,
    pub mic: Option<i32>,
    pub source: Option<String>,
    pub stereo: bool,
    pub output: Option<String>,

    pub preview_enabled: bool,
    pub macropad: MacropadSettings,

    /// Крестик окна прячет Escanor в трей, а не закрывает.
    pub close_to_tray: bool,
    /// Проверять новую версию при запуске.
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            video_enabled: true,
            camera: None,
            resolution: None,
            fps: 60,
            bitrate: 24,
            zoom: 1.0,
            ev: 0,
            manual_focus: false,
            focus_distance: 0.0,
            white_balance: "auto".into(),
            stabilization: false,
            ae_lock: false,
            awb_lock: false,
            audio_enabled: None,
            mic: None,
            source: None,
            stereo: false,
            output: None,
            preview_enabled: true,
            macropad: MacropadSettings::default(),
            close_to_tray: true,
            check_updates: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MacropadSettings {
    pub enabled: bool,
    pub columns: u32,
    pub rows: u32,
    /// Ориентация экрана телефона, пока показан макропад.
    pub orientation: Orientation,
    /// Затемнение экрана телефона при бездействии (AMOLED-режим включён всегда).
    pub amoled: Amoled,
    pub buttons: Vec<ButtonSettings>,
    /// Устройство вывода для звуков саундпада; `None` — системное по умолчанию.
    pub sound_output: Option<String>,
    /// Выравнивать громкость звуков саундпада.
    pub normalize_sounds: bool,
    /// Общая громкость саундпада, %.
    pub sound_volume: u32,
}

impl Default for MacropadSettings {
    fn default() -> Self {
        let mut pad = Self {
            enabled: false,
            columns: 3,
            rows: 2,
            orientation: Orientation::default(),
            amoled: Amoled::default(),
            buttons: Vec::new(),
            sound_output: None,
            normalize_sounds: true,
            sound_volume: 100,
        };
        pad.resize();
        pad
    }
}

impl MacropadSettings {
    /// Число кнопок на каждой странице (и в каждой папке) равно размеру сетки;
    /// лишние отбрасываются, новые пустые.
    pub fn resize(&mut self) {
        resize_page(&mut self.buttons, (self.columns * self.rows) as usize);
    }

    /// Все кнопки, включая лежащие в папках.
    pub fn all_buttons(&self) -> Vec<&ButtonSettings> {
        fn walk<'a>(buttons: &'a [ButtonSettings], out: &mut Vec<&'a ButtonSettings>) {
            for b in buttons {
                out.push(b);
                if b.folder {
                    walk(&b.children, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.buttons, &mut out);
        out
    }

    /// Имена всех картинок, на которые ссылаются кнопки, включая кнопки в папках.
    pub fn images(&self) -> Vec<&str> {
        self.all_buttons().into_iter().flat_map(|b| &b.states).filter_map(|s| s.image.as_deref()).collect()
    }

    /// Имена всех звуков кнопок, включая кнопки в папках. Звук кнопки, которая на время стала
    /// другого типа, тоже считается: он вернётся вместе с типом.
    pub fn sounds(&self) -> Vec<&str> {
        self.all_buttons().into_iter().map(|b| b.sound_file.as_str()).filter(|f| !f.is_empty()).collect()
    }

    /// Кнопки страницы по пути из номеров папок (пустой путь — корень).
    pub fn page(&self, path: &[usize]) -> &Vec<ButtonSettings> {
        path.iter().fold(&self.buttons, |page, &i| &page[i].children)
    }

    pub fn page_mut(&mut self, path: &[usize]) -> &mut Vec<ButtonSettings> {
        path.iter().fold(&mut self.buttons, |page, &i| &mut page[i].children)
    }
}

/// В папке первая ячейка — «Назад»: её место занято, кнопку туда не положить.
pub const BACK_SLOT: usize = 0;

fn resize_page(buttons: &mut Vec<ButtonSettings>, count: usize) {
    buttons.resize(count, ButtonSettings::default());
    for b in buttons {
        b.normalize();
        if b.folder {
            resize_page(&mut b.children, count);
        }
    }
}

/// Число состояний у переключателя.
pub const TOGGLE_STATES: usize = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ButtonSettings {
    /// Переключатель с двумя состояниями; иначе обычная кнопка (клавиши держатся, пока держат кнопку).
    pub toggle: bool,
    /// Сочетание клавиш — одно на кнопку: у переключателя меняется только вид.
    pub keys: KeyCombo,
    /// Всегда два: у обычной кнопки используется первое.
    pub states: Vec<StateSettings>,
    /// Текущее состояние переключателя.
    pub state: usize,
    /// Папка: открывает страницу с кнопками `children` (первая ячейка — «Назад»).
    pub folder: bool,
    pub children: Vec<ButtonSettings>,
    /// Программа: касание открывает `target` — exe, ярлык, файл или адрес сайта.
    pub launch: bool,
    pub target: String,
    /// Текст: касание печатает `snippet` на ПК, с `enter` — и нажимает Enter в конце.
    pub text: bool,
    pub snippet: String,
    pub enter: bool,
    /// Звук: касание проигрывает на ПК файл `sound_file` из папки макропада.
    pub sound: bool,
    pub sound_file: String,
}

impl Default for ButtonSettings {
    fn default() -> Self {
        let mut button = Self {
            toggle: false,
            keys: KeyCombo::default(),
            states: Vec::new(),
            state: 0,
            folder: false,
            children: Vec::new(),
            launch: false,
            target: String::new(),
            text: false,
            snippet: String::new(),
            enter: false,
            sound: false,
            sound_file: String::new(),
        };
        button.normalize();
        button
    }
}

impl ButtonSettings {
    /// Приводит в согласованный вид: всегда два состояния, у папки, программы, текста и звука нет переключателя.
    fn normalize(&mut self) {
        self.states.resize(TOGGLE_STATES, StateSettings::default());
        if self.folder || self.launch || self.text || self.sound {
            self.toggle = false;
        }
        if self.folder {
            self.launch = false;
            self.text = false;
            self.sound = false;
        }
        if !self.toggle || self.state >= TOGGLE_STATES {
            self.state = 0;
        }
    }

    /// Пустая ячейка: сюда можно положить кнопку, перетащив её в папку.
    pub fn is_empty(&self) -> bool {
        !self.folder
            && !self.launch
            && !self.text
            && !self.sound
            && self.keys.is_empty()
            && self.states.iter().all(|s| s.label.is_empty() && s.image.is_none())
    }

    /// Состояние, которое сейчас показывается на телефоне.
    pub fn current(&self) -> &StateSettings {
        &self.states[if self.toggle { self.state } else { 0 }]
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StateSettings {
    pub label: String,
    /// Имя файла в папке картинок.
    pub image: Option<String>,
    /// Перекрасить картинку в этот цвет (RGB); `None` — исходные цвета.
    pub tint: Option<[u8; 3]>,
}

pub fn config_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let portable = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
        match portable {
            Some(dir) if is_writable(&dir) => dir,
            _ => user_dir(),
        }
    })
    .clone()
}

/// Папка пользователя — если в папку программы писать нельзя.
fn user_dir() -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home).join("Escanor")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Escanor")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("escanor")
    }
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".escanor-write-test");
    std::fs::write(&probe, b"").is_ok() && std::fs::remove_file(&probe).is_ok()
}

const SETTINGS_FILE: &str = "settings.json";
const IMAGES_DIR: &str = "macropad";

fn settings_path() -> PathBuf {
    config_dir().join(SETTINGS_FILE)
}

pub fn images_dir() -> PathBuf {
    config_dir().join(IMAGES_DIR)
}

pub fn load() -> Settings {
    let mut settings: Settings = std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).map_err(|e| log::warn!("settings.json повреждён: {e}")).ok())
        .unwrap_or_default();
    settings.macropad.resize();
    settings
}

/// Пишет во временный файл и переименовывает — при сбое старые настройки не теряются.
pub fn save(settings: &Settings) -> Result<()> {
    // Тесты гоняют App::update — настоящий файл при этом трогать нельзя.
    if cfg!(test) {
        return Ok(());
    }
    let path = settings_path();
    std::fs::create_dir_all(config_dir())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(settings)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Уменьшает картинку до IMAGE_SIZE, сохраняет PNG в папку макропада и возвращает имя файла.
pub fn import_image(source: &Path) -> Result<String> {
    let image = image::open(source).with_context(|| format!("не удалось открыть {}", source.display()))?;
    save_image(image)
}

/// Сохраняет готовую картинку (например, иконку программы) в папку макропада.
pub fn save_image(image: impl Into<image::DynamicImage>) -> Result<String> {
    let image = image.into().thumbnail(IMAGE_SIZE, IMAGE_SIZE);
    std::fs::create_dir_all(images_dir())?;
    let name = new_file_name("png");
    image.save_with_format(images_dir().join(&name), image::ImageFormat::Png)?;
    Ok(name)
}

/// Свободное имя файла в папке макропада. Метка времени может совпасть, если за одну
/// миллисекунду добавлено несколько файлов (перетащили сразу несколько) — тогда с номером.
fn new_file_name(ext: &str) -> String {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    (0..)
        .map(|i| if i == 0 { format!("{stamp}.{ext}") } else { format!("{stamp}-{i}.{ext}") })
        .find(|name| !images_dir().join(name).exists())
        .expect("бесконечная последовательность имён")
}

/// Копирует звук в папку макропада — как картинки, он не зависит от того, где лежал исходный файл.
pub fn save_sound(source: &Path) -> Result<String> {
    let ext = source.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let name = new_file_name(&ext);
    // Тесты гоняют App::update — настоящую папку при этом трогать нельзя.
    if !cfg!(test) {
        std::fs::create_dir_all(images_dir())?;
        std::fs::copy(source, images_dir().join(&name))?;
    }
    Ok(name)
}

/// Готовый PNG для кнопки: картинка из папки макропада, при необходимости перекрашенная.
/// `None` — файла нет (его удалили) или он повреждён.
pub fn render_image(name: &str, tint: Option<[u8; 3]>) -> Option<Vec<u8>> {
    let path = images_dir().join(name);
    let Some([r, g, b]) = tint else { return std::fs::read(path).ok() };
    let mut image = image::open(path).ok()?.to_rgba8();
    // Форма сохраняется через прозрачность, цвет заменяется целиком — как у одноцветных иконок.
    for pixel in image.pixels_mut() {
        pixel.0 = [r, g, b, pixel.0[3]];
    }
    let mut png = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(png)
}

/// Удаляет копии картинок и звуков, на которые больше не ссылается ни одна кнопка.
pub fn remove_unused_files(pad: &MacropadSettings) {
    if cfg!(test) {
        return;
    }
    let used = [pad.images(), pad.sounds()].concat();
    let Ok(entries) = std::fs::read_dir(images_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !used.contains(&name.as_str()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_files_get_defaults_for_new_fields() {
        let s: Settings = serde_json::from_str(r#"{"fps": 30, "macropad": {"columns": 2}}"#).unwrap();
        assert_eq!(s.fps, 30);
        assert!(s.video_enabled);
        assert_eq!(s.macropad.rows, 2);
    }

    #[test]
    fn macropad_resizes_with_grid() {
        let mut pad = MacropadSettings::default();
        assert_eq!(pad.buttons.len(), 6);
        pad.buttons[0].states[0].label = "Mic".into();
        pad.columns = 1;
        pad.rows = 1;
        pad.resize();
        assert_eq!(pad.buttons.len(), 1);
        assert_eq!(pad.buttons[0].states[0].label, "Mic");
        assert_eq!(pad.buttons[0].states.len(), TOGGLE_STATES);
    }

    #[test]
    fn roundtrip() {
        let mut s = Settings::default();
        s.macropad.buttons[1].keys.ctrl = true;
        s.macropad.buttons[1].toggle = true;
        s.macropad.buttons[1].state = 1;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
    }

    #[test]
    fn folders_resize_and_keep_images() {
        let mut pad = MacropadSettings::default();
        pad.buttons[2].folder = true;
        pad.resize();
        assert_eq!(pad.buttons[2].children.len(), 6);
        pad.buttons[2].children[3].states[0].image = Some("inside.png".into());
        pad.columns = 2;
        pad.resize();
        assert_eq!(pad.buttons[2].children.len(), 4, "папка следует размеру сетки");
        assert_eq!(pad.images(), ["inside.png"], "картинки из папок не считаются лишними");
        assert_eq!(pad.page(&[2])[3].states[0].image.as_deref(), Some("inside.png"));
    }

    #[test]
    fn sounds_are_kept_even_if_button_changed_kind() {
        let mut pad = MacropadSettings::default();
        pad.buttons[0].sound = true;
        pad.buttons[0].sound_file = "1.mp3".into();
        pad.buttons[1].sound_file = "2.wav".into();
        pad.buttons[2].folder = true;
        pad.resize();
        pad.buttons[2].children[1].sound_file = "3.ogg".into();
        assert_eq!(pad.sounds(), ["1.mp3", "2.wav", "3.ogg"]);
    }
}
