//! manifest と CURRENT による世代管理 (docs/design/storage.md §4)。
//!
//! 有効なセグメント構成は `MANIFEST-<gen:010>` に記録され、`CURRENT` が
//! 有効な manifest 名を指す。切り替えは CURRENT.tmp への書き込み + atomic rename
//! で行い、どの時点でクラッシュしても完全な世代に復帰できる。

use std::fs::File;
use std::io::Write;
use std::path::Path;

use hamane_core::{Metric, Result};

use crate::format::{
    self, corrupted, metric_from_u8, metric_to_u8, put_string, Reader, MAGIC_MANIFEST,
};

pub const CURRENT_FILE: &str = "CURRENT";

/// manifest ファイル名: `MANIFEST-<gen:010>`
pub fn manifest_file_name(gen: u64) -> String {
    format!("MANIFEST-{gen:010}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentEntry {
    pub seg_id: u64,
    pub record_count: u64,
    pub tombstone_count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionEntry {
    pub collection_id: u32,
    pub name: String,
    pub dim: u32,
    pub metric: Metric,
    /// **年代順 (古い → 新しい)** で保持する (フォーマット v2、todo 506)。
    /// 部分コンパクション後は seg_id 順と一致しない。
    /// v1 (seg_id 昇順 = 年代順だった) はそのまま読める
    pub segments: Vec<SegmentEntry>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Manifest {
    pub gen: u64,
    pub next_collection_id: u32,
    pub next_seg_id: u64,
    /// この世代に反映済みの WAL seq (これ以下の WAL はリプレイ不要)
    pub wal_seq: u64,
    pub collections: Vec<CollectionEntry>,
}

/// v2 の magic (年代順セグメントリスト)。v1 (`MAGIC_MANIFEST`) も読める。
const MAGIC_MANIFEST_V2: [u8; 8] = *b"HAMANEF\x02";

impl Manifest {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_MANIFEST_V2);
        format::put_u64(&mut buf, self.gen);
        format::put_u32(&mut buf, self.next_collection_id);
        format::put_u64(&mut buf, self.next_seg_id);
        format::put_u64(&mut buf, self.wal_seq);
        format::put_u32(&mut buf, self.collections.len() as u32);
        for col in &self.collections {
            format::put_u32(&mut buf, col.collection_id);
            put_string(&mut buf, &col.name);
            format::put_u32(&mut buf, col.dim);
            format::put_u8(&mut buf, metric_to_u8(col.metric));
            format::put_u32(&mut buf, col.segments.len() as u32);
            for seg in &col.segments {
                format::put_u64(&mut buf, seg.seg_id);
                format::put_u64(&mut buf, seg.record_count);
                format::put_u64(&mut buf, seg.tombstone_count);
            }
        }
        let crc = crc32c::crc32c(&buf);
        format::put_u32(&mut buf, crc);
        buf
    }

    /// バイト列からデコードする (レプリカが fetch した manifest の解釈にも使う)。
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < MAGIC_MANIFEST.len() + 4
            || (buf[..8] != MAGIC_MANIFEST && buf[..8] != MAGIC_MANIFEST_V2)
        {
            return Err(corrupted("bad manifest magic"));
        }
        let is_v1 = buf[..8] == MAGIC_MANIFEST;
        let (content, footer) = buf.split_at(buf.len() - 4);
        let stored = u32::from_le_bytes(footer.try_into().unwrap());
        if crc32c::crc32c(content) != stored {
            return Err(corrupted("manifest checksum mismatch"));
        }
        let mut r = Reader::new(&content[8..]);
        let gen = r.u64()?;
        let next_collection_id = r.u32()?;
        let next_seg_id = r.u64()?;
        let wal_seq = r.u64()?;
        let collection_count = r.u32()?;
        let mut collections = Vec::with_capacity(collection_count as usize);
        for _ in 0..collection_count {
            let collection_id = r.u32()?;
            let name = r.string()?;
            let dim = r.u32()?;
            let metric = metric_from_u8(r.u8()?)?;
            let seg_count = r.u32()?;
            let mut segments = Vec::with_capacity(seg_count as usize);
            for _ in 0..seg_count {
                segments.push(SegmentEntry {
                    seg_id: r.u64()?,
                    record_count: r.u64()?,
                    tombstone_count: r.u64()?,
                });
            }
            // v1 は「seg_id 昇順 = 年代順」の不変条件を検証する。
            // v2 は年代順リストであり seg_id 順とは限らない (部分マージの結果が
            // 古い位置に入るため)
            if is_v1 && !segments.windows(2).all(|w| w[0].seg_id < w[1].seg_id) {
                return Err(corrupted("v1 segments not in ascending seg_id order"));
            }
            collections.push(CollectionEntry {
                collection_id,
                name,
                dim,
                metric,
                segments,
            });
        }
        if !r.is_empty() {
            return Err(corrupted("trailing bytes in manifest"));
        }
        Ok(Manifest {
            gen,
            next_collection_id,
            next_seg_id,
            wal_seq,
            collections,
        })
    }

    /// この manifest を `MANIFEST-<gen>` として書き、CURRENT を切り替える。
    ///
    /// 手順 (storage.md §4): manifest 書き込み + fsync → CURRENT.tmp + fsync →
    /// rename → 親ディレクトリ fsync。
    pub fn store(&self, db_dir: &Path) -> Result<()> {
        let name = manifest_file_name(self.gen);
        let path = db_dir.join(&name);
        let mut file = File::create(&path)?;
        file.write_all(&self.encode())?;
        file.sync_data()?;

        let tmp = db_dir.join("CURRENT.tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(name.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        std::fs::rename(&tmp, db_dir.join(CURRENT_FILE))?;
        File::open(db_dir)?.sync_all()?;
        Ok(())
    }

    /// CURRENT が指す manifest を読み込む。
    pub fn load(db_dir: &Path) -> Result<Self> {
        let current = std::fs::read_to_string(db_dir.join(CURRENT_FILE))?;
        let name = current.trim();
        if !name.starts_with("MANIFEST-") {
            return Err(corrupted(format!("CURRENT points to invalid name: {name}")));
        }
        let buf = std::fs::read(db_dir.join(name))?;
        let manifest = Self::decode(&buf)?;
        if manifest_file_name(manifest.gen) != name {
            return Err(corrupted("manifest gen does not match file name"));
        }
        Ok(manifest)
    }

    /// CURRENT が指さない MANIFEST と .tmp 残骸を削除する。
    pub fn gc(db_dir: &Path) -> Result<()> {
        let live = std::fs::read_to_string(db_dir.join(CURRENT_FILE))?;
        let live = live.trim();
        for entry in std::fs::read_dir(db_dir)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let is_stale_manifest = name.starts_with("MANIFEST-") && name != live && path.is_file();
            let is_tmp = name.ends_with(".tmp") && path.is_file();
            if is_stale_manifest || is_tmp {
                std::fs::remove_file(&path)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest(gen: u64) -> Manifest {
        Manifest {
            gen,
            next_collection_id: 3,
            next_seg_id: 10,
            wal_seq: 7,
            collections: vec![
                CollectionEntry {
                    collection_id: 1,
                    name: "docs".into(),
                    dim: 768,
                    metric: Metric::Cosine,
                    segments: vec![
                        SegmentEntry {
                            seg_id: 1,
                            record_count: 100,
                            tombstone_count: 0,
                        },
                        SegmentEntry {
                            seg_id: 5,
                            record_count: 20,
                            tombstone_count: 3,
                        },
                    ],
                },
                CollectionEntry {
                    collection_id: 2,
                    name: "画像".into(),
                    dim: 512,
                    metric: Metric::L2,
                    segments: vec![],
                },
            ],
        }
    }

    #[test]
    fn store_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let m = sample_manifest(42);
        m.store(dir.path()).unwrap();
        assert_eq!(Manifest::load(dir.path()).unwrap(), m);
    }

    #[test]
    fn generation_switch_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let old = sample_manifest(1);
        old.store(dir.path()).unwrap();
        let new = sample_manifest(2);
        new.store(dir.path()).unwrap();
        assert_eq!(Manifest::load(dir.path()).unwrap().gen, 2);
    }

    #[test]
    fn crash_between_steps_recovers_to_complete_generation() {
        // ステップ 1 の後 (新 manifest はあるが CURRENT は旧) → 旧世代が読める
        let dir = tempfile::tempdir().unwrap();
        let old = sample_manifest(1);
        old.store(dir.path()).unwrap();
        let new = sample_manifest(2);
        std::fs::write(dir.path().join(manifest_file_name(2)), new.encode()).unwrap();
        assert_eq!(Manifest::load(dir.path()).unwrap(), old);

        // ステップ 2 の後 (CURRENT.tmp が残っている) → まだ旧世代
        std::fs::write(dir.path().join("CURRENT.tmp"), "MANIFEST-0000000002\n").unwrap();
        assert_eq!(Manifest::load(dir.path()).unwrap(), old);

        // ステップ 3 (rename 完了) → 新世代
        std::fs::rename(
            dir.path().join("CURRENT.tmp"),
            dir.path().join(CURRENT_FILE),
        )
        .unwrap();
        assert_eq!(Manifest::load(dir.path()).unwrap(), new);
    }

    #[test]
    fn corrupted_manifest_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let m = sample_manifest(1);
        m.store(dir.path()).unwrap();

        let path = dir.path().join(manifest_file_name(1));
        let mut buf = std::fs::read(&path).unwrap();
        buf[10] ^= 0xFF;
        std::fs::write(&path, &buf).unwrap();
        assert!(Manifest::load(dir.path()).is_err());
    }

    #[test]
    fn gc_removes_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        sample_manifest(1).store(dir.path()).unwrap();
        sample_manifest(2).store(dir.path()).unwrap();
        std::fs::write(dir.path().join("CURRENT.tmp"), "junk").unwrap();

        Manifest::gc(dir.path()).unwrap();
        assert!(!dir.path().join(manifest_file_name(1)).exists());
        assert!(!dir.path().join("CURRENT.tmp").exists());
        assert_eq!(Manifest::load(dir.path()).unwrap().gen, 2);
    }
}
