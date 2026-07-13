//! 不変セグメントの書き出しと mmap 読み込み (docs/design/storage.md §3)。
//!
//! セグメントは `seg-<id:06>/` ディレクトリ内の 4 ファイル
//! (vectors.bin / ids.bin / meta.bin / tombstones.bin)。
//! 各ファイルの footer に「先頭から footer 直前まで」の CRC32C を置く。
//! 書き出しは `.tmp` ディレクトリに全ファイルを書いてから rename する。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use hamane_core::{Id, Metadata, Metric, Result};
use hamane_index::{HnswBuilder, HnswGraph, HnswParams, HnswView, VectorSource};
use memmap2::Mmap;

use crate::format::{
    self, corrupted, put_metadata, read_metadata, Reader, MAGIC_HNSW, MAGIC_IDS, MAGIC_META,
    MAGIC_TOMBSTONES, MAGIC_VECTORS,
};
use crate::memtable::MemtableSnapshot;

const VECTORS_HEADER_LEN: usize = 64; // magic[8] + dim u32 + count u64 + pad
const PLAIN_HEADER_LEN: usize = 16; // magic[8] + count u64

pub const FILE_VECTORS: &str = "vectors.bin";
pub const FILE_IDS: &str = "ids.bin";
pub const FILE_META: &str = "meta.bin";
pub const FILE_TOMBSTONES: &str = "tombstones.bin";
pub const FILE_HNSW: &str = "hnsw.bin";

/// フラッシュ時のインデックス構築指定 (docs/design/index.md §4)。
#[derive(Debug, Clone, Copy)]
pub struct IndexBuildSpec {
    pub metric: Metric,
    pub params: HnswParams,
    /// この行数未満のセグメントは HNSW を作らない (Flat で十分)
    pub min_rows: usize,
}

/// 行ベクトル列の VectorSource アダプタ (フラッシュ時の HNSW 構築用)。
struct RowsSource<'a>(Vec<&'a [f32]>);

impl VectorSource for RowsSource<'_> {
    fn len(&self) -> u32 {
        self.0.len() as u32
    }
    fn vector(&self, row: u32) -> &[f32] {
        self.0[row as usize]
    }
}

/// セグメントディレクトリ名: `seg-<id:06>`
pub fn segment_dir_name(seg_id: u64) -> String {
    format!("seg-{seg_id:06}")
}

/// manifest に載せるセグメントの要約。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentMeta {
    pub seg_id: u64,
    pub record_count: u64,
    pub tombstone_count: u64,
}

/// 親ディレクトリを fsync する (rename の永続化)。
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

fn write_file_with_crc(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = File::create(path)?;
    file.write_all(content)?;
    file.write_all(&crc32c::crc32c(content).to_le_bytes())?;
    file.sync_data()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 書き出し
// ---------------------------------------------------------------------------

pub struct SegmentWriter;

impl SegmentWriter {
    /// memtable の内容をセグメントとして書き出す。
    ///
    /// 行順は id 昇順 (決定的)。`.tmp` に全ファイルを書いて fsync した後
    /// rename で確定し、親ディレクトリを fsync する。
    /// `index` を指定し行数が min_rows 以上なら HNSW も構築・永続化する
    /// (seed は seg_id で固定 = 決定的構築)。
    pub fn write(
        collection_dir: &Path,
        seg_id: u64,
        memtable: &MemtableSnapshot,
        index: Option<IndexBuildSpec>,
    ) -> Result<SegmentMeta> {
        let final_dir = collection_dir.join(segment_dir_name(seg_id));
        let tmp_dir = collection_dir.join(format!("{}.tmp", segment_dir_name(seg_id)));
        if tmp_dir.exists() {
            std::fs::remove_dir_all(&tmp_dir)?;
        }
        // 前回の失敗 (フラッシュ再試行) の残骸。manifest 未参照なので消して安全
        if final_dir.exists() {
            std::fs::remove_dir_all(&final_dir)?;
        }
        std::fs::create_dir_all(&tmp_dir)?;

        // 行順 = id 昇順
        let mut rows: Vec<(Id, &[f32], &Metadata)> = memtable.iter().collect();
        rows.sort_by_key(|(id, _, _)| *id);
        let count = rows.len() as u64;

        // vectors.bin
        let dim = rows.first().map(|(_, v, _)| v.len()).unwrap_or(0) as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_VECTORS);
        format::put_u32(&mut buf, dim);
        format::put_u64(&mut buf, count);
        buf.resize(VECTORS_HEADER_LEN, 0);
        for (_, vector, _) in &rows {
            format::put_f32_slice(&mut buf, vector);
        }
        write_file_with_crc(&tmp_dir.join(FILE_VECTORS), &buf)?;

        // ids.bin: 行順 id 列 + (id, row) 昇順索引
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_IDS);
        format::put_u64(&mut buf, count);
        for (id, _, _) in &rows {
            format::put_u64(&mut buf, *id);
        }
        for (row, (id, _, _)) in rows.iter().enumerate() {
            format::put_u64(&mut buf, *id);
            format::put_u32(&mut buf, row as u32);
        }
        write_file_with_crc(&tmp_dir.join(FILE_IDS), &buf)?;

        // meta.bin: offsets + blob 連結
        let mut blobs = Vec::new();
        let mut offsets = Vec::with_capacity(rows.len() + 1);
        offsets.push(0u64);
        for (_, _, meta) in &rows {
            if !meta.is_empty() {
                put_metadata(&mut blobs, meta);
            }
            offsets.push(blobs.len() as u64);
        }
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_META);
        format::put_u64(&mut buf, count);
        for off in &offsets {
            format::put_u64(&mut buf, *off);
        }
        buf.extend_from_slice(&blobs);
        write_file_with_crc(&tmp_dir.join(FILE_META), &buf)?;

        // tombstones.bin: 昇順 id 列
        let mut tombstones: Vec<Id> = memtable.deletes().collect();
        tombstones.sort_unstable();
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_TOMBSTONES);
        format::put_u64(&mut buf, tombstones.len() as u64);
        for id in &tombstones {
            format::put_u64(&mut buf, *id);
        }
        write_file_with_crc(&tmp_dir.join(FILE_TOMBSTONES), &buf)?;

        // hnsw.bin (任意)
        if let Some(spec) = index {
            if rows.len() >= spec.min_rows && !rows.is_empty() {
                let source = RowsSource(rows.iter().map(|(_, v, _)| *v).collect());
                let params = HnswParams {
                    seed: seg_id,
                    ..spec.params
                };
                let builder = HnswBuilder::build(&source, spec.metric, params);
                write_file_with_crc(&tmp_dir.join(FILE_HNSW), &builder.serialize())?;
            }
        }

        // 確定: rename + 親ディレクトリ fsync
        std::fs::rename(&tmp_dir, &final_dir)?;
        sync_dir(collection_dir)?;

        Ok(SegmentMeta {
            seg_id,
            record_count: count,
            tombstone_count: tombstones.len() as u64,
        })
    }
}

// ---------------------------------------------------------------------------
// 読み込み
// ---------------------------------------------------------------------------

struct MappedFile {
    mmap: Mmap,
}

impl MappedFile {
    fn open(path: &Path, magic: &[u8; 8]) -> Result<Self> {
        let file = File::open(path)?;
        // Safety: セグメントは不変であり、開いた後に書き換えられない前提
        let mmap = unsafe { Mmap::map(&file)? };
        if mmap.len() < magic.len() + 4 || &mmap[..8] != magic {
            return Err(corrupted(format!("bad magic in {}", path.display())));
        }
        Ok(Self { mmap })
    }

    /// footer (CRC) を除いた本体。
    fn content(&self) -> &[u8] {
        &self.mmap[..self.mmap.len() - 4]
    }

    fn verify_checksum(&self, name: &str) -> Result<()> {
        let content = self.content();
        let stored = u32::from_le_bytes(self.mmap[self.mmap.len() - 4..].try_into().unwrap());
        if crc32c::crc32c(content) != stored {
            return Err(corrupted(format!("checksum mismatch in {name}")));
        }
        Ok(())
    }

    fn u64_at(&self, offset: usize) -> Result<u64> {
        let bytes = self
            .content()
            .get(offset..offset + 8)
            .ok_or_else(|| corrupted("offset out of range"))?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }
}

/// mmap で開いた不変セグメント。`Arc<Segment>` で共有される。
pub struct Segment {
    seg_id: u64,
    dim: usize,
    count: usize,
    tombstone_count: usize,
    vectors: MappedFile,
    ids: MappedFile,
    meta: MappedFile,
    tombstones: MappedFile,
    /// hnsw.bin (存在する場合のみ)。ビューは `hnsw()` で都度パースする
    hnsw: Option<MappedFile>,
}

impl VectorSource for Segment {
    fn len(&self) -> u32 {
        self.count as u32
    }
    fn vector(&self, row: u32) -> &[f32] {
        Segment::vector(self, row)
    }
}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segment")
            .field("seg_id", &self.seg_id)
            .field("dim", &self.dim)
            .field("count", &self.count)
            .field("tombstone_count", &self.tombstone_count)
            .finish()
    }
}

impl Segment {
    /// セグメントディレクトリを開く。ヘッダ整合のみ検証する
    /// (全 CRC 検証は `verify_checksums`)。
    pub fn open(collection_dir: &Path, seg_id: u64) -> Result<Segment> {
        let dir = collection_dir.join(segment_dir_name(seg_id));
        let vectors = MappedFile::open(&dir.join(FILE_VECTORS), &MAGIC_VECTORS)?;
        let ids = MappedFile::open(&dir.join(FILE_IDS), &MAGIC_IDS)?;
        let meta = MappedFile::open(&dir.join(FILE_META), &MAGIC_META)?;
        let tombstones = MappedFile::open(&dir.join(FILE_TOMBSTONES), &MAGIC_TOMBSTONES)?;
        let hnsw = if dir.join(FILE_HNSW).exists() {
            Some(MappedFile::open(&dir.join(FILE_HNSW), &MAGIC_HNSW)?)
        } else {
            None
        };

        let dim = u32::from_le_bytes(vectors.content()[8..12].try_into().unwrap()) as usize;
        let count = vectors.u64_at(12)? as usize;

        // 各ファイルの count・サイズ整合
        if ids.u64_at(8)? as usize != count || meta.u64_at(8)? as usize != count {
            return Err(corrupted("count mismatch across segment files"));
        }
        let expected_vectors = VECTORS_HEADER_LEN + count * dim * 4;
        if vectors.content().len() != expected_vectors {
            return Err(corrupted("vectors.bin size mismatch"));
        }
        let expected_ids = PLAIN_HEADER_LEN + count * 8 + count * 12;
        if ids.content().len() != expected_ids {
            return Err(corrupted("ids.bin size mismatch"));
        }
        if meta.content().len() < PLAIN_HEADER_LEN + (count + 1) * 8 {
            return Err(corrupted("meta.bin too small"));
        }
        let tombstone_count = tombstones.u64_at(8)? as usize;
        if tombstones.content().len() != PLAIN_HEADER_LEN + tombstone_count * 8 {
            return Err(corrupted("tombstones.bin size mismatch"));
        }

        // f32 アクセスに必要なアラインメント (mmap はページ境界なので通常成立)
        let data_ptr = vectors.content()[VECTORS_HEADER_LEN..].as_ptr();
        if !(data_ptr as usize).is_multiple_of(std::mem::align_of::<f32>()) {
            return Err(corrupted("vectors.bin data is not 4-byte aligned"));
        }

        let segment = Segment {
            seg_id,
            dim,
            count,
            tombstone_count,
            vectors,
            ids,
            meta,
            tombstones,
            hnsw,
        };
        // hnsw.bin があれば構造を一度検証しておく (行数の整合含む)
        if segment.hnsw.is_some() {
            segment.hnsw()?;
        }
        Ok(segment)
    }

    /// 全ファイルの CRC を検証する (open 時は省略される)。
    pub fn verify_checksums(&self) -> Result<()> {
        self.vectors.verify_checksum(FILE_VECTORS)?;
        self.ids.verify_checksum(FILE_IDS)?;
        self.meta.verify_checksum(FILE_META)?;
        self.tombstones.verify_checksum(FILE_TOMBSTONES)?;
        if let Some(h) = &self.hnsw {
            h.verify_checksum(FILE_HNSW)?;
        }
        Ok(())
    }

    /// HNSW ビューを返す (hnsw.bin がないセグメントは None)。
    /// パースは軽量 (層数に比例) なので呼び出しごとに行う。
    pub fn hnsw(&self) -> Result<Option<HnswView<'_>>> {
        let Some(mapped) = &self.hnsw else {
            return Ok(None);
        };
        let view = HnswView::open(mapped.content())?;
        if view.node_count() as usize != self.count {
            return Err(corrupted("hnsw node count does not match segment rows"));
        }
        Ok(Some(view))
    }

    /// このセグメントが HNSW を持つか。
    pub fn has_hnsw(&self) -> bool {
        self.hnsw.is_some()
    }

    pub fn seg_id(&self) -> u64 {
        self.seg_id
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// レコード数 (行数)。
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn tombstone_count(&self) -> usize {
        self.tombstone_count
    }

    /// 行のベクトルを zero-copy で返す。
    pub fn vector(&self, row: u32) -> &[f32] {
        assert!((row as usize) < self.count, "row out of range");
        let start = VECTORS_HEADER_LEN + row as usize * self.dim * 4;
        let bytes = &self.vectors.content()[start..start + self.dim * 4];
        // Safety: open 時に 4 バイトアラインメントと範囲を検証済み。
        // 行先頭は data 先頭 (64B 境界) + dim*4 の倍数なので 4 バイト境界に乗る
        unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const f32, self.dim) }
    }

    /// 行の id。
    pub fn id(&self, row: u32) -> Id {
        assert!((row as usize) < self.count, "row out of range");
        let off = PLAIN_HEADER_LEN + row as usize * 8;
        u64::from_le_bytes(self.ids.content()[off..off + 8].try_into().unwrap())
    }

    /// id から行番号を引く (二分探索)。
    pub fn row_of(&self, id: Id) -> Option<u32> {
        let base = PLAIN_HEADER_LEN + self.count * 8;
        let entry = |i: usize| -> (u64, u32) {
            let off = base + i * 12;
            let content = self.ids.content();
            (
                u64::from_le_bytes(content[off..off + 8].try_into().unwrap()),
                u32::from_le_bytes(content[off + 8..off + 12].try_into().unwrap()),
            )
        };
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let (mid_id, row) = entry(mid);
            match mid_id.cmp(&id) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(row),
            }
        }
        None
    }

    /// この id がセグメントに存在するか。
    pub fn contains(&self, id: Id) -> bool {
        self.row_of(id).is_some()
    }

    /// 行のメタデータをデコードして返す。
    pub fn metadata(&self, row: u32) -> Result<Metadata> {
        assert!((row as usize) < self.count, "row out of range");
        let offsets_base = PLAIN_HEADER_LEN;
        let blob_base = PLAIN_HEADER_LEN + (self.count + 1) * 8;
        let start = self.meta.u64_at(offsets_base + row as usize * 8)? as usize;
        let end = self.meta.u64_at(offsets_base + (row as usize + 1) * 8)? as usize;
        if start == end {
            return Ok(Metadata::new());
        }
        let blob = self
            .meta
            .content()
            .get(blob_base + start..blob_base + end)
            .ok_or_else(|| corrupted("metadata blob out of range"))?;
        read_metadata(&mut Reader::new(blob))
    }

    /// i 番目の tombstone id (昇順)。i < tombstone_count() であること。
    pub fn tombstone_at(&self, i: usize) -> Id {
        assert!(i < self.tombstone_count, "tombstone index out of range");
        let off = PLAIN_HEADER_LEN + i * 8;
        u64::from_le_bytes(self.tombstones.content()[off..off + 8].try_into().unwrap())
    }

    /// このセグメントの tombstone に id が含まれるか (二分探索)。
    pub fn is_tombstoned(&self, id: Id) -> bool {
        let content = self.tombstones.content();
        let at = |i: usize| -> u64 {
            let off = PLAIN_HEADER_LEN + i * 8;
            u64::from_le_bytes(content[off..off + 8].try_into().unwrap())
        };
        let (mut lo, mut hi) = (0usize, self.tombstone_count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match at(mid).cmp(&id) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }

    /// 全行のメタデータをデコードする (Flat 検索用)。
    pub fn decode_all_metadata(&self) -> Result<Vec<Metadata>> {
        (0..self.count as u32)
            .map(|row| self.metadata(row))
            .collect()
    }
}

/// collection ディレクトリ配下の `.tmp` 残骸を削除する (復旧時の掃除)。
pub fn remove_tmp_dirs(collection_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    if !collection_dir.exists() {
        return Ok(removed);
    }
    for entry in std::fs::read_dir(collection_dir)? {
        let path = entry?.path();
        if path.is_dir()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".tmp"))
        {
            std::fs::remove_dir_all(&path)?;
            removed.push(path);
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memtable::{Memtable, StoredRecord};
    use hamane_core::MetaValue;

    fn sample_memtable() -> Memtable {
        let mut mt = Memtable::new();
        for i in [5u64, 1, 3, 2, 4] {
            let mut meta = Metadata::new();
            if i % 2 == 1 {
                meta.insert("odd".into(), MetaValue::Bool(true));
                meta.insert("name".into(), MetaValue::Str(format!("rec-{i}")));
            }
            mt.upsert(
                i,
                StoredRecord {
                    vector: vec![i as f32, -(i as f32), 0.5],
                    metadata: meta,
                },
            );
        }
        mt.delete(100);
        mt.delete(50);
        mt
    }

    fn write_and_open(dir: &Path, mt: &Memtable) -> (SegmentMeta, Segment) {
        let meta = SegmentWriter::write(dir, 1, &mt.snapshot(), None).unwrap();
        let seg = Segment::open(dir, 1).unwrap();
        (meta, seg)
    }

    #[test]
    fn roundtrip_all_accessors() {
        let dir = tempfile::tempdir().unwrap();
        let mt = sample_memtable();
        let (meta, seg) = write_and_open(dir.path(), &mt);

        assert_eq!(meta.record_count, 5);
        assert_eq!(meta.tombstone_count, 2);
        assert_eq!(seg.len(), 5);
        assert_eq!(seg.dim(), 3);
        seg.verify_checksums().unwrap();

        // 行順は id 昇順
        for (row, expected_id) in (0..5u32).zip([1u64, 2, 3, 4, 5]) {
            assert_eq!(seg.id(row), expected_id);
            assert_eq!(seg.row_of(expected_id), Some(row));
            let rec = mt.get(expected_id).unwrap();
            assert_eq!(seg.vector(row), rec.vector.as_slice());
            assert_eq!(seg.metadata(row).unwrap(), rec.metadata);
        }
        assert_eq!(seg.row_of(99), None);
        assert!(seg.is_tombstoned(50));
        assert!(seg.is_tombstoned(100));
        assert!(!seg.is_tombstoned(51));
    }

    #[test]
    fn deterministic_bytes() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let mt = sample_memtable();
        SegmentWriter::write(dir1.path(), 7, &mt.snapshot(), None).unwrap();
        SegmentWriter::write(dir2.path(), 7, &mt.snapshot(), None).unwrap();
        for f in [FILE_VECTORS, FILE_IDS, FILE_META, FILE_TOMBSTONES] {
            let a = std::fs::read(dir1.path().join(segment_dir_name(7)).join(f)).unwrap();
            let b = std::fs::read(dir2.path().join(segment_dir_name(7)).join(f)).unwrap();
            assert_eq!(a, b, "{f} must be deterministic");
        }
    }

    #[test]
    fn search_flat_over_segment_matches_memtable() {
        use hamane_core::Metric;
        use hamane_index::search_flat;

        let dir = tempfile::tempdir().unwrap();
        let mt = sample_memtable();
        let (_, seg) = write_and_open(dir.path(), &mt);

        let query = [2.5f32, -2.5, 0.5];
        let expected = search_flat(mt.iter(), &query, 3, Metric::L2, None);

        let metas = seg.decode_all_metadata().unwrap();
        let iter = (0..seg.len() as u32).map(|r| (seg.id(r), seg.vector(r), &metas[r as usize]));
        let actual = search_flat(iter, &query, 3, Metric::L2, None);
        assert_eq!(actual, expected);
    }

    #[test]
    fn corruption_detected() {
        let dir = tempfile::tempdir().unwrap();
        let mt = sample_memtable();
        let (_, seg) = write_and_open(dir.path(), &mt);
        drop(seg);

        // vectors.bin の data を 1 バイト破壊 → verify_checksums で検出
        let path = dir.path().join(segment_dir_name(1)).join(FILE_VECTORS);
        let mut buf = std::fs::read(&path).unwrap();
        buf[VECTORS_HEADER_LEN] ^= 0xFF;
        std::fs::write(&path, &buf).unwrap();
        let seg = Segment::open(dir.path(), 1).unwrap(); // open は通る
        assert!(seg.verify_checksums().is_err());

        // サイズ不整合は open で検出
        let mut buf = std::fs::read(&path).unwrap();
        buf.truncate(buf.len() - 8);
        std::fs::write(&path, &buf).unwrap();
        assert!(Segment::open(dir.path(), 1).is_err());
    }

    #[test]
    fn empty_memtable_segment() {
        let dir = tempfile::tempdir().unwrap();
        let mut mt = Memtable::new();
        mt.delete(9); // tombstone のみ
        let (meta, seg) = write_and_open(dir.path(), &mt);
        assert_eq!(meta.record_count, 0);
        assert_eq!(meta.tombstone_count, 1);
        assert!(seg.is_empty());
        assert!(seg.is_tombstoned(9));
    }

    #[test]
    fn tmp_dir_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("seg-000009.tmp")).unwrap();
        SegmentWriter::write(dir.path(), 1, &sample_memtable().snapshot(), None).unwrap();
        let removed = remove_tmp_dirs(dir.path()).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(Segment::open(dir.path(), 1).is_ok()); // 本物は残る
    }
}
