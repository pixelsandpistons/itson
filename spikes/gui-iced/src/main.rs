//! Phase 0 GUI spike: a 10,000-message history in iced.
//!
//! iced 0.14 has no virtual list, so every message is in the widget tree.
//! `--bench` scrolls to the end, prints a JSON report and exits.

use gui_common::{ChatMessage, FrameStats, Options, PICTURE_H, PICTURE_W};
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::{Column, column, image, row, scrollable, text};
use iced::{Color, Element, Fill, Font, Subscription, Task, font, window};
use std::cell::RefCell;
use std::time::Instant;

const LIST: &str = "history";

#[derive(Debug, Clone)]
enum Msg {
    Frame(Instant),
    Scrolled(f32),
}

struct Spike {
    options: Options,
    messages: Vec<ChatMessage>,
    pictures: Vec<image::Handle>,
    stats: RefCell<FrameStats>,
    at_end: bool,
    done: bool,
}

impl Spike {
    fn new(options: Options) -> Self {
        Spike {
            options,
            messages: gui_common::messages(),
            pictures: gui_common::pictures()
                .into_iter()
                .map(|rgba| image::Handle::from_rgba(PICTURE_W, PICTURE_H, rgba))
                .collect(),
            stats: RefCell::new(FrameStats::new()),
            at_end: false,
            done: false,
        }
    }
}

fn update(state: &mut Spike, msg: Msg) -> Task<Msg> {
    match msg {
        Msg::Scrolled(relative_y) => {
            state.at_end = relative_y >= 0.999;
            Task::none()
        }
        Msg::Frame(now) => {
            state.stats.borrow_mut().frame(now);
            if !state.options.bench || state.done {
                return Task::none();
            }
            let timed_out = state.stats.borrow().elapsed().as_secs() >= state.options.seconds;
            if state.at_end || timed_out {
                state.done = true;
                let report = state.stats.borrow().report(
                    "iced 0.14 (wgpu, tiny-skia fallback)",
                    "naive",
                    state.at_end,
                    "build_ms measures view(): widget tree construction, not layout or drawing",
                );
                gui_common::emit(&report, &state.options);
                return iced::exit();
            }
            operation::scroll_by(
                LIST,
                AbsoluteOffset {
                    x: 0.0,
                    y: state.options.step,
                },
            )
        }
    }
}

fn message_row<'a>(m: &'a ChatMessage, pictures: &'a [image::Handle]) -> Element<'a, Msg> {
    let [r, g, b] = m.color;
    let bold = Font {
        weight: font::Weight::Bold,
        ..Font::DEFAULT
    };
    let mut body: Column<'a, Msg> = column![
        row![
            text(m.author).color(Color::from_rgb8(r, g, b)).font(bold),
            text(m.time.as_str())
                .size(12)
                .color(Color::from_rgb8(150, 150, 170)),
        ]
        .spacing(8),
        text(m.text.as_str()),
    ]
    .spacing(4);
    if let Some(handle) = m.picture.and_then(|k| pictures.get(k)) {
        body = body.push(
            image(handle.clone())
                .width(PICTURE_W as f32)
                .height(PICTURE_H as f32),
        );
    }
    body.into()
}

fn view(state: &Spike) -> Element<'_, Msg> {
    let started = Instant::now();
    let list = Column::with_children(
        state
            .messages
            .iter()
            .map(|m| message_row(m, &state.pictures)),
    )
    .spacing(12)
    .padding(12);
    let history = scrollable(list)
        .id(LIST)
        .on_scroll(|viewport| Msg::Scrolled(viewport.relative_offset().y))
        .width(Fill)
        .height(Fill);
    let page = column![text("#general").size(22), history]
        .spacing(8)
        .padding(8);
    state.stats.borrow_mut().build(started.elapsed());
    page.into()
}

fn subscription(_state: &Spike) -> Subscription<Msg> {
    window::frames().map(Msg::Frame)
}

fn main() -> iced::Result {
    let options = Options::from_args();
    iced::application(move || Spike::new(options.clone()), update, view)
        .subscription(subscription)
        .title("itson iced spike")
        .window_size((900.0, 700.0))
        .run()
}
