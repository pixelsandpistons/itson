//! Phase 0 GUI spike: a 10,000-message history in egui.
//!
//! Default mode lays out only the visible messages, using heights measured
//! from the wrapped text. `--naive` lays out every message every frame.
//! `--bench` scrolls to the end, prints a JSON report and exits.

use eframe::egui;
use gui_common::{ChatMessage, FrameStats, Options, PICTURE_H, PICTURE_W};
use std::time::Instant;

const PAD: f32 = 6.0;

struct Spike {
    options: Options,
    messages: Vec<ChatMessage>,
    textures: Vec<egui::TextureHandle>,
    stats: FrameStats,
    /// Cached row heights and their prefix sums for one wrap width.
    heights: Vec<f32>,
    tops: Vec<f32>,
    measured_width: f32,
    offset: f32,
    done: bool,
}

impl Spike {
    fn new(cc: &eframe::CreationContext<'_>, options: Options) -> Self {
        let textures = gui_common::pictures()
            .iter()
            .enumerate()
            .map(|(i, rgba)| {
                cc.egui_ctx.load_texture(
                    format!("picture-{i}"),
                    egui::ColorImage::from_rgba_unmultiplied(
                        [PICTURE_W as usize, PICTURE_H as usize],
                        rgba,
                    ),
                    egui::TextureOptions::default(),
                )
            })
            .collect();
        Spike {
            options,
            messages: gui_common::messages(),
            textures,
            stats: FrameStats::new(),
            heights: Vec::new(),
            tops: Vec::new(),
            measured_width: -1.0,
            offset: 0.0,
            done: false,
        }
    }

    fn measure(&mut self, ui: &egui::Ui, width: f32) {
        if (width - self.measured_width).abs() < 0.5 {
            return;
        }
        let font = egui::TextStyle::Body.resolve(ui.style());
        let header = ui.text_style_height(&egui::TextStyle::Body);
        let gap = ui.spacing().item_spacing.y;
        self.heights = self
            .messages
            .iter()
            .map(|m| {
                let text = ui
                    .painter()
                    .layout(m.text.clone(), font.clone(), egui::Color32::WHITE, width)
                    .size()
                    .y;
                let picture = if m.picture.is_some() {
                    PICTURE_H as f32 + gap
                } else {
                    0.0
                };
                header + gap + text + picture + 2.0 * PAD
            })
            .collect();
        self.tops = std::iter::once(0.0)
            .chain(self.heights.iter().scan(0.0, |sum, h| {
                *sum += h;
                Some(*sum)
            }))
            .collect();
        self.measured_width = width;
    }
}

fn draw_row(ui: &mut egui::Ui, m: &ChatMessage, textures: &[egui::TextureHandle]) {
    ui.horizontal(|ui| {
        let [r, g, b] = m.color;
        ui.label(
            egui::RichText::new(m.author)
                .strong()
                .color(egui::Color32::from_rgb(r, g, b)),
        );
        ui.label(egui::RichText::new(&m.time).weak());
    });
    ui.label(&m.text);
    if let Some(texture) = m.picture.and_then(|k| textures.get(k)) {
        ui.add(
            egui::Image::new(texture)
                .fit_to_exact_size(egui::vec2(PICTURE_W as f32, PICTURE_H as f32)),
        );
    }
}

impl eframe::App for Spike {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let started = Instant::now();
        self.stats.frame(started);
        let ctx = ui.ctx().clone();
        let mut at_end = false;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("#general");
            let mut area = egui::ScrollArea::vertical().auto_shrink(false);
            if self.options.bench {
                area = area.vertical_scroll_offset(self.offset);
            }
            let output = if self.options.naive {
                let textures = &self.textures;
                let messages = &self.messages;
                area.show(ui, |ui| {
                    for m in messages {
                        draw_row(ui, m, textures);
                    }
                })
            } else {
                let width = ui.available_width() - ui.spacing().scroll.bar_width - 2.0 * PAD;
                self.measure(ui, width);
                let total = self.tops.last().copied().unwrap_or(0.0);
                let (messages, textures, tops) = (&self.messages, &self.textures, &self.tops);
                area.show_viewport(ui, |ui, viewport| {
                    ui.set_height(total);
                    let origin = ui.max_rect().min;
                    let first = tops
                        .partition_point(|top| *top <= viewport.min.y)
                        .saturating_sub(1);
                    for (i, m) in messages.iter().enumerate().skip(first) {
                        let (Some(top), Some(bottom)) = (tops.get(i), tops.get(i + 1)) else {
                            break;
                        };
                        if *top > viewport.max.y {
                            break;
                        }
                        let rect = egui::Rect::from_min_max(
                            origin + egui::vec2(PAD, top + PAD),
                            origin + egui::vec2(PAD + width, bottom - PAD),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                            draw_row(ui, m, textures)
                        });
                    }
                })
            };
            at_end = output.state.clamped_offset().y + output.inner_rect.height()
                >= output.content_size.y - 1.0;
        });
        self.stats.build(started.elapsed());

        if self.options.bench && !self.done {
            let timed_out = self.stats.elapsed().as_secs() >= self.options.seconds;
            if at_end || timed_out {
                self.done = true;
                let mode = if self.options.naive {
                    "naive"
                } else {
                    "virtual"
                };
                let report = self.stats.report(
                    "egui 0.37 (eframe, wgpu)",
                    mode,
                    at_end,
                    "build_ms measures the ui() call: layout and widget work, not GPU painting",
                );
                gui_common::emit(&report, &self.options);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.offset += self.options.step;
                ctx.request_repaint();
            }
        }
    }
}

fn main() -> eframe::Result {
    let options = Options::from_args();
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("itson egui spike")
            .with_inner_size([900.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "itson-gui-egui",
        native,
        Box::new(move |cc| Ok(Box::new(Spike::new(cc, options)))),
    )
}
