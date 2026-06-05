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
use oxidex_client::OxidexClient;
use oxidex_core::config::{ThemeMode, load_config};
use oxidex_core::daemon_model::{
    DeviceSummary, IndexSummary, ScanJobSummary, SearchQueryParams, SearchResultRow, WatchSummary,
};
use oxidex_core::device::{DeviceInfo, list_known_devices};
use oxidex_core::index::{
    SearchFileType, SearchFilters, SearchHit, SearchIndex, SearchRequest, merge_search_request,
    parse_search_query,
};
use oxidex_core::model::{DeviceMetadata, FsType, SortDirection, SortKey};
use oxidex_core::snapshot;
use time::{OffsetDateTime, UtcOffset, macros::format_description};

fn main() -> eframe::Result {
    let standalone = std::env::args().any(|arg| arg == "--standalone");
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("org.mahouya.oxidex")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([820.0, 520.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "Oxidex",
        native_options,
        Box::new(move |cc| {
            if standalone {
                return Ok(Box::new(OxidexApp::new(cc)) as Box<dyn eframe::App>);
            }
            match DaemonGuiApp::new(cc) {
                Ok(app) => Ok(Box::new(app) as Box<dyn eframe::App>),
                Err(err) => Ok(Box::new(ServiceErrorApp::new(err)) as Box<dyn eframe::App>),
            }
        }),
    )
}

struct OxidexApp {
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

struct DaemonGuiApp {
    client: Option<OxidexClient>,
    query: String,
    filter_extensions: String,
    filter_path: String,
    filter_type: Option<SearchFileType>,
    selected_scope: String,
    rows: Vec<SearchResultRow>,
    devices: Vec<DeviceSummary>,
    indexes: Vec<IndexSummary>,
    jobs: Vec<ScanJobSummary>,
    watches: Vec<WatchSummary>,
    selected_hit: Option<SearchHit>,
    sort_key: SortKey,
    sort_direction: SortDirection,
    status: String,
    show_filters: bool,
    show_index_manager: bool,
    show_settings: bool,
    properties_row: Option<SearchResultRow>,
    theme: ThemeMode,
}

impl DaemonGuiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        let mut client = connect_or_start_daemon()?;
        let config = client.config_get().ok();
        let theme = config
            .as_ref()
            .map(|config| config.config.ui.theme)
            .unwrap_or(ThemeMode::System);
        apply_theme(&cc.egui_ctx, theme);

        let sort_key = config
            .as_ref()
            .map(|config| config.config.search.default_sort)
            .unwrap_or(SortKey::Name);
        let sort_direction = config
            .as_ref()
            .map(|config| config.config.search.default_direction)
            .unwrap_or(SortDirection::Asc);
        let devices = client.devices().unwrap_or_default();
        let indexes = client.indexes().unwrap_or_default();
        let jobs = client.jobs().unwrap_or_default();
        let watches = client.watch_status().unwrap_or_default();
        let mut app = Self {
            client: Some(client),
            query: String::new(),
            filter_extensions: String::new(),
            filter_path: String::new(),
            filter_type: None,
            selected_scope: String::new(),
            rows: Vec::new(),
            devices,
            indexes,
            jobs,
            watches,
            selected_hit: None,
            sort_key,
            sort_direction,
            status: "Connected to oxidexd.".into(),
            show_filters: config
                .as_ref()
                .map(|config| config.config.ui.show_filter_panel)
                .unwrap_or(true),
            show_index_manager: false,
            show_settings: false,
            properties_row: None,
            theme,
        };
        app.recompute_rows();
        Ok(app)
    }

    fn recompute_rows(&mut self) {
        let request = match self.search_request() {
            Ok(request) => request,
            Err(err) => {
                self.status = format!("Search error: {err}");
                return;
            }
        };
        let Some(client) = self.client.as_mut() else {
            return;
        };
        let previous = self.selected_hit.clone();
        let result = client.search(&SearchQueryParams {
            query: self.query.clone(),
            request: Some(request),
            device_filter: (!self.selected_scope.is_empty()).then_some(self.selected_scope.clone()),
            sort_key: self.sort_key,
            sort_direction: self.sort_direction,
            max_results: None,
        });
        match result {
            Ok(result) => {
                let count = result.rows.len();
                let truncated = result.truncated;
                self.rows = result.rows;
                self.selected_hit =
                    previous.filter(|hit| self.rows.iter().any(|row| row.hit == *hit));
                self.status = if truncated {
                    format!("Showing first {count} results.")
                } else {
                    format!("{count} result{}.", plural(count))
                };
            }
            Err(err) => self.status = format!("Search failed: {err:#}"),
        }
    }

    fn refresh_lists(&mut self) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        match (
            client.devices(),
            client.indexes(),
            client.jobs(),
            client.watch_status(),
        ) {
            (Ok(devices), Ok(indexes), Ok(jobs), Ok(watches)) => {
                self.devices = devices;
                self.indexes = indexes;
                self.jobs = jobs;
                self.watches = watches;
                self.status = "Refreshed devices and indexes.".into();
            }
            (Err(err), _, _, _)
            | (_, Err(err), _, _)
            | (_, _, Err(err), _)
            | (_, _, _, Err(err)) => self.status = format!("Refresh failed: {err:#}"),
        }
    }

    fn search_request(&self) -> Result<SearchRequest, oxidex_core::index::SearchParseError> {
        let mut request = parse_search_query(&self.query)?;
        request.add_filters(self.panel_filters());
        Ok(request)
    }

    fn panel_filters(&self) -> SearchFilters {
        let mut filters = SearchFilters::default();
        filters.add_extensions(&self.filter_extensions);
        filters.add_path_contains(&self.filter_path);
        filters.file_type = self.filter_type;
        filters
    }

    fn filter_summary(&self) -> Option<String> {
        let request = self.search_request().ok()?;
        let filters = request.filters();
        if filters.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !filters.extensions.is_empty() {
            parts.push(format!("ext:{}", filters.extensions.join(",")));
        }
        if let Some(file_type) = filters.file_type {
            parts.push(format!("type:{}", search_file_type_name(file_type)));
        }
        for path in filters.path_contains {
            parts.push(format!("path:{path}"));
        }
        Some(format!("Active filters: {}", parts.join("  ")))
    }

    fn selected_row(&self) -> Option<&SearchResultRow> {
        let hit = self.selected_hit.as_ref()?;
        self.rows.iter().find(|row| &row.hit == hit)
    }

    fn open_selected(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            self.status = "No result selected.".into();
            return;
        };
        let Some(client) = self.client.as_mut() else {
            self.status = "Daemon client is unavailable.".into();
            return;
        };
        match client.resolve_path(&row.hit.device_id, row.hit.record_idx) {
            Ok(path) if path.mounted => match open::that(&path.path) {
                Ok(()) => self.status = format!("Opened {}", path.path),
                Err(err) => self.status = format!("Failed to open {}: {err}", path.path),
            },
            Ok(_) => self.status = "This item is indexed, but its device is not mounted.".into(),
            Err(err) => self.status = format!("Failed to resolve path: {err:#}"),
        }
    }

    fn open_selected_location(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            self.status = "No result selected.".into();
            return;
        };
        let Some(client) = self.client.as_mut() else {
            self.status = "Daemon client is unavailable.".into();
            return;
        };
        match client.resolve_path(&row.hit.device_id, row.hit.record_idx) {
            Ok(path) if path.mounted => {
                let target = if row.is_dir {
                    PathBuf::from(&path.path)
                } else {
                    PathBuf::from(&path.path)
                        .parent()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from(&path.path))
                };
                match open::that(&target) {
                    Ok(()) => self.status = format!("Opened {}", target.display()),
                    Err(err) => self.status = format!("Failed to open {}: {err}", target.display()),
                }
            }
            Ok(_) => self.status = "This item is indexed, but its device is not mounted.".into(),
            Err(err) => self.status = format!("Failed to resolve path: {err:#}"),
        }
    }

    fn copy_selected_name(&mut self, ctx: &egui::Context) {
        let Some(row) = self.selected_row() else {
            self.status = "No result selected.".into();
            return;
        };
        ctx.copy_text(row.name.clone());
        self.status = "Copied file name.".into();
    }

    fn copy_selected_path(&mut self, ctx: &egui::Context) {
        let Some(row) = self.selected_row() else {
            self.status = "No result selected.".into();
            return;
        };
        ctx.copy_text(row.display_path.clone());
        self.status = "Copied full path.".into();
    }

    fn scan_device(&mut self, device_id: String) {
        let Some(client) = self.client.as_mut() else {
            self.status = "Daemon client is unavailable.".into();
            return;
        };
        self.status = format!("Indexing {device_id}...");
        match client.start_scan(&device_id) {
            Ok(result) => {
                self.status = format!(
                    "Queued scan job {} for {}.",
                    result.job_id, result.device_id
                );
                self.refresh_lists();
                self.recompute_rows();
            }
            Err(err) => self.status = format!("Indexing failed for {device_id}: {err:#}"),
        }
    }

    fn forget_index(&mut self, device_id: String) {
        let Some(client) = self.client.as_mut() else {
            self.status = "Daemon client is unavailable.".into();
            return;
        };
        match client.forget_index(&device_id) {
            Ok(indexes) => {
                self.indexes = indexes;
                self.rows.retain(|row| row.hit.device_id != device_id);
                if self
                    .selected_hit
                    .as_ref()
                    .map(|hit| hit.device_id == device_id)
                    .unwrap_or(false)
                {
                    self.selected_hit = None;
                }
                if self.selected_scope == device_id {
                    self.selected_scope.clear();
                }
                self.status = format!("Forgot {device_id}.");
            }
            Err(err) => self.status = format!("Failed to forget {device_id}: {err:#}"),
        }
    }

    fn apply_theme_setting(&mut self, ctx: &egui::Context, theme: ThemeMode) {
        let Some(client) = self.client.as_mut() else {
            self.status = "Daemon client is unavailable.".into();
            return;
        };
        let value = match theme {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        };
        match client.config_set("ui.theme", serde_json::Value::String(value.into())) {
            Ok(_) => {
                self.theme = theme;
                apply_theme(ctx, theme);
                self.status = format!("Theme set to {value}.");
            }
            Err(err) => self.status = format!("Failed to update theme: {err:#}"),
        }
    }
}

impl eframe::App for DaemonGuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && !ctx.egui_wants_keyboard_input() {
            self.open_selected();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !ctx.egui_wants_keyboard_input() {
            if self.properties_row.is_some() {
                self.properties_row = None;
            } else if self.show_settings {
                self.show_settings = false;
            } else if self.show_filters {
                self.show_filters = false;
            } else {
                self.selected_hit = None;
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::C))
            && !ctx.egui_wants_keyboard_input()
        {
            if ctx.input(|i| i.modifiers.shift) {
                self.copy_selected_name(&ctx);
            } else {
                self.copy_selected_path(&ctx);
            }
        }

        egui::Panel::top("daemon-top").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_lists();
                    self.recompute_rows();
                }
                if ui.button("Indexes").clicked() {
                    self.show_index_manager = true;
                    self.refresh_lists();
                }
                if ui.button("Settings").clicked() {
                    self.show_settings = true;
                }
                egui::ComboBox::from_id_salt("daemon-device-scope")
                    .selected_text(daemon_scope_label(&self.selected_scope, &self.indexes))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        let mut scope_changed = false;
                        if ui
                            .selectable_value(
                                &mut self.selected_scope,
                                String::new(),
                                "All devices",
                            )
                            .clicked()
                        {
                            scope_changed = true;
                        }
                        for index in &self.indexes {
                            let label = daemon_index_label(index);
                            if ui
                                .selectable_value(
                                    &mut self.selected_scope,
                                    index.device_id.clone(),
                                    label,
                                )
                                .clicked()
                            {
                                scope_changed = true;
                            }
                        }
                        if scope_changed {
                            self.recompute_rows();
                        }
                    });
                if ui.selectable_label(self.show_filters, "Filters").clicked() {
                    self.show_filters = !self.show_filters;
                }
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text(
                            "Search files, e.g. *.rs ext:txt path:src type:dir !cache size:<10mb",
                        )
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    self.recompute_rows();
                }
            });
            if self.show_filters {
                self.daemon_filter_panel(ui);
            }
            if let Some(summary) = self.filter_summary() {
                ui.small(summary);
            }
        });

        egui::Panel::bottom("daemon-status").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} object{} found",
                    self.rows.len(),
                    plural(self.rows.len())
                ));
                ui.separator();
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.daemon_toolbar(ui, &ctx);
            ui.separator();
            self.daemon_results(ui, &ctx);
        });

        if self.show_index_manager {
            self.daemon_index_manager(&ctx);
        }
        if self.show_settings {
            self.daemon_settings(&ctx);
        }
        self.daemon_properties(&ctx);
    }
}

impl DaemonGuiApp {
    fn daemon_filter_panel(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Ext");
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.filter_extensions)
                        .hint_text("rs,txt")
                        .desired_width(140.0),
                )
                .changed();
            ui.label("Type");
            let before_type = self.filter_type;
            egui::ComboBox::from_id_salt("daemon-type-filter")
                .selected_text(file_type_label(self.filter_type))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter_type, None, "All");
                    ui.selectable_value(&mut self.filter_type, Some(SearchFileType::File), "Files");
                    ui.selectable_value(
                        &mut self.filter_type,
                        Some(SearchFileType::Dir),
                        "Folders",
                    );
                    ui.selectable_value(
                        &mut self.filter_type,
                        Some(SearchFileType::Symlink),
                        "Symlinks",
                    );
                });
            changed |= before_type != self.filter_type;
            ui.label("Path");
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.filter_path)
                        .hint_text("src")
                        .desired_width(180.0),
                )
                .changed();
            if ui.button("Clear").clicked() {
                self.filter_extensions.clear();
                self.filter_path.clear();
                self.filter_type = None;
                changed = true;
            }
        });
        if changed {
            self.recompute_rows();
        }
    }

    fn daemon_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let has_selection = self.selected_row().is_some();
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
                self.copy_selected_name(ctx);
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Copy Path"))
                .clicked()
            {
                self.copy_selected_path(ctx);
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Properties"))
                .clicked()
            {
                self.properties_row = self.selected_row().cloned();
            }
            ui.separator();
            let mut sort_changed = false;
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Relevance,
                "Relevance",
            );
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
                self.recompute_rows();
            }
        });
    }

    fn daemon_results(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.indexes.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("No indexes yet");
                ui.label("Open Indexes to index an NTFS, EXT4, or Btrfs device.");
            });
            return;
        }
        if self.rows.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("No results");
                ui.label("Try a different name, wildcard, extension, path, or type filter.");
            });
            return;
        }

        let row_height = 24.0;
        TableBuilder::new(ui)
            .id_salt("daemon-results")
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
                body.rows(row_height, self.rows.len(), |mut row| {
                    let row_index = row.index();
                    let Some(result) = self.rows.get(row_index).cloned() else {
                        return;
                    };
                    let selected = self.selected_hit.as_ref() == Some(&result.hit);
                    row.set_selected(selected);
                    row.col(|ui| {
                        ui.label(&result.name);
                    });
                    row.col(|ui| {
                        ui.label(&result.display_path);
                    });
                    row.col(|ui| {
                        ui.label(format_size(result.size));
                    });
                    row.col(|ui| {
                        ui.label(format_time(result.mtime));
                    });

                    let response = row.response();
                    if response.secondary_clicked() {
                        self.selected_hit = Some(result.hit.clone());
                    }
                    response.clone().context_menu(|ui| {
                        self.selected_hit = Some(result.hit.clone());
                        if ui.button("Open").clicked() {
                            self.open_selected();
                            ui.close();
                        }
                        if ui.button("Open Folder").clicked() {
                            self.open_selected_location();
                            ui.close();
                        }
                        if ui.button("Copy Name").clicked() {
                            self.copy_selected_name(ctx);
                            ui.close();
                        }
                        if ui.button("Copy Path").clicked() {
                            self.copy_selected_path(ctx);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Rescan This Device").clicked() {
                            self.scan_device(result.hit.device_id.clone());
                            ui.close();
                        }
                        if ui.button("Forget This Index").clicked() {
                            self.forget_index(result.hit.device_id.clone());
                            ui.close();
                        }
                        if ui.button("Properties").clicked() {
                            self.properties_row = Some(result.clone());
                            ui.close();
                        }
                    });
                    if response.double_clicked() {
                        self.selected_hit = Some(result.hit);
                        self.open_selected();
                    } else if response.clicked() {
                        self.selected_hit = Some(result.hit);
                    }
                });
            });
    }

    fn daemon_index_manager(&mut self, ctx: &egui::Context) {
        let mut open = self.show_index_manager;
        egui::Window::new("Indexes")
            .open(&mut open)
            .default_width(980.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Refresh Devices").clicked() {
                        self.refresh_lists();
                    }
                    if ui.button("Refresh Jobs").clicked() {
                        self.refresh_lists();
                    }
                });
                for job in self.jobs.clone() {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Job {} {} {:?} {}%",
                            job.job_id, job.device_id, job.state, job.progress
                        ));
                        ui.label(fit(&job.message, 42));
                        if matches!(
                            job.state,
                            oxidex_core::daemon_model::ScanState::Queued
                                | oxidex_core::daemon_model::ScanState::Running
                        ) && ui.button("Cancel").clicked()
                            && let Some(client) = self.client.as_mut()
                        {
                            match client.cancel_scan(Some(job.job_id), None) {
                                Ok(result) => self.status = result.message,
                                Err(err) => self.status = format!("Failed to cancel scan: {err:#}"),
                            }
                        }
                    });
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(460.0)
                    .show(ui, |ui| {
                        for device in self.devices.clone() {
                            let indexed_count = self
                                .indexes
                                .iter()
                                .find(|index| index.device_id == device.device_id)
                                .map(|index| index.entry_count);
                            ui.horizontal(|ui| {
                                ui.label(fit(&device_summary_label(&device), 30));
                                ui.label(fit(device.fs_type.as_str(), 7));
                                ui.label(if device.mounted {
                                    "mounted"
                                } else {
                                    "not mounted"
                                });
                                ui.label(fit(&device.dev_node, 22));
                                ui.label(
                                    indexed_count
                                        .map(|count| format!("{count} entries"))
                                        .unwrap_or_else(|| "not indexed".into()),
                                );
                                if let Some(index) = self
                                    .indexes
                                    .iter()
                                    .find(|index| index.device_id == device.device_id)
                                    && let Some(state) = &index.state
                                {
                                    if let Some(error) = &state.last_error {
                                        ui.label(fit(error, 28));
                                    } else if let Some(watch) = self
                                        .watches
                                        .iter()
                                        .find(|watch| watch.device_id == device.device_id)
                                    {
                                        ui.label(fit(&watch.state, 16));
                                    }
                                }
                                let label = if indexed_count.is_some() {
                                    "Rescan"
                                } else {
                                    "Index"
                                };
                                if ui.button(label).clicked() {
                                    self.scan_device(device.device_id.clone());
                                }
                                if indexed_count.is_some() && ui.button("Forget").clicked() {
                                    self.forget_index(device.device_id.clone());
                                }
                            });
                            ui.separator();
                        }
                    });
            });
        self.show_index_manager = open;
    }

    fn daemon_settings(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.heading("Appearance");
                ui.horizontal(|ui| {
                    ui.label("Theme");
                    let mut next_theme = self.theme;
                    egui::ComboBox::from_id_salt("daemon-theme")
                        .selected_text(theme_label(self.theme))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut next_theme, ThemeMode::System, "System");
                            ui.selectable_value(&mut next_theme, ThemeMode::Light, "Light");
                            ui.selectable_value(&mut next_theme, ThemeMode::Dark, "Dark");
                        });
                    if next_theme != self.theme {
                        self.apply_theme_setting(ctx, next_theme);
                    }
                });
                ui.separator();
                ui.heading("Indexing");
                if let Some(client) = self.client.as_mut()
                    && let Ok(config) = client.config_get()
                {
                    ui.label(format!(
                        "Mounted live updates: {}",
                        yes_no(config.config.indexing.watch_mounted)
                    ));
                    ui.label(format!(
                        "Max parallel scans: {}",
                        config.config.indexing.max_parallel_scans
                    ));
                    ui.label(format!(
                        "Rofi max results: {}",
                        config.config.rofi.max_results
                    ));
                }
                ui.separator();
                ui.heading("Advanced");
                if let Some(client) = self.client.as_mut()
                    && ui.button("Show Config Path").clicked()
                {
                    match client.config_get() {
                        Ok(config) => self.status = config.path,
                        Err(err) => self.status = format!("Failed to read config: {err:#}"),
                    }
                }
            });
        self.show_settings = open;
    }

    fn daemon_properties(&mut self, ctx: &egui::Context) {
        let Some(row) = self.properties_row.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Properties")
            .open(&mut open)
            .default_width(540.0)
            .show(ctx, |ui| {
                property_row(ui, "Name", &row.name);
                property_row(ui, "Path", &row.display_path);
                property_row(ui, "Internal Path", &row.internal_path);
                property_row(ui, "Device", &row.device_label);
                property_row(ui, "Device ID", &row.hit.device_id);
                property_row(ui, "Record", &row.hit.record_idx.to_string());
                property_row(ui, "Filesystem", row.fs_type.as_str());
                property_row(ui, "Size", &format_size(row.size));
                property_row(ui, "Modified", &format_time(row.mtime));
                property_row(ui, "Directory", yes_no(row.is_dir));
                property_row(ui, "Symlink", yes_no(row.is_symlink));
                property_row(ui, "Mounted", yes_no(row.mounted));
                property_row(ui, "Last Indexed", &format_time(row.last_indexed_time));
                if let Some(index) = self
                    .indexes
                    .iter()
                    .find(|index| index.device_id == row.hit.device_id)
                    && let Some(state) = &index.state
                {
                    if let Some(scanner) = &state.last_scanner {
                        property_row(ui, "Scanner", scanner);
                    }
                    if let Some(error) = &state.last_error {
                        property_row(ui, "Last Error", error);
                    }
                    if let Some(stale) = &state.stale_reason {
                        property_row(ui, "Stale", stale);
                    }
                }
            });
        if !open {
            self.properties_row = None;
        }
    }
}

struct ServiceErrorApp {
    message: String,
}

impl ServiceErrorApp {
    fn new(err: anyhow::Error) -> Self {
        Self {
            message: format!(
                "Could not connect to oxidexd: {err:#}\n\nRun oxidex --standalone for the in-process fallback, or start oxidexd --foreground."
            ),
        }
    }
}

impl eframe::App for ServiceErrorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(72.0);
                ui.heading("Oxidex service is unavailable");
                ui.label(&self.message);
            });
        });
    }
}

impl OxidexApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let config = load_config().unwrap_or_default();
        apply_theme(&cc.egui_ctx, config.ui.theme);
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
            show_filters: config.ui.show_filter_panel,
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

    fn search_request(&self) -> Result<SearchRequest, oxidex_core::index::SearchParseError> {
        let mut request = parse_search_query(&self.query)?;
        request.add_filters(self.panel_filters());
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

impl eframe::App for OxidexApp {
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

impl OxidexApp {
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
        let filters = request.filters();
        if filters.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !filters.extensions.is_empty() {
            parts.push(format!("ext:{}", filters.extensions.join(",")));
        }
        if let Some(file_type) = filters.file_type {
            parts.push(format!("type:{}", search_file_type_name(file_type)));
        }
        for path in filters.path_contains {
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
) -> anyhow::Result<oxidex_core::model::ScanDatabase> {
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
            if let Some(rest) = line.trim().strip_prefix("OXIDEX_PROGRESS ")
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
            return oxidex_core::stream::read_scan_stream(&output[..]);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn connect_or_start_daemon() -> anyhow::Result<OxidexClient> {
    match OxidexClient::connect_default() {
        Ok(client) => Ok(client),
        Err(first_err) => {
            start_daemon_once()?;
            for _ in 0..20 {
                if let Ok(client) = OxidexClient::connect_default() {
                    return Ok(client);
                }
                thread::sleep(Duration::from_millis(100));
            }
            anyhow::bail!("failed to connect after attempting to start oxidexd: {first_err:#}")
        }
    }
}

fn start_daemon_once() -> anyhow::Result<()> {
    Command::new(sibling_binary("oxidexd"))
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

fn sibling_binary(name: &str) -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from(name);
    };
    if let Some(dir) = exe.parent() {
        let sibling = dir.join(name);
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from(name)
}

fn helper_path() -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from("oxidex-scanner-helper");
    };
    if let Some(dir) = exe.parent() {
        let sibling = dir.join("oxidex-scanner-helper");
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from("oxidex-scanner-helper")
}

fn mounted_path(index: &SearchIndex, rec_idx: u32, device: &DeviceInfo) -> PathBuf {
    let mut path = PathBuf::from(&device.primary_mount_point);
    let internal = index.internal_path(rec_idx).trim_start_matches('/');
    if !internal.is_empty() {
        path.push(internal);
    }
    path
}

fn apply_theme(ctx: &egui::Context, theme: ThemeMode) {
    match theme {
        ThemeMode::System => {}
        ThemeMode::Light => ctx.set_visuals(egui::Visuals::light()),
        ThemeMode::Dark => ctx.set_visuals(egui::Visuals::dark()),
    }
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

fn daemon_scope_label(scope: &str, indexes: &[IndexSummary]) -> String {
    if scope.is_empty() {
        return "All devices".into();
    }
    indexes
        .iter()
        .find(|index| index.device_id == scope)
        .map(daemon_index_label)
        .unwrap_or_else(|| scope.to_owned())
}

fn daemon_index_label(index: &IndexSummary) -> String {
    if !index.label.trim().is_empty() {
        format!("{} ({})", index.label.trim(), index.device_id)
    } else {
        index.device_id.clone()
    }
}

fn device_label(meta: &DeviceMetadata) -> String {
    if !meta.label.trim().is_empty() {
        format!("{} ({})", meta.label.trim(), meta.device_id)
    } else {
        meta.device_id.clone()
    }
}

fn device_summary_label(device: &DeviceSummary) -> String {
    if !device.label.trim().is_empty() {
        format!("{} ({})", device.label.trim(), device.device_id)
    } else {
        device.device_id.clone()
    }
}

fn theme_label(theme: ThemeMode) -> &'static str {
    match theme {
        ThemeMode::System => "System",
        ThemeMode::Light => "Light",
        ThemeMode::Dark => "Dark",
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
