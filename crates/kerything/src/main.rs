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
use kerything_core::index::{SearchHit, SearchIndex, merge_search};
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
    selected_scope: String,
    selected_row: Option<usize>,
    sort_key: SortKey,
    sort_direction: SortDirection,
    status: String,
    show_index_manager: bool,
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
            selected_scope: String::new(),
            selected_row: None,
            sort_key: SortKey::Name,
            sort_direction: SortDirection::Asc,
            status: String::new(),
            show_index_manager: false,
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
                            self.indexes
                                .retain(|idx| idx.metadata.device_id != device_id);
                            self.indexes.push(index);
                            self.indexes
                                .sort_by(|a, b| a.metadata.device_id.cmp(&b.metadata.device_id));
                            self.refresh_devices();
                            self.recompute_hits();
                            self.status = format!("Indexed {device_id}.");
                        }
                        Err(err) => {
                            self.status = format!("Indexing failed for {device_id}: {err:#}");
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
        let filter = (!self.selected_scope.is_empty()).then_some(self.selected_scope.as_str());
        self.hits = merge_search(
            &self.indexes,
            filter,
            &self.query,
            self.sort_key,
            self.sort_direction,
        );
        self.selected_row = None;
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

    fn open_selected(&mut self) {
        let Some(row) = self.selected_row else {
            return;
        };
        let Some((idx, rec_idx)) = self.hit_at(row) else {
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
        let Some(row) = self.selected_row else {
            return;
        };
        let Some((idx, rec_idx)) = self.hit_at(row) else {
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
        let Some(row) = self.selected_row else {
            return;
        };
        let Some((idx, rec_idx)) = self.hit_at(row) else {
            return;
        };
        let name = idx.name(rec_idx).to_owned();
        ctx.copy_text(name);
        self.status = "Copied file name.".into();
    }

    fn copy_selected_paths(&mut self, ctx: &egui::Context) {
        let Some(row) = self.selected_row else {
            return;
        };
        let Some((idx, rec_idx)) = self.hit_at(row) else {
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
        if device.metadata.fs_type == FsType::Btrfs {
            self.status = "Btrfs scanning is planned for v2; this Rust v1 helper exposes the type but does not parse Btrfs yet.".into();
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

                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search files...")
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    self.recompute_hits();
                }

                if ui.button("Indexes").clicked() {
                    self.show_index_manager = true;
                    self.refresh_devices();
                }
            });
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
    }
}

impl KerythingApp {
    fn results_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if ui.button("Open").clicked() {
                self.open_selected();
            }
            if ui.button("Open Folder").clicked() {
                self.open_selected_location();
            }
            if ui.button("Copy Name").clicked() {
                self.copy_selected_names(ctx);
            }
            if ui.button("Copy Path").clicked() {
                self.copy_selected_paths(ctx);
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
        let row_height = 24.0;
        TableBuilder::new(ui)
            .id_salt("results")
            .striped(true)
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
                    let selected = self.selected_row == Some(row_index);
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
                    if response.double_clicked() {
                        self.selected_row = Some(row_index);
                        self.open_selected();
                    } else if response.clicked() {
                        self.selected_row = Some(row_index);
                    }
                });
            });
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
                                let count = indexed.get(&meta.device_id).copied();
                                ui.label(fit(&device_label(meta), 28));
                                ui.label(fit(meta.fs_type.as_str(), 6));
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

                                let busy = self.scan_job.is_some();
                                let scan_label = if count.is_some() { "Rescan" } else { "Index" };
                                if ui
                                    .add_enabled(
                                        !busy && meta.fs_type.is_supported_v1(),
                                        egui::Button::new(scan_label),
                                    )
                                    .clicked()
                                {
                                    self.start_scan(device.clone());
                                }
                                if !meta.fs_type.is_supported_v1() {
                                    ui.label("v2");
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
    thread::spawn(move || {
        let reader = std::io::BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(rest) = line.trim().strip_prefix("KERYTHING_PROGRESS ")
                && let Ok(percent) = rest.trim().parse::<u8>()
            {
                let _ = tx.send(AppEvent::ScanProgress {
                    device_id: device_id.clone(),
                    percent: percent.min(100),
                });
            }
        }
    });

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
        }
        if let Some(status) = child.try_wait()? {
            let output = stdout_handle
                .join()
                .map_err(|_| anyhow::anyhow!("helper stdout reader panicked"))??;
            anyhow::ensure!(
                status.success(),
                "scanner helper failed with status {status}"
            );
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
