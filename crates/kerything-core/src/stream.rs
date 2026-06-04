use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::model::{FLAG_IS_DIR, FLAG_IS_SYMLINK, FsType, ROOT_PARENT, ScanDatabase, ScanRecord};

const MAGIC: &[u8; 8] = b"KRYSCAN1";
const VERSION: u32 = 1;

pub fn write_scan_stream(mut w: impl Write, db: &ScanDatabase) -> anyhow::Result<()> {
    w.write_all(MAGIC)?;
    w.write_u32::<LittleEndian>(VERSION)?;
    w.write_u8(match db.fs_type {
        FsType::Ntfs => 1,
        FsType::Ext4 => 2,
        FsType::Btrfs => 3,
    })?;
    w.write_u64::<LittleEndian>(db.records.len() as u64)?;
    for rec in &db.records {
        w.write_u32::<LittleEndian>(rec.parent)?;
        w.write_u64::<LittleEndian>(rec.size)?;
        w.write_i64::<LittleEndian>(rec.mtime)?;
        w.write_u32::<LittleEndian>(rec.name_offset)?;
        w.write_u32::<LittleEndian>(rec.name_len)?;
        w.write_u8(rec.flags)?;
    }
    w.write_u64::<LittleEndian>(db.string_pool.len() as u64)?;
    w.write_all(&db.string_pool)?;
    Ok(())
}

pub fn read_scan_stream(mut r: impl Read) -> anyhow::Result<ScanDatabase> {
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    anyhow::ensure!(&magic == MAGIC, "scan stream magic mismatch");

    let version = r.read_u32::<LittleEndian>()?;
    anyhow::ensure!(
        version == VERSION,
        "unsupported scan stream version {version}"
    );

    let fs_type = match r.read_u8()? {
        1 => FsType::Ntfs,
        2 => FsType::Ext4,
        3 => FsType::Btrfs,
        other => anyhow::bail!("unknown scan stream filesystem tag {other}"),
    };

    let record_count = r.read_u64::<LittleEndian>()?;
    anyhow::ensure!(
        record_count <= 500_000_000,
        "scan stream record count is too large"
    );
    let mut records = Vec::with_capacity(record_count as usize);
    for _ in 0..record_count {
        records.push(ScanRecord {
            parent: r.read_u32::<LittleEndian>()?,
            size: r.read_u64::<LittleEndian>()?,
            mtime: r.read_i64::<LittleEndian>()?,
            name_offset: r.read_u32::<LittleEndian>()?,
            name_len: r.read_u32::<LittleEndian>()?,
            flags: r.read_u8()?,
        });
    }

    let pool_len = r.read_u64::<LittleEndian>()?;
    anyhow::ensure!(
        pool_len <= 8 * 1024 * 1024 * 1024,
        "scan stream string pool is too large"
    );
    let mut string_pool = vec![0; pool_len as usize];
    r.read_exact(&mut string_pool)?;

    for (idx, rec) in records.iter().enumerate() {
        let start = rec.name_offset as usize;
        let len = rec.name_len as usize;
        let Some(end) = start.checked_add(len) else {
            anyhow::bail!("record {idx} name range overflow");
        };
        anyhow::ensure!(
            end <= string_pool.len(),
            "record {idx} name range out of bounds"
        );
        std::str::from_utf8(&string_pool[start..end])
            .map_err(|e| anyhow::anyhow!("record {idx} name is not UTF-8: {e}"))?;
        if rec.parent != ROOT_PARENT {
            anyhow::ensure!(
                (rec.parent as usize) < records.len(),
                "record {idx} parent out of bounds"
            );
        }
        anyhow::ensure!(
            rec.flags & !(FLAG_IS_DIR | FLAG_IS_SYMLINK) == 0,
            "record {idx} has unknown flags"
        );
    }

    Ok(ScanDatabase {
        fs_type,
        records,
        string_pool,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ROOT_PARENT;

    #[test]
    fn scan_stream_roundtrip() {
        let mut db = ScanDatabase::new(FsType::Ext4);
        db.push_record(ROOT_PARENT, "", 0, 0, true, false).unwrap();
        db.push_record(0, "résumé.txt", 42, 123, false, false)
            .unwrap();

        let mut buf = Vec::new();
        write_scan_stream(&mut buf, &db).unwrap();
        let decoded = read_scan_stream(&buf[..]).unwrap();

        assert_eq!(decoded.fs_type, FsType::Ext4);
        assert_eq!(decoded.records.len(), 2);
        assert_eq!(decoded.name(1), "résumé.txt");
    }

    #[test]
    fn scan_stream_rejects_invalid_parent() {
        let mut db = ScanDatabase::new(FsType::Ext4);
        db.push_record(ROOT_PARENT, "", 0, 0, true, false).unwrap();
        db.push_record(99, "orphan.txt", 1, 2, false, false)
            .unwrap();

        let mut buf = Vec::new();
        write_scan_stream(&mut buf, &db).unwrap();

        assert!(read_scan_stream(&buf[..]).is_err());
    }

    #[test]
    fn scan_stream_rejects_truncation() {
        let mut db = ScanDatabase::new(FsType::Ntfs);
        db.push_record(ROOT_PARENT, "", 0, 0, true, false).unwrap();

        let mut buf = Vec::new();
        write_scan_stream(&mut buf, &db).unwrap();
        buf.pop();

        assert!(read_scan_stream(&buf[..]).is_err());
    }
}
