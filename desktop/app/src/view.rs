//! Отрисовка окна. Главное видно сразу, второстепенное свёрнуто, а всё, что требует
//! действия пользователя, показывается баннером с нужной кнопкой.

use crate::app::{
    App, BITRATES, BitrateChoice, ButtonKind, Connection, DimChoice, Message, Modifier, Panel, Tab, UpdateState,
    WbChoice,
};
use crate::settings::{ButtonSettings, SOUND_MUTE_DB, TOGGLE_STATES};
use crate::style::{self, Tone};
use escanor_core::keys::{Key, KeyCombo};
use escanor_core::macropad::{Amoled, Orientation};
use escanor_core::vcam::VcamStatus;
use iced::font::Weight;
use iced::widget::{
    Column, PickList, Row, Toggler, button, column, container, image, mouse_area, pick_list, row, rule, scrollable,
    slider, space, text, text_editor, text_input, toggler, tooltip,
};
use iced::{Alignment, Color, ContentFit, Element, Font, Length, Theme, border};

const SIDE_WIDTH: f32 = 340.0;
const SMALL: u32 = 13;
/// Основной текст: подписи переключателей, списки, поля ввода.
const BODY: u32 = 14;
const PAD_CELL: f32 = 116.0;
const BOLD: Font = Font { weight: Weight::Semibold, ..Font::DEFAULT };

pub fn view(app: &App) -> Element<'_, Message> {
    let body = match app.tab {
        Tab::Stream => stream_view(app),
        Tab::Macropad => macropad_view(app),
        Tab::Settings => settings_view(app),
    };
    column![header(app), rule::horizontal(1), banners(app), body].into()
}

// --- Общие элементы ---

/// Группа настроек на слегка выделенном фоне.
fn panel<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).padding(16).width(Length::Fill).style(style::panel).into()
}

/// Заголовок группы, справа — необязательный переключатель.
fn group_header<'a>(title: &'a str, toggle: Option<Element<'a, Message>>) -> Element<'a, Message> {
    let mut header = row![text(title).size(15).font(BOLD)].align_y(Alignment::Center);
    if let Some(t) = toggle {
        header = header.push(space::horizontal()).push(t);
    }
    header.into()
}

/// Заголовок группы со значком «i»: пояснение показывается при наведении.
fn titled<'a>(title: &'a str, tip: impl Into<String>) -> Element<'a, Message> {
    row![text(title).size(15).font(BOLD), info(tip)].spacing(8).align_y(Alignment::Center).into()
}

/// Значок «i» с всплывающей подсказкой вместо текста, занимающего место.
fn info<'a>(tip: impl Into<String>) -> Element<'a, Message> {
    tooltip(
        container(text("i").size(11).font(BOLD).style(style::muted)).center(16).style(style::info_badge),
        container(text(tip.into()).size(SMALL)).padding([8, 12]).max_width(320).style(style::tooltip),
        tooltip::Position::Bottom,
    )
    .gap(6)
    .into()
}

/// Поле с подписью и пояснением во всплывающей подсказке.
fn field_tip<'a>(
    label: &'a str,
    tip: impl Into<String>,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![
        row![text(label).size(SMALL).style(style::muted), info(tip)].spacing(6).align_y(Alignment::Center),
        control.into()
    ]
    .spacing(6)
    .into()
}

fn field<'a>(label: &'a str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![text(label).size(SMALL).style(style::muted), control.into()].spacing(6).into()
}

fn hint<'a>(value: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(value).size(SMALL).style(style::muted).into()
}

/// Ссылка «▸ Заголовок», раскрывающая дополнительный блок.
fn disclosure<'a>(app: &App, panel: Panel, title: &'a str) -> Element<'a, Message> {
    let arrow = if app.is_open(panel) { "▾" } else { "▸" };
    button(text(format!("{arrow}  {title}")).size(SMALL))
        .padding([2, 0])
        .style(style::btn_link)
        .on_press(Message::Toggle(panel))
        .into()
}

/// Переключатель-сегмент: несколько вариантов, выбран один.
fn segmented<'a, T: Copy + PartialEq + 'a>(
    options: &[(T, &'a str)],
    selected: T,
    on: impl Fn(T) -> Message,
) -> Element<'a, Message> {
    segmented_rows(options, options.len(), selected, on)
}

/// Сегменты в несколько строк по `per_row`: когда вариантов много, в одну строку их подписи не влезают.
fn segmented_rows<'a, T: Copy + PartialEq + 'a>(
    options: &[(T, &'a str)],
    per_row: usize,
    selected: T,
    on: impl Fn(T) -> Message,
) -> Element<'a, Message> {
    let mut rows = Column::new().spacing(4);
    for chunk in options.chunks(per_row.max(1)) {
        let mut segments = Row::new().spacing(4);
        for &(value, label) in chunk {
            segments = segments.push(
                button(text(label).size(SMALL).center().wrapping(text::Wrapping::None))
                    .width(Length::Fill)
                    .padding([6, 8])
                    .style(style::segment(value == selected))
                    .on_press(on(value)),
            );
        }
        // Неполная последняя строка — такой же ширины ячейки, как остальные.
        for _ in chunk.len()..per_row {
            segments = segments.push(space().width(Length::Fill));
        }
        rows = rows.push(segments);
    }
    container(rows).padding(3).style(style::inset).into()
}

/// Выпадающий список в общем стиле, на всю ширину.
fn select<'a, T, L, V>(
    options: L,
    selected: Option<V>,
    on: impl Fn(T) -> Message + 'a,
) -> PickList<'a, T, L, V, Message>
where
    T: ToString + PartialEq + Clone + 'a,
    L: std::borrow::Borrow<[T]> + 'a,
    V: std::borrow::Borrow<T> + 'a,
{
    pick_list(options, selected, on).style(style::select).menu_style(style::menu).text_size(BODY).width(Length::Fill)
}

/// Переключатель с подписью.
fn switch<'a>(label: &'a str, value: bool, on: impl Fn(bool) -> Message + 'a) -> Toggler<'a, Message> {
    toggler(value).label(label).text_size(BODY).on_toggle(on)
}

fn pill<'a>(label: impl text::IntoFragment<'a>, tone: fn(&Theme) -> text::Style) -> Element<'a, Message> {
    container(text(label).size(SMALL).style(tone)).padding([4, 10]).style(style::pill).into()
}

// --- Шапка и баннеры ---

fn header(app: &App) -> Element<'_, Message> {
    let title = text("Escanor").size(20).font(BOLD).style(style::accent);
    let tabs = container(segmented(
        &[(Tab::Stream, "Камера и звук"), (Tab::Macropad, "Макропад"), (Tab::Settings, "Настройки")],
        app.tab,
        Message::SelectTab,
    ))
    .width(420);

    let mut right = row![].spacing(10).align_y(Alignment::Center);
    match &app.vcam {
        Some(VcamStatus::Ready) => right = right.push(pill("Веб-камера готова", style::muted)),
        Some(VcamStatus::Off) => right = right.push(pill("Веб-камера выключена", style::muted)),
        Some(VcamStatus::InUse { width, height }) => {
            right = right.push(pill(format!("Веб-камера используется · {width}×{height}"), style::ok))
        }
        _ => {}
    }
    right = right.push(connection(app));

    row![title, tabs, space::horizontal(), right].spacing(24).padding([10, 16]).align_y(Alignment::Center).into()
}

fn connection(app: &App) -> Element<'_, Message> {
    let devices: Vec<_> = app.adb_devices.iter().map(crate::app::DeviceChoice::from).collect();
    let mut row = row![].spacing(8).align_y(Alignment::Center);
    // Выбор телефона нужен, только если их несколько.
    if devices.len() > 1 {
        row = row.push(select(devices, app.device.clone(), Message::SelectDevice).text_size(SMALL).width(220));
    }
    match &app.connection {
        Connection::Connected { phone } => {
            row = row
                .push(pill(format!("● {} {}", phone.manufacturer, phone.model), style::ok))
                .push(button(text("Отключить").size(SMALL)).style(style::btn_link).on_press(Message::Disconnect));
        }
        Connection::Connecting { stage } => row = row.push(pill(stage.as_str(), style::warn)),
        Connection::Idle { .. } => {
            row = row.push(pill("Телефон не подключён", style::muted));
            if app.device.is_some() {
                row = row
                    .push(button(text("Подключить").size(SMALL)).style(style::btn_primary).on_press(Message::Connect));
            }
        }
    }
    row.into()
}

fn banner<'a>(tone: Tone, message: String, action: Option<(&'a str, Message)>) -> Element<'a, Message> {
    let mut content = row![text(message).size(14).width(Length::Fill)].spacing(12).align_y(Alignment::Center);
    if let Some((label, msg)) = action {
        let style = if matches!(tone, Tone::Danger) { style::btn_link } else { style::btn_primary };
        content = content.push(button(text(label).size(SMALL)).style(style).on_press(msg));
    }
    container(content).padding([10, 16]).width(Length::Fill).style(style::banner(tone)).into()
}

/// То, что требует внимания: ошибки и незавершённая настройка — с кнопкой, которая это решает.
fn banners(app: &App) -> Element<'_, Message> {
    let mut list = Column::new().spacing(8);
    let mut empty = true;
    let mut add = |list: Column<'static, Message>, item: Element<'static, Message>| {
        empty = false;
        list.push(item)
    };
    if let Some(error) = &app.error {
        list = add(list, banner(Tone::Danger, error.clone(), Some(("Скрыть", Message::DismissError))));
    }
    if let Some(e) = &app.adb_error {
        list = add(list, banner(Tone::Danger, format!("Не удалось запустить adb: {e}"), None));
    } else if let Connection::Idle { reason: Some(reason) } = &app.connection {
        list = add(
            list,
            banner(Tone::Warning, reason.clone(), app.device.as_ref().map(|_| ("Повторить", Message::Connect))),
        );
    } else if matches!(app.connection, Connection::Idle { .. }) && app.adb_devices.is_empty() {
        list = add(
            list,
            banner(
                Tone::Info,
                "Подключите телефон кабелем и включите «Отладку по USB» (Настройки → Для разработчиков).".into(),
                None,
            ),
        );
    }
    match &app.update {
        UpdateState::Available(release) => {
            list = add(
                list,
                banner(
                    Tone::Info,
                    format!("Доступна новая версия Escanor {} (сейчас {}).", release.version, crate::update::VERSION),
                    Some(("Обновить", Message::InstallUpdate)),
                ),
            )
        }
        UpdateState::Installing(release) => {
            list = add(list, banner(Tone::Info, format!("Скачиваю и устанавливаю Escanor {}…", release.version), None))
        }
        _ => {}
    }
    match &app.vcam {
        Some(VcamStatus::NotInstalled) => {
            list = add(
                list,
                banner(
                    Tone::Info,
                    "Веб-камера Escanor не установлена — без неё Zoom, Discord и браузеры не увидят телефон.".into(),
                    Some(("Установить", Message::InstallVcam)),
                ),
            )
        }
        Some(VcamStatus::Outdated) => {
            list = add(
                list,
                banner(
                    Tone::Warning,
                    "Веб-камеру Escanor нужно обновить до этой версии.".into(),
                    Some(("Обновить", Message::InstallVcam)),
                ),
            )
        }
        Some(VcamStatus::Failed(e)) => {
            let log = app.vcam_log.as_ref().map(|l| format!("\nЖурнал: {l}")).unwrap_or_default();
            list = add(
                list,
                banner(Tone::Danger, format!("Веб-камера: {e}{log}"), Some(("Переустановить", Message::InstallVcam))),
            )
        }
        _ => {}
    }
    if empty {
        return space().into();
    }
    container(list).padding([12, 16]).into()
}

// --- Камера и звук ---

fn stream_view(app: &App) -> Element<'_, Message> {
    let side = scrollable(column![video_group(app), image_group(app), audio_group(app)].spacing(14).padding(16))
        .width(SIDE_WIDTH)
        .height(Length::Fill);
    let main = column![preview(app), stats(app)].spacing(10).padding(16).width(Length::Fill).height(Length::Fill);
    row![main, side].into()
}

fn preview(app: &App) -> Element<'_, Message> {
    let content: Element<'_, Message> = match &app.preview {
        Some(handle) => {
            image(handle.clone()).content_fit(ContentFit::Contain).width(Length::Fill).height(Length::Fill).into()
        }
        None => {
            let message = match (&app.connection, app.preview_enabled, &app.video) {
                (_, false, _) => "Превью выключено — видео продолжает идти в веб-камеру",
                (Connection::Connected { .. }, _, None) if !app.video_enabled => "Видео выключено",
                (Connection::Connected { .. }, _, None) => "Запуск камеры…",
                (Connection::Connected { .. }, _, Some(_)) => "Ожидание кадров…",
                (Connection::Connecting { .. }, ..) => "Подключение…",
                (Connection::Idle { .. }, ..) => "Здесь появится изображение с телефона",
            };
            text(message).size(16).style(style::muted).into()
        }
    };
    container(content).center(Length::Fill).style(style::well).into()
}

fn stats(app: &App) -> Element<'_, Message> {
    let mut main: Vec<String> = Vec::new();
    let mut warning = None;
    let mut details: Vec<String> = Vec::new();
    if let Some(s) = &app.stats {
        if app.video.is_some() {
            if s.fps == 0.0 && s.packets_per_sec == 0.0 {
                warning = Some("телефон не присылает видео".to_string());
            } else if s.fps == 0.0 {
                warning = Some(format!("видео приходит, но не декодируется (ошибок: {})", s.decode_errors));
            }
            if let Some(l) = s.latency_ms {
                main.push(format!("задержка {l:.0} мс"));
            }
            main.push(format!("{:.0} к/с", s.fps));
            details.push(format!("{:.1} Мбит/с", s.bitrate_mbps));
            if let Some(d) = s.decode_ms {
                details.push(format!("декодирование {d:.1} мс"));
            }
            if let Some(d) = &s.decoder {
                details.push(d.clone());
            }
        }
        if let Some(r) = s.rtt_ms {
            main.push(format!("RTT {r:.1} мс"));
        }
        if let Some(b) = s.audio_buffer_ms {
            let mut audio = format!("звук: буфер {b:.0} мс");
            if let Some(o) = s.audio_output_ms {
                audio.push_str(&format!(", вывод {o:.1} мс"));
            }
            audio.push_str(&format!(", пропусков {}", s.audio_underruns));
            details.push(audio);
        }
    }
    if let Some(v) = &app.video {
        details.insert(0, format!("{} × {} · {}", v.width, v.height, v.encoder));
    }

    let mut line = row![].spacing(14).align_y(Alignment::Center);
    for item in main {
        line = line.push(text(item).size(14));
    }
    if let Some(w) = warning {
        line = line.push(text(w).size(14).style(style::warn));
    }
    if !details.is_empty() {
        line = line.push(disclosure(app, Panel::StatsDetails, "Подробнее"));
    }
    line = line.push(space::horizontal()).push(switch("Превью", app.preview_enabled, Message::PreviewEnabled));

    let mut block = column![line].spacing(4);
    if app.is_open(Panel::StatsDetails) && !details.is_empty() {
        block = block.push(hint(details.join("  ·  ")));
    }
    block.into()
}

fn video_group(app: &App) -> Element<'_, Message> {
    let camera = select(app.camera_choices(), app.camera.clone(), Message::SelectCamera).placeholder("Нет данных");
    let resolution =
        select(app.resolution_choices(), app.resolution.clone(), Message::SelectResolution).placeholder("—");
    let fps = select(app.fps_choices(), Some(app.fps), Message::SelectFps);

    let mut body = column![
        group_header("Видео", Some(toggler(app.video_enabled).on_toggle(Message::VideoEnabled).into())),
        field("Камера", camera),
        row![field("Разрешение", resolution), field("Частота", fps)].spacing(10),
        disclosure(app, Panel::VideoQuality, "Качество"),
    ]
    .spacing(12);
    if app.is_open(Panel::VideoQuality) {
        let bitrates: Vec<_> = BITRATES.iter().copied().map(BitrateChoice).collect();
        body = body.push(field_tip(
            "Битрейт",
            "Выше — чётче изображение, но больше данных по USB.",
            select(bitrates, Some(app.bitrate), Message::SelectBitrate),
        ));
    }
    panel(body)
}

fn image_group(app: &App) -> Element<'_, Message> {
    let mut body = column![disclosure(app, Panel::Image, "Настройки изображения")].spacing(12);
    if !app.is_open(Panel::Image) {
        return panel(body);
    }
    let Some(camera) = app.camera_info() else {
        return panel(body.push(hint("Появятся после подключения телефона.")));
    };
    if camera.zoom[1] > camera.zoom[0] {
        body = body.push(field(
            "Масштаб",
            row![
                slider(camera.zoom[0]..=camera.zoom[1], app.zoom, Message::Zoom).step(0.1_f32),
                text(format!("{:.1}×", app.zoom)).size(SMALL).width(44)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ));
    }
    if camera.ev[1] > camera.ev[0] {
        body = body.push(field(
            "Яркость (экспокоррекция)",
            row![
                slider(camera.ev[0]..=camera.ev[1], app.ev, Message::Ev),
                text(format!("{:+.1}", app.ev as f32 * camera.ev_step)).size(SMALL).width(44)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ));
    }
    body = body.push(field("Баланс белого", select(WbChoice::ALL, Some(app.white_balance), Message::WhiteBalance)));
    if camera.manual_focus {
        body = body.push(switch("Ручной фокус", app.manual_focus, Message::ManualFocus));
        if app.manual_focus && camera.min_focus_distance > 0.0 {
            let distance = if app.focus_distance <= 0.01 {
                "∞".to_string()
            } else {
                format!("{:.2} м", 1.0 / app.focus_distance)
            };
            body = body.push(
                row![
                    slider(0.0..=camera.min_focus_distance, app.focus_distance, Message::FocusDistance).step(0.01_f32),
                    text(distance).size(SMALL).width(44)
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }
    }
    body = body.push(switch("Зафиксировать экспозицию", app.ae_lock, Message::AeLock)).push(switch(
        "Зафиксировать баланс белого",
        app.awb_lock,
        Message::AwbLock,
    ));
    if camera.eis {
        body = body.push(switch("Стабилизация (+ задержка)", app.stabilization, Message::Stabilization));
    }
    if camera.flash {
        body = body.push(switch("Фонарик", app.torch, Message::Torch));
    }
    panel(body)
}

fn audio_group(app: &App) -> Element<'_, Message> {
    let mic = select(app.mic_choices(), Some(app.mic.clone()), Message::SelectMic);
    let output = select(app.output_choices(), app.output.clone(), Message::SelectOutput)
        .placeholder("Выберите виртуальный кабель");
    let refresh = button(text("↻").size(14)).style(style::btn_link).on_press(Message::RefreshOutputs);

    // У кабелей пара «… Input» (сюда пишем) / «… Output» (это микрофон в программах).
    let where_to_pick = match &app.output {
        Some(o) if o.id.is_some() && o.name.contains("Input") => {
            format!("В Zoom, Discord и т.п. выберите микрофон «{}».", o.name.replacen("Input", "Output", 1))
        }
        Some(o) if o.id.is_some() => "В Zoom, Discord и т.п. выберите как микрофон парную сторону этого кабеля.".into(),
        _ => "Выберите воспроизводящую сторону виртуального кабеля (например, CABLE Input).".into(),
    };

    let mut body = column![
        group_header("Микрофон", Some(toggler(app.audio_enabled).on_toggle(Message::AudioEnabled).into())),
        field("Микрофон телефона", mic),
        field_tip("Передавать в", where_to_pick, row![output, refresh].spacing(6).align_y(Alignment::Center)),
    ]
    .spacing(12);
    if let Some(info) = &app.audio {
        let api = info.api.as_ref().map(|a| format!(" · {a}")).unwrap_or_default();
        body =
            body.push(text(format!("● Идёт звук, {} кГц{api}", info.sample_rate / 1000)).size(SMALL).style(style::ok));
    }
    body = body.push(disclosure(app, Panel::AudioProcessing, "Обработка звука"));
    if app.is_open(Panel::AudioProcessing) {
        body = body.push(select(app.source_choices(), app.source.clone(), Message::SelectSource)).push(switch(
            "Стерео",
            app.stereo,
            Message::Stereo,
        ));
    }
    panel(body)
}

// --- Макропад ---

fn stepper<'a>(value: u32, on: fn(u32) -> Message) -> Element<'a, Message> {
    let step = |label: &'a str, next: u32, enabled: bool| {
        button(text(label).size(14).center()).width(28).style(style::btn).on_press_maybe(enabled.then(|| on(next)))
    };
    row![
        step("−", value.saturating_sub(1), value > 1),
        text(value).size(15).width(24).center(),
        step("+", value + 1, value < 6)
    ]
    .spacing(4)
    .align_y(Alignment::Center)
    .into()
}

fn macropad_view(app: &App) -> Element<'_, Message> {
    let pad = &app.pad;
    // Всё, что над сеткой, — в одной панели: строка про экран телефона и строка про звуки.
    let label = |value: &'static str| text(value).size(SMALL).style(style::muted);
    let phone = row![
        switch("На телефоне", pad.enabled, Message::PadEnabled),
        space::horizontal(),
        label("Сетка"),
        stepper(pad.columns, Message::PadColumns),
        text("×").size(BODY).style(style::muted),
        stepper(pad.rows, Message::PadRows),
        select(Orientation::ALL, Some(pad.orientation), Message::PadOrientation).width(180),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let mut screen =
        row![select(DimChoice::ALL, Some(DimChoice(pad.amoled.dim_after_secs)), Message::PadDim).width(180)]
            .spacing(8)
            .align_y(Alignment::Center);
    if pad.amoled.dim_after_secs > 0 {
        // Ползунок меняет значение сразу, а на телефон оно уходит, когда его отпустят.
        screen = screen.push(label("Яркость")).push(
            slider(Amoled::BRIGHTNESS, pad.amoled.dim_brightness, Message::PadDimBrightness)
                .on_release(Message::PadDimCommit),
        );
        screen = screen.push(text(format!("{} %", pad.amoled.dim_brightness)).size(SMALL).width(40));
    } else {
        screen = screen.push(space::horizontal());
    }
    screen = screen.push(info(
        "Экран телефона бережётся для AMOLED: чёрный фон, сетка раз в минуту сдвигается на пару пикселей, \
         без касаний экран приглушается — касание сразу возвращает яркость и нажимает кнопку.",
    ));
    let volume = if pad.sound_volume_db <= SOUND_MUTE_DB {
        "выкл".to_string()
    } else {
        format!("{} дБ", pad.sound_volume_db).replace('-', "−")
    };
    let sounds = row![
        label("Звуки"),
        select(app.output_choices(), Some(app.sound_output()), Message::PadSoundOutput).width(Length::FillPortion(1)),
        slider(SOUND_MUTE_DB..=0, pad.sound_volume_db, Message::PadSoundVolume).width(Length::FillPortion(1)),
        text(volume).size(SMALL).width(48),
        switch("Выравнивать", pad.normalize_sounds, Message::PadNormalize),
        info(
            "Кнопки «Звук» проигрывают файлы на этом ПК в выбранное устройство. Чтобы их слышали собеседники, \
             выберите виртуальный кабель, который в Discord или игре выбран микрофоном. Ползунок — общая \
             громкость в децибелах, крайнее левое положение — без звука. «Выравнивать» делает громкие \
             звуки тише, а тихие громче, чтобы все звучали примерно одинаково.",
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let toolbar = column![phone, screen, rule::horizontal(1), sounds].spacing(10);

    // Путь по папкам.
    let mut crumbs = row![].spacing(8).align_y(Alignment::Center);
    if app.pad_path.is_empty() {
        crumbs = crumbs.push(text("Главная страница").size(14));
    } else {
        crumbs = crumbs.push(button(text("↑ Наверх").size(SMALL)).style(style::btn).on_press(Message::PadUp));
        let mut page = &pad.buttons;
        let mut names = vec!["Главная".to_string()];
        for &i in &app.pad_path {
            let name = page[i].states[0].label.clone();
            names.push(if name.is_empty() { format!("Папка {}", i + 1) } else { name });
            page = &page[i].children;
        }
        crumbs = crumbs.push(text(names.join("  ›  ")).size(14));
    }
    crumbs = crumbs.push(info(
        "Перетаскивайте кнопки мышью: на другую — поменять местами, на папку — положить внутрь, \
         на «← Назад» — перенести на уровень выше. Папка открывается двойным щелчком. \
         Программу или звук можно перетащить из Проводника прямо на ячейку.",
    ));

    let page = app.pad_page();
    let mut grid = column![].spacing(10);
    for r in 0..pad.rows as usize {
        let mut line = row![].spacing(10);
        for c in 0..pad.columns as usize {
            let index = r * pad.columns as usize + c;
            if index < page.len() {
                line = line.push(pad_cell(app, index));
            }
        }
        grid = grid.push(line);
    }
    // Отпустили мышь мимо ячеек или увели курсор с сетки — перетаскивание отменяется.
    let grid = mouse_area(grid).on_release(Message::PadDragCancel).on_exit(Message::PadDragCancel);

    let main = column![panel(toolbar), crumbs, container(grid).center_x(Length::Fill)].spacing(16).padding(24);

    row![
        scrollable(main).width(Length::Fill).height(Length::Fill),
        scrollable(container(button_editor(app)).padding(16)).width(SIDE_WIDTH + 20.0).height(Length::Fill)
    ]
    .into()
}

fn pad_cell(app: &App, index: usize) -> Element<'_, Message> {
    let Some(b) = app.pad_page().get(index) else { return space().into() };
    let back = app.is_back(index);
    let selected = index == app.pad_selected && !back;
    let dragging = app.pad_drag == Some(index);
    let target = app.pad_drag.is_some_and(|source| source != index) && app.pad_hover == Some(index);
    let state = b.current();

    let mut content = column![].spacing(6).align_x(Alignment::Center);
    if back {
        content = content.push(text("←").size(26)).push(text("Назад").size(13));
    } else {
        match app.pad_image(state) {
            Some(Some(handle)) => {
                content = content.push(image(handle.clone()).width(Length::Fill).height(if state.label.is_empty() {
                    Length::Fill
                } else {
                    Length::FillPortion(3)
                }))
            }
            Some(None) => content = content.push(text("нет файла").size(12).style(style::bad)),
            None if b.folder => content = content.push(text("▤").size(26).style(style::accent)),
            None if b.launch => content = content.push(text("▶").size(24).style(style::accent)),
            None if b.text => content = content.push(text("¶").size(24).style(style::accent)),
            None if b.sound => content = content.push(text("♪").size(24).style(style::accent)),
            None => {}
        }
        if !state.label.is_empty() {
            content = content.push(text(state.label.as_str()).size(13).center());
        } else if b.folder && state.image.is_none() {
            content = content.push(text("Папка").size(12).style(style::muted));
        } else if b.launch && state.image.is_none() {
            content = content.push(text("Программа").size(12).style(style::muted));
        } else if b.text && state.image.is_none() {
            content = content.push(text("Текст").size(12).style(style::muted));
        } else if b.sound && state.image.is_none() {
            content = content.push(text("Звук").size(12).style(style::muted));
        } else if state.image.is_none() && !b.keys.is_empty() {
            content = content.push(text(b.keys.to_string()).size(12).center().style(style::muted));
        } else if state.image.is_none() {
            content = content.push(text("+").size(22).style(style::muted));
        }
        if b.toggle {
            content = content.push(text(format!("{}/{TOGGLE_STATES}", b.state + 1)).size(11).style(style::muted));
        }
    }

    let cell =
        container(content).center(Length::Fill).width(PAD_CELL).height(PAD_CELL).padding(8).style(move |_: &Theme| {
            container::Style {
                background: Some(if back { style::BG } else { style::PANEL }.into()),
                border: border::rounded(style::R_LG).width(if selected || target { 2 } else { 1 }).color(if target {
                    style::ACCENT_SOFT
                } else if selected {
                    style::ACCENT
                } else {
                    style::BORDER
                }),
                text_color: Some(if dragging { style::MUTED } else { style::TEXT }),
                ..Default::default()
            }
        });
    let mut area = mouse_area(cell)
        .on_press(Message::PadDragStart(index))
        .on_release(Message::PadDrop(index))
        .on_enter(Message::PadDragEnter(index))
        .on_exit(Message::PadDragExit(index))
        .interaction(if back { iced::mouse::Interaction::Pointer } else { iced::mouse::Interaction::Grab });
    if b.folder && !back {
        area = area.on_double_click(Message::PadOpen(index));
    }
    area.into()
}

/// Сочетание в виде «клавиш»: [Ctrl] + [Alt] + [Мышь 4].
fn keycaps<'a>(combo: &KeyCombo) -> Element<'a, Message> {
    if combo.is_empty() {
        return text("Не задано").size(15).style(style::muted).into();
    }
    let mut names: Vec<&'static str> = Vec::new();
    for (on, name) in [(combo.ctrl, "Ctrl"), (combo.shift, "Shift"), (combo.alt, "Alt"), (combo.win, "Win")] {
        if on {
            names.push(name);
        }
    }
    if let Some(key) = combo.key {
        names.push(key.label());
    }
    let mut caps = Row::new().spacing(6).align_y(Alignment::Center);
    for (i, name) in names.into_iter().enumerate() {
        if i > 0 {
            caps = caps.push(text("+").size(SMALL).style(style::muted));
        }
        caps = caps.push(container(text(name).size(14)).padding([4, 10]).style(style::keycap));
    }
    caps.wrap().vertical_spacing(6).into()
}

/// Готовые цвета для иконок.
const TINTS: [[u8; 3]; 10] = [
    [0xFF, 0xFF, 0xFF],
    [0x9E, 0x9E, 0x9E],
    [0xE5, 0x39, 0x35],
    [0xFB, 0x8C, 0x00],
    [0xFD, 0xD8, 0x35],
    [0x43, 0xA0, 0x47],
    [0x00, 0xAC, 0xC1],
    [0x1E, 0x88, 0xE5],
    [0x8E, 0x24, 0xAA],
    [0xD8, 0x1B, 0x60],
];

fn swatch<'a>(color: [u8; 3], selected: bool) -> Element<'a, Message> {
    let [r, g, b] = color;
    button(space().width(20).height(20))
        .padding(0)
        .on_press(Message::PadTint(Some(color)))
        .style(move |_: &Theme, _| button::Style {
            background: Some(Color::from_rgb8(r, g, b).into()),
            border: border::rounded(10).width(if selected { 3 } else { 1 }).color(if selected {
                style::ACCENT_SOFT
            } else {
                style::BORDER
            }),
            ..Default::default()
        })
        .into()
}

/// Секунды с сотыми: «1,25 с».
fn seconds(ms: u32) -> String {
    format!("{:.2} с", ms as f32 / 1000.0).replace('.', ",")
}

/// Обрезка звука кнопки: волна, начало и конец отрезка, обрезка тишины и прослушивание.
fn sound_trim<'a>(app: &App, b: &ButtonSettings) -> Option<Element<'a, Message>> {
    let title = titled(
        "Обрезка",
        "Играет только выделенная часть — лишнее в начале и в конце пропускается, сам файл не меняется. \
         Начало и конец можно двигать ползунком или вписать в секундах, например 1,25. «Убрать тишину» \
         находит, где звук начинается и заканчивается.",
    );
    let info = match app.selected_sound_info()? {
        None => return Some(column![title, hint("Читаю звук…")].spacing(12).into()),
        Some(Err(e)) => {
            return Some(
                column![title, text(format!("Не удалось прочитать: {e}")).size(SMALL).style(style::bad)]
                    .spacing(12)
                    .into(),
            );
        }
        Some(Ok(info)) => info,
    };
    let duration = info.duration_ms.max(1);
    let (start, end) = (b.sound_start_ms, b.sound_end_ms.unwrap_or(duration));

    // Волна: столбики по пикам; вырезанное — приглушено.
    const HEIGHT: f32 = 48.0;
    let mut wave = Row::new().spacing(1).height(HEIGHT).align_y(Alignment::Center);
    let bars = info.peaks.len().max(1) as u64;
    for (i, &peak) in info.peaks.iter().enumerate() {
        let middle = ((i as u64 * 2 + 1) * duration as u64 / (bars * 2)) as u32;
        let color = if (start..=end).contains(&middle) { style::ACCENT } else { style::BORDER };
        // Корень поднимает тихие места, иначе на волне видны только самые громкие пики.
        let height = (peak.sqrt() * HEIGHT).max(2.0);
        wave = wave.push(
            container(space())
                .width(Length::Fill)
                .height(height)
                .style(move |_: &Theme| container::Style { background: Some(color.into()), ..Default::default() }),
        );
    }
    let wave = container(wave).padding([6, 8]).style(style::inset);

    let step: u32 = if duration > 60_000 { 100 } else { 10 };
    let row_for = |name: &'a str, value: u32, on: fn(u32) -> Message, input: &str, typed: fn(String) -> Message| {
        row![
            text(name).size(SMALL).style(style::muted).width(52),
            slider(0..=duration, value, on).step(step),
            text_input("0,00", input)
                .style(style::input)
                .on_input(typed)
                .on_submit(Message::PadTrimInputDone)
                .size(SMALL)
                .width(64),
            text("с").size(SMALL).style(style::muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
    };
    let trimmed = start > 0 || b.sound_end_ms.is_some();
    Some(
        column![
            title,
            wave,
            row_for("Начало", start, Message::PadSoundStart, &app.trim_start_input, Message::PadTrimStartInput),
            row_for("Конец", end, Message::PadSoundEnd, &app.trim_end_input, Message::PadTrimEndInput),
            hint(format!("Играет {} из {}", seconds(end - start), seconds(duration))),
            row![
                button(text("Убрать тишину").size(SMALL).wrapping(text::Wrapping::None))
                    .style(style::btn)
                    .on_press(Message::PadTrimSilence),
                button(text("Сбросить").size(SMALL))
                    .style(style::btn_link)
                    .on_press_maybe(trimmed.then_some(Message::PadTrimReset)),
                space::horizontal(),
                button(text("▶ Прослушать").size(SMALL).wrapping(text::Wrapping::None))
                    .style(style::btn_primary)
                    .on_press(Message::PadPreviewSound),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        ]
        .spacing(10)
        .into(),
    )
}

fn button_editor(app: &App) -> Element<'_, Message> {
    let index = app.pad_selected;
    let (Some(b), Some(state)) = (app.selected_button(), app.edited_state()) else {
        return panel(hint("Выберите кнопку в сетке, чтобы настроить её."));
    };
    let edited = app.edited_state_index();

    // Тип кнопки и состояния.
    let kind_tip = if b.folder {
        "Нажатие на телефоне открывает страницу с кнопками папки; первая ячейка в ней — «← Назад»."
    } else if b.launch {
        "Касание открывает программу, файл или сайт — как двойной щелчок в Проводнике."
    } else if b.sound {
        "Касание проигрывает звук на ПК в устройство, выбранное в блоке «Звуки»; повторное — останавливает."
    } else if b.text {
        "Касание печатает заготовленный текст в активное окно на ПК."
    } else if b.toggle {
        "Нажатие отправляет сочетание и переключает кнопку на другое состояние — со своей картинкой и подписью."
    } else {
        "Клавиши нажаты, пока палец на кнопке — подходит и для push-to-talk."
    };
    let mut kind = column![
        titled("Кнопка", kind_tip),
        segmented_rows(
            &[
                (ButtonKind::Normal, "Клавиши"),
                (ButtonKind::Toggle, "Вкл/выкл"),
                (ButtonKind::App, "Программа"),
                (ButtonKind::Text, "Текст"),
                (ButtonKind::Sound, "Звук"),
                (ButtonKind::Folder, "Папка"),
            ],
            3,
            if b.folder {
                ButtonKind::Folder
            } else if b.launch {
                ButtonKind::App
            } else if b.text {
                ButtonKind::Text
            } else if b.sound {
                ButtonKind::Sound
            } else if b.toggle {
                ButtonKind::Toggle
            } else {
                ButtonKind::Normal
            },
            Message::PadKind
        ),
    ]
    .spacing(10);
    if b.folder {
        kind = kind.push(
            button(text("Открыть папку").size(SMALL)).style(style::btn_primary).on_press(Message::PadOpen(index)),
        );
    }
    if b.toggle {
        kind = kind.push(segmented(&[(0, "Состояние 1"), (1, "Состояние 2")], edited, Message::PadState));
    }

    // Внешний вид: картинка, подпись, цвет.
    let thumb: Element<'_, Message> = match app.pad_image(state) {
        Some(Some(handle)) => image(handle.clone()).width(64).height(64).into(),
        Some(None) => container(text("нет файла").size(11).style(style::bad)).center(64).into(),
        None => container(text("нет\nкартинки").size(11).center().style(style::muted)).center(64).into(),
    };
    let thumb = container(thumb).style(style::inset);
    let image_buttons = if state.image.is_some() {
        row![
            button(text("Заменить…").size(SMALL)).style(style::btn).on_press(Message::PadPickImage),
            button(text("Убрать").size(SMALL)).style(style::btn_link).on_press(Message::PadClearImage),
        ]
    } else {
        row![button(text("Выбрать картинку…").size(SMALL)).style(style::btn).on_press(Message::PadPickImage)]
    }
    .spacing(6);
    let mut look = column![
        group_header("Вид", None),
        row![
            thumb,
            column![
                text_input("Подпись", &state.label).style(style::input).on_input(Message::PadLabel).size(14),
                image_buttons
            ]
            .spacing(8)
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    ]
    .spacing(12);
    if matches!(app.pad_image(state), Some(None)) {
        look = look
            .push(text("Файл картинки удалён из папки программы — выберите его заново.").size(SMALL).style(style::bad));
    }
    if state.image.is_some() {
        let mut swatches = Row::new().spacing(6);
        for color in TINTS {
            swatches = swatches.push(swatch(color, state.tint == Some(color)));
        }
        look = look.push(field_tip(
            "Цвет иконки",
            "Лучше всего для иконок с прозрачным фоном.",
            column![
                swatches,
                row![
                    button(text("Исходный").size(SMALL))
                        .style(if state.tint.is_none() { style::btn_primary } else { style::btn })
                        .on_press(Message::PadTint(None)),
                    text_input("#RRGGBB", &app.tint_input)
                        .style(style::input)
                        .on_input(Message::PadTintHex)
                        .size(SMALL)
                        .width(100),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(8),
        ));
    }

    // У папки нет своего действия — только вид.
    if b.folder {
        return column![panel(kind), panel(look)].spacing(14).into();
    }
    if b.launch {
        let target = column![
            titled(
                "Что открывать",
                "Подойдёт .exe, ярлык .lnk, любой файл или адрес сайта. Программу можно просто перетащить \
                 из Проводника на ячейку сетки — подпись и иконка подставятся сами.",
            ),
            text_input("C:\\Program Files\\…\\app.exe или https://…", &b.target)
                .style(style::input)
                .on_input(Message::PadTarget)
                .size(14),
            row![
                button(text("Выбрать программу…").size(SMALL)).style(style::btn).on_press(Message::PadPickApp),
                // Иконку файла знает только Проводник; у сайта её нет.
                button(text("Взять её иконку").size(SMALL)).style(style::btn).on_press_maybe(
                    (cfg!(windows) && !b.target.trim().is_empty() && !b.target.contains("://"))
                        .then_some(Message::PadAppIcon)
                ),
            ]
            .spacing(6),
        ]
        .spacing(12);
        return column![panel(kind), panel(look), panel(target)].spacing(14).into();
    }

    if b.text {
        let snippet = column![
            titled(
                "Что печатать",
                "Текст печатается в окно, где сейчас курсор, — с любыми символами и эмодзи, при любой раскладке. \
                 Каждый перевод строки нажимает Enter.",
            ),
            text_editor(&app.snippet_editor)
                .placeholder("Например: Всем привет!")
                .style(style::editor)
                .on_action(Message::PadSnippet)
                .height(96)
                .size(14),
            switch("Нажать Enter в конце", b.enter, Message::PadEnter),
        ]
        .spacing(12);
        return column![panel(kind), panel(look), panel(snippet)].spacing(14).into();
    }

    if b.sound {
        let (status, pick): (Element<'_, Message>, _) = if b.sound_file.is_empty() {
            (hint("Звук не выбран."), "Выбрать звук…")
        } else if app.sound_exists(&b.sound_file) {
            (text("♪ Звук сохранён в папке программы").size(SMALL).style(style::ok).into(), "Заменить…")
        } else {
            (
                text("Файл звука удалён из папки программы — выберите его заново.")
                    .size(SMALL)
                    .style(style::bad)
                    .into(),
                "Выбрать звук…",
            )
        };
        let sound = column![
            titled(
                "Какой звук",
                "mp3, wav, ogg или flac. Звук копируется в папку программы, как картинки. Его можно просто \
                 перетащить из Проводника на ячейку сетки.",
            ),
            status,
            button(text(pick).size(SMALL)).style(style::btn).on_press(Message::PadPickSound),
        ]
        .spacing(12);
        let mut panels = column![panel(kind), panel(look), panel(sound)].spacing(14);
        if app.sound_exists(&b.sound_file)
            && let Some(trim) = sound_trim(app, b)
        {
            panels = panels.push(panel(trim));
        }
        return panels.into();
    }

    // Что нажимается на ПК.
    let record = if app.recording {
        button(text("Отмена").size(SMALL)).style(style::btn).on_press(Message::PadRecord(false))
    } else {
        button(text("Записать").size(SMALL)).style(style::btn_primary).on_press(Message::PadRecord(true))
    };
    let shown: Element<'_, Message> = if app.recording {
        text("Нажмите сочетание: клавиши, средняя кнопка мыши, кнопки 4/5 или колесо. Esc — отмена.")
            .size(14)
            .style(style::warn)
            .into()
    } else {
        keycaps(&b.keys)
    };
    let mut action = column![
        titled(
            "Действие на ПК",
            "Windows не передаёт нажатия в окна, запущенные от администратора, если Escanor запущен без этих прав.",
        ),
        shown,
        row![record, button(text("Очистить").size(SMALL)).style(style::btn_link).on_press(Message::PadClearKeys)]
            .spacing(8),
        disclosure(app, Panel::ManualKeys, "Выбрать вручную"),
    ]
    .spacing(12);
    if app.is_open(Panel::ManualKeys) {
        let modifier = |label: &'static str, on: bool, m: Modifier| {
            switch(label, on, move |v| Message::PadModifier(m, v)).width(Length::Fill)
        };
        action = action
            .push(row![modifier("Ctrl", b.keys.ctrl, Modifier::Ctrl), modifier("Shift", b.keys.shift, Modifier::Shift)])
            .push(row![modifier("Alt", b.keys.alt, Modifier::Alt), modifier("Win", b.keys.win, Modifier::Win)])
            .push(select(Key::ALL, b.keys.key, Message::PadKey).placeholder("Клавиша, F13–F24, медиа или мышь"));
    }

    column![panel(kind), panel(look), panel(action)].spacing(14).into()
}

// --- Настройки ---

fn settings_view(app: &App) -> Element<'_, Message> {
    let mut launch = column![titled(
        "Запуск",
        "В трее Escanor продолжает работать: телефон остаётся подключённым, веб-камера, звук и макропад \
         работают. Двойной щелчок по значку в трее показывает окно, «Выход» в его меню закрывает программу.",
    )]
    .spacing(12);
    if cfg!(windows) {
        launch = launch.push(switch("Запускать вместе с Windows (сразу в трей)", app.autostart, Message::SetAutostart));
    }
    launch = launch.push(switch("Крестик сворачивает в трей", app.close_to_tray, Message::SetCloseToTray));

    let status: Element<'_, Message> = match &app.update {
        UpdateState::Idle => space().into(),
        UpdateState::Checking => hint("Проверяю…"),
        UpdateState::UpToDate => text("Установлена последняя версия.").size(SMALL).style(style::ok).into(),
        UpdateState::Available(r) => {
            text(format!("Доступна версия {}.", r.version)).size(SMALL).style(style::accent).into()
        }
        UpdateState::Installing(r) => hint(format!("Устанавливаю {}…", r.version)),
        UpdateState::Failed(e) => text(e.as_str()).size(SMALL).style(style::bad).into(),
    };
    let mut actions =
        row![
            button(text("Проверить обновления").size(SMALL)).style(style::btn).on_press_maybe(
                (!matches!(app.update, UpdateState::Checking | UpdateState::Installing(_)))
                    .then_some(Message::CheckUpdates)
            )
        ]
        .spacing(8);
    if matches!(app.update, UpdateState::Available(_)) {
        actions = actions
            .push(button(text("Обновить").size(SMALL)).style(style::btn_primary).on_press(Message::InstallUpdate));
    }
    let updates = column![
        titled(
            "Обновления",
            format!(
                "Новые версии берутся из релизов github.com/{}. Пакет проверяется по контрольной сумме, \
                 настройки и картинки макропада при обновлении не трогаются.",
                crate::update::REPO
            ),
        ),
        text(format!("Escanor {}", crate::update::VERSION)).size(15),
        status,
        actions,
        switch("Проверять при запуске", app.check_updates, Message::SetCheckUpdates),
    ]
    .spacing(12);

    let folder = crate::settings::config_dir();
    let data = column![
        titled(
            "Данные",
            "Настройки и картинки макропада хранятся рядом с программой — папку можно просто скопировать \
             на другой компьютер.",
        ),
        text(folder.display().to_string()).size(SMALL),
        button(text("Открыть папку").size(SMALL)).style(style::btn).on_press(Message::OpenSettingsFolder),
    ]
    .spacing(12);

    scrollable(
        container(column![panel(launch), panel(updates), panel(data)].spacing(14).max_width(640))
            .padding(24)
            .center_x(Length::Fill),
    )
    .height(Length::Fill)
    .into()
}
