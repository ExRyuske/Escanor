//! Макропад: экран телефона как панель кнопок. Раскладка задаётся на ПК,
//! нажатия приходят с телефона и превращаются в нажатия клавиш на ПК.

use crate::keys::KeyCombo;
use serde::{Deserialize, Serialize};

/// Ориентация экрана телефона, пока показан макропад.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    /// Как повернёт пользователь (по датчику).
    #[default]
    Auto,
    Portrait,
    Landscape,
    ReversePortrait,
    ReverseLandscape,
}

impl Orientation {
    pub const ALL: [Orientation; 5] =
        [Self::Auto, Self::Portrait, Self::Landscape, Self::ReversePortrait, Self::ReverseLandscape];
}

impl std::fmt::Display for Orientation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "Как повёрнут телефон",
            Self::Portrait => "Вертикально",
            Self::Landscape => "Горизонтально",
            Self::ReversePortrait => "Вертикально, перевёрнуто",
            Self::ReverseLandscape => "Горизонтально, перевёрнуто",
        })
    }
}

/// Внешний вид одного состояния кнопки.
#[derive(Debug, Clone, PartialEq)]
pub struct PadState {
    pub label: String,
    /// PNG, уже уменьшенный и перекрашенный.
    pub image_png: Option<Vec<u8>>,
}

/// Что делает кнопка.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PadKind {
    /// Нажимает сочетание клавиш на ПК.
    #[default]
    Keys,
    /// Открывает страницу-папку (номер в `PadLayout::pages`).
    Folder(usize),
    /// «Назад» — первая ячейка каждой папки, возвращает на родительскую страницу.
    Back,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PadButton {
    pub kind: PadKind,
    /// Переключатель: нажатие отправляет сочетание и переходит к следующему состоянию.
    /// Обычная кнопка: сочетание удерживается, пока держат кнопку.
    pub toggle: bool,
    /// Одно сочетание на кнопку: у переключателя меняется только вид.
    pub keys: KeyCombo,
    /// Программа, файл или ссылка: открывается касанием вместо нажатия клавиш.
    pub launch: Option<String>,
    /// Одно состояние у обычной кнопки и папки, два — у переключателя.
    pub states: Vec<PadState>,
    pub state: usize,
}

impl PadButton {
    pub fn current(&self) -> Option<&PadState> {
        self.states.get(self.state).or_else(|| self.states.first())
    }
}

/// Страница макропада: корневая или папка.
#[derive(Debug, Clone, PartialEq)]
pub struct PadPage {
    /// Куда ведёт «Назад»; у корневой страницы — `None`.
    pub parent: Option<usize>,
    /// По строкам, `columns * rows` штук.
    pub buttons: Vec<PadButton>,
}

/// Макропад всегда бережёт AMOLED-экран: чёрный фон и сдвиг пикселей. Настраивается
/// затемнение при бездействии: когда и насколько.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Amoled {
    /// Через сколько секунд без касаний приглушать экран; 0 — никогда.
    pub dim_after_secs: u32,
    /// Яркость экрана в затемнении, % от максимальной.
    pub dim_brightness: u32,
}

impl Amoled {
    pub const BRIGHTNESS: std::ops::RangeInclusive<u32> = 1..=60;
}

impl Default for Amoled {
    fn default() -> Self {
        Self { dim_after_secs: 60, dim_brightness: 15 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PadLayout {
    pub columns: u32,
    pub rows: u32,
    pub orientation: Orientation,
    pub amoled: Amoled,
    /// Страница 0 — корневая, остальные — папки.
    pub pages: Vec<PadPage>,
}

impl PadLayout {
    pub fn button(&self, page: usize, index: usize) -> Option<&PadButton> {
        self.pages.get(page)?.buttons.get(index)
    }

    pub fn button_mut(&mut self, page: usize, index: usize) -> Option<&mut PadButton> {
        self.pages.get_mut(page)?.buttons.get_mut(index)
    }
}
