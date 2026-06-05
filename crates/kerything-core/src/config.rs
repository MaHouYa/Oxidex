use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{SortDirection, SortKey};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AppConfig {
    pub version: u32,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub indexing: IndexingConfig,
    #[serde(default)]
    pub rofi: RofiConfig,
    #[serde(default)]
    pub devices: Vec<DeviceConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct UiConfig {
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default = "default_true")]
    pub show_filter_panel: bool,
    #[serde(default = "default_true")]
    pub remember_window_size: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct SearchConfig {
    #[serde(default = "default_sort")]
    pub default_sort: SortKey,
    #[serde(default)]
    pub default_direction: SortDirection,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct IndexingConfig {
    #[serde(default)]
    pub scan_on_startup: bool,
    #[serde(default)]
    pub auto_rescan_removable: bool,
    #[serde(default = "default_max_parallel_scans")]
    pub max_parallel_scans: usize,
    #[serde(default = "default_true")]
    pub watch_mounted: bool,
    #[serde(default = "default_live_update_flush_seconds")]
    pub live_update_flush_seconds: u64,
    #[serde(default = "default_live_update_max_dirty_seconds")]
    pub live_update_max_dirty_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct RofiConfig {
    #[serde(default = "default_rofi_max_results")]
    pub max_results: usize,
    #[serde(default = "default_true")]
    pub show_device_label: bool,
    #[serde(default)]
    pub show_full_path: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct DeviceConfig {
    pub device_id: String,
    pub enabled: bool,
    pub display_name: Option<String>,
    #[serde(default)]
    pub rules: Vec<PathRule>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct PathRule {
    pub kind: RuleKind,
    pub pattern: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Include,
    Exclude,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            ui: UiConfig::default(),
            search: SearchConfig::default(),
            indexing: IndexingConfig::default(),
            rofi: RofiConfig::default(),
            devices: Vec::new(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            show_filter_panel: true,
            remember_window_size: true,
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_sort: default_sort(),
            default_direction: SortDirection::Asc,
            max_results: default_max_results(),
        }
    }
}

impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            scan_on_startup: false,
            auto_rescan_removable: false,
            max_parallel_scans: default_max_parallel_scans(),
            watch_mounted: true,
            live_update_flush_seconds: default_live_update_flush_seconds(),
            live_update_max_dirty_seconds: default_live_update_max_dirty_seconds(),
        }
    }
}

impl Default for RofiConfig {
    fn default() -> Self {
        Self {
            max_results: default_rofi_max_results(),
            show_device_label: true,
            show_full_path: false,
        }
    }
}

impl DeviceConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

pub fn config_path() -> anyhow::Result<PathBuf> {
    let dirs = xdg::BaseDirectories::with_prefix("kerything");
    dirs.get_config_file("config.toml")
        .ok_or_else(|| anyhow::anyhow!("unable to resolve XDG config directory"))
}

pub fn load_config() -> anyhow::Result<AppConfig> {
    load_config_from_path(&config_path()?)
}

pub fn load_config_from_path(path: &Path) -> anyhow::Result<AppConfig> {
    if !path.exists() {
        let config = AppConfig::default();
        save_config_to_path(path, &config)?;
        return Ok(config);
    }

    let text = fs::read_to_string(path)?;
    let config: AppConfig = toml::from_str(&text)?;
    validate_config(&config)?;
    Ok(config)
}

pub fn save_config(config: &AppConfig) -> anyhow::Result<()> {
    save_config_to_path(&config_path()?, config)
}

pub fn save_config_to_path(path: &Path, config: &AppConfig) -> anyhow::Result<()> {
    validate_config(config)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(config)?;
    fs::write(path, text)?;
    Ok(())
}

pub fn validate_config(config: &AppConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        config.version == 1,
        "unsupported config version {}",
        config.version
    );
    anyhow::ensure!(
        config.search.max_results > 0,
        "search.max_results must be greater than zero"
    );
    anyhow::ensure!(
        config.indexing.max_parallel_scans > 0,
        "indexing.max_parallel_scans must be greater than zero"
    );
    anyhow::ensure!(
        config.indexing.live_update_flush_seconds > 0,
        "indexing.live_update_flush_seconds must be greater than zero"
    );
    anyhow::ensure!(
        config.indexing.live_update_max_dirty_seconds >= config.indexing.live_update_flush_seconds,
        "indexing.live_update_max_dirty_seconds must be at least live_update_flush_seconds"
    );
    anyhow::ensure!(
        config.rofi.max_results > 0,
        "rofi.max_results must be greater than zero"
    );

    for device in &config.devices {
        anyhow::ensure!(
            !device.device_id.trim().is_empty(),
            "device_id must not be empty"
        );
        for rule in &device.rules {
            anyhow::ensure!(
                !rule.pattern.trim().is_empty(),
                "rule pattern must not be empty"
            );
        }
    }
    Ok(())
}

fn default_true() -> bool {
    true
}

fn default_sort() -> SortKey {
    SortKey::Relevance
}

fn default_max_results() -> usize {
    20_000
}

fn default_max_parallel_scans() -> usize {
    1
}

fn default_live_update_flush_seconds() -> u64 {
    10
}

fn default_live_update_max_dirty_seconds() -> u64 {
    60
}

fn default_rofi_max_results() -> usize {
    200
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_v3_defaults() {
        let config = AppConfig::default();
        assert_eq!(config.version, 1);
        assert_eq!(config.ui.theme, ThemeMode::System);
        assert_eq!(config.search.default_sort, SortKey::Relevance);
        assert_eq!(config.search.default_direction, SortDirection::Asc);
        assert_eq!(config.search.max_results, 20_000);
        assert_eq!(config.indexing.max_parallel_scans, 1);
        assert!(config.indexing.watch_mounted);
        assert_eq!(config.rofi.max_results, 200);
    }

    #[test]
    fn toml_roundtrip() {
        let config = AppConfig {
            devices: vec![DeviceConfig {
                device_id: "partuuid:abc".into(),
                enabled: true,
                display_name: Some("Main Linux".into()),
                rules: vec![PathRule {
                    kind: RuleKind::Exclude,
                    pattern: "/var/cache/**".into(),
                }],
            }],
            ..AppConfig::default()
        };

        let text = toml::to_string_pretty(&config).unwrap();
        let decoded: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(decoded, config);
        validate_config(&decoded).unwrap();
    }

    #[test]
    fn invalid_version_is_rejected() {
        let mut config = AppConfig::default();
        config.version = 2;
        assert!(validate_config(&config).is_err());
    }
}
