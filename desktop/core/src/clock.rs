//! Сопоставление часов телефона и ПК, чтобы мерить задержку «сенсор → ПК».
//!
//! ПК шлёт ping со своим временем, телефон отвечает своим временем по двум часам
//! (CLOCK_BOOTTIME для камеры и CLOCK_MONOTONIC для звука). Смещение берём по ответу
//! с наименьшим RTT за последние замеры — у него наименьшая погрешность.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

/// Монотонное время ПК в микросекундах.
pub fn now_us() -> i64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneClock {
    Realtime,
    Monotonic,
}

const UNKNOWN: i64 = i64::MIN;

/// Смещения «время телефона − время ПК», доступные из потоков приёма.
#[derive(Debug)]
pub struct ClockOffsets {
    realtime: AtomicI64,
    monotonic: AtomicI64,
    /// Время пути «ПК → телефон → ПК» у последнего замера, мкс.
    last_rtt: AtomicI64,
}

impl Default for ClockOffsets {
    fn default() -> Self {
        Self {
            realtime: AtomicI64::new(UNKNOWN),
            monotonic: AtomicI64::new(UNKNOWN),
            last_rtt: AtomicI64::new(UNKNOWN),
        }
    }
}

impl ClockOffsets {
    pub fn get(&self, clock: PhoneClock) -> Option<i64> {
        let value = match clock {
            PhoneClock::Realtime => self.realtime.load(Ordering::Relaxed),
            PhoneClock::Monotonic => self.monotonic.load(Ordering::Relaxed),
        };
        (value != UNKNOWN).then_some(value)
    }

    /// Задержка от момента `phone_us` (по часам телефона) до текущего момента на ПК.
    pub fn latency_us(&self, clock: PhoneClock, phone_us: i64) -> Option<i64> {
        self.get(clock).map(|offset| now_us() - (phone_us - offset))
    }

    pub fn rtt_us(&self) -> Option<i64> {
        let value = self.last_rtt.load(Ordering::Relaxed);
        (value != UNKNOWN).then_some(value)
    }

    pub fn reset(&self) {
        self.realtime.store(UNKNOWN, Ordering::Relaxed);
        self.monotonic.store(UNKNOWN, Ordering::Relaxed);
        self.last_rtt.store(UNKNOWN, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    rtt: i64,
    realtime: i64,
    monotonic: i64,
}

/// Накопитель замеров; живёт в потоке движка и обновляет `ClockOffsets`.
#[derive(Debug, Default)]
pub struct ClockSync {
    samples: VecDeque<Sample>,
}

impl ClockSync {
    const WINDOW: usize = 16;

    pub fn on_pong(&mut self, sent_us: i64, realtime_us: i64, monotonic_us: i64, offsets: &ClockOffsets) {
        let received = now_us();
        let rtt = received - sent_us;
        if rtt < 0 {
            return;
        }
        offsets.last_rtt.store(rtt, Ordering::Relaxed);
        let midpoint = sent_us + rtt / 2;
        if self.samples.len() == Self::WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample { rtt, realtime: realtime_us - midpoint, monotonic: monotonic_us - midpoint });
        let best = self.samples.iter().min_by_key(|s| s.rtt).unwrap();
        offsets.realtime.store(best.realtime, Ordering::Relaxed);
        offsets.monotonic.store(best.monotonic, Ordering::Relaxed);
    }

    pub fn reset(&mut self, offsets: &ClockOffsets) {
        self.samples.clear();
        offsets.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_lowest_rtt_sample() {
        let offsets = ClockOffsets::default();
        let mut sync = ClockSync::default();
        assert_eq!(offsets.get(PhoneClock::Realtime), None);

        let t = now_us();
        // Телефон опережает ПК на 1 000 000 мкс; ответ пришёл «мгновенно».
        sync.on_pong(t, t + 1_000_000, t + 500_000, &offsets);
        let rt = offsets.get(PhoneClock::Realtime).unwrap();
        assert!((rt - 1_000_000).abs() < 5_000, "offset {rt}");

        // Замер с огромным RTT не должен вытеснить точный.
        sync.on_pong(t - 400_000, t + 9_999_999, t, &offsets);
        let rt2 = offsets.get(PhoneClock::Realtime).unwrap();
        assert_eq!(rt, rt2);
    }
}
