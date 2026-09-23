//! Оформление в тонах YeruVerse и YeruNeko: те же цвета, скругления и поведение кнопок,
//! что в их style.css.

use iced::theme::palette::{Extended, Pair};
use iced::theme::{Palette, Theme};
use iced::widget::{button, container, text};
use iced::{Background, Color, border};

pub const BG: Color = rgb(0x0F, 0x11, 0x15);
pub const PANEL: Color = rgb(0x17, 0x1A, 0x21);
pub const TEXT: Color = rgb(0xE8, 0xEA, 0xED);
pub const MUTED: Color = rgb(0x8B, 0x93, 0xA1);
pub const ACCENT: Color = rgb(0x8B, 0x5C, 0xF6);
pub const ACCENT_SOFT: Color = rgb(0xA7, 0x8B, 0xFA);
pub const OK: Color = rgb(0x3E, 0xCF, 0x8E);
pub const WARN: Color = rgb(0xF0, 0xA0, 0x20);
pub const BAD: Color = rgb(0xFF, 0x6B, 0x6B);
/// `color-mix(text 14%, transparent)` поверх фона.
pub const BORDER: Color = rgb(0x2D, 0x2F, 0x33);
/// Чуть темнее фона — под превью видео.
pub const WELL: Color = rgb(0x0A, 0x0C, 0x10);

/// Скругления: метки, кнопки и поля, большие панели.
pub const R_SM: f32 = 8.0;
pub const RADIUS: f32 = 12.0;
pub const R_LG: f32 = 16.0;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}

pub fn theme() -> Theme {
    let palette = Palette { background: BG, text: TEXT, primary: ACCENT, success: OK, warning: WARN, danger: BAD };
    Theme::custom_with_fn("Yeru", palette, |palette| {
        let mut extended = Extended::generate(palette);
        // Панели и поля — ровно как в CSS, а не вычисленные оттенки фона.
        extended.background.weak = Pair::new(PANEL, TEXT);
        extended.background.strong = Pair::new(BORDER, TEXT);
        extended.primary.base = Pair::new(ACCENT, Color::WHITE);
        extended.primary.strong = Pair::new(ACCENT_SOFT, Color::WHITE);
        extended
    })
}

// --- Текст ---

pub fn muted(_: &Theme) -> text::Style {
    text::Style { color: Some(MUTED) }
}

pub fn accent(_: &Theme) -> text::Style {
    text::Style { color: Some(ACCENT_SOFT) }
}

pub fn ok(_: &Theme) -> text::Style {
    text::Style { color: Some(OK) }
}

pub fn warn(_: &Theme) -> text::Style {
    text::Style { color: Some(WARN) }
}

pub fn bad(_: &Theme) -> text::Style {
    text::Style { color: Some(BAD) }
}

// --- Кнопки ---

/// Обычная кнопка: фон панели, тонкая рамка; при наведении рамка становится акцентной.
pub fn btn(_: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let disabled = matches!(status, button::Status::Disabled);
    button::Style {
        background: Some(Background::Color(PANEL)),
        text_color: if disabled { MUTED } else { TEXT },
        border: border::rounded(RADIUS).width(1).color(if hovered { ACCENT } else { BORDER }),
        ..Default::default()
    }
}

/// Главное действие: заливка акцентом, белый текст.
pub fn btn_primary(_: &Theme, status: button::Status) -> button::Style {
    let color = match status {
        button::Status::Hovered | button::Status::Pressed => ACCENT_SOFT,
        button::Status::Disabled => alpha(ACCENT, 0.4),
        button::Status::Active => ACCENT,
    };
    button::Style {
        background: Some(Background::Color(color)),
        text_color: Color::WHITE,
        border: border::rounded(RADIUS),
        ..Default::default()
    }
}

/// Кнопка-ссылка без рамки: приглушённая, при наведении — обычного цвета.
pub fn btn_link(_: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style { background: None, text_color: if hovered { TEXT } else { MUTED }, ..Default::default() }
}

/// Сегмент переключателя: выбранный залит акцентом, остальные — как ссылки.
pub fn segment(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        if selected {
            button::Style { border: border::rounded(R_SM), ..btn_primary(theme, button::Status::Active) }
        } else {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then_some(Background::Color(BORDER)),
                text_color: if hovered { TEXT } else { MUTED },
                border: border::rounded(R_SM),
                ..Default::default()
            }
        }
    }
}

// --- Контейнеры ---

pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PANEL)),
        border: border::rounded(R_LG).width(1).color(BORDER),
        ..Default::default()
    }
}

/// Утопленная область внутри панели: поле сегментов, миниатюра.
pub fn inset(_: &Theme) -> container::Style {
    container::Style { background: Some(Background::Color(BG)), border: border::rounded(RADIUS), ..Default::default() }
}

pub fn well(_: &Theme) -> container::Style {
    container::Style { background: Some(Background::Color(WELL)), border: border::rounded(R_LG), ..Default::default() }
}

pub fn pill(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PANEL)),
        border: border::rounded(20).width(1).color(BORDER),
        ..Default::default()
    }
}

/// Всплывающая подсказка: панель с рамкой поверх содержимого.
pub fn tooltip(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PANEL)),
        text_color: Some(TEXT),
        border: border::rounded(R_SM).width(1).color(BORDER),
        shadow: iced::Shadow {
            color: alpha(Color::BLACK, 0.5),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
        ..Default::default()
    }
}

/// Кружок «i», у которого при наведении появляется подсказка.
pub fn info_badge(_: &Theme) -> container::Style {
    container::Style { border: border::rounded(8).width(1).color(MUTED), ..Default::default() }
}

/// Плашка клавиши в сочетании.
pub fn keycap(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG)),
        border: border::rounded(R_SM).width(1).color(BORDER),
        ..Default::default()
    }
}

#[derive(Clone, Copy)]
pub enum Tone {
    Info,
    Warning,
    Danger,
}

/// Баннер: полупрозрачная заливка цветом тона и рамка того же цвета.
pub fn banner(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |_| {
        let color = match tone {
            Tone::Info => ACCENT,
            Tone::Warning => WARN,
            Tone::Danger => BAD,
        };
        container::Style {
            background: Some(Background::Color(alpha(color, 0.12))),
            text_color: Some(TEXT),
            border: border::rounded(RADIUS).width(1).color(alpha(color, 0.45)),
            ..Default::default()
        }
    }
}

// --- Поля ---

/// Выпадающий список: как кнопка — фон панели, тонкая рамка, акцент при наведении.
pub fn select(_: &Theme, status: iced::widget::pick_list::Status) -> iced::widget::pick_list::Style {
    use iced::widget::pick_list::Status;
    let active = !matches!(status, Status::Active);
    iced::widget::pick_list::Style {
        text_color: TEXT,
        placeholder_color: MUTED,
        handle_color: if active { ACCENT_SOFT } else { MUTED },
        background: Background::Color(BG),
        border: border::rounded(RADIUS).width(1).color(if active { ACCENT } else { BORDER }),
    }
}

/// Раскрытое меню списка.
pub fn menu(_: &Theme) -> iced::overlay::menu::Style {
    iced::overlay::menu::Style {
        background: Background::Color(PANEL),
        border: border::rounded(RADIUS).width(1).color(BORDER),
        text_color: TEXT,
        selected_text_color: Color::WHITE,
        selected_background: Background::Color(ACCENT),
        shadow: iced::Shadow::default(),
    }
}

pub fn input(_: &Theme, status: iced::widget::text_input::Status) -> iced::widget::text_input::Style {
    use iced::widget::text_input::Status;
    let focused = matches!(status, Status::Focused { .. } | Status::Hovered);
    iced::widget::text_input::Style {
        background: Background::Color(BG),
        border: border::rounded(RADIUS).width(1).color(if focused { ACCENT } else { BORDER }),
        icon: MUTED,
        placeholder: MUTED,
        value: TEXT,
        selection: alpha(ACCENT, 0.45),
    }
}
