// Без консольного окна в сборке для Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod appicon;
mod icon;
mod settings;
mod style;
mod system;
mod tray;
mod update;
mod view;

use app::{App, Message};
use escanor_core::video::PreviewFrame;
use escanor_core::{Event, EventSink};
use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use iced::{Size, Subscription, window};
use std::sync::{Arc, Mutex};

fn main() -> iced::Result {
    // Режим установки виртуальной камеры: программа перезапускает себя с правами администратора.
    if std::env::args().any(|a| a == "--install-vcam") {
        let code = match escanor_core::vcam::run_install() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e:#}");
                1
            }
        };
        std::process::exit(code);
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // Второй экземпляр только выводит первый на передний план.
    if !system::take_single_instance() {
        return Ok(());
    }
    update::cleanup();

    let start_hidden = std::env::args().any(|a| a == system::TRAY_ARG);
    const ICON: u32 = 64;
    let window = window::Settings {
        size: Size::new(1280.0, 800.0),
        min_size: Some(Size::new(980.0, 640.0)),
        icon: window::icon::from_rgba(icon::render(ICON), ICON, ICON).ok(),
        // При автозапуске окно не мелькает: оно сразу создаётся скрытым.
        visible: !start_hidden,
        exit_on_close_request: false,
        ..Default::default()
    };
    iced::application(move || App::boot(settings::load(), start_hidden), App::update, view::view)
        .title("Escanor")
        .window(window)
        .subscription(|app: &App| {
            let mut subscriptions = vec![
                Subscription::run(engine_events),
                Subscription::run(tray::events).map(Message::Tray),
                window::open_events().map(Message::WindowOpened),
                window::close_requests().map(Message::CloseRequested),
                iced::event::listen_with(|event, _status, _window| match event {
                    iced::Event::Window(window::Event::FileDropped(path)) => Some(Message::FileDropped(path)),
                    _ => None,
                }),
            ];
            if app.recording {
                subscriptions.push(iced::event::listen_with(|event, _status, _window| match event {
                    iced::Event::Keyboard(_)
                    | iced::Event::Mouse(
                        iced::mouse::Event::ButtonPressed(_) | iced::mouse::Event::WheelScrolled { .. },
                    ) => Some(Message::PadInput(event)),
                    _ => None,
                }));
            }
            Subscription::batch(subscriptions)
        })
        .theme(|_: &App| style::theme())
        .run()
}

enum Internal {
    Event(Event),
    PreviewReady,
}

/// Запускает движок и превращает его события в сообщения интерфейса.
/// Кадры превью не копятся: если интерфейс не успевает, промежуточные заменяются свежими.
fn engine_events() -> impl Stream<Item = Message> {
    iced::stream::channel(64, async |mut output: mpsc::Sender<Message>| {
        let (tx, mut rx) = mpsc::unbounded::<Internal>();
        let latest_preview: Arc<Mutex<Option<PreviewFrame>>> = Arc::default();

        let slot = latest_preview.clone();
        let sink: EventSink = Arc::new(move |event| match event {
            Event::Preview(frame) => {
                if slot.lock().unwrap().replace(frame).is_none() {
                    let _ = tx.unbounded_send(Internal::PreviewReady);
                }
            }
            other => {
                let _ = tx.unbounded_send(Internal::Event(other));
            }
        });
        let handle = escanor_core::spawn(sink);
        let _ = output.send(Message::EngineReady(handle)).await;

        while let Some(item) = rx.next().await {
            let message = match item {
                Internal::Event(event) => Message::Engine(event),
                Internal::PreviewReady => match latest_preview.lock().unwrap().take() {
                    Some(frame) => Message::Engine(Event::Preview(frame)),
                    None => continue,
                },
            };
            if output.send(message).await.is_err() {
                break;
            }
        }
    })
}
