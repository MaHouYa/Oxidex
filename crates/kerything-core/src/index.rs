use std::cmp::Ordering;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

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
pub enum SearchTerm {
    Contains(String),
    Phrase(String),
    Wildcard(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct SearchRequest {
    pub terms: Vec<SearchTerm>,
    pub filters: SearchFilters,
}

impl SearchRequest {
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.filters.is_empty()
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
        if !token.quoted {
            let lowered = token.value.to_ascii_lowercase();
            if lowered.starts_with("ext:") {
                request.filters.add_extensions(&token.value[4..]);
                continue;
            }
            if lowered.starts_with("path:") {
                request.filters.add_path_contains(&token.value[5..]);
                continue;
            }
            if lowered.starts_with("type:") {
                request.filters.file_type = Some(parse_file_type(&token.value[5..])?);
                continue;
            }
        }

        let folded = token.value.to_lowercase();
        if folded.contains('*') {
            request.terms.push(SearchTerm::Wildcard(folded));
        } else if token.quoted {
            request.terms.push(SearchTerm::Phrase(folded));
        } else {
            request.terms.push(SearchTerm::Contains(folded));
        }
    }
    Ok(request)
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
            terms: query
                .split_whitespace()
                .map(|s| SearchTerm::Contains(s.to_lowercase()))
                .collect(),
            filters: SearchFilters::default(),
        });
        self.search_request(&request, sort_key, direction)
    }

    pub fn search_request(
        &self,
        request: &SearchRequest,
        sort_key: SortKey,
        direction: SortDirection,
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
            let rank = self.rank_for(sort_key);
            hits.par_sort_unstable_by(|a, b| {
                rank[*a as usize]
                    .cmp(&rank[*b as usize])
                    .then_with(|| a.cmp(b))
            });
        }

        if direction == SortDirection::Desc {
            hits.reverse();
        }
        hits
    }

    fn matches_request(&self, record_idx: u32, request: &SearchRequest) -> bool {
        let name = self.folded_name(record_idx);
        for term in &request.terms {
            match term {
                SearchTerm::Contains(needle) | SearchTerm::Phrase(needle) => {
                    if !name.contains(needle) {
                        return false;
                    }
                }
                SearchTerm::Wildcard(pattern) => {
                    if !wildcard_matches(pattern, name) {
                        return false;
                    }
                }
            }
        }

        let rec = &self.records[record_idx as usize];
        if let Some(file_type) = request.filters.file_type {
            let matches = match file_type {
                SearchFileType::Dir => rec.is_dir(),
                SearchFileType::Symlink => rec.is_symlink(),
                SearchFileType::File => !rec.is_dir() && !rec.is_symlink(),
            };
            if !matches {
                return false;
            }
        }

        if !request.filters.extensions.is_empty() {
            let Some(ext) = file_extension(name) else {
                return false;
            };
            if !request
                .filters
                .extensions
                .iter()
                .any(|wanted| wanted == ext)
            {
                return false;
            }
        }

        if !request.filters.path_contains.is_empty() {
            let path = self.internal_path(record_idx).to_lowercase();
            if !request
                .filters
                .path_contains
                .iter()
                .all(|needle| path.contains(needle))
            {
                return false;
            }
        }

        true
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
}

pub fn merge_search(
    indexes: &[SearchIndex],
    device_filter: Option<&str>,
    query: &str,
    sort_key: SortKey,
    direction: SortDirection,
) -> Vec<SearchHit> {
    let request = parse_search_query(query).unwrap_or_else(|_| SearchRequest {
        terms: query
            .split_whitespace()
            .map(|s| SearchTerm::Contains(s.to_lowercase()))
            .collect(),
        filters: SearchFilters::default(),
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
    let mut hits = Vec::new();
    for index in indexes {
        if let Some(device_id) = device_filter
            && index.metadata.device_id != device_id
        {
            continue;
        }

        hits.extend(
            index
                .search_request(request, sort_key, SortDirection::Asc)
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
            (Some(ia), Some(ib)) => compare_hits(ia, a.record_idx, ib, b.record_idx, sort_key),
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
) -> Ordering {
    let ar = &a_idx.records[a as usize];
    let br = &b_idx.records[b as usize];
    let ord = match sort_key {
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
    for term in &request.terms {
        match term {
            SearchTerm::Contains(value) | SearchTerm::Phrase(value) => {
                out.push(value.clone());
            }
            SearchTerm::Wildcard(pattern) => {
                out.extend(
                    pattern
                        .split('*')
                        .filter(|part| part.len() >= 3)
                        .map(str::to_owned),
                );
            }
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
}
