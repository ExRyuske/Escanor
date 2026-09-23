//! Ядро Escanor для ПК: связь с телефоном, декодирование, звук и виртуальная камера.

pub mod adb;
pub mod audio;
pub mod clock;
pub mod decoder;
pub mod engine;
pub mod keys;
pub mod launcher;
pub mod macropad;
pub mod protocol;
pub mod vcam;
pub mod video;
#[cfg(windows)]
mod wasapi;

pub use engine::{Command, EngineHandle, Event, EventSink, Stats, spawn};
