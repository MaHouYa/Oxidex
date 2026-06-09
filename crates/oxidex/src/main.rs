use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use fonts::CjkFontStatus;
use i18n::{LanguageMode, ResolvedLanguage, Text, language_mode_value, system_language, tr};
use oxidex_client::OxidexClient;
use oxidex_core::config::{AppConfig, ThemeMode, load_config, save_config};
use oxidex_core::daemon_model::{
    DeviceSummary, IndexSummary, ScanJobSummary, SearchQueryParams, SearchResultRow, WatchSummary,
};
use oxidex_core::device::{DeviceInfo, list_known_devices};
use oxidex_core::index::{
    SearchFileType, SearchFilters, SearchHit, SearchIndex, SearchRequest, merge_search_request,
    parse_search_query,
};
use oxidex_core::model::{DeviceMetadata, SortDirection, SortKey};
use oxidex_core::snapshot;
use time::{OffsetDateTime, UtcOffset, macros::format_description};
use tracing_subscriber::EnvFilter;

mod fonts;
mod i18n;

#[derive(Clone, Debug, Default)]
struct GuiArgs {
    standalone: bool,
    debug: bool,
    log_level: Option<String>,
}

fn main() -> eframe::Result {
    let args = match parse_gui_args() {
        Ok(Some(args)) => args,
        Ok(None) => return Ok(()),
        Err(err) => {
            eprintln!("oxidex: {err:#}");
            print_gui_usage();
            return Ok(());
        }
    };
    init_terminal_logging("oxidex", args.debug, args.log_level.as_deref());
    tracing::info!(
        standalone = args.standalone,
        debug = args.debug,
        "starting Oxidex GUI"
    );

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
            if args.standalone {
                tracing::debug!("starting standalone GUI mode");
                return Ok(Box::new(OxidexApp::new(cc)) as Box<dyn eframe::App>);
            }
            match DaemonGuiApp::new(cc, &args) {
                Ok(app) => Ok(Box::new(app) as Box<dyn eframe::App>),
                Err(err) => Ok(Box::new(ServiceErrorApp::new(cc, err)) as Box<dyn eframe::App>),
            }
        }),
    )
}

fn parse_gui_args() -> anyhow::Result<Option<GuiArgs>> {
    let mut parsed = GuiArgs::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--standalone" => parsed.standalone = true,
            "--debug" => parsed.debug = true,
            "--log-level" => {
                let Some(value) = args.next() else {
                    anyhow::bail!("--log-level requires a value");
                };
                parsed.log_level = Some(value);
            }
            "--help" | "-h" => {
                print_gui_usage();
                return Ok(None);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    Ok(Some(parsed))
}

fn print_gui_usage() {
    eprintln!("Usage: oxidex [--standalone] [--debug] [--log-level <level>]");
    eprintln!("Levels: trace, debug, info, warn, error. RUST_LOG overrides --log-level.");
}

fn init_terminal_logging(binary: &str, debug: bool, log_level: Option<&str>) {
    let default_level = log_level.unwrap_or(if debug { "debug" } else { "warn" });
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    if let Err(err) = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_thread_ids(debug)
        .with_file(debug)
        .with_line_number(debug)
        .try_init()
    {
        eprintln!("{binary}: failed to initialize terminal logging: {err}");
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ImeFrame {
    active_preedit: bool,
}

fn current_ime_frame(ctx: &egui::Context) -> ImeFrame {
    let debug_enabled = tracing::enabled!(tracing::Level::DEBUG);
    let mut frame = ImeFrame::default();
    ctx.input(|input| {
        frame = ime_frame_from_events(&input.events, debug_enabled);
    });
    frame
}

fn ime_frame_from_events(events: &[egui::Event], debug_enabled: bool) -> ImeFrame {
    let mut frame = ImeFrame::default();
    for event in events {
        let egui::Event::Ime(ime_event) = event else {
            continue;
        };
        match ime_event {
            egui::ImeEvent::Enabled => {
                if debug_enabled {
                    tracing::debug!("IME enabled");
                }
            }
            egui::ImeEvent::Preedit(text) => {
                frame.active_preedit = !text.is_empty();
                if debug_enabled {
                    tracing::debug!(
                        chars = text.chars().count(),
                        bytes = text.len(),
                        "IME preedit"
                    );
                }
            }
            egui::ImeEvent::Commit(text) => {
                frame.active_preedit = false;
                if debug_enabled {
                    tracing::debug!(
                        chars = text.chars().count(),
                        bytes = text.len(),
                        "IME commit"
                    );
                }
            }
            egui::ImeEvent::Disabled => {
                frame.active_preedit = false;
                if debug_enabled {
                    tracing::debug!("IME disabled");
                }
            }
        }
    }
    frame
}

#[cfg(test)]
mod ime_tests {
    use super::*;

    #[test]
    fn ime_enabled_does_not_mean_active_preedit() {
        let frame = ime_frame_from_events(&[egui::Event::Ime(egui::ImeEvent::Enabled)], false);
        assert!(!frame.active_preedit);
    }

    #[test]
    fn ime_preedit_delays_search_refresh() {
        let frame = ime_frame_from_events(
            &[egui::Event::Ime(egui::ImeEvent::Preedit("ni".into()))],
            false,
        );
        assert!(frame.active_preedit);
    }

    #[test]
    fn ime_commit_allows_search_refresh() {
        let frame = ime_frame_from_events(
            &[
                egui::Event::Ime(egui::ImeEvent::Preedit("ni".into())),
                egui::Event::Ime(egui::ImeEvent::Commit("你".into())),
            ],
            false,
        );
        assert!(!frame.active_preedit);
    }

    #[test]
    fn commit_only_ime_event_inserts_after_existing_text() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("commit-only-ime");
        let mut text = "abc".to_owned();

        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::TextEdit::singleline(&mut text)
                .id(id)
                .show(ui)
                .response
                .response
                .request_focus();
        });

        let mut state = egui::TextEdit::load_state(&ctx, id).expect("text edit state");
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(text.chars().count()),
            )));
        egui::TextEdit::store_state(&ctx, id, state);
        ctx.memory_mut(|memory| memory.request_focus(id));

        let mut input = egui::RawInput::default();
        input
            .events
            .push(egui::Event::Ime(egui::ImeEvent::Commit("你好".into())));
        let _ = ctx.run_ui(input, |ui| {
            egui::TextEdit::singleline(&mut text).id(id).show(ui);
        });

        assert_eq!(text, "abc你好");
    }
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
    show_settings: bool,
    properties_hit: Option<SearchHit>,
    last_scan_errors: HashMap<String, String>,
    scan_job: Option<ScanJob>,
    language_mode: LanguageMode,
    language: ResolvedLanguage,
    cjk_font_fallback: bool,
    cjk_preferred_font: String,
    cjk_font_status: CjkFontStatus,
}

struct ScanJob {
    device_id: String,
    cancel: Arc<AtomicBool>,
    progress: u8,
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
    language_mode: LanguageMode,
    language: ResolvedLanguage,
    cjk_font_fallback: bool,
    cjk_preferred_font: String,
    cjk_font_status: CjkFontStatus,
}

impl DaemonGuiApp {
    fn t(&self, key: Text) -> &'static str {
        tr(self.language, key)
    }

    fn new(cc: &eframe::CreationContext<'_>, args: &GuiArgs) -> anyhow::Result<Self> {
        tracing::debug!("connecting GUI to oxidexd");
        let mut client = connect_or_start_daemon(args.debug, args.log_level.as_deref())?;
        let config = client.config_get().ok();
        let theme = config
            .as_ref()
            .map(|config| config.config.ui.theme)
            .unwrap_or(ThemeMode::System);
        let ui_config = config
            .as_ref()
            .map(|config| config.config.clone())
            .unwrap_or_default();
        let (language, cjk_font_status) = apply_gui_preferences(&cc.egui_ctx, &ui_config);

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
        tracing::debug!(
            devices = devices.len(),
            indexes = indexes.len(),
            jobs = jobs.len(),
            watches = watches.len(),
            "loaded initial daemon state"
        );
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
            status: tr(language, Text::ConnectedToDaemon).into(),
            show_filters: config
                .as_ref()
                .map(|config| config.config.ui.show_filter_panel)
                .unwrap_or(true),
            show_index_manager: false,
            show_settings: false,
            properties_row: None,
            theme,
            language_mode: ui_config.ui.language,
            language,
            cjk_font_fallback: ui_config.ui.cjk_font_fallback,
            cjk_preferred_font: ui_config.ui.cjk_preferred_font.clone(),
            cjk_font_status,
        };
        app.recompute_rows();
        Ok(app)
    }

    fn recompute_rows(&mut self) {
        let request = match self.search_request() {
            Ok(request) => request,
            Err(err) => {
                self.status = format!("{}: {err}", self.t(Text::SearchError));
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
                tracing::debug!(
                    query = %self.query,
                    rows = count,
                    truncated,
                    "daemon search completed"
                );
                self.rows = result.rows;
                self.selected_hit =
                    previous.filter(|hit| self.rows.iter().any(|row| row.hit == *hit));
                self.status = if truncated {
                    format!("{}: {count}.", self.t(Text::ShowingFirstResults))
                } else {
                    format!("{} {}.", count, result_word(self.language, count))
                };
            }
            Err(err) => self.status = format!("{}: {err:#}", self.t(Text::SearchFailed)),
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
                tracing::debug!(
                    devices = devices.len(),
                    indexes = indexes.len(),
                    jobs = jobs.len(),
                    watches = watches.len(),
                    "refreshed daemon lists"
                );
                self.devices = devices;
                self.indexes = indexes;
                self.jobs = jobs;
                self.watches = watches;
                self.status = self.t(Text::Refreshed).into();
            }
            (Err(err), _, _, _)
            | (_, Err(err), _, _)
            | (_, _, Err(err), _)
            | (_, _, _, Err(err)) => {
                tracing::warn!(error = %format!("{err:#}"), "failed to refresh daemon lists");
                self.status = format!("{}: {err:#}", self.t(Text::SearchFailed))
            }
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
        Some(format!(
            "{}: {}",
            self.t(Text::ActiveFilters),
            parts.join("  ")
        ))
    }

    fn selected_row(&self) -> Option<&SearchResultRow> {
        let hit = self.selected_hit.as_ref()?;
        self.rows.iter().find(|row| &row.hit == hit)
    }

    fn open_selected(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            self.status = self.t(Text::NoResultSelected).into();
            return;
        };
        let Some(client) = self.client.as_mut() else {
            self.status = self.t(Text::DaemonClientUnavailable).into();
            return;
        };
        match client.resolve_path(&row.hit.device_id, row.hit.record_idx) {
            Ok(path) if path.mounted => match open::that(&path.path) {
                Ok(()) => self.status = format!("{} {}", self.t(Text::Opened), path.path),
                Err(err) => {
                    self.status = format!("{} {}: {err}", self.t(Text::FailedToOpen), path.path)
                }
            },
            Ok(_) => self.status = self.t(Text::IndexedDeviceUnmounted).into(),
            Err(err) => self.status = format!("{}: {err:#}", self.t(Text::FailedToResolvePath)),
        }
    }

    fn open_selected_location(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            self.status = self.t(Text::NoResultSelected).into();
            return;
        };
        let Some(client) = self.client.as_mut() else {
            self.status = self.t(Text::DaemonClientUnavailable).into();
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
                    Ok(()) => {
                        self.status = format!("{} {}", self.t(Text::Opened), target.display())
                    }
                    Err(err) => {
                        self.status =
                            format!("{} {}: {err}", self.t(Text::FailedToOpen), target.display())
                    }
                }
            }
            Ok(_) => self.status = self.t(Text::IndexedDeviceUnmounted).into(),
            Err(err) => self.status = format!("{}: {err:#}", self.t(Text::FailedToResolvePath)),
        }
    }

    fn copy_selected_name(&mut self, ctx: &egui::Context) {
        let Some(row) = self.selected_row() else {
            self.status = self.t(Text::NoResultSelected).into();
            return;
        };
        ctx.copy_text(row.name.clone());
        self.status = self.t(Text::CopiedFileName).into();
    }

    fn copy_selected_path(&mut self, ctx: &egui::Context) {
        let Some(row) = self.selected_row() else {
            self.status = self.t(Text::NoResultSelected).into();
            return;
        };
        ctx.copy_text(row.display_path.clone());
        self.status = self.t(Text::CopiedFullPath).into();
    }

    fn scan_device(&mut self, device_id: String) {
        let indexing_label = self.t(Text::IndexingDevice);
        let queued_label = self.t(Text::QueuedScanJob);
        let failed_label = self.t(Text::IndexingFailed);
        let unavailable_label = self.t(Text::DaemonClientUnavailable);
        let Some(client) = self.client.as_mut() else {
            self.status = unavailable_label.into();
            return;
        };
        self.status = format!("{indexing_label} {device_id}...");
        tracing::info!(device_id = %device_id, "requesting daemon scan");
        match client.start_scan(&device_id) {
            Ok(result) => {
                tracing::info!(
                    job_id = result.job_id,
                    device_id = %result.device_id,
                    "daemon scan queued"
                );
                self.status = format!("{queued_label} {}: {}.", result.job_id, result.device_id);
                self.refresh_lists();
                self.recompute_rows();
            }
            Err(err) => {
                tracing::warn!(
                    device_id = %device_id,
                    error = %format!("{err:#}"),
                    "failed to start daemon scan"
                );
                self.status = format!("{failed_label} {device_id}: {err:#}");
            }
        }
    }

    fn forget_index(&mut self, device_id: String) {
        let Some(client) = self.client.as_mut() else {
            self.status = self.t(Text::DaemonClientUnavailable).into();
            return;
        };
        match client.forget_index(&device_id) {
            Ok(indexes) => {
                tracing::info!(device_id = %device_id, "forgot index through daemon");
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
                self.status = format!("{} {device_id}.", self.t(Text::Forgot));
            }
            Err(err) => {
                tracing::warn!(
                    device_id = %device_id,
                    error = %format!("{err:#}"),
                    "failed to forget index through daemon"
                );
                self.status = format!("{} {device_id}: {err:#}", self.t(Text::FailedToForget))
            }
        }
    }

    fn apply_theme_setting(&mut self, ctx: &egui::Context, theme: ThemeMode) {
        let Some(client) = self.client.as_mut() else {
            self.status = self.t(Text::DaemonClientUnavailable).into();
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
                self.status = format!("{}: {}.", self.t(Text::ThemeSet), value);
            }
            Err(err) => self.status = format!("{}: {err:#}", self.t(Text::FailedToUpdateTheme)),
        }
    }

    fn apply_language_setting(&mut self, language_mode: LanguageMode) {
        let Some(client) = self.client.as_mut() else {
            self.status = self.t(Text::DaemonClientUnavailable).into();
            return;
        };
        match client.config_set(
            "ui.language",
            serde_json::Value::String(language_mode_value(language_mode).into()),
        ) {
            Ok(_) => {
                self.language_mode = language_mode;
                self.language = system_language(language_mode);
                self.status = format!(
                    "{}: {}",
                    self.t(Text::Language),
                    language_label(self.language, language_mode)
                );
            }
            Err(err) => self.status = format!("{}: {err:#}", self.t(Text::FailedToUpdateLanguage)),
        }
    }

    fn apply_cjk_font_settings(&mut self, ctx: &egui::Context) {
        let Some(client) = self.client.as_mut() else {
            self.status = self.t(Text::DaemonClientUnavailable).into();
            return;
        };
        let fallback = self.cjk_font_fallback;
        let preferred = self.cjk_preferred_font.trim().to_owned();
        let result = client
            .config_set("ui.cjk_font_fallback", serde_json::Value::Bool(fallback))
            .and_then(|_| {
                client.config_set(
                    "ui.cjk_preferred_font",
                    serde_json::Value::String(preferred.clone()),
                )
            });
        match result {
            Ok(_) => {
                self.cjk_preferred_font = preferred;
                self.cjk_font_status =
                    fonts::configure_fonts(ctx, self.cjk_font_fallback, &self.cjk_preferred_font);
                apply_theme(ctx, self.theme);
                self.status = self.t(Text::CjkFontsLoaded).into();
            }
            Err(err) => {
                self.status = format!("{}: {err:#}", self.t(Text::FailedToUpdateCjkFontSettings))
            }
        }
    }
}

impl eframe::App for DaemonGuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ime_frame = current_ime_frame(&ctx);
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
                if ui.button(self.t(Text::Refresh)).clicked() {
                    self.refresh_lists();
                    self.recompute_rows();
                }
                if ui.button(self.t(Text::Indexes)).clicked() {
                    self.show_index_manager = true;
                    self.refresh_lists();
                }
                if ui.button(self.t(Text::Settings)).clicked() {
                    self.show_settings = true;
                }
                egui::ComboBox::from_id_salt("daemon-device-scope")
                    .selected_text(daemon_scope_label(
                        self.language,
                        &self.selected_scope,
                        &self.indexes,
                    ))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        let all_devices_label = self.t(Text::AllDevices);
                        let mut scope_changed = false;
                        if ui
                            .selectable_value(
                                &mut self.selected_scope,
                                String::new(),
                                all_devices_label,
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
                let filters_label = self.t(Text::Filters);
                if ui
                    .selectable_label(self.show_filters, filters_label)
                    .clicked()
                {
                    self.show_filters = !self.show_filters;
                }
                let search_hint = self.t(Text::SearchHintDaemon);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .id_salt("daemon-search-query")
                        .hint_text(search_hint)
                        .desired_width(f32::INFINITY),
                );
                if response.changed() && !ime_frame.active_preedit {
                    self.recompute_rows();
                }
            });
            if self.show_filters {
                self.daemon_filter_panel(ui, &ime_frame);
            }
            if let Some(summary) = self.filter_summary() {
                ui.small(summary);
            }
        });

        egui::Panel::bottom("daemon-status").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} {}",
                    self.rows.len(),
                    self.t(Text::ObjectsFound)
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
    fn daemon_filter_panel(&mut self, ui: &mut egui::Ui, ime_frame: &ImeFrame) {
        let mut changed = false;
        ui.horizontal(|ui| {
            let ext_label = self.t(Text::Ext);
            let type_label = self.t(Text::Type);
            let all_label = self.t(Text::All);
            let files_label = self.t(Text::Files);
            let folders_label = self.t(Text::Folders);
            let symlinks_label = self.t(Text::Symlinks);
            let path_label = self.t(Text::Path);
            let clear_label = self.t(Text::Clear);
            ui.label(ext_label);
            let extensions_response = ui.add(
                egui::TextEdit::singleline(&mut self.filter_extensions)
                    .id_salt("daemon-filter-extensions")
                    .hint_text("rs,txt")
                    .desired_width(140.0),
            );
            changed |= extensions_response.changed();
            ui.label(type_label);
            let before_type = self.filter_type;
            egui::ComboBox::from_id_salt("daemon-type-filter")
                .selected_text(file_type_label(self.language, self.filter_type))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter_type, None, all_label);
                    ui.selectable_value(
                        &mut self.filter_type,
                        Some(SearchFileType::File),
                        files_label,
                    );
                    ui.selectable_value(
                        &mut self.filter_type,
                        Some(SearchFileType::Dir),
                        folders_label,
                    );
                    ui.selectable_value(
                        &mut self.filter_type,
                        Some(SearchFileType::Symlink),
                        symlinks_label,
                    );
                });
            changed |= before_type != self.filter_type;
            ui.label(path_label);
            let path_response = ui.add(
                egui::TextEdit::singleline(&mut self.filter_path)
                    .id_salt("daemon-filter-path")
                    .hint_text("src")
                    .desired_width(180.0),
            );
            changed |= path_response.changed();
            if ui.button(clear_label).clicked() {
                self.filter_extensions.clear();
                self.filter_path.clear();
                self.filter_type = None;
                changed = true;
            }
        });
        if changed && !ime_frame.active_preedit {
            self.recompute_rows();
        }
    }

    fn daemon_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let has_selection = self.selected_row().is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::Open)))
                .clicked()
            {
                self.open_selected();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::OpenFolder)))
                .clicked()
            {
                self.open_selected_location();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::CopyName)))
                .clicked()
            {
                self.copy_selected_name(ctx);
            }
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::CopyPath)))
                .clicked()
            {
                self.copy_selected_path(ctx);
            }
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::Properties)))
                .clicked()
            {
                self.properties_row = self.selected_row().cloned();
            }
            ui.separator();
            let relevance_label = self.t(Text::Relevance);
            let name_label = self.t(Text::Name);
            let path_label = self.t(Text::Path);
            let size_label = self.t(Text::Size);
            let date_label = self.t(Text::Date);
            let mut sort_changed = false;
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Relevance,
                relevance_label,
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Name,
                name_label,
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Path,
                path_label,
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Size,
                size_label,
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Mtime,
                date_label,
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
                ui.heading(self.t(Text::NoIndexesYet));
                ui.label(self.t(Text::OpenIndexesHint));
            });
            return;
        }
        if self.rows.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading(self.t(Text::NoResults));
                ui.label(self.t(Text::TryDifferentSearchHint));
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
                    ui.strong(self.t(Text::Name));
                });
                row.col(|ui| {
                    ui.strong(self.t(Text::Path));
                });
                row.col(|ui| {
                    ui.strong(self.t(Text::Size));
                });
                row.col(|ui| {
                    ui.strong(self.t(Text::Modified));
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
                        result_cell_label(ui, &result.name);
                    });
                    row.col(|ui| {
                        result_cell_label(ui, &result.display_path);
                    });
                    row.col(|ui| {
                        result_cell_label(ui, format_size(result.size));
                    });
                    row.col(|ui| {
                        result_cell_label(ui, format_time(result.mtime));
                    });

                    let response = row.response();
                    if response.secondary_clicked() {
                        self.selected_hit = Some(result.hit.clone());
                    }
                    response.clone().context_menu(|ui| {
                        self.selected_hit = Some(result.hit.clone());
                        if ui.button(self.t(Text::Open)).clicked() {
                            self.open_selected();
                            ui.close();
                        }
                        if ui.button(self.t(Text::OpenFolder)).clicked() {
                            self.open_selected_location();
                            ui.close();
                        }
                        if ui.button(self.t(Text::CopyName)).clicked() {
                            self.copy_selected_name(ctx);
                            ui.close();
                        }
                        if ui.button(self.t(Text::CopyPath)).clicked() {
                            self.copy_selected_path(ctx);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button(self.t(Text::RescanThisDevice)).clicked() {
                            self.scan_device(result.hit.device_id.clone());
                            ui.close();
                        }
                        if ui.button(self.t(Text::ForgetThisIndex)).clicked() {
                            self.forget_index(result.hit.device_id.clone());
                            ui.close();
                        }
                        if ui.button(self.t(Text::Properties)).clicked() {
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
        egui::Window::new(self.t(Text::Indexes))
            .open(&mut open)
            .default_width(980.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button(self.t(Text::RefreshDevices)).clicked() {
                        self.refresh_lists();
                    }
                    if ui.button(self.t(Text::RefreshJobs)).clicked() {
                        self.refresh_lists();
                    }
                });
                for job in self.jobs.clone() {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{} {} {} {:?} {}%",
                            self.t(Text::Indexing),
                            job.job_id,
                            job.device_id,
                            job.state,
                            job.progress
                        ));
                        ui.label(fit(&job.message, 42));
                        if matches!(
                            job.state,
                            oxidex_core::daemon_model::ScanState::Queued
                                | oxidex_core::daemon_model::ScanState::Running
                        ) && ui.button(self.t(Text::Cancel)).clicked()
                            && let Some(client) = self.client.as_mut()
                        {
                            match client.cancel_scan(Some(job.job_id), None) {
                                Ok(result) => self.status = result.message,
                                Err(err) => {
                                    self.status =
                                        format!("{}: {err:#}", self.t(Text::FailedToCancelScan));
                                }
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
                                ui.label(fit(&device.fs_type_name, 7));
                                ui.label(if device.mounted {
                                    self.t(Text::Mounted)
                                } else {
                                    self.t(Text::NotMounted)
                                });
                                ui.label(fit(&device.dev_node, 22));
                                ui.label(
                                    indexed_count
                                        .map(|count| {
                                            format!("{count} {}", self.t(Text::EntryPlural))
                                        })
                                        .unwrap_or_else(|| self.t(Text::NotIndexed).into()),
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
                                } else if let Some(reason) = &device.scan_unavailable_reason {
                                    ui.label(fit(reason, 28));
                                }
                                let label = if indexed_count.is_some() {
                                    self.t(Text::Rescan)
                                } else {
                                    self.t(Text::Index)
                                };
                                if ui
                                    .add_enabled(device.scan_supported, egui::Button::new(label))
                                    .on_disabled_hover_text(
                                        device
                                            .scan_unavailable_reason
                                            .as_deref()
                                            .unwrap_or("Device cannot be scanned."),
                                    )
                                    .clicked()
                                {
                                    self.scan_device(device.device_id.clone());
                                }
                                if indexed_count.is_some()
                                    && ui.button(self.t(Text::ForgetThisIndex)).clicked()
                                {
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
        egui::Window::new(self.t(Text::Settings))
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.heading(self.t(Text::Appearance));
                ui.horizontal(|ui| {
                    ui.label(self.t(Text::Theme));
                    let mut next_theme = self.theme;
                    egui::ComboBox::from_id_salt("daemon-theme")
                        .selected_text(theme_label(self.language, self.theme))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut next_theme,
                                ThemeMode::System,
                                self.t(Text::System),
                            );
                            ui.selectable_value(
                                &mut next_theme,
                                ThemeMode::Light,
                                self.t(Text::Light),
                            );
                            ui.selectable_value(
                                &mut next_theme,
                                ThemeMode::Dark,
                                self.t(Text::Dark),
                            );
                        });
                    if next_theme != self.theme {
                        self.apply_theme_setting(ctx, next_theme);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(self.t(Text::Language));
                    let mut next_language = self.language_mode;
                    egui::ComboBox::from_id_salt("daemon-language")
                        .selected_text(language_label(self.language, self.language_mode))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut next_language,
                                LanguageMode::System,
                                self.t(Text::System),
                            );
                            ui.selectable_value(
                                &mut next_language,
                                LanguageMode::EnUs,
                                self.t(Text::English),
                            );
                            ui.selectable_value(
                                &mut next_language,
                                LanguageMode::ZhCn,
                                self.t(Text::SimplifiedChinese),
                            );
                        });
                    if next_language != self.language_mode {
                        self.apply_language_setting(next_language);
                    }
                });
                ui.label(self.t(Text::CjkFonts));
                ui.horizontal(|ui| {
                    let cjk_fallback_label = self.t(Text::CjkFontFallback);
                    let cjk_preferred_label = self.t(Text::CjkPreferredFont);
                    let apply_label = self.t(Text::Apply);
                    ui.checkbox(&mut self.cjk_font_fallback, cjk_fallback_label);
                    ui.label(cjk_preferred_label);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.cjk_preferred_font)
                            .id_salt("daemon-cjk-preferred-font")
                            .desired_width(180.0),
                    );
                    if ui.button(apply_label).clicked() {
                        self.apply_cjk_font_settings(ctx);
                    }
                });
                ui.label(if self.cjk_font_status.unavailable() {
                    self.t(Text::CjkFontsMissing)
                } else if self.cjk_font_status.loaded_fonts.is_empty() {
                    self.t(Text::CjkFontFallback)
                } else {
                    self.t(Text::CjkFontsLoaded)
                });
                if !self.cjk_font_status.loaded_fonts.is_empty() {
                    ui.small(self.cjk_font_status.loaded_fonts.join(", "));
                }
                ui.separator();
                ui.heading(self.t(Text::Indexing));
                if let Some(client) = self.client.as_mut()
                    && let Ok(config) = client.config_get()
                {
                    ui.label(format!(
                        "{}: {}",
                        self.t(Text::MountedLiveUpdates),
                        yes_no(self.language, config.config.indexing.watch_mounted)
                    ));
                    ui.label(format!(
                        "{}: {}",
                        self.t(Text::MaxParallelScans),
                        config.config.indexing.max_parallel_scans
                    ));
                    ui.label(format!(
                        "{}: {}",
                        self.t(Text::RofiMaxResults),
                        config.config.rofi.max_results
                    ));
                }
                ui.separator();
                ui.heading(self.t(Text::Advanced));
                let show_config_label = self.t(Text::ShowConfigPath);
                let failed_read_label = self.t(Text::FailedToReadConfig);
                let unavailable_label = self.t(Text::DaemonClientUnavailable);
                let show_config_clicked = ui.button(show_config_label).clicked();
                if show_config_clicked {
                    match self.client.as_mut().map(|client| client.config_get()) {
                        Some(Ok(config)) => self.status = config.path,
                        Some(Err(err)) => {
                            self.status = format!("{failed_read_label}: {err:#}");
                        }
                        None => self.status = unavailable_label.into(),
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
        egui::Window::new(self.t(Text::Properties))
            .open(&mut open)
            .default_width(540.0)
            .show(ctx, |ui| {
                property_row(ui, self.t(Text::Name), &row.name);
                property_row(ui, self.t(Text::Path), &row.display_path);
                property_row(ui, self.t(Text::InternalPath), &row.internal_path);
                property_row(ui, self.t(Text::Device), &row.device_label);
                property_row(ui, self.t(Text::DeviceId), &row.hit.device_id);
                property_row(ui, self.t(Text::Record), &row.hit.record_idx.to_string());
                property_row(ui, self.t(Text::Filesystem), row.fs_type.as_str());
                property_row(ui, self.t(Text::Size), &format_size(row.size));
                property_row(ui, self.t(Text::Modified), &format_time(row.mtime));
                property_row(
                    ui,
                    self.t(Text::Directory),
                    yes_no(self.language, row.is_dir),
                );
                property_row(
                    ui,
                    self.t(Text::Symlink),
                    yes_no(self.language, row.is_symlink),
                );
                property_row(
                    ui,
                    self.t(Text::Mounted),
                    yes_no(self.language, row.mounted),
                );
                property_row(
                    ui,
                    self.t(Text::LastIndexed),
                    &format_time(row.last_indexed_time),
                );
                if let Some(index) = self
                    .indexes
                    .iter()
                    .find(|index| index.device_id == row.hit.device_id)
                    && let Some(state) = &index.state
                {
                    if let Some(scanner) = &state.last_scanner {
                        property_row(ui, self.t(Text::Scanner), scanner);
                    }
                    if let Some(error) = &state.last_error {
                        property_row(ui, self.t(Text::LastError), error);
                    }
                    if let Some(stale) = &state.stale_reason {
                        property_row(ui, self.t(Text::Stale), stale);
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
    language: ResolvedLanguage,
}

impl ServiceErrorApp {
    fn new(cc: &eframe::CreationContext<'_>, err: anyhow::Error) -> Self {
        let config = load_config().unwrap_or_default();
        let (language, _) = apply_gui_preferences(&cc.egui_ctx, &config);
        Self {
            message: format!("{err:#}\n\n{}", tr(language, Text::ServiceFallbackHint)),
            language,
        }
    }
}

impl eframe::App for ServiceErrorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(72.0);
                ui.heading(tr(self.language, Text::ServiceUnavailable));
                ui.label(&self.message);
            });
        });
    }
}

impl OxidexApp {
    fn t(&self, key: Text) -> &'static str {
        tr(self.language, key)
    }

    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        tracing::debug!("initializing standalone GUI state");
        let config = load_config().unwrap_or_default();
        let (language, cjk_font_status) = apply_gui_preferences(&cc.egui_ctx, &config);
        let devices = list_known_devices().unwrap_or_default();
        let indexes = snapshot::load_all_indexes().unwrap_or_default();
        tracing::debug!(
            devices = devices.len(),
            indexes = indexes.len(),
            "loaded standalone state"
        );
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
            show_settings: false,
            properties_hit: None,
            last_scan_errors: HashMap::new(),
            scan_job: None,
            language_mode: config.ui.language,
            language,
            cjk_font_fallback: config.ui.cjk_font_fallback,
            cjk_preferred_font: config.ui.cjk_preferred_font.clone(),
            cjk_font_status,
        };
        app.status = format!(
            "{}: {}.",
            app.t(Text::LoadedPersistedIndexes),
            app.indexes.len(),
        );
        app.recompute_hits();
        app
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
                self.status = format!("{}: {err}", self.t(Text::SearchError));
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
        tracing::debug!(
            query = %self.query,
            hits = self.hits.len(),
            "standalone search recomputed"
        );
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
        self.devices.iter().find(|dev| dev.device_id == device_id)
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
            self.status = self.t(Text::NoResultSelected).into();
            return;
        };
        let Some(device) = self.device_by_id(&idx.metadata.device_id) else {
            self.status = self.t(Text::DeviceNotAttached).into();
            return;
        };
        if !device.mounted || device.primary_mount_point.is_empty() {
            self.status = self.t(Text::IndexedDeviceUnmounted).into();
            return;
        }
        let path = mounted_path(idx, rec_idx, device);
        match open::that(&path) {
            Ok(()) => self.status = format!("{} {}", self.t(Text::Opened), path.display()),
            Err(err) => {
                self.status = format!("{} {}: {err}", self.t(Text::FailedToOpen), path.display())
            }
        }
    }

    fn open_selected_location(&mut self) {
        let Some((idx, rec_idx)) = self.selected_index_record() else {
            self.status = self.t(Text::NoResultSelected).into();
            return;
        };
        let Some(device) = self.device_by_id(&idx.metadata.device_id) else {
            self.status = self.t(Text::DeviceNotAttached).into();
            return;
        };
        if !device.mounted || device.primary_mount_point.is_empty() {
            self.status = self.t(Text::IndexedDeviceUnmounted).into();
            return;
        }
        let mut path = PathBuf::from(&device.primary_mount_point);
        let internal_dir = idx.internal_dir(rec_idx).trim_start_matches('/');
        if !internal_dir.is_empty() {
            path.push(internal_dir);
        }
        match open::that(&path) {
            Ok(()) => self.status = format!("{} {}", self.t(Text::Opened), path.display()),
            Err(err) => {
                self.status = format!("{} {}: {err}", self.t(Text::FailedToOpen), path.display())
            }
        }
    }

    fn copy_selected_names(&mut self, ctx: &egui::Context) {
        let Some((idx, rec_idx)) = self.selected_index_record() else {
            self.status = self.t(Text::NoResultSelected).into();
            return;
        };
        let name = idx.name(rec_idx).to_owned();
        ctx.copy_text(name);
        self.status = self.t(Text::CopiedFileName).into();
    }

    fn copy_selected_paths(&mut self, ctx: &egui::Context) {
        let Some((idx, rec_idx)) = self.selected_index_record() else {
            self.status = self.t(Text::NoResultSelected).into();
            return;
        };
        let (mounted, mp) = self
            .device_by_id(&idx.metadata.device_id)
            .map(|dev| (dev.mounted, dev.primary_mount_point.as_str()))
            .unwrap_or((false, ""));
        ctx.copy_text(idx.display_path(rec_idx, mounted, mp));
        self.status = self.t(Text::CopiedFullPath).into();
    }

    fn start_scan(&mut self, device: DeviceInfo) {
        if self.scan_job.is_some() {
            self.status = self.t(Text::AnotherIndexingJobRunning).into();
            return;
        }
        self.status = "Scanning requires oxidexd and oxidex-scannerd. Start Oxidex normally or use oxidex-cli scan.".into();
        tracing::info!(
            device_id = %device.device_id,
            fs_type = %device.fs_type_name,
            dev_node = %device.dev_node,
            "standalone scan blocked; daemon scanner is required"
        );
    }

    fn cancel_scan(&mut self) {
        if let Some(job) = &self.scan_job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = format!("{} {}...", self.t(Text::Cancelling), job.device_id);
        }
    }

    fn forget_index(&mut self, device_id: &str) {
        self.indexes
            .retain(|idx| idx.metadata.device_id != device_id);
        if let Err(err) = snapshot::delete_index(device_id) {
            self.status = format!(
                "{}: {err:#}",
                self.t(Text::RemovedInMemoryButSnapshotDeleteFailed)
            );
        } else {
            self.status = format!("{} {device_id}.", self.t(Text::Forgot));
        }
        if self.selected_scope == device_id {
            self.selected_scope.clear();
        }
        self.recompute_hits();
    }
}

impl eframe::App for OxidexApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ime_frame = current_ime_frame(&ctx);
        self.handle_keyboard(&ctx);
        if self.scan_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::Panel::top("top").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.t(Text::Scope));
                let scopes: Vec<(String, String)> = self
                    .indexes
                    .iter()
                    .map(|idx| (idx.metadata.device_id.clone(), device_label(&idx.metadata)))
                    .collect();
                let mut scope_changed = false;
                let all_indexed_label = self.t(Text::AllIndexedDevices);
                egui::ComboBox::from_id_salt("scope")
                    .selected_text(scope_label(
                        self.language,
                        &self.selected_scope,
                        &self.indexes,
                    ))
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut self.selected_scope,
                                String::new(),
                                all_indexed_label,
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

                if ui.button(self.t(Text::IndexManager)).clicked() {
                    self.show_index_manager = true;
                    self.refresh_devices();
                }

                if ui.button(self.t(Text::Settings)).clicked() {
                    self.show_settings = true;
                }

                if ui
                    .selectable_label(self.show_filters, self.t(Text::Filters))
                    .clicked()
                {
                    self.show_filters = !self.show_filters;
                }

                let search_hint = self.t(Text::SearchHint);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .id_salt("standalone-search-query")
                        .hint_text(search_hint)
                        .desired_width(f32::INFINITY),
                );
                if response.changed() && !ime_frame.active_preedit {
                    self.recompute_hits();
                }
            });
            if self.show_filters {
                ui.separator();
                self.filter_panel(ui, &ime_frame);
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
                    "{} {}",
                    self.hits.len(),
                    self.t(Text::ObjectsFound)
                ));
                ui.separator();
                ui.label(&self.status);
                if let Some(job) = &self.scan_job {
                    ui.separator();
                    ui.add(
                        egui::ProgressBar::new(job.progress as f32 / 100.0).desired_width(120.0),
                    );
                    if ui.button(self.t(Text::Cancel)).clicked() {
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
        if self.show_settings {
            self.settings_window(&ctx);
        }
        self.properties_window(&ctx);
    }
}

impl OxidexApp {
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.open_selected();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.properties_hit.is_some() {
                self.properties_hit = None;
            } else if self.show_settings {
                self.show_settings = false;
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

    fn filter_panel(&mut self, ui: &mut egui::Ui, ime_frame: &ImeFrame) {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            let extension_label = self.t(Text::Extension);
            let type_label = self.t(Text::Type);
            let all_label = self.t(Text::All);
            let files_label = self.t(Text::Files);
            let folders_label = self.t(Text::Folders);
            let symlinks_label = self.t(Text::Symlinks);
            let path_label = self.t(Text::Path);
            let clear_filters_label = self.t(Text::ClearFilters);
            ui.label(extension_label);
            let extensions_response = ui.add(
                egui::TextEdit::singleline(&mut self.filter_extensions)
                    .id_salt("standalone-filter-extensions")
                    .hint_text("rs,txt")
                    .desired_width(120.0),
            );
            changed |= extensions_response.changed();

            ui.label(type_label);
            egui::ComboBox::from_id_salt("type-filter")
                .selected_text(file_type_label(self.language, self.filter_type))
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut self.filter_type, None, all_label)
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut self.filter_type,
                            Some(SearchFileType::File),
                            files_label,
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut self.filter_type,
                            Some(SearchFileType::Dir),
                            folders_label,
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut self.filter_type,
                            Some(SearchFileType::Symlink),
                            symlinks_label,
                        )
                        .changed();
                });

            ui.label(path_label);
            let path_response = ui.add(
                egui::TextEdit::singleline(&mut self.filter_path)
                    .id_salt("standalone-filter-path")
                    .hint_text("src")
                    .desired_width(180.0),
            );
            changed |= path_response.changed();

            if ui.button(clear_filters_label).clicked() {
                self.filter_extensions.clear();
                self.filter_path.clear();
                self.filter_type = None;
                changed = true;
            }
        });

        if changed && !ime_frame.active_preedit {
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
        Some(format!(
            "{}: {}",
            self.t(Text::ActiveFilters),
            parts.join("  ")
        ))
    }

    fn results_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let has_selection = self.selected_index_record().is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::Open)))
                .clicked()
            {
                self.open_selected();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::OpenFolder)))
                .clicked()
            {
                self.open_selected_location();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::CopyName)))
                .clicked()
            {
                self.copy_selected_names(ctx);
            }
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::CopyPath)))
                .clicked()
            {
                self.copy_selected_paths(ctx);
            }
            if ui
                .add_enabled(has_selection, egui::Button::new(self.t(Text::Properties)))
                .clicked()
            {
                self.properties_hit = self.selected_hit.clone();
            }
            ui.separator();
            let name_label = self.t(Text::Name);
            let path_label = self.t(Text::Path);
            let size_label = self.t(Text::Size);
            let date_label = self.t(Text::Date);
            let mut sort_changed = false;
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Name,
                name_label,
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Path,
                path_label,
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Size,
                size_label,
            );
            sort_changed |= sort_button(
                ui,
                &mut self.sort_key,
                &mut self.sort_direction,
                SortKey::Mtime,
                date_label,
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
                ui.heading(self.t(Text::NoIndexesYet));
                ui.label(self.t(Text::OpenIndexManagerHint));
            });
            return;
        }
        if self.hits.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading(self.t(Text::NoResults));
                ui.label(self.t(Text::TryDifferentSearchHint));
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
                    ui.strong(self.t(Text::Name));
                });
                row.col(|ui| {
                    ui.strong(self.t(Text::Path));
                });
                row.col(|ui| {
                    ui.strong(self.t(Text::Size));
                });
                row.col(|ui| {
                    ui.strong(self.t(Text::Modified));
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
                        result_cell_label(ui, name);
                    });
                    row.col(|ui| {
                        result_cell_label(ui, path);
                    });
                    row.col(|ui| {
                        result_cell_label(ui, size);
                    });
                    row.col(|ui| {
                        result_cell_label(ui, modified);
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
        let can_rescan = false;
        if ui
            .add_enabled(has_selection, egui::Button::new(self.t(Text::Open)))
            .clicked()
        {
            self.open_selected();
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new(self.t(Text::OpenFolder)))
            .clicked()
        {
            self.open_selected_location();
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new(self.t(Text::CopyName)))
            .clicked()
        {
            self.copy_selected_names(ctx);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new(self.t(Text::CopyPath)))
            .clicked()
        {
            self.copy_selected_paths(ctx);
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(
                has_selection && can_rescan && self.scan_job.is_none(),
                egui::Button::new(self.t(Text::RescanThisDevice)),
            )
            .on_disabled_hover_text("Scanning requires oxidexd and oxidex-scannerd.")
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
                egui::Button::new(self.t(Text::ForgetThisIndex)),
            )
            .clicked()
        {
            if let Some(device_id) = self.selected_hit.as_ref().map(|hit| hit.device_id.clone()) {
                self.forget_index(&device_id);
            }
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new(self.t(Text::Properties)))
            .clicked()
        {
            self.properties_hit = self.selected_hit.clone();
            ui.close();
        }
    }

    fn index_manager(&mut self, ctx: &egui::Context) {
        let mut open = self.show_index_manager;
        egui::Window::new(self.t(Text::Indexes))
            .open(&mut open)
            .default_width(980.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button(self.t(Text::RefreshDevices)).clicked() {
                        self.refresh_devices();
                    }
                    if let Some(job) = &self.scan_job {
                        ui.label(format!(
                            "{} {}: {}%",
                            self.t(Text::IndexingDevice),
                            job.device_id,
                            job.progress
                        ));
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
                                let index = self.index_by_id(&device.device_id);
                                let count = indexed.get(&device.device_id).copied();
                                ui.label(fit(&device_info_label(&device), 28));
                                ui.label(fit(&device.fs_type_name, 7));
                                ui.label(if device.mounted {
                                    self.t(Text::Mounted)
                                } else {
                                    self.t(Text::NotMounted)
                                });
                                ui.label(fit(&device.dev_node, 22));
                                ui.label(match count {
                                    Some(n) => format!("{n} {}", self.t(Text::EntryPlural)),
                                    None => self.t(Text::NotIndexed).to_owned(),
                                });
                                ui.label(
                                    index
                                        .map(|idx| {
                                            format!(
                                                "{} {}",
                                                self.t(Text::IndexedDevice),
                                                format_time(idx.last_indexed_time)
                                            )
                                        })
                                        .unwrap_or_else(|| self.t(Text::NeverIndexed).to_owned()),
                                );
                                if let Some(err) = self.last_scan_errors.get(&device.device_id) {
                                    ui.label(fit(err, 36));
                                } else if let Some(reason) = &device.scan_unavailable_reason {
                                    ui.label(fit(reason, 36));
                                }

                                let busy = self.scan_job.is_some();
                                let scan_label = if count.is_some() {
                                    self.t(Text::Rescan)
                                } else {
                                    self.t(Text::Index)
                                };
                                if ui
                                    .add_enabled(false, egui::Button::new(scan_label))
                                    .on_disabled_hover_text(
                                        "Scanning requires oxidexd and oxidex-scannerd.",
                                    )
                                    .clicked()
                                {
                                    self.start_scan(device.clone());
                                }
                                if count.is_some()
                                    && ui
                                        .add_enabled(
                                            !busy,
                                            egui::Button::new(self.t(Text::ForgetThisIndex)),
                                        )
                                        .clicked()
                                {
                                    self.forget_index(&device.device_id);
                                }
                            });
                            ui.separator();
                        }
                    });
            });
        self.show_index_manager = open;
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new(self.t(Text::Settings))
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.heading(self.t(Text::Appearance));
                ui.horizontal(|ui| {
                    ui.label(self.t(Text::Theme));
                    let mut next_theme = load_config()
                        .map(|config| config.ui.theme)
                        .unwrap_or(ThemeMode::System);
                    egui::ComboBox::from_id_salt("standalone-theme")
                        .selected_text(theme_label(self.language, next_theme))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut next_theme,
                                ThemeMode::System,
                                self.t(Text::System),
                            );
                            ui.selectable_value(
                                &mut next_theme,
                                ThemeMode::Light,
                                self.t(Text::Light),
                            );
                            ui.selectable_value(
                                &mut next_theme,
                                ThemeMode::Dark,
                                self.t(Text::Dark),
                            );
                        });
                    if ui.button(self.t(Text::Apply)).clicked() {
                        self.apply_standalone_theme(ctx, next_theme);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(self.t(Text::Language));
                    let mut next_language = self.language_mode;
                    egui::ComboBox::from_id_salt("standalone-language")
                        .selected_text(language_label(self.language, self.language_mode))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut next_language,
                                LanguageMode::System,
                                self.t(Text::System),
                            );
                            ui.selectable_value(
                                &mut next_language,
                                LanguageMode::EnUs,
                                self.t(Text::English),
                            );
                            ui.selectable_value(
                                &mut next_language,
                                LanguageMode::ZhCn,
                                self.t(Text::SimplifiedChinese),
                            );
                        });
                    if next_language != self.language_mode {
                        self.apply_standalone_language(next_language);
                    }
                });
                ui.label(self.t(Text::CjkFonts));
                ui.horizontal(|ui| {
                    let cjk_fallback_label = self.t(Text::CjkFontFallback);
                    let cjk_preferred_label = self.t(Text::CjkPreferredFont);
                    let apply_label = self.t(Text::Apply);
                    ui.checkbox(&mut self.cjk_font_fallback, cjk_fallback_label);
                    ui.label(cjk_preferred_label);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.cjk_preferred_font)
                            .id_salt("standalone-cjk-preferred-font")
                            .desired_width(180.0),
                    );
                    if ui.button(apply_label).clicked() {
                        self.apply_standalone_cjk_fonts(ctx);
                    }
                });
                ui.label(if self.cjk_font_status.unavailable() {
                    self.t(Text::CjkFontsMissing)
                } else if self.cjk_font_status.loaded_fonts.is_empty() {
                    self.t(Text::CjkFontFallback)
                } else {
                    self.t(Text::CjkFontsLoaded)
                });
                if !self.cjk_font_status.loaded_fonts.is_empty() {
                    ui.small(self.cjk_font_status.loaded_fonts.join(", "));
                }
            });
        self.show_settings = open;
    }

    fn save_ui_config(&mut self, update: impl FnOnce(&mut AppConfig)) -> bool {
        match load_config() {
            Ok(mut config) => {
                update(&mut config);
                match save_config(&config) {
                    Ok(()) => {
                        self.status = self.t(Text::SettingsSaved).into();
                        true
                    }
                    Err(err) => {
                        self.status = format!("{}: {err:#}", self.t(Text::FailedToSaveSettings));
                        false
                    }
                }
            }
            Err(err) => {
                self.status = format!("{}: {err:#}", self.t(Text::FailedToReadConfig));
                false
            }
        }
    }

    fn apply_standalone_theme(&mut self, ctx: &egui::Context, theme: ThemeMode) {
        if !self.save_ui_config(|config| {
            config.ui.theme = theme;
        }) {
            return;
        }
        apply_theme(ctx, theme);
        self.status = format!(
            "{}: {}.",
            self.t(Text::ThemeSet),
            theme_label(self.language, theme)
        );
    }

    fn apply_standalone_language(&mut self, language_mode: LanguageMode) {
        if !self.save_ui_config(|config| {
            config.ui.language = language_mode;
        }) {
            return;
        }
        self.language_mode = language_mode;
        self.language = system_language(language_mode);
        self.status = format!(
            "{}: {}",
            self.t(Text::Language),
            language_label(self.language, language_mode)
        );
    }

    fn apply_standalone_cjk_fonts(&mut self, ctx: &egui::Context) {
        let preferred = self.cjk_preferred_font.trim().to_owned();
        let fallback = self.cjk_font_fallback;
        if !self.save_ui_config(|config| {
            config.ui.cjk_font_fallback = fallback;
            config.ui.cjk_preferred_font = preferred.clone();
        }) {
            return;
        }
        self.cjk_preferred_font = preferred;
        self.cjk_font_status =
            fonts::configure_fonts(ctx, self.cjk_font_fallback, &self.cjk_preferred_font);
        self.status = self.t(Text::CjkFontsLoaded).into();
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

        egui::Window::new(self.t(Text::Properties))
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                property_row(ui, self.t(Text::Name), idx.name(rec_idx));
                property_row(
                    ui,
                    self.t(Text::Path),
                    &idx.display_path(rec_idx, mounted, mp),
                );
                property_row(ui, self.t(Text::InternalPath), idx.internal_path(rec_idx));
                property_row(ui, self.t(Text::Device), &device_label(&idx.metadata));
                property_row(ui, self.t(Text::DeviceId), &idx.metadata.device_id);
                property_row(ui, self.t(Text::Filesystem), idx.metadata.fs_type.as_str());
                property_row(ui, self.t(Text::Size), &format_size(rec.size));
                property_row(ui, self.t(Text::Modified), &format_time(rec.mtime));
                property_row(
                    ui,
                    self.t(Text::Directory),
                    yes_no(self.language, rec.is_dir()),
                );
                property_row(
                    ui,
                    self.t(Text::Symlink),
                    yes_no(self.language, rec.is_symlink()),
                );
                property_row(ui, self.t(Text::Mounted), yes_no(self.language, mounted));
                property_row(
                    ui,
                    self.t(Text::LastIndexed),
                    &format_time(idx.last_indexed_time),
                );
            });

        if !open {
            self.properties_hit = None;
        }
    }
}

fn connect_or_start_daemon(
    debug_mode: bool,
    log_level: Option<&str>,
) -> anyhow::Result<OxidexClient> {
    match OxidexClient::connect_default() {
        Ok(client) => {
            tracing::debug!("connected to existing oxidexd");
            Ok(client)
        }
        Err(first_err) => {
            tracing::warn!(error = %first_err, "oxidexd socket unavailable; attempting auto-start");
            start_daemon_once(debug_mode, log_level)?;
            for _ in 0..20 {
                if let Ok(client) = OxidexClient::connect_default() {
                    tracing::debug!("connected to auto-started oxidexd");
                    return Ok(client);
                }
                thread::sleep(Duration::from_millis(100));
            }
            anyhow::bail!("failed to connect after attempting to start oxidexd: {first_err:#}")
        }
    }
}

fn start_daemon_once(debug_mode: bool, log_level: Option<&str>) -> anyhow::Result<()> {
    let binary = sibling_binary("oxidexd");
    tracing::debug!(
        binary = %binary.display(),
        debug_mode,
        ?log_level,
        "auto-starting oxidexd"
    );
    let mut command = Command::new(binary);
    command.arg("--foreground");
    if debug_mode {
        command.arg("--debug");
    }
    if let Some(level) = log_level {
        command.arg("--log-level").arg(level);
    }
    command.stdin(Stdio::null());
    if !debug_mode {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    command.spawn()?;
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

fn mounted_path(index: &SearchIndex, rec_idx: u32, device: &DeviceInfo) -> PathBuf {
    let mut path = PathBuf::from(&device.primary_mount_point);
    let internal = index.internal_path(rec_idx).trim_start_matches('/');
    if !internal.is_empty() {
        path.push(internal);
    }
    path
}

fn apply_gui_preferences(
    ctx: &egui::Context,
    config: &AppConfig,
) -> (ResolvedLanguage, CjkFontStatus) {
    let status = fonts::configure_fonts(
        ctx,
        config.ui.cjk_font_fallback,
        &config.ui.cjk_preferred_font,
    );
    apply_theme(ctx, config.ui.theme);
    (system_language(config.ui.language), status)
}

fn apply_theme(ctx: &egui::Context, theme: ThemeMode) {
    match theme {
        ThemeMode::System => {}
        ThemeMode::Light => ctx.set_visuals(egui::Visuals::light()),
        ThemeMode::Dark => ctx.set_visuals(egui::Visuals::dark()),
    }
}

fn scope_label(language: ResolvedLanguage, scope: &str, indexes: &[SearchIndex]) -> String {
    if scope.is_empty() {
        return tr(language, Text::AllIndexedDevices).into();
    }
    indexes
        .iter()
        .find(|idx| idx.metadata.device_id == scope)
        .map(|idx| device_label(&idx.metadata))
        .unwrap_or_else(|| scope.to_owned())
}

fn daemon_scope_label(language: ResolvedLanguage, scope: &str, indexes: &[IndexSummary]) -> String {
    if scope.is_empty() {
        return tr(language, Text::AllDevices).into();
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

fn device_info_label(device: &DeviceInfo) -> String {
    if !device.label.trim().is_empty() {
        format!("{} ({})", device.label.trim(), device.device_id)
    } else {
        device.device_id.clone()
    }
}

fn device_summary_label(device: &DeviceSummary) -> String {
    if !device.label.trim().is_empty() {
        format!("{} ({})", device.label.trim(), device.device_id)
    } else {
        device.device_id.clone()
    }
}

fn theme_label(language: ResolvedLanguage, theme: ThemeMode) -> &'static str {
    match theme {
        ThemeMode::System => tr(language, Text::System),
        ThemeMode::Light => tr(language, Text::Light),
        ThemeMode::Dark => tr(language, Text::Dark),
    }
}

fn language_label(language: ResolvedLanguage, mode: LanguageMode) -> &'static str {
    match mode {
        LanguageMode::System => tr(language, Text::System),
        LanguageMode::EnUs => tr(language, Text::English),
        LanguageMode::ZhCn => tr(language, Text::SimplifiedChinese),
    }
}

fn file_type_label(language: ResolvedLanguage, file_type: Option<SearchFileType>) -> &'static str {
    match file_type {
        None => tr(language, Text::All),
        Some(SearchFileType::File) => tr(language, Text::Files),
        Some(SearchFileType::Dir) => tr(language, Text::Folders),
        Some(SearchFileType::Symlink) => tr(language, Text::Symlinks),
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

fn result_cell_label(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.add(egui::Label::new(text).selectable(false));
}

fn yes_no(language: ResolvedLanguage, value: bool) -> &'static str {
    match (language, value) {
        (ResolvedLanguage::ZhCn, true) => "是",
        (ResolvedLanguage::ZhCn, false) => "否",
        (_, true) => "yes",
        (_, false) => "no",
    }
}

fn result_word(language: ResolvedLanguage, count: usize) -> &'static str {
    match language {
        ResolvedLanguage::ZhCn => tr(language, Text::ResultPlural),
        ResolvedLanguage::EnUs if count == 1 => tr(language, Text::ResultSingular),
        ResolvedLanguage::EnUs => tr(language, Text::ResultPlural),
    }
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
