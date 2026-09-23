//! Иконка в трее, как в YeruHost.
//!
//! Вместе с автозапуском: Escanor стартует вместе с Windows сразу в трее и держит
//! телефон подключённым, не занимая место на панели задач. Окно при этом не
//! закрывается, а прячется — движок, видео и звук продолжают работать.

use iced::futures::Stream;
use iced::futures::channel::mpsc;
use std::sync::OnceLock;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Один пункт на показ и скрытие: состояние знает приложение.
    Toggle,
    Quit,
}

const SHOW: &str = "Показать окно";
const HIDE: &str = "Скрыть окно";

/// Идентификаторы пунктов меню: их читает поток событий, созданный подпиской.
static IDS: OnceLock<(MenuId, MenuId)> = OnceLock::new();

pub struct Tray {
    /// Пока объект жив, значок висит в трее.
    _icon: TrayIcon,
    toggle: MenuItem,
}

impl Tray {
    pub fn new() -> Option<Self> {
        let toggle = MenuItem::new(HIDE, true, None);
        let quit = MenuItem::new("Выход", true, None);
        let menu = Menu::new();
        menu.append(&toggle).ok()?;
        menu.append(&quit).ok()?;
        let _ = IDS.set((toggle.id().clone(), quit.id().clone()));

        const SIZE: u32 = 32;
        let icon = tray_icon::Icon::from_rgba(crate::icon::render(SIZE), SIZE, SIZE).ok()?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Escanor")
            .with_icon(icon)
            .build()
            .map_err(|e| log::warn!("трей недоступен: {e}"))
            .ok()?;
        Some(Self { _icon: tray, toggle })
    }

    /// Подпись пункта меню вслед за состоянием окна.
    pub fn set_hidden(&self, hidden: bool) {
        self.toggle.set_text(if hidden { SHOW } else { HIDE });
    }
}

/// События трея для подписки iced. Каналы `tray-icon` глобальные и читаются из любого потока.
pub fn events() -> impl Stream<Item = TrayCommand> {
    let (tx, rx) = mpsc::unbounded();
    let menu_tx = tx.clone();
    std::thread::spawn(move || {
        while let Ok(event) = MenuEvent::receiver().recv() {
            let Some((toggle, quit)) = IDS.get() else { continue };
            let command = if event.id == *quit {
                TrayCommand::Quit
            } else if event.id == *toggle {
                TrayCommand::Toggle
            } else {
                continue;
            };
            if menu_tx.unbounded_send(command).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        while let Ok(event) = TrayIconEvent::receiver().recv() {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) && tx.unbounded_send(TrayCommand::Toggle).is_err() {
                break;
            }
        }
    });
    rx
}
