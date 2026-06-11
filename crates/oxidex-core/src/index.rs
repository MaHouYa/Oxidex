use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::model::{DeviceMetadata, ROOT_PARENT, ScanDatabase, SortDirection, SortKey};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexedRecord {
    pub parent: u32,
    pub size: u64,
    pub mtime: i64,
    pub name_offset: u32,
    pub name_len: u32,
    pub folded_offset: u32,
    pub folded_len: u32,
    pub flags: u8,
}

impl IndexedRecord {
    pub fn is_dir(&self) -> bool {
        self.flags & crate::model::FLAG_IS_DIR != 0
    }

    pub fn is_symlink(&self) -> bool {
        self.flags & crate::model::FLAG_IS_SYMLINK != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct TrigramEntry {
    pub trigram: u32,
    pub record_idx: u32,
}

impl Ord for TrigramEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.trigram
            .cmp(&other.trigram)
            .then_with(|| self.record_idx.cmp(&other.record_idx))
    }
}

impl PartialOrd for TrigramEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchIndex {
    pub metadata: DeviceMetadata,
    pub generation: u64,
    pub last_indexed_time: i64,
    pub records: Vec<IndexedRecord>,
    pub string_pool: Vec<u8>,
    pub folded_pool: Vec<u8>,
    pub internal_paths: Vec<String>,
    pub flat_index: Vec<TrigramEntry>,
    pub order_by_name: Vec<u32>,
    pub order_by_path: Vec<u32>,
    pub order_by_size: Vec<u32>,
    pub order_by_mtime: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchHit {
    pub device_id: String,
    pub record_idx: u32,
}

impl PartialEq for SearchHit {
    fn eq(&self, other: &Self) -> bool {
        self.device_id == other.device_id && self.record_idx == other.record_idx
    }
}

impl Eq for SearchHit {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveRecordMetadata {
    pub size: u64,
    pub mtime: i64,
    pub is_dir: bool,
    pub is_symlink: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveUpdateEvent {
    Created {
        internal_path: String,
        metadata: LiveRecordMetadata,
    },
    Removed {
        internal_path: String,
    },
    Renamed {
        old_internal_path: String,
        new_internal_path: String,
        metadata: LiveRecordMetadata,
    },
    Metadata {
        internal_path: String,
        metadata: LiveRecordMetadata,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchFileType {
    File,
    Dir,
    Symlink,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct SearchFilters {
    pub extensions: Vec<String>,
    pub file_type: Option<SearchFileType>,
    pub path_contains: Vec<String>,
}

impl SearchFilters {
    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty() && self.file_type.is_none() && self.path_contains.is_empty()
    }

    pub fn add_extensions(&mut self, value: &str) {
        for ext in value.split(',') {
            let ext = normalize_extension(ext);
            if !ext.is_empty() && !self.extensions.iter().any(|existing| existing == &ext) {
                self.extensions.push(ext);
            }
        }
    }

    pub fn add_path_contains(&mut self, value: &str) {
        let value = value.trim().to_lowercase();
        if !value.is_empty() {
            self.path_contains.push(value);
        }
    }

    pub fn merge(&mut self, other: SearchFilters) {
        for ext in other.extensions {
            if !self.extensions.iter().any(|existing| existing == &ext) {
                self.extensions.push(ext);
            }
        }
        if other.file_type.is_some() {
            self.file_type = other.file_type;
        }
        self.path_contains.extend(other.path_contains);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchClause {
    NameContains(String),
    NamePhrase(String),
    NameWildcard(String),
    Extension(Vec<String>),
    FileType(SearchFileType),
    PathContains(String),
    Size(SizeFilter),
    Mtime(TimeFilter),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SizeFilter {
    pub min: Option<u64>,
    pub max: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct TimeFilter {
    pub min: Option<i64>,
    pub max: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct SearchExplanation {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct SearchRequest {
    pub include: Vec<SearchClause>,
    pub exclude: Vec<SearchClause>,
}

impl SearchRequest {
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    pub fn add_filters(&mut self, filters: SearchFilters) {
        if !filters.extensions.is_empty() {
            self.include
                .push(SearchClause::Extension(filters.extensions.clone()));
        }
        if let Some(file_type) = filters.file_type {
            self.include.push(SearchClause::FileType(file_type));
        }
        for path in filters.path_contains {
            self.include.push(SearchClause::PathContains(path));
        }
    }

    pub fn filters(&self) -> SearchFilters {
        let mut filters = SearchFilters::default();
        for clause in self.include.iter().chain(self.exclude.iter()) {
            match clause {
                SearchClause::Extension(exts) => {
                    for ext in exts {
                        filters.add_extensions(ext);
                    }
                }
                SearchClause::FileType(file_type) => filters.file_type = Some(*file_type),
                SearchClause::PathContains(value) => filters.add_path_contains(value),
                _ => {}
            }
        }
        filters
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchParseError {
    message: String,
}

impl SearchParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SearchParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SearchParseError {}

pub fn parse_search_query(query: &str) -> Result<SearchRequest, SearchParseError> {
    let mut request = SearchRequest::default();
    for token in tokenize_query(query)? {
        if token.value.is_empty() {
            continue;
        }
        let (negated, value) = split_negation(&token.value, token.quoted);
        if value.is_empty() {
            continue;
        }
        let clause = parse_clause(value, token.quoted)?;
        if negated {
            request.exclude.push(clause);
        } else {
            request.include.push(clause);
        }
    }
    Ok(request)
}

pub fn explain_search_query(query: &str) -> Result<SearchExplanation, SearchParseError> {
    let request = parse_search_query(query)?;
    let include: Vec<_> = request.include.iter().map(clause_label).collect();
    let exclude: Vec<_> = request.exclude.iter().map(clause_label).collect();
    let mut parts = Vec::new();
    if !include.is_empty() {
        parts.push(format!("include {}", include.join(", ")));
    }
    if !exclude.is_empty() {
        parts.push(format!("exclude {}", exclude.join(", ")));
    }
    Ok(SearchExplanation {
        include,
        exclude,
        summary: if parts.is_empty() {
            "match everything".to_owned()
        } else {
            parts.join("; ")
        },
    })
}

fn split_negation(value: &str, quoted: bool) -> (bool, &str) {
    if quoted {
        return (false, value);
    }
    if let Some(rest) = value.strip_prefix('!')
        && !rest.is_empty()
    {
        return (true, rest);
    }
    if let Some(rest) = value.strip_prefix('-')
        && !rest.is_empty()
    {
        return (true, rest);
    }
    (false, value)
}

fn parse_clause(value: &str, quoted: bool) -> Result<SearchClause, SearchParseError> {
    if !quoted {
        let lowered = value.to_ascii_lowercase();
        if lowered.starts_with("ext:") {
            let mut filters = SearchFilters::default();
            filters.add_extensions(&value[4..]);
            if filters.extensions.is_empty() {
                return Err(SearchParseError::new(
                    "ext: requires at least one extension",
                ));
            }
            return Ok(SearchClause::Extension(filters.extensions));
        }
        if lowered.starts_with("path:") {
            let needle = value[5..].trim().to_lowercase();
            if needle.is_empty() {
                return Err(SearchParseError::new("path: requires text to match"));
            }
            return Ok(SearchClause::PathContains(needle));
        }
        if lowered.starts_with("type:") {
            return Ok(SearchClause::FileType(parse_file_type(&value[5..])?));
        }
        if lowered.starts_with("size:") {
            return Ok(SearchClause::Size(parse_size_filter(&value[5..])?));
        }
        if lowered.starts_with("mtime:") {
            return Ok(SearchClause::Mtime(parse_time_filter(&value[6..])?));
        }
    }

    let folded = value.to_lowercase();
    if folded.contains('*') {
        Ok(SearchClause::NameWildcard(folded))
    } else if quoted {
        Ok(SearchClause::NamePhrase(folded))
    } else {
        Ok(SearchClause::NameContains(folded))
    }
}

fn clause_label(clause: &SearchClause) -> String {
    match clause {
        SearchClause::NameContains(value) => format!("name contains '{value}'"),
        SearchClause::NamePhrase(value) => format!("name phrase '{value}'"),
        SearchClause::NameWildcard(value) => format!("name wildcard '{value}'"),
        SearchClause::Extension(values) => format!("extension {}", values.join(",")),
        SearchClause::FileType(value) => format!("type {}", search_file_type_name(*value)),
        SearchClause::PathContains(value) => format!("path contains '{value}'"),
        SearchClause::Size(value) => format!("size {}", range_label(value.min, value.max)),
        SearchClause::Mtime(value) => format!("mtime {}", range_label(value.min, value.max)),
    }
}

fn range_label<T: std::fmt::Display>(min: Option<T>, max: Option<T>) -> String {
    match (min, max) {
        (Some(min), Some(max)) => format!("{min}..{max}"),
        (Some(min), None) => format!(">={min}"),
        (None, Some(max)) => format!("<={max}"),
        (None, None) => "any".to_owned(),
    }
}

fn search_file_type_name(file_type: SearchFileType) -> &'static str {
    match file_type {
        SearchFileType::File => "file",
        SearchFileType::Dir => "dir",
        SearchFileType::Symlink => "symlink",
    }
}

fn parse_size_filter(value: &str) -> Result<SizeFilter, SearchParseError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(SearchParseError::new("size: requires a value"));
    }
    if let Some((start, end)) = value.split_once("..") {
        return Ok(SizeFilter {
            min: Some(parse_size_value(start)?),
            max: Some(parse_size_value(end)?),
        });
    }
    if let Some(rest) = value.strip_prefix(">=") {
        return Ok(SizeFilter {
            min: Some(parse_size_value(rest)?),
            max: None,
        });
    }
    if let Some(rest) = value.strip_prefix('>') {
        return Ok(SizeFilter {
            min: Some(parse_size_value(rest)?.saturating_add(1)),
            max: None,
        });
    }
    if let Some(rest) = value.strip_prefix("<=") {
        return Ok(SizeFilter {
            min: None,
            max: Some(parse_size_value(rest)?),
        });
    }
    if let Some(rest) = value.strip_prefix('<') {
        return Ok(SizeFilter {
            min: None,
            max: Some(parse_size_value(rest)?.saturating_sub(1)),
        });
    }
    let exact = parse_size_value(value)?;
    Ok(SizeFilter {
        min: Some(exact),
        max: Some(exact),
    })
}

fn parse_size_value(value: &str) -> Result<u64, SearchParseError> {
    let value = value.trim();
    let split = value
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(value.len());
    let number = value[..split]
        .parse::<u64>()
        .map_err(|_| SearchParseError::new(format!("invalid size value '{value}'")))?;
    let unit = value[split..].trim().to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "" | "b" => 1,
        "kb" => 1_000,
        "kib" => 1024,
        "mb" => 1_000_000,
        "mib" => 1024 * 1024,
        "gb" => 1_000_000_000,
        "gib" => 1024 * 1024 * 1024,
        "tb" => 1_000_000_000_000,
        "tib" => 1024_u64.pow(4),
        other => {
            return Err(SearchParseError::new(format!(
                "unsupported size unit '{other}'"
            )));
        }
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| SearchParseError::new(format!("size value '{value}' is too large")))
}

fn parse_time_filter(value: &str) -> Result<TimeFilter, SearchParseError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(SearchParseError::new("mtime: requires a value"));
    }
    if value.eq_ignore_ascii_case("today") {
        let (min, max) = local_day_range(0)?;
        return Ok(TimeFilter {
            min: Some(min),
            max: Some(max),
        });
    }
    if value.eq_ignore_ascii_case("yesterday") {
        let (min, max) = local_day_range(-1)?;
        return Ok(TimeFilter {
            min: Some(min),
            max: Some(max),
        });
    }
    if let Some((start, end)) = value.split_once("..") {
        return Ok(TimeFilter {
            min: Some(parse_date_start(start)?),
            max: Some(parse_date_end(end)?),
        });
    }
    if let Some(rest) = value.strip_prefix(">=") {
        return Ok(TimeFilter {
            min: Some(parse_time_bound(rest, BoundKind::Start)?),
            max: None,
        });
    }
    if let Some(rest) = value.strip_prefix('>') {
        return Ok(TimeFilter {
            min: Some(parse_time_bound(rest, BoundKind::Start)?),
            max: None,
        });
    }
    if let Some(rest) = value.strip_prefix("<=") {
        return Ok(TimeFilter {
            min: None,
            max: Some(parse_time_bound(rest, BoundKind::End)?),
        });
    }
    if let Some(rest) = value.strip_prefix('<') {
        if let Some(days) = parse_relative_days(rest) {
            return Ok(TimeFilter {
                min: Some(now_unix() - days * 86_400),
                max: Some(now_unix()),
            });
        }
        return Ok(TimeFilter {
            min: None,
            max: Some(parse_time_bound(rest, BoundKind::Start)?),
        });
    }
    let (min, max) = date_range(value)?;
    Ok(TimeFilter {
        min: Some(min),
        max: Some(max),
    })
}

#[derive(Clone, Copy)]
enum BoundKind {
    Start,
    End,
}

fn parse_time_bound(value: &str, kind: BoundKind) -> Result<i64, SearchParseError> {
    if let Some(days) = parse_relative_days(value) {
        let boundary = now_unix() - days * 86_400;
        return Ok(boundary);
    }
    match kind {
        BoundKind::Start => parse_date_start(value),
        BoundKind::End => parse_date_end(value),
    }
}

fn parse_relative_days(value: &str) -> Option<i64> {
    let value = value.trim().to_ascii_lowercase();
    let days = value.strip_suffix('d')?.parse::<i64>().ok()?;
    (days >= 0).then_some(days)
}

fn date_range(value: &str) -> Result<(i64, i64), SearchParseError> {
    Ok((parse_date_start(value)?, parse_date_end(value)?))
}

fn parse_date_start(value: &str) -> Result<i64, SearchParseError> {
    date_to_unix(value, Time::MIDNIGHT)
}

fn parse_date_end(value: &str) -> Result<i64, SearchParseError> {
    date_to_unix(
        value,
        Time::from_hms(23, 59, 59).map_err(|err| SearchParseError::new(err.to_string()))?,
    )
}

fn date_to_unix(value: &str, time: Time) -> Result<i64, SearchParseError> {
    let date = parse_date(value)?;
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    Ok(PrimitiveDateTime::new(date, time)
        .assume_offset(offset)
        .unix_timestamp())
}

fn parse_date(value: &str) -> Result<Date, SearchParseError> {
    let mut parts = value.trim().split('-');
    let year = parts
        .next()
        .and_then(|part| part.parse::<i32>().ok())
        .ok_or_else(|| SearchParseError::new(format!("invalid date '{value}'")))?;
    let month = parts
        .next()
        .and_then(|part| part.parse::<u8>().ok())
        .and_then(|month| Month::try_from(month).ok())
        .ok_or_else(|| SearchParseError::new(format!("invalid date '{value}'")))?;
    let day = parts
        .next()
        .and_then(|part| part.parse::<u8>().ok())
        .ok_or_else(|| SearchParseError::new(format!("invalid date '{value}'")))?;
    if parts.next().is_some() {
        return Err(SearchParseError::new(format!("invalid date '{value}'")));
    }
    Date::from_calendar_date(year, month, day)
        .map_err(|_| SearchParseError::new(format!("invalid date '{value}'")))
}

fn local_day_range(day_offset: i64) -> Result<(i64, i64), SearchParseError> {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let date = OffsetDateTime::now_utc()
        .to_offset(offset)
        .date()
        .checked_add(Duration::days(day_offset))
        .ok_or_else(|| SearchParseError::new("date offset is out of range"))?;
    let start = PrimitiveDateTime::new(date, Time::MIDNIGHT)
        .assume_offset(offset)
        .unix_timestamp();
    let end = PrimitiveDateTime::new(
        date,
        Time::from_hms(23, 59, 59).map_err(|err| SearchParseError::new(err.to_string()))?,
    )
    .assume_offset(offset)
    .unix_timestamp();
    Ok((start, end))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

impl SearchIndex {
    pub fn from_scan(
        metadata: DeviceMetadata,
        scan: ScanDatabase,
        last_indexed_time: i64,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            metadata.fs_type == scan.fs_type,
            "scan fs type {} does not match device fs type {}",
            scan.fs_type,
            metadata.fs_type
        );

        let mut folded_pool = Vec::with_capacity(scan.string_pool.len());
        let mut records = Vec::with_capacity(scan.records.len());

        for rec in scan.records {
            let start = rec.name_offset as usize;
            let end = start + rec.name_len as usize;
            anyhow::ensure!(
                end <= scan.string_pool.len(),
                "scan record name out of bounds"
            );

            let name = std::str::from_utf8(&scan.string_pool[start..end])?;
            let folded = name.to_lowercase();
            let folded_offset = u32::try_from(folded_pool.len())?;
            let folded_len = u32::try_from(folded.len())?;
            folded_pool.extend_from_slice(folded.as_bytes());

            records.push(IndexedRecord {
                parent: rec.parent,
                size: rec.size,
                mtime: rec.mtime,
                name_offset: rec.name_offset,
                name_len: rec.name_len,
                folded_offset,
                folded_len,
                flags: rec.flags,
            });
        }

        let mut idx = Self {
            metadata,
            generation: 1,
            last_indexed_time,
            records,
            string_pool: scan.string_pool,
            folded_pool,
            internal_paths: Vec::new(),
            flat_index: Vec::new(),
            order_by_name: Vec::new(),
            order_by_path: Vec::new(),
            order_by_size: Vec::new(),
            order_by_mtime: Vec::new(),
        };
        idx.rebuild_accelerators();
        Ok(idx)
    }

    pub fn rebuild_accelerators(&mut self) {
        self.internal_paths = (0..self.records.len())
            .map(|i| self.build_path_for(i as u32, 0))
            .collect();
        self.flat_index = self.build_trigram_index();
        self.build_sort_orders();
    }

    pub fn name(&self, record_idx: u32) -> &str {
        let rec = &self.records[record_idx as usize];
        let start = rec.name_offset as usize;
        let end = start + rec.name_len as usize;
        std::str::from_utf8(&self.string_pool[start..end]).unwrap_or("")
    }

    pub fn folded_name(&self, record_idx: u32) -> &str {
        let rec = &self.records[record_idx as usize];
        let start = rec.folded_offset as usize;
        let end = start + rec.folded_len as usize;
        std::str::from_utf8(&self.folded_pool[start..end]).unwrap_or("")
    }

    pub fn internal_path(&self, record_idx: u32) -> &str {
        self.internal_paths
            .get(record_idx as usize)
            .map(String::as_str)
            .unwrap_or("/")
    }

    pub fn internal_dir(&self, record_idx: u32) -> &str {
        let rec = &self.records[record_idx as usize];
        if rec.parent == ROOT_PARENT {
            "/"
        } else {
            self.internal_path(rec.parent)
        }
    }

    pub fn record_by_internal_path(&self, internal_path: &str) -> Option<u32> {
        let normalized = normalize_internal_path(internal_path);
        self.internal_paths
            .iter()
            .position(|path| path == &normalized)
            .and_then(|idx| u32::try_from(idx).ok())
    }

    pub fn apply_live_event(&mut self, event: LiveUpdateEvent) -> anyhow::Result<bool> {
        self.apply_live_events(std::iter::once(event))
    }

    pub fn apply_live_events(
        &mut self,
        events: impl IntoIterator<Item = LiveUpdateEvent>,
    ) -> anyhow::Result<bool> {
        let mut changed = false;
        for event in events {
            changed |= self.apply_live_event_without_rebuild(event)?;
        }
        if changed {
            self.generation = self.generation.saturating_add(1);
            self.rebuild_accelerators();
        }
        Ok(changed)
    }

    fn apply_live_event_without_rebuild(&mut self, event: LiveUpdateEvent) -> anyhow::Result<bool> {
        let changed = match event {
            LiveUpdateEvent::Created {
                internal_path,
                metadata,
            } => self.add_live_record(&internal_path, &metadata)?,
            LiveUpdateEvent::Removed { internal_path } => self
                .record_by_internal_path(&internal_path)
                .map(|idx| self.remove_subtree(idx))
                .transpose()?
                .unwrap_or(false),
            LiveUpdateEvent::Renamed {
                old_internal_path,
                new_internal_path,
                metadata,
            } => {
                if let Some(idx) = self.record_by_internal_path(&old_internal_path) {
                    self.rename_live_record(idx, &new_internal_path, &metadata)?
                } else {
                    self.add_live_record(&new_internal_path, &metadata)?
                }
            }
            LiveUpdateEvent::Metadata {
                internal_path,
                metadata,
            } => self
                .record_by_internal_path(&internal_path)
                .map(|idx| self.refresh_record_metadata(idx, &metadata))
                .unwrap_or(false),
        };
        Ok(changed)
    }

    pub fn remove_subtree(&mut self, record_idx: u32) -> anyhow::Result<bool> {
        if (record_idx as usize) >= self.records.len() {
            return Ok(false);
        }
        let mut keep = vec![true; self.records.len()];
        let mut stack = vec![record_idx];
        while let Some(idx) = stack.pop() {
            if (idx as usize) >= keep.len() || !keep[idx as usize] {
                continue;
            }
            keep[idx as usize] = false;
            for (child_idx, rec) in self.records.iter().enumerate() {
                if rec.parent == idx {
                    stack.push(child_idx as u32);
                }
            }
        }
        self.rebuild_from_keep(&keep)?;
        Ok(true)
    }

    pub fn refresh_record_metadata(
        &mut self,
        record_idx: u32,
        metadata: &LiveRecordMetadata,
    ) -> bool {
        let Some(record) = self.records.get_mut(record_idx as usize) else {
            return false;
        };
        record.size = metadata.size;
        record.mtime = metadata.mtime;
        record.flags = live_flags(metadata);
        true
    }

    fn add_live_record(
        &mut self,
        internal_path: &str,
        metadata: &LiveRecordMetadata,
    ) -> anyhow::Result<bool> {
        let internal_path = normalize_internal_path(internal_path);
        if let Some(idx) = self.record_by_internal_path(&internal_path) {
            return Ok(self.refresh_record_metadata(idx, metadata));
        }
        let (parent_path, name) = split_internal_path(&internal_path);
        if name.is_empty() {
            return Ok(false);
        }
        let parent = self
            .record_by_internal_path(parent_path)
            .unwrap_or(ROOT_PARENT);
        self.push_indexed_record(parent, name, metadata)?;
        Ok(true)
    }

    fn rename_live_record(
        &mut self,
        record_idx: u32,
        new_internal_path: &str,
        metadata: &LiveRecordMetadata,
    ) -> anyhow::Result<bool> {
        let new_internal_path = normalize_internal_path(new_internal_path);
        let (parent_path, name) = split_internal_path(&new_internal_path);
        if name.is_empty() {
            return Ok(false);
        }
        let parent = self
            .record_by_internal_path(parent_path)
            .unwrap_or(ROOT_PARENT);
        let name_offset = u32::try_from(self.string_pool.len())?;
        let name_len = u32::try_from(name.len())?;
        self.string_pool.extend_from_slice(name.as_bytes());
        let folded = name.to_lowercase();
        let folded_offset = u32::try_from(self.folded_pool.len())?;
        let folded_len = u32::try_from(folded.len())?;
        self.folded_pool.extend_from_slice(folded.as_bytes());

        let Some(record) = self.records.get_mut(record_idx as usize) else {
            return Ok(false);
        };
        record.parent = parent;
        record.name_offset = name_offset;
        record.name_len = name_len;
        record.folded_offset = folded_offset;
        record.folded_len = folded_len;
        record.size = metadata.size;
        record.mtime = metadata.mtime;
        record.flags = live_flags(metadata);
        Ok(true)
    }

    fn push_indexed_record(
        &mut self,
        parent: u32,
        name: &str,
        metadata: &LiveRecordMetadata,
    ) -> anyhow::Result<u32> {
        let name_offset = u32::try_from(self.string_pool.len())?;
        let name_len = u32::try_from(name.len())?;
        self.string_pool.extend_from_slice(name.as_bytes());
        let folded = name.to_lowercase();
        let folded_offset = u32::try_from(self.folded_pool.len())?;
        let folded_len = u32::try_from(folded.len())?;
        self.folded_pool.extend_from_slice(folded.as_bytes());
        let idx = u32::try_from(self.records.len())?;
        self.records.push(IndexedRecord {
            parent,
            size: metadata.size,
            mtime: metadata.mtime,
            name_offset,
            name_len,
            folded_offset,
            folded_len,
            flags: live_flags(metadata),
        });
        Ok(idx)
    }

    fn rebuild_from_keep(&mut self, keep: &[bool]) -> anyhow::Result<()> {
        let mut old_to_new = vec![None; self.records.len()];
        let mut records = Vec::new();
        let mut string_pool = Vec::new();
        let mut folded_pool = Vec::new();
        for (old_idx, should_keep) in keep.iter().copied().enumerate() {
            if !should_keep {
                continue;
            }
            let new_idx = u32::try_from(records.len())?;
            old_to_new[old_idx] = Some(new_idx);
            let old = &self.records[old_idx];
            let name = self.name(old_idx as u32);
            let folded = name.to_lowercase();
            let name_offset = u32::try_from(string_pool.len())?;
            let name_len = u32::try_from(name.len())?;
            string_pool.extend_from_slice(name.as_bytes());
            let folded_offset = u32::try_from(folded_pool.len())?;
            let folded_len = u32::try_from(folded.len())?;
            folded_pool.extend_from_slice(folded.as_bytes());
            records.push(IndexedRecord {
                parent: old.parent,
                size: old.size,
                mtime: old.mtime,
                name_offset,
                name_len,
                folded_offset,
                folded_len,
                flags: old.flags,
            });
        }
        for (old_idx, should_keep) in keep.iter().copied().enumerate() {
            if !should_keep {
                continue;
            }
            let Some(new_idx) = old_to_new[old_idx] else {
                continue;
            };
            let old_parent = self.records[old_idx].parent;
            records[new_idx as usize].parent = if old_parent == ROOT_PARENT {
                ROOT_PARENT
            } else {
                old_to_new
                    .get(old_parent as usize)
                    .and_then(|value| *value)
                    .unwrap_or(ROOT_PARENT)
            };
        }
        self.records = records;
        self.string_pool = string_pool;
        self.folded_pool = folded_pool;
        Ok(())
    }

    pub fn display_prefix(&self, mounted: bool, primary_mount_point: &str) -> String {
        if mounted && !primary_mount_point.trim().is_empty() {
            primary_mount_point.trim_end_matches('/').to_owned()
        } else if !self.metadata.label.trim().is_empty() {
            format!("[{}]", self.metadata.label.trim())
        } else {
            format!("[{}]", self.metadata.device_id)
        }
    }

    pub fn display_path(
        &self,
        record_idx: u32,
        mounted: bool,
        primary_mount_point: &str,
    ) -> String {
        join_prefix(
            &self.display_prefix(mounted, primary_mount_point),
            self.internal_path(record_idx),
        )
    }

    pub fn search(&self, query: &str, sort_key: SortKey, direction: SortDirection) -> Vec<u32> {
        let request = parse_search_query(query).unwrap_or_else(|_| SearchRequest {
            include: query
                .split_whitespace()
                .map(|s| SearchClause::NameContains(s.to_lowercase()))
                .collect(),
            exclude: Vec::new(),
        });
        self.search_request(&request, sort_key, direction)
    }

    pub fn search_request(
        &self,
        request: &SearchRequest,
        sort_key: SortKey,
        direction: SortDirection,
    ) -> Vec<u32> {
        self.search_request_with_relevance_context(
            request,
            sort_key,
            direction,
            RelevanceContext::current(),
        )
    }

    fn search_request_with_relevance_context(
        &self,
        request: &SearchRequest,
        sort_key: SortKey,
        direction: SortDirection,
        relevance_context: RelevanceContext,
    ) -> Vec<u32> {
        let mut hits = if request.is_empty() {
            self.pick_order(sort_key).clone()
        } else {
            let tokens = request_candidate_tokens(request);
            let candidates = self.candidates_for_tokens(&tokens);
            candidates
                .into_par_iter()
                .filter(|&idx| self.matches_request(idx, request))
                .collect()
        };

        if !request.is_empty() {
            if sort_key == SortKey::Relevance {
                hits.par_sort_unstable_by(|a, b| {
                    self.relevance_score(*a, request, relevance_context)
                        .cmp(&self.relevance_score(*b, request, relevance_context))
                        .then_with(|| self.folded_name(*a).cmp(self.folded_name(*b)))
                        .then_with(|| a.cmp(b))
                });
            } else {
                let rank = self.rank_for(sort_key);
                hits.par_sort_unstable_by(|a, b| {
                    rank[*a as usize]
                        .cmp(&rank[*b as usize])
                        .then_with(|| a.cmp(b))
                });
            }
        }

        if direction == SortDirection::Desc {
            hits.reverse();
        }
        hits
    }

    fn matches_request(&self, record_idx: u32, request: &SearchRequest) -> bool {
        for clause in &request.include {
            if !self.matches_clause(record_idx, clause) {
                return false;
            }
        }

        for clause in &request.exclude {
            if self.matches_clause(record_idx, clause) {
                return false;
            }
        }
        true
    }

    fn matches_clause(&self, record_idx: u32, clause: &SearchClause) -> bool {
        let name = self.folded_name(record_idx);
        let rec = &self.records[record_idx as usize];
        match clause {
            SearchClause::NameContains(needle) | SearchClause::NamePhrase(needle) => {
                name.contains(needle)
            }
            SearchClause::NameWildcard(pattern) => wildcard_matches(pattern, name),
            SearchClause::Extension(extensions) => file_extension(name)
                .map(|ext| extensions.iter().any(|wanted| wanted == ext))
                .unwrap_or(false),
            SearchClause::FileType(file_type) => match file_type {
                SearchFileType::Dir => rec.is_dir(),
                SearchFileType::Symlink => rec.is_symlink(),
                SearchFileType::File => !rec.is_dir() && !rec.is_symlink(),
            },
            SearchClause::PathContains(needle) => self
                .internal_path(record_idx)
                .to_lowercase()
                .contains(needle),
            SearchClause::Size(filter) => {
                filter.min.map(|min| rec.size >= min).unwrap_or(true)
                    && filter.max.map(|max| rec.size <= max).unwrap_or(true)
            }
            SearchClause::Mtime(filter) => {
                filter.min.map(|min| rec.mtime >= min).unwrap_or(true)
                    && filter.max.map(|max| rec.mtime <= max).unwrap_or(true)
            }
        }
    }

    fn build_path_for(&self, record_idx: u32, depth: usize) -> String {
        if record_idx as usize >= self.records.len() || depth > 4096 {
            return "/".to_owned();
        }

        let rec = &self.records[record_idx as usize];
        let name = self.name(record_idx);
        if rec.parent == ROOT_PARENT || name.is_empty() {
            if name.is_empty() {
                "/".to_owned()
            } else {
                format!("/{name}")
            }
        } else if rec.parent == record_idx {
            format!("/{name}")
        } else {
            let parent = self.build_path_for(rec.parent, depth + 1);
            if parent == "/" {
                format!("/{name}")
            } else {
                format!("{parent}/{name}")
            }
        }
    }

    fn build_trigram_index(&self) -> Vec<TrigramEntry> {
        let chunks: Vec<Vec<TrigramEntry>> = (0..self.records.len() as u32)
            .into_par_iter()
            .map(|record_idx| {
                let name = self.folded_name(record_idx).as_bytes();
                if name.len() < 3 {
                    return Vec::new();
                }

                let mut tris = Vec::with_capacity(name.len() - 2);
                for win in name.windows(3) {
                    tris.push(trigram(win));
                }
                tris.sort_unstable();
                tris.dedup();
                tris.into_iter()
                    .map(|tri| TrigramEntry {
                        trigram: tri,
                        record_idx,
                    })
                    .collect()
            })
            .collect();

        let mut flat: Vec<_> = chunks.into_iter().flatten().collect();
        flat.par_sort_unstable();
        flat
    }

    fn build_sort_orders(&mut self) {
        let n = self.records.len() as u32;
        self.order_by_name = (0..n).collect();
        self.order_by_path = (0..n).collect();
        self.order_by_size = (0..n).collect();
        self.order_by_mtime = (0..n).collect();

        let folded_pool = &self.folded_pool;
        let records = &self.records;
        self.order_by_name.par_sort_unstable_by(|a, b| {
            folded_slice(folded_pool, &records[*a as usize])
                .cmp(folded_slice(folded_pool, &records[*b as usize]))
                .then_with(|| a.cmp(b))
        });

        self.order_by_path.par_sort_unstable_by(|a, b| {
            self.internal_paths[*a as usize]
                .cmp(&self.internal_paths[*b as usize])
                .then_with(|| a.cmp(b))
        });

        self.order_by_size.par_sort_unstable_by(|a, b| {
            records[*a as usize]
                .size
                .cmp(&records[*b as usize].size)
                .then_with(|| {
                    folded_slice(folded_pool, &records[*a as usize])
                        .cmp(folded_slice(folded_pool, &records[*b as usize]))
                })
                .then_with(|| a.cmp(b))
        });

        self.order_by_mtime.par_sort_unstable_by(|a, b| {
            records[*a as usize]
                .mtime
                .cmp(&records[*b as usize].mtime)
                .then_with(|| {
                    folded_slice(folded_pool, &records[*a as usize])
                        .cmp(folded_slice(folded_pool, &records[*b as usize]))
                })
                .then_with(|| a.cmp(b))
        });
    }

    fn pick_order(&self, sort_key: SortKey) -> &Vec<u32> {
        match sort_key {
            SortKey::Relevance => &self.order_by_name,
            SortKey::Name => &self.order_by_name,
            SortKey::Path => &self.order_by_path,
            SortKey::Size => &self.order_by_size,
            SortKey::Mtime => &self.order_by_mtime,
        }
    }

    fn rank_for(&self, sort_key: SortKey) -> Vec<u32> {
        let mut rank = vec![0u32; self.records.len()];
        for (pos, idx) in self.pick_order(sort_key).iter().copied().enumerate() {
            rank[idx as usize] = pos as u32;
        }
        rank
    }

    fn candidates_for_tokens(&self, tokens: &[String]) -> Vec<u32> {
        let mut used_index = false;
        let mut candidates = Vec::new();

        for token in tokens {
            let bytes = token.as_bytes();
            if bytes.len() < 3 {
                continue;
            }
            used_index = true;

            for win in bytes.windows(3) {
                let tri = trigram(win);
                let range = trigram_range(&self.flat_index, tri);
                if range.0 == range.1 {
                    return Vec::new();
                }

                if candidates.is_empty() {
                    candidates = self.flat_index[range.0..range.1]
                        .iter()
                        .map(|entry| entry.record_idx)
                        .collect();
                } else {
                    let mut next = Vec::new();
                    let mut a = 0;
                    let mut b = range.0;
                    while a < candidates.len() && b < range.1 {
                        let av = candidates[a];
                        let bv = self.flat_index[b].record_idx;
                        match av.cmp(&bv) {
                            Ordering::Less => a += 1,
                            Ordering::Greater => b += 1,
                            Ordering::Equal => {
                                next.push(av);
                                a += 1;
                                b += 1;
                            }
                        }
                    }
                    candidates = next;
                    if candidates.is_empty() {
                        return Vec::new();
                    }
                }
            }
        }

        if used_index {
            candidates
        } else {
            (0..self.records.len() as u32).collect()
        }
    }

    fn relevance_score(
        &self,
        record_idx: u32,
        request: &SearchRequest,
        context: RelevanceContext,
    ) -> RelevanceScore {
        let name = self.folded_name(record_idx);
        let rec = &self.records[record_idx as usize];
        let path_depth = self
            .internal_path(record_idx)
            .trim_matches('/')
            .split('/')
            .filter(|part| !part.is_empty())
            .count() as u32;
        let mut match_score = 100u32;

        for clause in &request.include {
            match clause {
                SearchClause::NameContains(needle) | SearchClause::NamePhrase(needle) => {
                    let score = if name == needle {
                        0
                    } else if name.starts_with(needle) {
                        10
                    } else if name.contains(needle) {
                        20
                    } else {
                        80
                    };
                    match_score = match_score.min(score);
                }
                SearchClause::NameWildcard(pattern) => {
                    if wildcard_matches(pattern, name) {
                        match_score = match_score.min(if pattern.trim_matches('*') == name {
                            5
                        } else if pattern.ends_with('*')
                            && name.starts_with(pattern.trim_end_matches('*'))
                        {
                            15
                        } else {
                            30
                        });
                    }
                }
                SearchClause::Extension(_)
                | SearchClause::FileType(_)
                | SearchClause::PathContains(_)
                | SearchClause::Size(_)
                | SearchClause::Mtime(_) => {}
            }
        }

        RelevanceScore {
            match_score,
            recency_bucket: context.recency_bucket(rec.mtime),
            name_len: name.len() as u32,
            path_depth,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RelevanceContext {
    now_unix: i64,
}

impl RelevanceContext {
    fn current() -> Self {
        Self {
            now_unix: now_unix(),
        }
    }

    fn recency_bucket(self, mtime: i64) -> u32 {
        const DAY: i64 = 86_400;
        const WEEK: i64 = 7 * DAY;
        const MONTH: i64 = 30 * DAY;
        const HALF_YEAR: i64 = 180 * DAY;
        const UNKNOWN_RECENCY: u32 = 5;

        if self.now_unix <= 0 || mtime <= 0 {
            return UNKNOWN_RECENCY;
        }

        let age = self.now_unix.saturating_sub(mtime);
        if age <= DAY {
            0
        } else if age <= WEEK {
            1
        } else if age <= MONTH {
            2
        } else if age <= HALF_YEAR {
            3
        } else {
            4
        }
    }
}

// Field order is the relevance ranking order. Text match quality stays dominant;
// recency only promotes a result within the same match-quality class.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct RelevanceScore {
    match_score: u32,
    recency_bucket: u32,
    name_len: u32,
    path_depth: u32,
}

pub fn merge_search(
    indexes: &[SearchIndex],
    device_filter: Option<&str>,
    query: &str,
    sort_key: SortKey,
    direction: SortDirection,
) -> Vec<SearchHit> {
    let request = parse_search_query(query).unwrap_or_else(|_| SearchRequest {
        include: query
            .split_whitespace()
            .map(|s| SearchClause::NameContains(s.to_lowercase()))
            .collect(),
        exclude: Vec::new(),
    });
    merge_search_request(indexes, device_filter, &request, sort_key, direction)
}

pub fn merge_search_request(
    indexes: &[SearchIndex],
    device_filter: Option<&str>,
    request: &SearchRequest,
    sort_key: SortKey,
    direction: SortDirection,
) -> Vec<SearchHit> {
    let relevance_context = RelevanceContext::current();
    let mut hits = Vec::new();
    for index in indexes {
        if let Some(device_id) = device_filter
            && index.metadata.device_id != device_id
        {
            continue;
        }

        hits.extend(
            index
                .search_request_with_relevance_context(
                    request,
                    sort_key,
                    SortDirection::Asc,
                    relevance_context,
                )
                .into_iter()
                .map(|record_idx| SearchHit {
                    device_id: index.metadata.device_id.clone(),
                    record_idx,
                }),
        );
    }

    hits.par_sort_unstable_by(|a, b| {
        let ia = indexes
            .iter()
            .find(|idx| idx.metadata.device_id == a.device_id);
        let ib = indexes
            .iter()
            .find(|idx| idx.metadata.device_id == b.device_id);
        match (ia, ib) {
            (Some(ia), Some(ib)) => compare_hits(
                ia,
                a.record_idx,
                ib,
                b.record_idx,
                sort_key,
                request,
                relevance_context,
            ),
            _ => a
                .device_id
                .cmp(&b.device_id)
                .then_with(|| a.record_idx.cmp(&b.record_idx)),
        }
    });

    if direction == SortDirection::Desc {
        hits.reverse();
    }
    hits
}

fn compare_hits(
    a_idx: &SearchIndex,
    a: u32,
    b_idx: &SearchIndex,
    b: u32,
    sort_key: SortKey,
    request: &SearchRequest,
    relevance_context: RelevanceContext,
) -> Ordering {
    let ar = &a_idx.records[a as usize];
    let br = &b_idx.records[b as usize];
    let ord = match sort_key {
        SortKey::Relevance => a_idx
            .relevance_score(a, request, relevance_context)
            .cmp(&b_idx.relevance_score(b, request, relevance_context))
            .then_with(|| a_idx.folded_name(a).cmp(b_idx.folded_name(b))),
        SortKey::Name => a_idx.folded_name(a).cmp(b_idx.folded_name(b)),
        SortKey::Path => a_idx.internal_path(a).cmp(b_idx.internal_path(b)),
        SortKey::Size => ar
            .size
            .cmp(&br.size)
            .then_with(|| a_idx.folded_name(a).cmp(b_idx.folded_name(b))),
        SortKey::Mtime => ar
            .mtime
            .cmp(&br.mtime)
            .then_with(|| a_idx.folded_name(a).cmp(b_idx.folded_name(b))),
    };
    ord.then_with(|| a_idx.metadata.device_id.cmp(&b_idx.metadata.device_id))
        .then_with(|| a.cmp(&b))
}

#[derive(Clone, Debug)]
struct QueryToken {
    value: String,
    quoted: bool,
}

fn tokenize_query(query: &str) -> Result<Vec<QueryToken>, SearchParseError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut token_was_quoted = false;
    let mut chars = query.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                quoted = !quoted;
                token_was_quoted = true;
            }
            '\\' if quoted => {
                if let Some(next) = chars.next() {
                    current.push(next);
                } else {
                    current.push(ch);
                }
            }
            ch if ch.is_whitespace() && !quoted => {
                if !current.is_empty() || token_was_quoted {
                    tokens.push(QueryToken {
                        value: std::mem::take(&mut current),
                        quoted: token_was_quoted,
                    });
                    token_was_quoted = false;
                }
            }
            _ => current.push(ch),
        }
    }

    if quoted {
        return Err(SearchParseError::new("unterminated quoted search phrase"));
    }
    if !current.is_empty() || token_was_quoted {
        tokens.push(QueryToken {
            value: current,
            quoted: token_was_quoted,
        });
    }
    Ok(tokens)
}

fn parse_file_type(value: &str) -> Result<SearchFileType, SearchParseError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "file" | "regular" => Ok(SearchFileType::File),
        "dir" | "directory" | "folder" => Ok(SearchFileType::Dir),
        "symlink" | "link" => Ok(SearchFileType::Symlink),
        other => Err(SearchParseError::new(format!(
            "unsupported type filter '{other}'; use file, dir, or symlink"
        ))),
    }
}

fn normalize_extension(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('.')
        .to_lowercase()
        .trim()
        .to_owned()
}

fn request_candidate_tokens(request: &SearchRequest) -> Vec<String> {
    let mut out = Vec::new();
    for clause in &request.include {
        match clause {
            SearchClause::NameContains(value) | SearchClause::NamePhrase(value) => {
                out.push(value.clone());
            }
            SearchClause::NameWildcard(pattern) => {
                out.extend(
                    pattern
                        .split('*')
                        .filter(|part| part.len() >= 3)
                        .map(str::to_owned),
                );
            }
            SearchClause::Extension(_)
            | SearchClause::FileType(_)
            | SearchClause::PathContains(_)
            | SearchClause::Size(_)
            | SearchClause::Mtime(_) => {}
        }
    }
    out
}

fn file_extension(name: &str) -> Option<&str> {
    let (_, ext) = name.rsplit_once('.')?;
    (!ext.is_empty()).then_some(ext)
}

fn wildcard_matches(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == text;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    let mut remainder = text;
    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');

    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if idx == 0 && anchored_start {
            let Some(next) = remainder.strip_prefix(part) else {
                return false;
            };
            remainder = next;
            continue;
        }
        let Some(pos) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[pos + part.len()..];
    }

    if anchored_end && let Some(last) = parts.iter().rev().find(|part| !part.is_empty()) {
        text.ends_with(last)
    } else {
        true
    }
}

pub fn join_prefix(prefix: &str, internal_path: &str) -> String {
    if prefix.is_empty() {
        internal_path.to_owned()
    } else if internal_path == "/" {
        if prefix.starts_with('/') {
            prefix.to_owned()
        } else {
            format!("{prefix}/")
        }
    } else {
        format!("{}{}", prefix.trim_end_matches('/'), internal_path)
    }
}

fn normalize_internal_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len().max(1));
    if !path.starts_with('/') {
        out.push('/');
    }
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                out.push('/');
            }
            prev_slash = true;
        } else {
            out.push(ch);
            prev_slash = false;
        }
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    if out.is_empty() { "/".to_owned() } else { out }
}

fn split_internal_path(path: &str) -> (&str, &str) {
    let path = path.trim_end_matches('/');
    if path.is_empty() || path == "/" {
        return ("/", "");
    }
    let Some(pos) = path.rfind('/') else {
        return ("/", path);
    };
    let parent = if pos == 0 { "/" } else { &path[..pos] };
    (parent, &path[pos + 1..])
}

fn live_flags(metadata: &LiveRecordMetadata) -> u8 {
    let mut flags = 0;
    if metadata.is_dir {
        flags |= crate::model::FLAG_IS_DIR;
    }
    if metadata.is_symlink {
        flags |= crate::model::FLAG_IS_SYMLINK;
    }
    flags
}

fn folded_slice<'a>(pool: &'a [u8], rec: &IndexedRecord) -> &'a str {
    let start = rec.folded_offset as usize;
    let end = start + rec.folded_len as usize;
    std::str::from_utf8(&pool[start..end]).unwrap_or("")
}

fn trigram(bytes: &[u8]) -> u32 {
    debug_assert!(bytes.len() >= 3);
    ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | (bytes[2] as u32)
}

fn trigram_range(index: &[TrigramEntry], tri: u32) -> (usize, usize) {
    let start = index.partition_point(|entry| entry.trigram < tri);
    let end = index.partition_point(|entry| entry.trigram <= tri);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FsType, ScanDatabase};

    fn sample_index() -> SearchIndex {
        let mut scan = ScanDatabase::new(FsType::Ext4);
        scan.push_record(ROOT_PARENT, "", 0, 0, true, false)
            .unwrap();
        scan.push_record(0, "Résumé.TXT", 10, 2, false, false)
            .unwrap();
        scan.push_record(0, "alpha.log", 20, 1, false, false)
            .unwrap();
        let src = scan.push_record(0, "src", 0, 3, true, false).unwrap();
        scan.push_record(src, "main.rs", 30, 4, false, false)
            .unwrap();
        scan.push_record(src, "lib.RS", 40, 5, false, false)
            .unwrap();
        scan.push_record(0, "latest", 0, 6, false, true).unwrap();
        SearchIndex::from_scan(
            DeviceMetadata {
                device_id: "uuid:test".into(),
                dev_node: "/dev/test".into(),
                fs_type: FsType::Ext4,
                label: "Test".into(),
                uuid: "test".into(),
                partuuid: String::new(),
            },
            scan,
            123,
        )
        .unwrap()
    }

    fn index_with_files(device_id: &str, files: &[(&str, i64)]) -> SearchIndex {
        let mut scan = ScanDatabase::new(FsType::Ext4);
        scan.push_record(ROOT_PARENT, "", 0, 0, true, false)
            .unwrap();
        for (name, mtime) in files {
            scan.push_record(0, name, 10, *mtime, false, false).unwrap();
        }
        SearchIndex::from_scan(
            DeviceMetadata {
                device_id: device_id.into(),
                dev_node: format!("/dev/{device_id}"),
                fs_type: FsType::Ext4,
                label: device_id.into(),
                uuid: device_id.into(),
                partuuid: String::new(),
            },
            scan,
            1_000_000,
        )
        .unwrap()
    }

    #[test]
    fn unicode_lowercase_search_works() {
        let idx = sample_index();
        let hits = idx.search("résumé", SortKey::Name, SortDirection::Asc);
        assert_eq!(hits, vec![1]);
    }

    #[test]
    fn short_token_fallback_works() {
        let idx = sample_index();
        let hits = idx.search("lo", SortKey::Name, SortDirection::Asc);
        assert_eq!(hits, vec![2]);
    }

    #[test]
    fn path_reconstruction_works() {
        let idx = sample_index();
        assert_eq!(idx.internal_path(1), "/Résumé.TXT");
        assert_eq!(idx.display_path(1, false, ""), "[Test]/Résumé.TXT");
    }

    #[test]
    fn wildcard_search_works() {
        let idx = sample_index();
        let request = parse_search_query("*.rs").unwrap();
        let hits = idx.search_request(&request, SortKey::Name, SortDirection::Asc);
        assert_eq!(hits, vec![5, 4]);
    }

    #[test]
    fn extension_filter_is_case_insensitive_and_dot_optional() {
        let idx = sample_index();
        let request = parse_search_query("ext:.RS").unwrap();
        let hits = idx.search_request(&request, SortKey::Path, SortDirection::Asc);
        assert_eq!(hits, vec![5, 4]);
    }

    #[test]
    fn filter_prefixes_are_case_insensitive() {
        let idx = sample_index();
        let request = parse_search_query("EXT:RS TYPE:FILE PATH:SRC").unwrap();
        let hits = idx.search_request(&request, SortKey::Path, SortDirection::Asc);
        assert_eq!(hits, vec![5, 4]);
    }

    #[test]
    fn extension_filter_accepts_multiple_values() {
        let idx = sample_index();
        let request = parse_search_query("ext:rs,txt").unwrap();
        let hits = idx.search_request(&request, SortKey::Path, SortDirection::Asc);
        assert_eq!(hits, vec![1, 5, 4]);
    }

    #[test]
    fn type_filter_works() {
        let idx = sample_index();
        let dirs = parse_search_query("type:dir").unwrap();
        assert_eq!(
            idx.search_request(&dirs, SortKey::Path, SortDirection::Asc),
            vec![0, 3]
        );

        let links = parse_search_query("type:symlink").unwrap();
        assert_eq!(
            idx.search_request(&links, SortKey::Path, SortDirection::Asc),
            vec![6]
        );
    }

    #[test]
    fn path_filter_combines_with_name_terms() {
        let idx = sample_index();
        let request = parse_search_query("path:src main").unwrap();
        let hits = idx.search_request(&request, SortKey::Path, SortDirection::Asc);
        assert_eq!(hits, vec![4]);
    }

    #[test]
    fn quoted_phrase_search_works() {
        let idx = sample_index();
        let request = parse_search_query("\"résumé.txt\"").unwrap();
        let hits = idx.search_request(&request, SortKey::Name, SortDirection::Asc);
        assert_eq!(hits, vec![1]);
    }

    #[test]
    fn invalid_query_reports_error() {
        assert!(parse_search_query("\"unterminated").is_err());
    }

    #[test]
    fn negation_filters_matching_names() {
        let idx = sample_index();
        let request = parse_search_query("*.rs !lib").unwrap();
        let hits = idx.search_request(&request, SortKey::Path, SortDirection::Asc);
        assert_eq!(hits, vec![4]);
    }

    #[test]
    fn size_filter_works() {
        let idx = sample_index();
        let request = parse_search_query("type:file size:<35").unwrap();
        let hits = idx.search_request(&request, SortKey::Path, SortDirection::Asc);
        assert_eq!(hits, vec![1, 2, 4]);
    }

    #[test]
    fn mtime_date_range_filter_works() {
        let idx = sample_index();
        let request = parse_search_query("mtime:1970-01-01..2100-01-01").unwrap();
        let hits = idx.search_request(&request, SortKey::Path, SortDirection::Asc);
        assert_eq!(hits.len(), idx.records.len());
    }

    #[test]
    fn relevance_prefers_exact_and_prefix_names() {
        let idx = sample_index();
        let request = parse_search_query("main").unwrap();
        let hits = idx.search_request(&request, SortKey::Relevance, SortDirection::Asc);
        assert_eq!(hits.first().copied(), Some(4));
    }

    #[test]
    fn relevance_uses_recency_within_same_match_quality() {
        let idx = index_with_files(
            "uuid:recency",
            &[("report-a.txt", 10_000), ("report-b.txt", 999_000)],
        );
        let request = parse_search_query("report").unwrap();

        let hits = idx.search_request_with_relevance_context(
            &request,
            SortKey::Relevance,
            SortDirection::Asc,
            RelevanceContext {
                now_unix: 1_000_000,
            },
        );

        assert_eq!(hits, vec![2, 1]);
    }

    #[test]
    fn relevance_keeps_match_quality_above_recency() {
        let idx = index_with_files(
            "uuid:quality",
            &[("report.txt", 10_000), ("fresh-report.txt", 999_000)],
        );
        let request = parse_search_query("report").unwrap();

        let hits = idx.search_request_with_relevance_context(
            &request,
            SortKey::Relevance,
            SortDirection::Asc,
            RelevanceContext {
                now_unix: 1_000_000,
            },
        );

        assert_eq!(hits, vec![1, 2]);
    }

    #[test]
    fn relevance_treats_unknown_mtime_as_least_recent() {
        const DAY: i64 = 86_400;
        let context = RelevanceContext {
            now_unix: 20_000_000,
        };

        assert_eq!(context.recency_bucket(context.now_unix - DAY), 0);
        assert_eq!(context.recency_bucket(context.now_unix - DAY - 1), 1);
        assert_eq!(context.recency_bucket(context.now_unix - 7 * DAY - 1), 2);
        assert_eq!(context.recency_bucket(context.now_unix - 30 * DAY - 1), 3);
        assert_eq!(context.recency_bucket(context.now_unix - 180 * DAY - 1), 4);
        assert_eq!(context.recency_bucket(0), 5);
        assert_eq!(context.recency_bucket(-1), 5);
    }

    #[test]
    fn merged_relevance_uses_recency_and_keeps_deterministic_ties() {
        let now = now_unix();
        let indexes = vec![
            index_with_files("uuid:b", &[("report-b.txt", now - 200 * 86_400)]),
            index_with_files("uuid:a", &[("report-a.txt", now - 60)]),
            index_with_files("uuid:c", &[("report-c.txt", now - 60)]),
        ];
        let request = parse_search_query("report").unwrap();

        let hits = merge_search_request(
            &indexes,
            None,
            &request,
            SortKey::Relevance,
            SortDirection::Asc,
        );

        assert_eq!(
            hits,
            vec![
                SearchHit {
                    device_id: "uuid:a".into(),
                    record_idx: 1,
                },
                SearchHit {
                    device_id: "uuid:c".into(),
                    record_idx: 1,
                },
                SearchHit {
                    device_id: "uuid:b".into(),
                    record_idx: 1,
                },
            ]
        );
    }

    #[test]
    fn live_update_create_rename_and_remove_work() {
        let mut idx = sample_index();
        assert!(
            idx.apply_live_event(LiveUpdateEvent::Created {
                internal_path: "/src/new.rs".into(),
                metadata: LiveRecordMetadata {
                    size: 7,
                    mtime: 9,
                    is_dir: false,
                    is_symlink: false,
                },
            })
            .unwrap()
        );
        let new_idx = idx.record_by_internal_path("/src/new.rs").unwrap();
        assert_eq!(idx.name(new_idx), "new.rs");

        assert!(
            idx.apply_live_event(LiveUpdateEvent::Renamed {
                old_internal_path: "/src/new.rs".into(),
                new_internal_path: "/src/new_name.rs".into(),
                metadata: LiveRecordMetadata {
                    size: 8,
                    mtime: 10,
                    is_dir: false,
                    is_symlink: false,
                },
            })
            .unwrap()
        );
        assert!(idx.record_by_internal_path("/src/new.rs").is_none());
        assert!(idx.record_by_internal_path("/src/new_name.rs").is_some());

        assert!(
            idx.apply_live_event(LiveUpdateEvent::Removed {
                internal_path: "/src/new_name.rs".into(),
            })
            .unwrap()
        );
        assert!(idx.record_by_internal_path("/src/new_name.rs").is_none());
    }

    #[test]
    fn live_update_batch_rebuilds_once() {
        let mut idx = sample_index();
        let generation = idx.generation;
        assert!(
            idx.apply_live_events([
                LiveUpdateEvent::Created {
                    internal_path: "/src/a.rs".into(),
                    metadata: LiveRecordMetadata {
                        size: 1,
                        mtime: 1,
                        is_dir: false,
                        is_symlink: false,
                    },
                },
                LiveUpdateEvent::Created {
                    internal_path: "/src/b.rs".into(),
                    metadata: LiveRecordMetadata {
                        size: 2,
                        mtime: 2,
                        is_dir: false,
                        is_symlink: false,
                    },
                },
            ])
            .unwrap()
        );
        assert_eq!(idx.generation, generation + 1);
        assert!(idx.record_by_internal_path("/src/a.rs").is_some());
        assert!(idx.record_by_internal_path("/src/b.rs").is_some());
    }
}
