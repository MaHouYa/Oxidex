use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use kerything_core::device::{DeviceInfo, list_known_devices};
use kerything_core::index::{
    SearchFileType, SearchFilters, SearchHit, SearchIndex, SearchRequest, merge_search_request,
    parse_search_query,
};
use kerything_core::model::{DeviceMetadata, FsType, SortDirection, SortKey};
use kerything_core::snapshot;
use time::{OffsetDateTime, UtcOffset, macros::format_description};

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("net.reikooters.kerything")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([820.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Kerything",
        native_options,
        Box::new(|_cc| Ok(Box::new(KerythingApp::new()))),
    )
}

struct KerythingApp {
    devices: Vec<DeviceInfo>,
    indexes: Vec<SearchIndex>,
    hits: Vec<SearchHit>,
    query: String,
    filter_extensions: String,
    filter_path: String,
    filter_type: Option<SearchFileType>,
    selected_scope: String,
    selected_hit: Option<SearchHit>,
    sort_key: SortKey,
    sort_direction: SortDirection,
    status: String,
    show_filters: bool,
    show_index_manager: bool,
    properties_hit: Option<SearchHit>,
    last_scan_errors: HashMap<String, String>,
    scan_job: Option<ScanJob>,
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
}

struct ScanJob {
    device_id: String,
    cancel: Arc<AtomicBool>,
    progress: u8,
}

enum AppEvent {
    ScanProgress {
        device_id: String,
        percent: u8,
    },
    ScanFinished {
        device_id: String,
        result: Box<anyhow::Result<SearchIndex>>,
    },
}

impl KerythingApp {
    fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let devices = list_known_devices().unwrap_or_default();
        let indexes = snapshot::load_all_indexes().unwrap_or_default();
        let mut app = Self {
            devices,
            indexes,
            hits: Vec::new(),
            query: String::new(),
            filter_extensions: String::new(),
            filter_path: String::new(),
            filter_type: None,
            selected_scope: String::new(),
            selected_hit: None,
            sort_key: SortKey::Name,
            sort_direction: SortDirection::Asc,
            status: String::new(),
            show_filters: false,
            show_index_manager: false,
            properties_hit: None,
            last_scan_errors: HashMap::new(),
            scan_job: None,
            tx,
            rx,
        };
        app.status = format!(
            "Loaded {} persisted index{}.",
            app.indexes.len(),
            plural(app.indexes.len())
        );
        app.recompute_hits();
        app
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                AppEvent::ScanProgress { device_id, percent } => {
                    if let Some(job) = &mut self.scan_job
                        && job.device_id == device_id
                    {
                        job.progress = percent;
                        self.status = format!("Indexing {device_id}: {percent}%");
                    }
                }
                AppEvent::ScanFinished { device_id, result } => {
                    self.scan_job = None;
                    match *result {
                        Ok(index) => {
                            let indexed_fs_type = index.metadata.fs_type;
                            self.last_scan_errors.remove(&device_id);
                            self.indexes
                                .retain(|idx| idx.metadata.device_id != device_id);
                            self.indexes.push(index);
                            self.indexes
                                .sort_by(|a, b| a.metadata.device_id.cmp(&b.metadata.device_id));
                            self.refresh_devices();
                            self.recompute_hits();
                            self.status = if indexed_fs_type == FsType::Btrfs {
                                format!(
                                    "Indexed {device_id}. Btrfs V2 indexes the default root only."
                                )
                            } else {
                                format!("Indexed {device_id}.")
                            };
                        }
                        Err(err) => {
                            let message = format!("{err:#}");
                            self.last_scan_errors
                                .insert(device_id.clone(), message.clone());
                            self.status = format!("Indexing failed for {device_id}: {message}");
                        }
                    }
                }
            }
        }
    }

    fn refresh_devices(&mut self) {
        self.devices = list_known_devices().unwrap_or_default();
    }

    fn recompute_hits(&mut self) {
        let previous_selection = self.selected_hit.clone();
        let filter = (!self.selected_scope.is_empty()).then_some(self.selected_scope.as_str());
        let request = match self.search_request() {
            Ok(request) => request,
            Err(err) => {
                self.status = format!("Search error: {err}");
                return;
            }
        };
        self.hits = merge_search_request(
            &self.indexes,
            filter,
            &request,
            self.sort_key,
            self.sort_direction,
        );
        self.selected_hit = previous_selection.filter(|hit| self.hits.iter().any(|h| h == hit));
    }

    fn search_request(&self) -> Result<SearchRequest, kerything_core::index::SearchParseError> {
        let mut request = parse_search_query(&self.query)?;
        request.filters.merge(self.panel_filters());
        Ok(request)
    }

    fn panel_filters(&self) -> SearchFilters {
        let mut filters = SearchFilters::default();
        filters.add_extensions(&self.filter_extensions);
        filters.add_path_contains(&self.filter_path);
        filters.file_type = self.filter_type;
        filters
    }

    fn device_by_id(&self, device_id: &str) -> Option<&DeviceInfo> {
        self.devices
            .iter()
            .find(|dev| dev.metadata.device_id == device_id)
    }

    fn index_by_id(&self, device_id: &str) -> Option<&SearchIndex> {
        self.indexes
            .iter()
            .find(|idx| idx.metadata.device_id == device_id)
    }

    fn hit_at(&self, row: usize) -> Option<(&SearchIndex, u32)> {
        let hit = self.hits.get(row)?;
        self.index_by_id(&hit.device_id)
            .map(|idx| (idx, hit.record_idx))
    }

    fn selected_index_record(&self) -> Option<(&SearchIndex, u32)> {
        let hit = self.selected_hit.as_ref()?;
        self.index_by_id(&hit.device_id)
            .map(|idx| (idx, hit.record_idx))
    }

    fn select_row(&mut self, row: usize) {
        self.selected_hit = self.hits.get(row).cloned();
    }

    fn open_selected(&mut self) {
        let Some((idx, rec_idx)) = self.selected_index_record() else {
            self.status = "No result selected.".into();
            return;
        };
        let Some(device) = self.device_by_id(&idx.metadata.device_id) else {
            self.status = "Device is not currently attached.".into();
            return;
        };
        if !device.mounted || device.primary_mount_point.is_empty() {
            self.status = "This item is indexed, but its device is not mounted.".into();
            return;
        }
        let path = mounted_path(idx, rec_idx, device);
        match open::that(&path) {
            Ok(()) => self.status = format!("Opened {}", path.display()),
            Err(err) => self.status = format!("Failed to open {}: {err}", path.display()),
        }
    }

    fn open_selected_location(&mut self) {
        let Some((idx, rec_idx)) = self.selected_index_record() else {
            self.status = "No result selected.".into();
            return;
        };
        let Some(device) = self.device_by_id(&idx.metadata.device_id) else {
            self.status = "Device is not currently attached.".into();
            return;
        };
        if !device.mounted || device.primary_mount_point.is_empty() {
            self.status = "This item is indexed, but its device is not mounted.".into();
            return;
        }
        let mut path = PathBuf::from(&device.primary_mount_point);
        let internal_dir = idx.internal_dir(rec_idx).trim_start_matches('/');
        if !internal_dir.is_empty() {
            path.push(internal_dir);
        }
        match open::that(&path) {
            Ok(()) => self.status = format!("Opened {}", path.display()),
            Err(err) => self.status = format!("Failed to open {}: {err}", path.display()),
        }
    }

    fn copy_selected_names(&mut self, ctx: &egui::Context) {
        let Some((idx, rec_idx)) = self.selected_index_record() else {
            self.status = "No result selected.".into();
            return;
        };
        let name = idx.name(rec_idx).to_owned();
        ctx.copy_text(name);
        self.status = "Copied file name.".into();
    }

    fn copy_selected_paths(&mut self, ctx: &egui::Context) {
        let Some((idx, rec_idx)) = self.selected_index_record() else {
            self.status = "No result selected.".into();
            return;
        };
        let (mounted, mp) = self
            .device_by_id(&idx.metadata.device_id)
            .map(|dev| (dev.mounted, dev.primary_mount_point.as_str()))
            .unwrap_or((false, ""));
        ctx.copy_text(idx.display_path(rec_idx, mounted, mp));
        self.status = "Copied full path.".into();
    }

    fn start_scan(&mut self, device: DeviceInfo) {
        if self.scan_job.is_some() {
            self.status = "Another indexing job is already running.".into();
            return;
        }

        let cancel = Arc::new(AtomicBool::new(false));
        self.scan_job = Some(ScanJob {
            device_id: device.metadata.device_id.clone(),
            cancel: cancel.clone(),
            progress: 0,
        });
        self.status = format!("Indexing {}...", device.metadata.device_id);
        let tx = self.tx.clone();
        thread::spawn(move || run_scan_job(device, cancel, tx));
    }

    fn cancel_scan(&mut self) {
        if let Some(job) = &self.scan_job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = format!("Cancelling {}...", job.device_id);
        }
    }

    fn forget_index(&mut self, device_id: &str) {
        self.indexes
            .retain(|idx| idx.metadata.device_id != device_id);
        if let Err(err) = snapshot::delete_index(device_id) {
            self.status =
                format!("Removed in-memory index, but failed to delete snapshot: {err:#}");
        } else {
            self.status = format!("Forgot {device_id}.");
        }
        if self.selected_scope == device_id {
            self.selected_scope.clear();
        }
        self.recompute_hits();
    }
}

impl eframe::App for KerythingApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events();
        let ctx = ui.ctx().clone();
        self.handle_keyboard(&ctx);
        if self.scan_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::Panel::top("top").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Scope");
                let scopes: Vec<(String, String)> = self
                    .indexes
                    .iter()
                    .map(|idx| (idx.metadata.device_id.clone(), device_label(&idx.metadata)))
                    .collect();
                let mut scope_changed = false;
                egui::ComboBox::from_id_salt("scope")
                    .selected_text(scope_label(&self.selected_scope, &self.indexes))
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut self.selected_scope,
                                String::new(),
                                "All indexed devices",
                            )
                            .changed()
                        {
                            scope_changed = true;
                        }
                        for (device_id, label) in scopes {
                            if ui
                                .selectable_value(&mut self.selected_scope, device_id, label)
                                .changed()
                            {
                                scope_changed = true;
                            }
                        }
                    });
                if scope_changed {
                    self.recompute_hits();
                }

                if ui.button("Index Manager").clicked() {
                    self.show_index_manager = true;
                    self.refresh_devices();
                }

                if ui.selectable_label(self.show_filters, "Filters").clicked() {
                    self.show_filters = !self.show_filters;
                }

                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search files, e.g. *.rs ext:txt path:src type:dir")
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    self.recompute_hits();
                }
            });
            if self.show_filters {
                ui.separator();
                self.filter_panel(ui);
            }
            if let Some(summary) = self.filter_summary() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(summary);
                });
            }
        });

        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} object{} found",
                    self.hits.len(),
                    plural(self.hits.len())
                ));
                ui.separator();
                ui.label(&self.status);
                if let Some(job) = &self.scan_job {
                    ui.separator();
                    ui.add(
                        egui::ProgressBar::new(job.progress as f32 / 100.0).desired_width(120.0),
                    );
                    if ui.button("Cancel").clicked() {
                        self.cancel_scan();
                    }
                }
            });
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.results_toolbar(ui, &ctx);
            ui.separator();
            self.results_table(ui);
        });

        if self.show_index_manager {
            self.index_manager(&ctx);
        }
        self.properties_window(&ctx);
    }
}

impl KerythingApp {
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.properties_hit = None;
            }
            return;
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.open_selected();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.properties_hit.is_some() {
                self.properties_hit = None;
            } else if self.show_filters {
                self.show_filters = false;
            } else {
                self.selected_hit = None;
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::C)) {
            if ctx.input(|i| i.modifiers.shift) {
                self.copy_selected_names(ctx);
            } else {
                self.copy_selected_paths(ctx);
            }
        }
    }

    fn filter_panel(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Extension");
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.filter_extensions)
                        .hint_text("rs,txt")
                        .desired_width(120.0),
                )
                .changed();

            ui.label("Type");
            egui::ComboBox::from_id_salt("type-filter")
                .selected_text(file_type_label(self.filter_type))
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut self.filter_type, None, "All")
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut self.filter_type,
                            Some(SearchFileType::File),
                            "Files",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut self.filter_type,
                            Some(SearchFileType::Dir),
                            "Folders",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut self.filter_type,
                            Some(SearchFileType::Symlink),
                            "Symlinks",
                        )
                        .changed();
                });

            ui.label("Path");
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.filter_path)
                        .hint_text("src")
                        .desired_width(180.0),
                )
                .changed();

            if ui.button("Clear Filters").clicked() {
                self.filter_extensions.clear();
                self.filter_path.clear();
                self.filter_type = None;
                changed = true;
            }
        });

        if changed {
            self.recompute_hits();
        }
    }

    fn filter_summary(&self) -> Option<String> {
        let request = self.search_request().ok()?;
        if request.filters.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !request.filters.extensions.is_empty() {
            parts.push(format!("ext:{}", request.filters.extensions.join(",")));
        }
        if let Some(file_type) = request.filters.file_type {
            parts.push(format!("type:{}", search_file_type_name(file_type)));
        }
        for path in request.filters.path_contains {
            parts.push(format!("path:{path}"));
        }
        Some(format!("Active filters: {}", parts.join("  ")))
    }

    fn results_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let has_selection = self.selected_index_record().is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_selection, egui::Button::new("Open"))
                .clicked()
            {
                self.open_selected();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Open Folder"))
                .clicked()
            {
                self.open_selected_location();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Copy Name"))
                .clicked()
            {
                self.copy_selected_names(ctx);
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Copy Path"))
                .clicked()
            {
                self.copy_selected_paths(ctx);
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Properties"))
                .clicked()
            {
                self.properties_hit = self.selected_hit.clone();
            }
            ui.separator();
            let mut sort_changed = false;
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Name,
                "Name",
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Path,
                "Path",
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Size,
                "Size",
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Mtime,
                "Date",
            );
            if sort_changed {
                self.recompute_hits();
            }
        });
    }

    fn results_table(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.indexes.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("No indexes yet");
                ui.label("Open Index Manager to index an NTFS, EXT4, or Btrfs device.");
            });
            return;
        }
        if self.hits.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("No results");
                ui.label("Try a different name, wildcard, extension, path, or type filter.");
            });
            return;
        }

        let row_height = 24.0;
        TableBuilder::new(ui)
            .id_salt("results")
            .striped(true)
            .sense(egui::Sense::click())
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(260.0).at_least(120.0).clip(true))
            .column(Column::remainder().at_least(220.0).clip(true))
            .column(Column::initial(110.0).at_least(80.0).clip(true))
            .column(Column::initial(150.0).at_least(120.0).clip(true))
            .header(row_height, |mut row| {
                row.col(|ui| {
                    ui.strong("Name");
                });
                row.col(|ui| {
                    ui.strong("Path");
                });
                row.col(|ui| {
                    ui.strong("Size");
                });
                row.col(|ui| {
                    ui.strong("Modified");
                });
            })
            .body(|body| {
                body.rows(row_height, self.hits.len(), |mut row| {
                    let row_index = row.index();
                    let Some(hit) = self.hits.get(row_index).cloned() else {
                        return;
                    };
                    let Some((idx, rec_idx)) = self.hit_at(row_index) else {
                        return;
                    };
                    let rec = &idx.records[rec_idx as usize];
                    let (mounted, mp) = self
                        .device_by_id(&idx.metadata.device_id)
                        .map(|dev| (dev.mounted, dev.primary_mount_point.as_str()))
                        .unwrap_or((false, ""));
                    let name = idx.name(rec_idx).to_owned();
                    let path = idx.display_path(rec_idx, mounted, mp);
                    let size = format_size(rec.size);
                    let modified = format_time(rec.mtime);
                    let selected = self.selected_hit.as_ref() == Some(&hit);
                    row.set_selected(selected);

                    row.col(|ui| {
                        ui.label(name);
                    });
                    row.col(|ui| {
                        ui.label(path);
                    });
                    row.col(|ui| {
                        ui.label(size);
                    });
                    row.col(|ui| {
                        ui.label(modified);
                    });

                    let response = row.response();
                    if response.secondary_clicked() {
                        self.selected_hit = Some(hit.clone());
                    }
                    response.clone().context_menu(|ui| {
                        self.selected_hit = Some(hit.clone());
                        self.result_context_menu(ui, &ctx);
                    });
                    if response.double_clicked() {
                        self.select_row(row_index);
                        self.open_selected();
                    } else if response.clicked() {
                        self.select_row(row_index);
                    }
                });
            });
    }

    fn result_context_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let has_selection = self.selected_index_record().is_some();
        let selected_device = self
            .selected_hit
            .as_ref()
            .and_then(|hit| self.device_by_id(&hit.device_id))
            .cloned();
        let can_rescan = selected_device
            .as_ref()
            .map(|device| device.metadata.fs_type.is_supported_for_scan())
            .unwrap_or(false);
        if ui
            .add_enabled(has_selection, egui::Button::new("Open"))
            .clicked()
        {
            self.open_selected();
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Open Folder"))
            .clicked()
        {
            self.open_selected_location();
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Copy Name"))
            .clicked()
        {
            self.copy_selected_names(ctx);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Copy Path"))
            .clicked()
        {
            self.copy_selected_paths(ctx);
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(
                has_selection && can_rescan && self.scan_job.is_none(),
                egui::Button::new("Rescan Device"),
            )
            .clicked()
        {
            if let Some(device) = selected_device {
                self.start_scan(device);
            }
            ui.close();
        }
        if ui
            .add_enabled(
                has_selection && self.scan_job.is_none(),
                egui::Button::new("Forget Index"),
            )
            .clicked()
        {
            if let Some(device_id) = self.selected_hit.as_ref().map(|hit| hit.device_id.clone()) {
                self.forget_index(&device_id);
            }
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Properties"))
            .clicked()
        {
            self.properties_hit = self.selected_hit.clone();
            ui.close();
        }
    }

    fn index_manager(&mut self, ctx: &egui::Context) {
        let mut open = self.show_index_manager;
        egui::Window::new("Indexes")
            .open(&mut open)
            .default_width(980.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Refresh Devices").clicked() {
                        self.refresh_devices();
                    }
                    if let Some(job) = &self.scan_job {
                        ui.label(format!("Indexing {}: {}%", job.device_id, job.progress));
                    }
                });
                ui.separator();

                let indexed: HashMap<String, usize> = self
                    .indexes
                    .iter()
                    .map(|idx| (idx.metadata.device_id.clone(), idx.records.len()))
                    .collect();

                egui::ScrollArea::vertical()
                    .max_height(460.0)
                    .show(ui, |ui| {
                        for device in self.devices.clone() {
                            ui.horizontal(|ui| {
                                let meta = &device.metadata;
                                let index = self.index_by_id(&meta.device_id);
                                let count = indexed.get(&meta.device_id).copied();
                                ui.label(fit(&device_label(meta), 28));
                                ui.label(fit(meta.fs_type.as_str(), 7));
                                ui.label(if device.mounted {
                                    "mounted"
                                } else {
                                    "not mounted"
                                });
                                ui.label(fit(&meta.dev_node, 22));
                                ui.label(match count {
                                    Some(n) => format!("{n} entries"),
                                    None => "not indexed".to_owned(),
                                });
                                ui.label(
                                    index
                                        .map(|idx| {
                                            format!(
                                                "indexed {}",
                                                format_time(idx.last_indexed_time)
                                            )
                                        })
                                        .unwrap_or_else(|| "never indexed".to_owned()),
                                );
                                if let Some(err) = self.last_scan_errors.get(&meta.device_id) {
                                    ui.label(fit(err, 36));
                                }

                                let busy = self.scan_job.is_some();
                                let scan_label = if count.is_some() { "Rescan" } else { "Index" };
                                if ui
                                    .add_enabled(
                                        !busy && meta.fs_type.is_supported_for_scan(),
                                        egui::Button::new(scan_label),
                                    )
                                    .clicked()
                                {
                                    self.start_scan(device.clone());
                                }
                                if count.is_some()
                                    && ui.add_enabled(!busy, egui::Button::new("Forget")).clicked()
                                {
                                    self.forget_index(&meta.device_id);
                                }
                            });
                            ui.separator();
                        }
                    });
            });
        self.show_index_manager = open;
    }

    fn properties_window(&mut self, ctx: &egui::Context) {
        let Some(hit) = self.properties_hit.clone() else {
            return;
        };
        let Some(idx) = self.index_by_id(&hit.device_id) else {
            self.properties_hit = None;
            return;
        };
        let rec_idx = hit.record_idx;
        let Some(rec) = idx.records.get(rec_idx as usize) else {
            self.properties_hit = None;
            return;
        };
        let device = self.device_by_id(&idx.metadata.device_id);
        let (mounted, mp) = device
            .map(|dev| (dev.mounted, dev.primary_mount_point.as_str()))
            .unwrap_or((false, ""));
        let mut open = true;

        egui::Window::new("Properties")
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                property_row(ui, "Name", idx.name(rec_idx));
                property_row(ui, "Path", &idx.display_path(rec_idx, mounted, mp));
                property_row(ui, "Internal Path", idx.internal_path(rec_idx));
                property_row(ui, "Device", &device_label(&idx.metadata));
                property_row(ui, "Device ID", &idx.metadata.device_id);
                property_row(ui, "Filesystem", idx.metadata.fs_type.as_str());
                property_row(ui, "Size", &format_size(rec.size));
                property_row(ui, "Modified", &format_time(rec.mtime));
                property_row(ui, "Directory", yes_no(rec.is_dir()));
                property_row(ui, "Symlink", yes_no(rec.is_symlink()));
                property_row(ui, "Mounted", yes_no(mounted));
                property_row(ui, "Last Indexed", &format_time(idx.last_indexed_time));
            });

        if !open {
            self.properties_hit = None;
        }
    }
}

fn run_scan_job(device: DeviceInfo, cancel: Arc<AtomicBool>, tx: Sender<AppEvent>) {
    let result = scan_device_with_helper(&device, cancel.clone(), tx.clone()).and_then(|scan| {
        let index = SearchIndex::from_scan(device.metadata.clone(), scan, now_unix())?;
        snapshot::save_index(&index)?;
        Ok(index)
    });
    let _ = tx.send(AppEvent::ScanFinished {
        device_id: device.metadata.device_id,
        result: Box::new(result),
    });
}

fn scan_device_with_helper(
    device: &DeviceInfo,
    cancel: Arc<AtomicBool>,
    tx: Sender<AppEvent>,
) -> anyhow::Result<kerything_core::model::ScanDatabase> {
    let helper = helper_path();
    let mut child = Command::new("pkexec")
        .arg(helper)
        .arg(&device.metadata.dev_node)
        .arg(device.metadata.fs_type.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("helper stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("helper stderr unavailable"))?;

    let stdout_handle = thread::spawn(move || {
        let mut data = Vec::new();
        stdout.read_to_end(&mut data).map(|_| data)
    });

    let device_id = device.metadata.device_id.clone();
    let stderr_handle = thread::spawn(move || -> std::io::Result<String> {
        let reader = std::io::BufReader::new(stderr);
        let mut diagnostics = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if let Some(rest) = line.trim().strip_prefix("KERYTHING_PROGRESS ")
                && let Ok(percent) = rest.trim().parse::<u8>()
            {
                let _ = tx.send(AppEvent::ScanProgress {
                    device_id: device_id.clone(),
                    percent: percent.min(100),
                });
            } else if !line.trim().is_empty() {
                diagnostics.push(line);
            }
        }
        Ok(diagnostics.join("\n"))
    });

    let mut cancellation_requested = false;
    loop {
        if cancel.load(Ordering::Relaxed) && !cancellation_requested {
            cancellation_requested = true;
            let _ = child.kill();
        }
        if let Some(status) = child.try_wait()? {
            let output = stdout_handle
                .join()
                .map_err(|_| anyhow::anyhow!("helper stdout reader panicked"))??;
            let diagnostics = stderr_handle
                .join()
                .map_err(|_| anyhow::anyhow!("helper stderr reader panicked"))??;
            if cancellation_requested {
                anyhow::bail!("scan cancelled");
            }
            if !status.success() {
                if diagnostics.trim().is_empty() {
                    anyhow::bail!("scanner helper failed with status {status}");
                }
                anyhow::bail!("scanner helper failed with status {status}: {diagnostics}");
            }
            return kerything_core::stream::read_scan_stream(&output[..]);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn helper_path() -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from("kerything-scanner-helper");
    };
    if let Some(dir) = exe.parent() {
        let sibling = dir.join("kerything-scanner-helper");
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from("kerything-scanner-helper")
}

fn mounted_path(index: &SearchIndex, rec_idx: u32, device: &DeviceInfo) -> PathBuf {
    let mut path = PathBuf::from(&device.primary_mount_point);
    let internal = index.internal_path(rec_idx).trim_start_matches('/');
    if !internal.is_empty() {
        path.push(internal);
    }
    path
}

fn scope_label(scope: &str, indexes: &[SearchIndex]) -> String {
    if scope.is_empty() {
        return "All indexed devices".into();
    }
    indexes
        .iter()
        .find(|idx| idx.metadata.device_id == scope)
        .map(|idx| device_label(&idx.metadata))
        .unwrap_or_else(|| scope.to_owned())
}

fn device_label(meta: &DeviceMetadata) -> String {
    if !meta.label.trim().is_empty() {
        format!("{} ({})", meta.label.trim(), meta.device_id)
    } else {
        meta.device_id.clone()
    }
}

fn file_type_label(file_type: Option<SearchFileType>) -> &'static str {
    match file_type {
        None => "All",
        Some(SearchFileType::File) => "Files",
        Some(SearchFileType::Dir) => "Folders",
        Some(SearchFileType::Symlink) => "Symlinks",
    }
}

fn search_file_type_name(file_type: SearchFileType) -> &'static str {
    match file_type {
        SearchFileType::File => "file",
        SearchFileType::Dir => "dir",
        SearchFileType::Symlink => "symlink",
    }
}

fn property_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("{label}:"));
        ui.monospace(value);
    });
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn sort_button(
    ui: &mut egui::Ui,
    key: &mut SortKey,
    direction: &mut SortDirection,
    button_key: SortKey,
    label: &str,
) -> bool {
    let active = *key == button_key;
    let text = if active {
        match direction {
            SortDirection::Asc => format!("{label} ↑"),
            SortDirection::Desc => format!("{label} ↓"),
        }
    } else {
        label.to_owned()
    };
    if ui.button(text).clicked() {
        if *key == button_key {
            *direction = match direction {
                SortDirection::Asc => SortDirection::Desc,
                SortDirection::Desc => SortDirection::Asc,
            };
        } else {
            *key = button_key;
            *direction = SortDirection::Asc;
        }
        true
    } else {
        false
    }
}

fn fit(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count <= width {
        format!("{s:<width$}")
    } else if width > 1 {
        let mut out: String = s.chars().take(width - 1).collect();
        out.push('…');
        out
    } else {
        "…".into()
    }
}

fn format_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_time(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }

    let Ok(time) = OffsetDateTime::from_unix_timestamp(ts) else {
        return ts.to_string();
    };
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    time.to_offset(offset)
        .format(format_description!("[year]-[month]-[day] [hour]:[minute]"))
        .unwrap_or_else(|_| ts.to_string())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
