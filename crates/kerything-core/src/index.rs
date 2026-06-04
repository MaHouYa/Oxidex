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

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub device_id: String,
    pub record_idx: u32,
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
        let tokens: Vec<String> = query
            .split_whitespace()
            .map(|s| s.to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        let mut hits = if tokens.is_empty() {
            self.pick_order(sort_key).clone()
        } else {
            let candidates = self.candidates_for_tokens(&tokens);
            let token_refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
            candidates
                .into_par_iter()
                .filter(|&idx| {
                    let name = self.folded_name(idx);
                    token_refs.iter().all(|tok| name.contains(tok))
                })
                .collect()
        };

        if !tokens.is_empty() {
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
    let mut hits = Vec::new();
    for index in indexes {
        if let Some(device_id) = device_filter
            && index.metadata.device_id != device_id
        {
            continue;
        }

        hits.extend(
            index
                .search(query, sort_key, SortDirection::Asc)
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
}
