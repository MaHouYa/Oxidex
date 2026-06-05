use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use ntfs::structured_values::{NtfsFileAttributeFlags, NtfsFileName, NtfsFileNamespace};
use ntfs::{Ntfs, NtfsAttributeType, NtfsFileFlags};

use crate::model::{FsType, ROOT_PARENT, ScanDatabase};
use crate::scanner::{ProgressCallback, ScanCancellation};

const ROOT_MFT_RECORD: u64 = 5;

struct PendingRecord {
    mft_record: u64,
    parent_mft: u64,
    name: String,
    size: u64,
    mtime: i64,
    is_dir: bool,
    is_symlink: bool,
}

struct NameInfo {
    parent_mft: u64,
    namespace: NtfsFileNamespace,
    name: String,
    size: u64,
    mtime: i64,
    attrs: NtfsFileAttributeFlags,
}

pub fn scan(
    path: &Path,
    progress: &mut ProgressCallback<'_>,
    cancellation: &ScanCancellation,
) -> anyhow::Result<ScanDatabase> {
    progress(0, 1);
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let ntfs = Ntfs::new(&mut reader)?;

    let record_count = {
        let mft = ntfs.file(&mut reader, 0)?;
        let item = mft
            .data(&mut reader, "")
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("NTFS $MFT has no unnamed $DATA attribute"))?;
        let attr = item.to_attribute()?;
        attr.value_length() / u64::from(ntfs.file_record_size())
    };

    let mut pending = Vec::new();
    pending.push(PendingRecord {
        mft_record: ROOT_MFT_RECORD,
        parent_mft: u64::MAX,
        name: String::new(),
        size: 0,
        mtime: 0,
        is_dir: true,
        is_symlink: false,
    });

    for record_number in 0..record_count {
        if record_number & 4095 == 0 {
            cancellation.ensure_not_cancelled()?;
            progress(record_number, record_count.max(1));
        }

        let Ok(ntfs_file) = ntfs.file(&mut reader, record_number) else {
            continue;
        };
        if !ntfs_file.flags().contains(NtfsFileFlags::IN_USE) {
            continue;
        }

        let is_dir = ntfs_file.is_directory();
        let std_mtime = ntfs_file
            .info()
            .ok()
            .map(|info| ntfs_time_to_unix(info.modification_time().nt_timestamp()));

        let data_size = ntfs_file
            .data(&mut reader, "")
            .transpose()
            .ok()
            .flatten()
            .and_then(|item| item.to_attribute().ok().map(|attr| attr.value_length()));

        let mut names = Vec::new();
        let mut attrs = ntfs_file.attributes();
        while let Some(item) = attrs.next(&mut reader) {
            let Ok(item) = item else {
                continue;
            };
            let Ok(attr) = item.to_attribute() else {
                continue;
            };
            if attr.ty().ok() != Some(NtfsAttributeType::FileName) {
                continue;
            }
            let Ok(file_name) = attr.structured_value::<_, NtfsFileName>(&mut reader) else {
                continue;
            };

            names.push(NameInfo {
                parent_mft: file_name.parent_directory_reference().file_record_number(),
                namespace: file_name.namespace(),
                name: file_name.name().to_string_lossy(),
                size: file_name.data_size(),
                mtime: ntfs_time_to_unix(file_name.modification_time().nt_timestamp()),
                attrs: file_name.file_attributes(),
            });
        }

        for name in names_to_keep(&names) {
            if record_number <= 38 && name.name.starts_with('$') {
                continue;
            }
            if record_number == ROOT_MFT_RECORD && (name.name == "." || name.name.is_empty()) {
                continue;
            }

            let display_name = if name.name == "." {
                String::new()
            } else {
                name.name.clone()
            };
            let is_symlink = name.attrs.contains(NtfsFileAttributeFlags::REPARSE_POINT);

            pending.push(PendingRecord {
                mft_record: record_number,
                parent_mft: name.parent_mft,
                name: display_name,
                size: data_size.unwrap_or(name.size),
                mtime: std_mtime.unwrap_or(name.mtime),
                is_dir,
                is_symlink,
            });
        }
    }

    progress(1, 1);
    pending_to_scan_db(pending)
}

fn names_to_keep(names: &[NameInfo]) -> Vec<&NameInfo> {
    names
        .iter()
        .filter(|candidate| {
            if candidate.namespace != NtfsFileNamespace::Dos {
                return true;
            }

            !names.iter().any(|other| {
                other.namespace != NtfsFileNamespace::Dos
                    && other.parent_mft == candidate.parent_mft
            })
        })
        .collect()
}

fn pending_to_scan_db(pending: Vec<PendingRecord>) -> anyhow::Result<ScanDatabase> {
    let mut db = ScanDatabase::new(FsType::Ntfs);
    let mut mft_to_record = HashMap::new();
    for rec in &pending {
        let idx = db.push_record(
            ROOT_PARENT,
            &rec.name,
            rec.size,
            rec.mtime,
            rec.is_dir,
            rec.is_symlink,
        )?;
        mft_to_record.entry(rec.mft_record).or_insert(idx);
    }

    for (idx, rec) in pending.iter().enumerate() {
        let parent = if rec.parent_mft == u64::MAX {
            ROOT_PARENT
        } else {
            mft_to_record
                .get(&rec.parent_mft)
                .copied()
                .unwrap_or(ROOT_PARENT)
        };
        db.records[idx].parent = parent;
    }

    Ok(db)
}

fn ntfs_time_to_unix(filetime100ns: u64) -> i64 {
    const UNIX_EPOCH_IN_FILETIME_100NS: u64 = 116_444_736_000_000_000;
    const TICKS_PER_SECOND: u64 = 10_000_000;
    if filetime100ns < UNIX_EPOCH_IN_FILETIME_100NS {
        0
    } else {
        ((filetime100ns - UNIX_EPOCH_IN_FILETIME_100NS) / TICKS_PER_SECOND) as i64
    }
}
