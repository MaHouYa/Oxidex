use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};

use crate::daemon_model::IndexStateSummary;
use crate::index::{IndexedRecord, SearchIndex, TrigramEntry};
use crate::model::{DeviceMetadata, FsType};

const MAGIC: &[u8; 8] = b"KRYIDX01";
const VERSION: u32 = 1;

pub fn index_dir() -> anyhow::Result<PathBuf> {
    let dirs = xdg::BaseDirectories::with_prefix("oxidex");
    dirs.get_data_file("indexes")
        .ok_or_else(|| anyhow::anyhow!("unable to resolve XDG data directory"))
}

pub fn snapshot_path_for(device_id: &str) -> anyhow::Result<PathBuf> {
    Ok(index_dir()?.join(format!("{}.kidx", escape_device_id(device_id))))
}

pub fn state_path_for(device_id: &str) -> anyhow::Result<PathBuf> {
    Ok(index_dir()?.join(format!("{}.state.json", escape_device_id(device_id))))
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct IndexStateV1 {
    pub version: u32,
    pub device_id: String,
    pub last_success_time: Option<i64>,
    pub last_failure_time: Option<i64>,
    pub last_error: Option<String>,
    pub last_scan_duration_ms: Option<u64>,
    pub last_scanner: Option<String>,
    pub last_entry_count: Option<usize>,
    pub stale_reason: Option<String>,
    pub live_watch_state: Option<String>,
    pub rules_fingerprint: Option<u64>,
    pub snapshot_size: Option<u64>,
}

impl IndexStateV1 {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            version: 1,
            device_id: device_id.into(),
            ..Self::default()
        }
    }

    pub fn summary(&self) -> IndexStateSummary {
        IndexStateSummary {
            last_success_time: self.last_success_time,
            last_failure_time: self.last_failure_time,
            last_error: self.last_error.clone(),
            last_scan_duration_ms: self.last_scan_duration_ms,
            last_scanner: self.last_scanner.clone(),
            snapshot_size: self.snapshot_size,
            stale_reason: self.stale_reason.clone(),
            live_watch_state: self.live_watch_state.clone(),
        }
    }
}

pub fn save_index(index: &SearchIndex) -> anyhow::Result<PathBuf> {
    let path = snapshot_path_for(&index.metadata.device_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp = path.with_extension("kidx.tmp");
    {
        let f = File::create(&tmp)?;
        let mut w = BufWriter::new(f);
        write_index(&mut w, index)?;
        w.flush()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn save_index_state(state: &IndexStateV1) -> anyhow::Result<PathBuf> {
    let path = state_path_for(&state.device_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("state.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn load_index_state(device_id: &str) -> anyhow::Result<Option<IndexStateV1>> {
    let path = state_path_for(device_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let state: IndexStateV1 = serde_json::from_slice(&fs::read(path)?)?;
    anyhow::ensure!(
        state.version == 1,
        "unsupported index state version {}",
        state.version
    );
    Ok(Some(state))
}

pub fn load_all_index_states() -> anyhow::Result<Vec<IndexStateV1>> {
    let dir = index_dir()?;
    let mut states = Vec::new();
    if !dir.exists() {
        return Ok(states);
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.ends_with(".state.json"))
            .unwrap_or(false)
        {
            continue;
        }
        match serde_json::from_slice::<IndexStateV1>(&fs::read(&path)?) {
            Ok(state) if state.version == 1 => states.push(state),
            Ok(state) => eprintln!(
                "Failed to load index state {}: unsupported version {}",
                path.display(),
                state.version
            ),
            Err(err) => eprintln!("Failed to load index state {}: {err}", path.display()),
        }
    }
    Ok(states)
}

pub fn load_all_indexes() -> anyhow::Result<Vec<SearchIndex>> {
    let dir = index_dir()?;
    let mut indexes = Vec::new();
    if !dir.exists() {
        return Ok(indexes);
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("kidx") {
            continue;
        }
        match load_index(&path) {
            Ok(index) => indexes.push(index),
            Err(err) => eprintln!("Failed to load snapshot {}: {err}", path.display()),
        }
    }
    Ok(indexes)
}

pub fn load_index(path: &Path) -> anyhow::Result<SearchIndex> {
    let f = File::open(path)?;
    let mut r = BufReader::new(f);
    read_index(&mut r)
}

pub fn delete_index(device_id: &str) -> anyhow::Result<()> {
    let path = snapshot_path_for(device_id)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    let state = state_path_for(device_id)?;
    if state.exists() {
        fs::remove_file(state)?;
    }
    Ok(())
}

pub fn write_index(mut w: impl Write, index: &SearchIndex) -> anyhow::Result<()> {
    w.write_all(MAGIC)?;
    w.write_u32::<LittleEndian>(VERSION)?;

    write_string(&mut w, &index.metadata.device_id)?;
    write_string(&mut w, &index.metadata.dev_node)?;
    w.write_u8(match index.metadata.fs_type {
        FsType::Ntfs => 1,
        FsType::Ext4 => 2,
        FsType::Btrfs => 3,
        FsType::Ext3 => 4,
    })?;
    write_string(&mut w, &index.metadata.label)?;
    write_string(&mut w, &index.metadata.uuid)?;
    write_string(&mut w, &index.metadata.partuuid)?;

    w.write_u64::<LittleEndian>(index.generation)?;
    w.write_i64::<LittleEndian>(index.last_indexed_time)?;

    write_indexed_records(&mut w, &index.records)?;
    write_bytes(&mut w, &index.string_pool)?;
    write_bytes(&mut w, &index.folded_pool)?;
    write_string_vec(&mut w, &index.internal_paths)?;
    write_trigrams(&mut w, &index.flat_index)?;
    write_u32_vec(&mut w, &index.order_by_name)?;
    write_u32_vec(&mut w, &index.order_by_path)?;
    write_u32_vec(&mut w, &index.order_by_size)?;
    write_u32_vec(&mut w, &index.order_by_mtime)?;
    Ok(())
}

pub fn read_index(mut r: impl Read) -> anyhow::Result<SearchIndex> {
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    anyhow::ensure!(&magic == MAGIC, "snapshot magic mismatch");

    let version = r.read_u32::<LittleEndian>()?;
    anyhow::ensure!(version == VERSION, "unsupported snapshot version {version}");

    let device_id = read_string(&mut r)?;
    let dev_node = read_string(&mut r)?;
    let fs_type = match r.read_u8()? {
        1 => FsType::Ntfs,
        2 => FsType::Ext4,
        3 => FsType::Btrfs,
        4 => FsType::Ext3,
        other => anyhow::bail!("unknown snapshot filesystem tag {other}"),
    };
    let label = read_string(&mut r)?;
    let uuid = read_string(&mut r)?;
    let partuuid = read_string(&mut r)?;

    let generation = r.read_u64::<LittleEndian>()?;
    let last_indexed_time = r.read_i64::<LittleEndian>()?;
    let records = read_indexed_records(&mut r)?;
    let string_pool = read_bytes(&mut r, 8 * 1024 * 1024 * 1024)?;
    let folded_pool = read_bytes(&mut r, 8 * 1024 * 1024 * 1024)?;
    let internal_paths = read_string_vec(&mut r)?;
    let flat_index = read_trigrams(&mut r)?;
    let order_by_name = read_u32_vec(&mut r)?;
    let order_by_path = read_u32_vec(&mut r)?;
    let order_by_size = read_u32_vec(&mut r)?;
    let order_by_mtime = read_u32_vec(&mut r)?;

    let index = SearchIndex {
        metadata: DeviceMetadata {
            device_id,
            dev_node,
            fs_type,
            label,
            uuid,
            partuuid,
        },
        generation,
        last_indexed_time,
        records,
        string_pool,
        folded_pool,
        internal_paths,
        flat_index,
        order_by_name,
        order_by_path,
        order_by_size,
        order_by_mtime,
    };
    validate_index(&index)?;
    Ok(index)
}

fn validate_index(index: &SearchIndex) -> anyhow::Result<()> {
    let n = index.records.len();
    anyhow::ensure!(
        index.internal_paths.len() == n,
        "snapshot path vector size mismatch"
    );
    for (i, rec) in index.records.iter().enumerate() {
        validate_range(
            "name",
            i,
            rec.name_offset,
            rec.name_len,
            index.string_pool.len(),
        )?;
        validate_range(
            "folded name",
            i,
            rec.folded_offset,
            rec.folded_len,
            index.folded_pool.len(),
        )?;
        validate_utf8_range("name", i, rec.name_offset, rec.name_len, &index.string_pool)?;
        validate_utf8_range(
            "folded name",
            i,
            rec.folded_offset,
            rec.folded_len,
            &index.folded_pool,
        )?;
        if rec.parent != crate::model::ROOT_PARENT {
            anyhow::ensure!((rec.parent as usize) < n, "record {i} parent out of bounds");
        }
    }

    for order in [
        &index.order_by_name,
        &index.order_by_path,
        &index.order_by_size,
        &index.order_by_mtime,
    ] {
        anyhow::ensure!(order.len() == n, "snapshot sort order size mismatch");
        let mut seen = vec![false; n];
        for &idx in order {
            anyhow::ensure!(
                (idx as usize) < n,
                "snapshot sort order contains out-of-bounds record"
            );
            anyhow::ensure!(
                !seen[idx as usize],
                "snapshot sort order contains duplicate record"
            );
            seen[idx as usize] = true;
        }
    }
    for entry in &index.flat_index {
        anyhow::ensure!(
            (entry.record_idx as usize) < n,
            "snapshot trigram index contains out-of-bounds record"
        );
    }
    anyhow::ensure!(
        index.flat_index.windows(2).all(|pair| pair[0] <= pair[1]),
        "snapshot trigram index is not sorted"
    );
    Ok(())
}

fn validate_range(
    kind: &str,
    idx: usize,
    offset: u32,
    len: u32,
    pool_len: usize,
) -> anyhow::Result<()> {
    let start = offset as usize;
    let Some(end) = start.checked_add(len as usize) else {
        anyhow::bail!("record {idx} {kind} range overflow");
    };
    anyhow::ensure!(end <= pool_len, "record {idx} {kind} range out of bounds");
    Ok(())
}

fn validate_utf8_range(
    kind: &str,
    idx: usize,
    offset: u32,
    len: u32,
    pool: &[u8],
) -> anyhow::Result<()> {
    let start = offset as usize;
    let end = start + len as usize;
    std::str::from_utf8(&pool[start..end])
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("record {idx} {kind} is not UTF-8: {e}"))
}

fn write_indexed_records(mut w: impl Write, records: &[IndexedRecord]) -> anyhow::Result<()> {
    w.write_u64::<LittleEndian>(records.len() as u64)?;
    for rec in records {
        w.write_u32::<LittleEndian>(rec.parent)?;
        w.write_u64::<LittleEndian>(rec.size)?;
        w.write_i64::<LittleEndian>(rec.mtime)?;
        w.write_u32::<LittleEndian>(rec.name_offset)?;
        w.write_u32::<LittleEndian>(rec.name_len)?;
        w.write_u32::<LittleEndian>(rec.folded_offset)?;
        w.write_u32::<LittleEndian>(rec.folded_len)?;
        w.write_u8(rec.flags)?;
    }
    Ok(())
}

fn read_indexed_records(mut r: impl Read) -> anyhow::Result<Vec<IndexedRecord>> {
    let len = r.read_u64::<LittleEndian>()?;
    anyhow::ensure!(len <= 500_000_000, "snapshot record count is too large");
    let mut records = Vec::with_capacity(len as usize);
    for _ in 0..len {
        records.push(IndexedRecord {
            parent: r.read_u32::<LittleEndian>()?,
            size: r.read_u64::<LittleEndian>()?,
            mtime: r.read_i64::<LittleEndian>()?,
            name_offset: r.read_u32::<LittleEndian>()?,
            name_len: r.read_u32::<LittleEndian>()?,
            folded_offset: r.read_u32::<LittleEndian>()?,
            folded_len: r.read_u32::<LittleEndian>()?,
            flags: r.read_u8()?,
        });
    }
    Ok(records)
}

fn write_trigrams(mut w: impl Write, entries: &[TrigramEntry]) -> anyhow::Result<()> {
    w.write_u64::<LittleEndian>(entries.len() as u64)?;
    for entry in entries {
        w.write_u32::<LittleEndian>(entry.trigram)?;
        w.write_u32::<LittleEndian>(entry.record_idx)?;
    }
    Ok(())
}

fn read_trigrams(mut r: impl Read) -> anyhow::Result<Vec<TrigramEntry>> {
    let len = r.read_u64::<LittleEndian>()?;
    anyhow::ensure!(len <= 16_000_000_000, "snapshot trigram index is too large");
    let mut entries = Vec::with_capacity(len.min(usize::MAX as u64) as usize);
    for _ in 0..len {
        entries.push(TrigramEntry {
            trigram: r.read_u32::<LittleEndian>()?,
            record_idx: r.read_u32::<LittleEndian>()?,
        });
    }
    Ok(entries)
}

fn write_u32_vec(mut w: impl Write, values: &[u32]) -> anyhow::Result<()> {
    w.write_u64::<LittleEndian>(values.len() as u64)?;
    for &value in values {
        w.write_u32::<LittleEndian>(value)?;
    }
    Ok(())
}

fn read_u32_vec(mut r: impl Read) -> anyhow::Result<Vec<u32>> {
    let len = r.read_u64::<LittleEndian>()?;
    anyhow::ensure!(len <= 500_000_000, "snapshot vector is too large");
    let mut values = Vec::with_capacity(len as usize);
    for _ in 0..len {
        values.push(r.read_u32::<LittleEndian>()?);
    }
    Ok(values)
}

fn write_bytes(mut w: impl Write, bytes: &[u8]) -> anyhow::Result<()> {
    w.write_u64::<LittleEndian>(bytes.len() as u64)?;
    w.write_all(bytes)?;
    Ok(())
}

fn read_bytes(mut r: impl Read, max: u64) -> anyhow::Result<Vec<u8>> {
    let len = r.read_u64::<LittleEndian>()?;
    anyhow::ensure!(len <= max, "snapshot byte vector is too large");
    let mut bytes = vec![0u8; len as usize];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn write_string(mut w: impl Write, s: &str) -> anyhow::Result<()> {
    write_bytes(&mut w, s.as_bytes())
}

fn read_string(mut r: impl Read) -> anyhow::Result<String> {
    let bytes = read_bytes(&mut r, 16 * 1024 * 1024)?;
    Ok(String::from_utf8(bytes)?)
}

fn write_string_vec(mut w: impl Write, values: &[String]) -> anyhow::Result<()> {
    w.write_u64::<LittleEndian>(values.len() as u64)?;
    for value in values {
        write_string(&mut w, value)?;
    }
    Ok(())
}

fn read_string_vec(mut r: impl Read) -> anyhow::Result<Vec<String>> {
    let len = r.read_u64::<LittleEndian>()?;
    anyhow::ensure!(len <= 500_000_000, "snapshot string vector is too large");
    let mut values = Vec::with_capacity(len as usize);
    for _ in 0..len {
        values.push(read_string(&mut r)?);
    }
    Ok(values)
}

fn escape_device_id(device_id: &str) -> String {
    device_id
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::SearchIndex;
    use crate::model::{DeviceMetadata, FsType, ROOT_PARENT, ScanDatabase};

    fn sample_index_with_fs(fs_type: FsType) -> SearchIndex {
        let mut scan = ScanDatabase::new(fs_type);
        scan.push_record(ROOT_PARENT, "", 0, 0, true, false)
            .unwrap();
        scan.push_record(0, "hello.txt", 5, 100, false, false)
            .unwrap();
        SearchIndex::from_scan(
            DeviceMetadata {
                device_id: "uuid:test".into(),
                dev_node: "/dev/test".into(),
                fs_type,
                label: "Test".into(),
                uuid: "test".into(),
                partuuid: String::new(),
            },
            scan,
            42,
        )
        .unwrap()
    }

    fn sample_index() -> SearchIndex {
        sample_index_with_fs(FsType::Ext4)
    }

    #[test]
    fn snapshot_roundtrip() {
        let index = sample_index();
        let mut bytes = Vec::new();
        write_index(&mut bytes, &index).unwrap();
        let decoded = read_index(&bytes[..]).unwrap();
        assert_eq!(decoded.metadata.device_id, "uuid:test");
        assert_eq!(decoded.name(1), "hello.txt");
        assert_eq!(
            decoded.search(
                "HELLO",
                crate::model::SortKey::Name,
                crate::model::SortDirection::Asc
            ),
            vec![1]
        );
    }

    #[test]
    fn snapshot_rejects_truncation() {
        let index = sample_index();
        let mut bytes = Vec::new();
        write_index(&mut bytes, &index).unwrap();
        bytes.truncate(bytes.len() / 2);

        assert!(read_index(&bytes[..]).is_err());
    }

    #[test]
    fn snapshot_rejects_duplicate_sort_order_entry() {
        let mut index = sample_index();
        index.order_by_name = vec![0, 0];

        let mut bytes = Vec::new();
        write_index(&mut bytes, &index).unwrap();

        assert!(read_index(&bytes[..]).is_err());
    }

    #[test]
    fn snapshot_ext3_roundtrip() {
        let index = sample_index_with_fs(FsType::Ext3);
        let mut bytes = Vec::new();
        write_index(&mut bytes, &index).unwrap();
        let decoded = read_index(&bytes[..]).unwrap();
        assert_eq!(decoded.metadata.fs_type, FsType::Ext3);
    }

    #[test]
    fn index_state_summary_carries_v4_health_fields() {
        let mut state = IndexStateV1::new("partuuid:test");
        state.last_success_time = Some(10);
        state.last_error = Some("boom".into());
        state.last_scanner = Some("scannerd".into());
        state.snapshot_size = Some(42);
        state.stale_reason = Some("watch_desync".into());
        let summary = state.summary();
        assert_eq!(summary.last_success_time, Some(10));
        assert_eq!(summary.last_error.as_deref(), Some("boom"));
        assert_eq!(summary.last_scanner.as_deref(), Some("scannerd"));
        assert_eq!(summary.snapshot_size, Some(42));
        assert_eq!(summary.stale_reason.as_deref(), Some("watch_desync"));
    }
}
