use std::collections::VecDeque;

use eframe::egui;

const MAX_LOG_LINES: usize = 160;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("org.mahouya.oxidex.ime-probe")
            .with_inner_size([900.0, 620.0])
            .with_min_inner_size([700.0, 480.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "Oxidex IME Probe",
        native_options,
        Box::new(|_| Ok(Box::<ImeProbeApp>::default())),
    )
}

struct ImeProbeApp {
    empty: String,
    prefilled: String,
    unicode_prefix: String,
    log: VecDeque<String>,
    frame: u64,
}

impl Default for ImeProbeApp {
    fn default() -> Self {
        Self {
            empty: String::new(),
            prefilled: "abc".to_owned(),
            unicode_prefix: "测试".to_owned(),
            log: VecDeque::new(),
            frame: 0,
        }
    }
}

impl eframe::App for ImeProbeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame = self.frame.saturating_add(1);
        self.collect_events(ui.ctx());

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("Oxidex IME Probe");
            ui.label(format!("Oxidex package {} / renderer glow", env!("CARGO_PKG_VERSION")));
            ui.separator();

            ui.label("Plain egui TextEdit fields. No Oxidex IME wrapper, no manual repair.");
            ui.label("Test with Fcitx/IBus: focus a field, compose Chinese/Japanese/Korean text, then commit.");
            ui.label("The important case is the prefilled field: type after `abc` and confirm whether the committed word remains.");
            ui.add_space(8.0);

            self.text_row(ui, "empty", "Starts empty", "empty-field");
            self.text_row(ui, "prefilled", "Starts with ASCII text", "prefilled-field");
            self.text_row(
                ui,
                "unicode_prefix",
                "Starts with CJK text",
                "unicode-prefix-field",
            );

            ui.horizontal(|ui| {
                if ui.button("Reset fields").clicked() {
                    self.empty.clear();
                    self.prefilled = "abc".to_owned();
                    self.unicode_prefix = "测试".to_owned();
                    self.push_log("reset fields".to_owned());
                }
                if ui.button("Clear log").clicked() {
                    self.log.clear();
                }
            });

            ui.separator();
            ui.heading("Raw egui input events");
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.log {
                        ui.monospace(line);
                    }
                });
        });
    }
}

impl ImeProbeApp {
    fn text_row(
        &mut self,
        ui: &mut egui::Ui,
        field: &'static str,
        label: &str,
        id_salt: &'static str,
    ) {
        ui.horizontal(|ui| {
            ui.label(label);
            let id = ui.make_persistent_id(id_salt);
            let (before, after, response) = {
                let text = match field {
                    "empty" => &mut self.empty,
                    "prefilled" => &mut self.prefilled,
                    "unicode_prefix" => &mut self.unicode_prefix,
                    _ => unreachable!(),
                };
                let before = text.clone();
                let response = egui::TextEdit::singleline(text)
                    .id(id)
                    .desired_width(360.0)
                    .show(ui)
                    .response
                    .response;
                (before, text.clone(), response)
            };

            if response.changed() || response.gained_focus() || response.lost_focus() {
                self.push_log(format!(
                    "frame {} field={field} changed={} focus={} gained={} lost={} before={:?} after={:?}",
                    self.frame,
                    response.changed(),
                    response.has_focus(),
                    response.gained_focus(),
                    response.lost_focus(),
                    before,
                    after,
                ));
            }

            if ui.button("Focus").clicked() {
                response.request_focus();
                self.push_log(format!("frame {} field={field} requested focus", self.frame));
            }
        });
    }

    fn collect_events(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            match event {
                egui::Event::Ime(ime) => self.push_log(format!(
                    "frame {} IME {}",
                    self.frame,
                    format_ime_event(&ime)
                )),
                egui::Event::Text(text) => {
                    self.push_log(format!("frame {} Text {:?}", self.frame, text));
                }
                egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    if pressed {
                        self.push_log(format!(
                            "frame {} Key {:?} modifiers={:?}",
                            self.frame, key, modifiers
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    fn push_log(&mut self, line: String) {
        println!("{line}");
        self.log.push_back(line);
        while self.log.len() > MAX_LOG_LINES {
            self.log.pop_front();
        }
    }
}

fn format_ime_event(event: &egui::ImeEvent) -> String {
    match event {
        egui::ImeEvent::Enabled => "Enabled".to_owned(),
        egui::ImeEvent::Preedit(text) => {
            format!(
                "Preedit chars={} bytes={} text={text:?}",
                text.chars().count(),
                text.len()
            )
        }
        egui::ImeEvent::Commit(text) => {
            format!(
                "Commit chars={} bytes={} text={text:?}",
                text.chars().count(),
                text.len()
            )
        }
        egui::ImeEvent::Disabled => "Disabled".to_owned(),
    }
}
