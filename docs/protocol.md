# Протокол Escanor v6

Телефон слушает `127.0.0.1:27183`, ПК подключается через `adb forward tcp:27183 tcp:27183`.
ПК открывает три TCP-соединения; первый байт каждого — номер канала:

| Байт | Канал | Формат |
|---|---|---|
| `1` | управление | JSON, одна строка на сообщение (`\n`) |
| `2` | видео | медиапакеты |
| `3` | звук | медиапакеты |

Порядок: управление → `hello` → видео и звук → ждать `channel` для обоих и `devices`.

## Медиапакет (big-endian)

```
u32 size | u8 flags | i64 pts_us | payload[size]
```

`flags`: `1` — конфигурация кодека (SPS/PPS), `2` — ключевой кадр.

- **Видео**: H.264 Annex B, Constrained Baseline, без B-кадров, BT.709 limited range.
  Один пакет — один кадр. `pts_us` — время снимка сенсора (часы из `timestamp_source`).
- **Звук**: PCM s16le, 48 кГц, 1 или 2 канала, по 5 мс. `pts_us` — по CLOCK_MONOTONIC.

## Управление

ПК → телефон (`type`):

| type | поля |
|---|---|
| `hello` | `version` |
| `get_devices` | — |
| `ping` | `t` — время ПК, мкс |
| `start_video` | `camera`, `physical`, `width`, `height`, `fps`, `bitrate` (видео всегда H.264) |
| `stop_video`, `request_keyframe` | — |
| `set_controls` | любые из: `zoom`, `ev`, `focus` (`continuous`/`manual`), `focus_distance`, `white_balance`, `torch`, `stabilization`, `ae_lock`, `awb_lock` |
| `start_audio` | `device` (id или null), `source`, `channels` |
| `stop_audio` | — |
| `macropad` | `columns`, `rows`, `orientation` (`auto`, `portrait`, `landscape`, `reverse_portrait`, `reverse_landscape`), `amoled {dim_after_secs, dim_brightness}` (затемнение: через сколько секунд без касаний, 0 — никогда; яркость в %), `pages: [{parent, buttons: [{kind, target, states: [{label, image}], state}]}]` — страница 0 корневая; `kind`: `keys`, `folder` (открывает страницу `target`), `back`; image: PNG в base64 или null |
| `macropad_state` | `page`, `id`, `state` — переключатель сменил состояние |
| `macropad_off` | — |

Телефон → ПК: `hello {version, phone}`, `devices {cameras, microphones, audio_sources}`,
`pong {t, realtime_us, monotonic_us}`, `channel {channel}`, `video_started`, `video_stopped`,
`video_error {message}`, `audio_started {sample_rate, channels, device}`, `audio_stopped`,
`audio_error {message}`, `macropad_press {page, id, down}` (кнопка нажата/отпущена; папки и «Назад» телефон открывает сам), `error {message}`.

Задержка «сенсор → ПК» считается по смещению часов, полученному из `ping`/`pong` с минимальным RTT.
