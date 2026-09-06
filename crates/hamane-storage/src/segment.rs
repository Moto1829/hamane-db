//! 不変セグメントの書き出しと mmap 読み込み (docs/design/storage.md §3)。
//!
//! セグメントは `seg-<id:06>/` ディレクトリ内の 4 ファイル
//! (vectors.bin / ids.bin / meta.bin / tombstones.bin)。
//! 各ファイルの footer に「先頭から footer 直前まで」の CRC32C を置く。
//! 書き出しは `.tmp` ディレクトリに全ファイルを書いてから rename する。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use hamane_core::pq::{self, PqCodebook};
use hamane_core::{dot_scalar, l2_squared_scalar, Id, Metadata, Metric, Result};
use hamane_index::{HnswBuilder, HnswGraph, HnswParams, HnswView, VectorSource};
use memmap2::Mmap;

use crate::format::{
    self, corrupted, put_metadata, read_metadata, Reader, MAGIC_HNSW, MAGIC_IDS, MAGIC_IVF,
    MAGIC_IVFPQ, MAGIC_META, MAGIC_PQ, MAGIC_SQ8, MAGIC_TOMBSTONES, MAGIC_VECTORS,
};
use crate::memtable::MemtableSnapshot;

const VECTORS_HEADER_LEN: usize = 64; // magic[8] + dim u32 + count u64 + pad
const PLAIN_HEADER_LEN: usize = 16; // magic[8] + count u64
const SQ8_HEADER_LEN: usize = 64; // magic[8] + dim u32 + count u64 + min f32 + max f32 + pad
const PQ_HEADER_LEN: usize = 64; // magic[8] + dim u32 + count u64 + m u32 + nbits u8 + ksub u32 + pad
const IVF_HEADER_LEN: usize = 64; // magic[8] + dim u32 + count u64 + nlist u32 + pad
const IVFPQ_HEADER_LEN: usize = 64; // magic[8] + dim u32 + count u64 + nlist u32 + m u32 + nbits u8 + ksub u32 + pad

/// IVF 粗量子化の目安点数/クラスタ (これ未満なら nlist を縮小する)。
const IVF_MIN_PER_LIST: usize = 39;

pub const FILE_VECTORS: &str = "vectors.bin";
pub const FILE_IDS: &str = "ids.bin";
pub const FILE_META: &str = "meta.bin";
pub const FILE_TOMBSTONES: &str = "tombstones.bin";
pub const FILE_HNSW: &str = "hnsw.bin";
pub const FILE_SQ8: &str = "vectors_sq8.bin";
pub const FILE_PQ: &str = "vectors_pq.bin";
pub const FILE_IVF: &str = "ivf.bin";
pub const FILE_IVFPQ: &str = "ivfpq.bin";

/// フラッシュ時の量子化方式 (docs/design/quantization.md §5)。
/// HNSW 探索の 1 段目を量子化距離で行い、f32 で再ランクして精度を保つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantization {
    /// スカラー量子化 (todo 602)。ベクトルを 1/4 に圧縮
    Sq8,
    /// 直積量子化 (todo 1003)。`m` サブベクトルに分割。`None` は dim から自動決定
    Pq { m: Option<usize> },
}

/// セグメントに構築する主索引の種類 (docs/design/quantization.md §2.2)。
/// HNSW と IVF はどちらも枝刈り機構なのでセグメント単位で排他。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexKind {
    /// HNSW グラフ (既定)。量子化と組み合わせ可能
    #[default]
    Hnsw,
    /// IVF 転置ファイル (todo 1004)。nprobe クラスタだけを Flat 走査
    Ivf,
    /// IVF-PQ (todo 1005)。IVF の枝刈り + 残差 PQ 圧縮。quantization=Pq が必須
    IvfPq,
}

/// フラッシュ時のインデックス構築指定 (docs/design/index.md §4)。
#[derive(Debug, Clone, Copy)]
pub struct IndexBuildSpec {
    pub metric: Metric,
    pub params: HnswParams,
    /// この行数未満のセグメントは索引を作らない (Flat で十分)
    pub min_rows: usize,
    /// 主索引の種類 (HNSW / IVF)
    pub index: IndexKind,
    /// 量子化ベクトルも書く (None = f32 のみ)。HNSW 探索の距離計算を量子化し、
    /// f32 再ランクと組み合わせて使う (docs/design/quantization.md)
    pub quantization: Option<Quantization>,
}

/// count 件のセグメントに対する IVF のクラスタ数を決める。
/// `sqrt(count)` を基本に [16, 65536] にクランプし、
/// 点数/クラスタが目安を下回るなら縮小する。
fn choose_nlist(count: usize) -> usize {
    let base = (count as f64).sqrt().round() as usize;
    base.clamp(16, 65536).min(count / IVF_MIN_PER_LIST).max(1)
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

        // 主索引 (任意)。HNSW か IVF のどちらか一方 (排他)
        if let Some(spec) = index {
            if rows.len() >= spec.min_rows && !rows.is_empty() {
                match spec.index {
                    IndexKind::Hnsw => {
                        Self::write_hnsw(&tmp_dir, seg_id, &spec, &rows, dim, count)?
                    }
                    IndexKind::Ivf => Self::write_ivf(&tmp_dir, seg_id, &rows, dim, count)?,
                    IndexKind::IvfPq => {
                        // 残差 PQ の m は quantization=Pq{m} から取る (validate で保証)
                        let m = match spec.quantization {
                            Some(Quantization::Pq { m }) => m,
                            _ => None,
                        };
                        Self::write_ivfpq(&tmp_dir, seg_id, m, &rows, dim, count)?
                    }
                }
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

    /// hnsw.bin (+ 任意の量子化ベクトル) を書く。
    fn write_hnsw(
        tmp_dir: &Path,
        seg_id: u64,
        spec: &IndexBuildSpec,
        rows: &[(Id, &[f32], &Metadata)],
        dim: u32,
        count: u64,
    ) -> Result<()> {
        let source = RowsSource(rows.iter().map(|(_, v, _)| *v).collect());
        let params = HnswParams {
            seed: seg_id,
            ..spec.params
        };
        let builder = HnswBuilder::build(&source, spec.metric, params);
        write_file_with_crc(&tmp_dir.join(FILE_HNSW), &builder.serialize())?;

        // 量子化ベクトル (任意)。HNSW 探索の 1 段目に使い f32 で再ランクする
        match spec.quantization {
            // vectors_sq8.bin (todo 602): グローバル min/max で u8 量子化
            Some(Quantization::Sq8) => {
                let params = hamane_core::sq8::Sq8Params::fit(rows.iter().map(|(_, v, _)| *v));
                let mut buf = Vec::with_capacity(SQ8_HEADER_LEN + rows.len() * dim as usize);
                buf.extend_from_slice(&MAGIC_SQ8);
                format::put_u32(&mut buf, dim);
                format::put_u64(&mut buf, count);
                buf.extend_from_slice(&params.min.to_le_bytes());
                buf.extend_from_slice(&params.max.to_le_bytes());
                buf.resize(SQ8_HEADER_LEN, 0);
                for (_, vector, _) in rows {
                    params.quantize(vector, &mut buf);
                }
                write_file_with_crc(&tmp_dir.join(FILE_SQ8), &buf)?;
            }
            // vectors_pq.bin (todo 1003): サブベクトルごとのコードブック
            Some(Quantization::Pq { m }) => {
                // 明示 m が dim を割り切らなければ自動決定にフォールバック。自動決定も
                // 不可 (素数次元など) なら PQ をスキップして通常 HNSW にフォールバック
                // する (フラッシュを絶対に失敗させない。docs/design/quantization.md §4)
                let m = m
                    .filter(|&m| m > 0 && (dim as usize).is_multiple_of(m))
                    .or_else(|| pq::choose_m(dim as usize));
                if let Some(m) = m {
                    let vecs: Vec<&[f32]> = rows.iter().map(|(_, v, _)| *v).collect();
                    // seed はセグメント ID で決定的に (HNSW と同様)
                    let codebook = PqCodebook::train(
                        &vecs,
                        dim as usize,
                        m,
                        seg_id,
                        pq::DEFAULT_TRAIN_SAMPLE,
                        pq::DEFAULT_MAX_ITER,
                    )?;
                    let cb = codebook.centroids();
                    let mut buf = Vec::with_capacity(PQ_HEADER_LEN + cb.len() * 4 + rows.len() * m);
                    buf.extend_from_slice(&MAGIC_PQ);
                    format::put_u32(&mut buf, dim);
                    format::put_u64(&mut buf, count);
                    format::put_u32(&mut buf, m as u32);
                    buf.push(pq::NBITS);
                    format::put_u32(&mut buf, pq::KSUB as u32);
                    buf.resize(PQ_HEADER_LEN, 0);
                    format::put_f32_slice(&mut buf, cb);
                    for (_, vector, _) in rows {
                        codebook.encode(vector, &mut buf);
                    }
                    write_file_with_crc(&tmp_dir.join(FILE_PQ), &buf)?;
                }
            }
            None => {}
        }
        Ok(())
    }

    /// ivf.bin (粗セントロイド + CSR 転置リスト) を書く (todo 1004)。
    fn write_ivf(
        tmp_dir: &Path,
        seg_id: u64,
        rows: &[(Id, &[f32], &Metadata)],
        dim: u32,
        count: u64,
    ) -> Result<()> {
        let d = dim as usize;
        let vecs: Vec<&[f32]> = rows.iter().map(|(_, v, _)| *v).collect();
        let nlist = choose_nlist(rows.len());
        // サンプルで粗 k-means を学習し、全行を最近セントロイドに割り当てる
        let sample = pq::subsample(&vecs, pq::DEFAULT_TRAIN_SAMPLE);
        let centroids = pq::kmeans(&sample, d, nlist, seg_id, pq::DEFAULT_MAX_ITER);
        let mut lists: Vec<Vec<u32>> = vec![Vec::new(); nlist];
        for (row, v) in vecs.iter().enumerate() {
            let l = pq::nearest(v, &centroids, d, nlist) as usize;
            lists[l].push(row as u32); // row 昇順に push = list 内 entries 昇順
        }

        let mut buf = Vec::with_capacity(
            IVF_HEADER_LEN + centroids.len() * 4 + (nlist + 1) * 8 + rows.len() * 4,
        );
        buf.extend_from_slice(&MAGIC_IVF);
        format::put_u32(&mut buf, dim);
        format::put_u64(&mut buf, count);
        format::put_u32(&mut buf, nlist as u32);
        buf.resize(IVF_HEADER_LEN, 0);
        format::put_f32_slice(&mut buf, &centroids);
        // CSR: offsets (prefix sum) → entries
        let mut offset = 0u64;
        format::put_u64(&mut buf, offset);
        for list in &lists {
            offset += list.len() as u64;
            format::put_u64(&mut buf, offset);
        }
        for list in &lists {
            for &row in list {
                format::put_u32(&mut buf, row);
            }
        }
        write_file_with_crc(&tmp_dir.join(FILE_IVF), &buf)?;
        Ok(())
    }

    /// ivfpq.bin (粗セントロイド + 残差 PQ コードブック + CSR(entries+codes)) を
    /// 書く (todo 1005)。各行を所属クラスタの粗セントロイドを引いた残差として
    /// PQ 符号化する (IVFADC)。m が解決できなければ通常 IVF にフォールバック。
    fn write_ivfpq(
        tmp_dir: &Path,
        seg_id: u64,
        m: Option<usize>,
        rows: &[(Id, &[f32], &Metadata)],
        dim: u32,
        count: u64,
    ) -> Result<()> {
        let d = dim as usize;
        // 明示 m が dim を割り切らなければ自動決定。不可なら通常 IVF にフォールバック
        let m = match m
            .filter(|&m| m > 0 && d.is_multiple_of(m))
            .or_else(|| pq::choose_m(d))
        {
            Some(m) => m,
            None => return Self::write_ivf(tmp_dir, seg_id, rows, dim, count),
        };

        let vecs: Vec<&[f32]> = rows.iter().map(|(_, v, _)| *v).collect();
        let nlist = choose_nlist(rows.len());
        // 粗 k-means → 各行の所属リスト
        let sample = pq::subsample(&vecs, pq::DEFAULT_TRAIN_SAMPLE);
        let centroids = pq::kmeans(&sample, d, nlist, seg_id, pq::DEFAULT_MAX_ITER);
        let assign: Vec<u32> = vecs
            .iter()
            .map(|v| pq::nearest(v, &centroids, d, nlist))
            .collect();
        // 残差 r = x − centroid[list]
        let residuals: Vec<Vec<f32>> = vecs
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let c = &centroids[assign[i] as usize * d..(assign[i] as usize + 1) * d];
                v.iter().zip(c).map(|(x, cc)| x - cc).collect()
            })
            .collect();
        // 残差空間で PQ コードブックを 1 組学習 (リスト非依存の共有コードブック)
        let res_slices: Vec<&[f32]> = residuals.iter().map(|r| r.as_slice()).collect();
        let codebook = PqCodebook::train(
            &res_slices,
            d,
            m,
            seg_id ^ 0xF1F1_F1F1,
            pq::DEFAULT_TRAIN_SAMPLE,
            pq::DEFAULT_MAX_ITER,
        )?;
        // 残差 PQ コード (行順)
        let codes: Vec<Vec<u8>> = residuals
            .iter()
            .map(|r| {
                let mut c = Vec::with_capacity(m);
                codebook.encode(r, &mut c);
                c
            })
            .collect();
        // リストごとに行を集める (row 昇順 = entries 昇順)
        let mut lists: Vec<Vec<u32>> = vec![Vec::new(); nlist];
        for (row, &l) in assign.iter().enumerate() {
            lists[l as usize].push(row as u32);
        }

        let cb = codebook.centroids();
        let mut buf = Vec::with_capacity(
            IVFPQ_HEADER_LEN
                + centroids.len() * 4
                + cb.len() * 4
                + (nlist + 1) * 8
                + rows.len() * 4
                + rows.len() * m,
        );
        buf.extend_from_slice(&MAGIC_IVFPQ);
        format::put_u32(&mut buf, dim);
        format::put_u64(&mut buf, count);
        format::put_u32(&mut buf, nlist as u32);
        format::put_u32(&mut buf, m as u32);
        buf.push(pq::NBITS);
        format::put_u32(&mut buf, pq::KSUB as u32);
        buf.resize(IVFPQ_HEADER_LEN, 0);
        format::put_f32_slice(&mut buf, &centroids);
        format::put_f32_slice(&mut buf, cb);
        // CSR: offsets → entries → codes (entries と同順)
        let mut offset = 0u64;
        format::put_u64(&mut buf, offset);
        for list in &lists {
            offset += list.len() as u64;
            format::put_u64(&mut buf, offset);
        }
        for list in &lists {
            for &row in list {
                format::put_u32(&mut buf, row);
            }
        }
        for list in &lists {
            for &row in list {
                buf.extend_from_slice(&codes[row as usize]);
            }
        }
        write_file_with_crc(&tmp_dir.join(FILE_IVFPQ), &buf)?;
        Ok(())
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
    /// vectors_sq8.bin (存在する場合のみ、todo 602)
    sq8: Option<MappedFile>,
    /// vectors_pq.bin (存在する場合のみ、todo 1003)。コードブックは open 時に
    /// 一度だけ復元し (~MB)、コード列は mmap のまま (zero-copy)
    pq: Option<PqSegment>,
    /// ivf.bin (存在する場合のみ、todo 1004)。粗セントロイドは open 時に復元し、
    /// CSR (offsets/entries) は mmap のまま (zero-copy)。HNSW とは排他
    ivf: Option<IvfSegment>,
    /// ivfpq.bin (存在する場合のみ、todo 1005)。粗セントロイド + 残差 PQ
    /// コードブックは open 時に復元、CSR (offsets/entries/codes) は mmap のまま
    ivfpq: Option<IvfPqSegment>,
}

/// IVF-PQ の保持物。粗セントロイドと残差 PQ コードブックは owned、
/// CSR (offsets/entries/codes) は `mapped` を参照する。
struct IvfPqSegment {
    centroids: Vec<f32>, // nlist × dim
    codebook: PqCodebook,
    dim: usize,
    nlist: usize,
    mapped: MappedFile,
    offsets_start: usize, // (nlist+1) × u64
    entries_start: usize, // count × u32
    codes_start: usize,   // count × m × u8
}

/// IVF-PQ へのビュー。probe リスト選択と、リストごとの残差 ADC を担う。
pub struct IvfPqView<'a> {
    centroids: &'a [f32],
    codebook: &'a PqCodebook,
    dim: usize,
    nlist: usize,
    offsets: &'a [u8], // (nlist+1) × u64
    entries: &'a [u8], // count × u32
    codes: &'a [u8],   // count × m × u8
}

impl IvfPqView<'_> {
    #[inline]
    fn offset(&self, i: usize) -> usize {
        u64::from_le_bytes(self.offsets[i * 8..i * 8 + 8].try_into().unwrap()) as usize
    }

    /// CSR インデックス i (entries/codes は同順) の行番号。
    #[inline]
    pub fn entry(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.entries[i * 4..i * 4 + 4].try_into().unwrap())
    }

    /// リスト l の CSR インデックス範囲 (entry/code_distance に渡す)。
    #[inline]
    pub fn list_range(&self, l: usize) -> std::ops::Range<usize> {
        self.offset(l)..self.offset(l + 1)
    }

    /// 粗セントロイド l。
    #[inline]
    fn centroid(&self, l: usize) -> &[f32] {
        &self.centroids[l * self.dim..(l + 1) * self.dim]
    }

    /// クエリに近い nprobe 個のリスト ID を返す (粗量子化は L2 選択)。
    pub fn nprobe_lists(&self, query: &[f32], nprobe: usize) -> Vec<usize> {
        let mut dists: Vec<(f32, usize)> = (0..self.nlist)
            .map(|l| (l2_squared_scalar(query, self.centroid(l)), l))
            .collect();
        let take = nprobe.clamp(1, self.nlist);
        dists.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("distances are finite"));
        dists.iter().take(take).map(|&(_, l)| l).collect()
    }

    /// リスト l 用の残差 ADC の (LUT, バイアス) を返す。
    /// `distance_key = bias + Σ_j lut[code[j]]` が「小さいほど近い」推定になる。
    ///
    /// - L2: `||q − x||² = ||(q − centroid) − r||²`。クエリ残差で LUT、bias=0
    /// - Dot/Cosine: `dot(q, x) = dot(q, centroid) + dot(q, r)`。LUT は生クエリ、
    ///   bias = −dot(q, centroid[l])
    pub fn build_list_lut(&self, query: &[f32], l: usize, metric: Metric) -> (Vec<f32>, f32) {
        let c = self.centroid(l);
        match metric {
            Metric::L2 => {
                let residual: Vec<f32> = query.iter().zip(c).map(|(q, cc)| q - cc).collect();
                (self.codebook.build_lut(&residual, metric), 0.0)
            }
            Metric::Cosine | Metric::Dot => {
                let lut = self.codebook.build_lut(query, metric);
                (lut, -dot_scalar(query, c))
            }
        }
    }

    /// CSR インデックス i の残差 ADC 距離キー (bias は呼び出し側で加算)。
    #[inline]
    pub fn code_distance(&self, lut: &[f32], i: usize) -> f32 {
        let m = self.codebook.m();
        let code = &self.codes[i * m..i * m + m];
        self.codebook.distance_key(lut, code)
    }
}

/// IVF 転置ファイルの保持物。粗セントロイドは owned、CSR は `mapped` を参照する。
struct IvfSegment {
    centroids: Vec<f32>, // nlist × dim
    dim: usize,
    nlist: usize,
    mapped: MappedFile,
    /// content 内の offsets 配列開始 (`(nlist+1) × u64`)
    offsets_start: usize,
    /// content 内の entries 配列開始 (`count × u32`)
    entries_start: usize,
}

/// IVF 転置ファイルへのビュー。nprobe クラスタの候補行を返す。
pub struct IvfView<'a> {
    centroids: &'a [f32],
    dim: usize,
    nlist: usize,
    offsets: &'a [u8], // (nlist+1) × u64
    entries: &'a [u8], // count × u32
}

impl IvfView<'_> {
    #[inline]
    fn offset(&self, i: usize) -> usize {
        u64::from_le_bytes(self.offsets[i * 8..i * 8 + 8].try_into().unwrap()) as usize
    }

    #[inline]
    fn entry(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.entries[i * 4..i * 4 + 4].try_into().unwrap())
    }

    /// クエリに近い `nprobe` 個のリストの全行番号を返す (粗量子化は L2 選択)。
    /// nlist は sqrt(count) オーダなので全セントロイド計算 + ソートで十分安価。
    pub fn candidate_rows(&self, query: &[f32], nprobe: usize) -> Vec<u32> {
        let mut dists: Vec<(f32, usize)> = (0..self.nlist)
            .map(|l| {
                let c = &self.centroids[l * self.dim..(l + 1) * self.dim];
                (l2_squared_scalar(query, c), l)
            })
            .collect();
        let take = nprobe.clamp(1, self.nlist);
        dists.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("distances are finite"));
        let mut rows = Vec::new();
        for &(_, l) in dists.iter().take(take) {
            for i in self.offset(l)..self.offset(l + 1) {
                rows.push(self.entry(i));
            }
        }
        rows
    }
}

/// PQ 量子化セグメントの保持物。コードブックは owned (mmap 生存期間から独立、
/// 検索ごとの再構築を避ける)、コード列は `mapped` の mmap を参照する。
struct PqSegment {
    codebook: PqCodebook,
    mapped: MappedFile,
    /// content 内のコード列開始オフセット (`count × m` バイトが続く)
    codes_start: usize,
}

/// PQ 量子化ベクトルへの zero-copy ビュー。ADC (LUT) で距離を推定する。
pub struct PqView<'a> {
    codebook: &'a PqCodebook,
    codes: &'a [u8],
}

impl PqView<'_> {
    /// クエリと全セントロイドの距離表 (LUT) を作る。検索の開始時に 1 回。
    pub fn build_lut(&self, query: &[f32], metric: Metric) -> Vec<f32> {
        self.codebook.build_lut(query, metric)
    }

    /// 行 row のコード。
    #[inline]
    fn code(&self, row: u32) -> &[u8] {
        let m = self.codebook.m();
        let start = row as usize * m;
        &self.codes[start..start + m]
    }

    /// LUT と行 row から「小さいほど近い」距離キーを推定する (表引き加算)。
    #[inline]
    pub fn distance_key(&self, lut: &[f32], row: u32) -> f32 {
        self.codebook.distance_key(lut, self.code(row))
    }
}

/// vectors_pq.bin を mmap で開き、ヘッダ検証とコードブック復元を行う。
fn load_pq(path: &Path, seg_dim: usize, seg_count: usize) -> Result<PqSegment> {
    let mapped = MappedFile::open(path, &MAGIC_PQ)?;
    let content = mapped.content();
    if content.len() < PQ_HEADER_LEN {
        return Err(corrupted("pq header too short"));
    }
    let dim = u32::from_le_bytes(content[8..12].try_into().unwrap()) as usize;
    let count = u64::from_le_bytes(content[12..20].try_into().unwrap()) as usize;
    let m = u32::from_le_bytes(content[20..24].try_into().unwrap()) as usize;
    let nbits = content[24];
    let ksub = u32::from_le_bytes(content[25..29].try_into().unwrap()) as usize;
    if dim != seg_dim || count != seg_count {
        return Err(corrupted("pq dim/count mismatch with segment"));
    }
    if nbits != pq::NBITS || ksub != pq::KSUB {
        return Err(corrupted("pq unsupported nbits/ksub"));
    }
    if m == 0 || !dim.is_multiple_of(m) {
        return Err(corrupted("pq invalid m"));
    }
    let dsub = dim / m;
    let cb_bytes = m * ksub * dsub * 4;
    let codes_start = PQ_HEADER_LEN + cb_bytes;
    if content.len() != codes_start + count * m {
        return Err(corrupted("pq data size mismatch"));
    }
    // コードブック (f32) を復元
    let mut centroids = Vec::with_capacity(m * ksub * dsub);
    for chunk in content[PQ_HEADER_LEN..codes_start].as_chunks::<4>().0 {
        centroids.push(f32::from_le_bytes(*chunk));
    }
    let codebook = PqCodebook::from_centroids(m, dsub, centroids)?;
    Ok(PqSegment {
        codebook,
        mapped,
        codes_start,
    })
}

/// ivf.bin を mmap で開き、ヘッダ検証と粗セントロイド復元を行う。
fn load_ivf(path: &Path, seg_dim: usize, seg_count: usize) -> Result<IvfSegment> {
    let mapped = MappedFile::open(path, &MAGIC_IVF)?;
    let content = mapped.content();
    if content.len() < IVF_HEADER_LEN {
        return Err(corrupted("ivf header too short"));
    }
    let dim = u32::from_le_bytes(content[8..12].try_into().unwrap()) as usize;
    let count = u64::from_le_bytes(content[12..20].try_into().unwrap()) as usize;
    let nlist = u32::from_le_bytes(content[20..24].try_into().unwrap()) as usize;
    if dim != seg_dim || count != seg_count {
        return Err(corrupted("ivf dim/count mismatch with segment"));
    }
    if nlist == 0 {
        return Err(corrupted("ivf nlist is zero"));
    }
    let offsets_start = IVF_HEADER_LEN + nlist * dim * 4;
    let entries_start = offsets_start + (nlist + 1) * 8;
    if content.len() != entries_start + count * 4 {
        return Err(corrupted("ivf data size mismatch"));
    }
    // 粗セントロイドを復元
    let mut centroids = Vec::with_capacity(nlist * dim);
    for chunk in content[IVF_HEADER_LEN..offsets_start].as_chunks::<4>().0 {
        centroids.push(f32::from_le_bytes(*chunk));
    }
    // 転置リストの末尾オフセットが総行数に一致することを確認 (CSR 整合)
    let last = u64::from_le_bytes(
        content[entries_start - 8..entries_start]
            .try_into()
            .unwrap(),
    );
    if last as usize != count {
        return Err(corrupted("ivf offsets do not sum to count"));
    }
    Ok(IvfSegment {
        centroids,
        dim,
        nlist,
        mapped,
        offsets_start,
        entries_start,
    })
}

/// ivfpq.bin を mmap で開き、ヘッダ検証・粗セントロイド・残差 PQ コードブックの
/// 復元を行う。
fn load_ivfpq(path: &Path, seg_dim: usize, seg_count: usize) -> Result<IvfPqSegment> {
    let mapped = MappedFile::open(path, &MAGIC_IVFPQ)?;
    let content = mapped.content();
    if content.len() < IVFPQ_HEADER_LEN {
        return Err(corrupted("ivfpq header too short"));
    }
    let dim = u32::from_le_bytes(content[8..12].try_into().unwrap()) as usize;
    let count = u64::from_le_bytes(content[12..20].try_into().unwrap()) as usize;
    let nlist = u32::from_le_bytes(content[20..24].try_into().unwrap()) as usize;
    let m = u32::from_le_bytes(content[24..28].try_into().unwrap()) as usize;
    let nbits = content[28];
    let ksub = u32::from_le_bytes(content[29..33].try_into().unwrap()) as usize;
    if dim != seg_dim || count != seg_count {
        return Err(corrupted("ivfpq dim/count mismatch with segment"));
    }
    if nlist == 0 {
        return Err(corrupted("ivfpq nlist is zero"));
    }
    if nbits != pq::NBITS || ksub != pq::KSUB {
        return Err(corrupted("ivfpq unsupported nbits/ksub"));
    }
    if m == 0 || !dim.is_multiple_of(m) {
        return Err(corrupted("ivfpq invalid m"));
    }
    let dsub = dim / m;
    let coarse_start = IVFPQ_HEADER_LEN;
    let cb_start = coarse_start + nlist * dim * 4;
    let offsets_start = cb_start + m * ksub * dsub * 4;
    let entries_start = offsets_start + (nlist + 1) * 8;
    let codes_start = entries_start + count * 4;
    if content.len() != codes_start + count * m {
        return Err(corrupted("ivfpq data size mismatch"));
    }
    // 粗セントロイドと残差 PQ コードブックを復元
    let mut centroids = Vec::with_capacity(nlist * dim);
    for chunk in content[coarse_start..cb_start].as_chunks::<4>().0 {
        centroids.push(f32::from_le_bytes(*chunk));
    }
    let mut cb = Vec::with_capacity(m * ksub * dsub);
    for chunk in content[cb_start..offsets_start].as_chunks::<4>().0 {
        cb.push(f32::from_le_bytes(*chunk));
    }
    let codebook = PqCodebook::from_centroids(m, dsub, cb)?;
    // CSR 末尾オフセットが総行数に一致することを確認
    let last = u64::from_le_bytes(
        content[entries_start - 8..entries_start]
            .try_into()
            .unwrap(),
    );
    if last as usize != count {
        return Err(corrupted("ivfpq offsets do not sum to count"));
    }
    Ok(IvfPqSegment {
        centroids,
        codebook,
        dim,
        nlist,
        mapped,
        offsets_start,
        entries_start,
        codes_start,
    })
}

/// SQ8 量子化ベクトルへの zero-copy ビュー。
/// `dist_key(query_codes, ...)` で HNSW 探索用の距離クロージャを作る。
pub struct Sq8View<'a> {
    dim: usize,
    params: hamane_core::sq8::Sq8Params,
    data: &'a [u8],
}

impl Sq8View<'_> {
    /// 量子化パラメータ (クエリの量子化に使う)。
    pub fn params(&self) -> hamane_core::sq8::Sq8Params {
        self.params
    }

    /// クエリベクトルを同じスケールで量子化する。
    pub fn quantize_query(&self, query: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(query.len());
        self.params.quantize(query, &mut out);
        out
    }

    /// 行 row の量子化コード。
    #[inline]
    pub fn codes(&self, row: u32) -> &[u8] {
        let start = row as usize * self.dim;
        &self.data[start..start + self.dim]
    }

    /// 「小さいほど近い」距離キーを返す (metric.distance_key と同じ意味論の近似)。
    #[inline]
    pub fn distance_key(
        &self,
        metric: Metric,
        query_codes: &[u8],
        query_code_sum: u64,
        row: u32,
    ) -> f32 {
        let s = self.params.scale();
        match metric {
            Metric::L2 => {
                let accum = hamane_core::sq8::sq8_l2_accum(query_codes, self.codes(row));
                s * s * accum as f32
            }
            Metric::Cosine | Metric::Dot => {
                let (dot_q, sum_b) = hamane_core::sq8::sq8_dot_accum(query_codes, self.codes(row));
                let min = self.params.min;
                let dot = self.dim as f32 * min * min
                    + min * s * (query_code_sum + sum_b) as f32
                    + s * s * dot_q as f32;
                -dot
            }
        }
    }
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
        let sq8 = if dir.join(FILE_SQ8).exists() {
            Some(MappedFile::open(&dir.join(FILE_SQ8), &MAGIC_SQ8)?)
        } else {
            None
        };

        let dim = u32::from_le_bytes(vectors.content()[8..12].try_into().unwrap()) as usize;
        let count = vectors.u64_at(12)? as usize;

        // vectors_pq.bin (任意): コードブックを復元して保持する
        let pq = if dir.join(FILE_PQ).exists() {
            Some(load_pq(&dir.join(FILE_PQ), dim, count)?)
        } else {
            None
        };

        // ivf.bin (任意): 粗セントロイドを復元して保持する
        let ivf = if dir.join(FILE_IVF).exists() {
            Some(load_ivf(&dir.join(FILE_IVF), dim, count)?)
        } else {
            None
        };

        // ivfpq.bin (任意): 粗セントロイド + 残差 PQ コードブックを復元
        let ivfpq = if dir.join(FILE_IVFPQ).exists() {
            Some(load_ivfpq(&dir.join(FILE_IVFPQ), dim, count)?)
        } else {
            None
        };

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
            sq8,
            pq,
            ivf,
            ivfpq,
        };
        // hnsw.bin / vectors_sq8.bin があれば構造を一度検証しておく (行数の整合含む)。
        // vectors_pq.bin は load_pq 内で検証済み
        if segment.hnsw.is_some() {
            segment.hnsw()?;
        }
        if segment.sq8.is_some() {
            segment.sq8_view()?;
        }
        Ok(segment)
    }

    /// PQ 量子化ベクトルのビューを返す (vectors_pq.bin がなければ None)。
    /// コードブックは open 時に検証・復元済みなので軽量。
    pub fn pq_view(&self) -> Option<PqView<'_>> {
        let pq = self.pq.as_ref()?;
        Some(PqView {
            codebook: &pq.codebook,
            codes: &pq.mapped.content()[pq.codes_start..],
        })
    }

    /// このセグメントが PQ 量子化ベクトルを持つか。
    pub fn has_pq(&self) -> bool {
        self.pq.is_some()
    }

    /// IVF 転置ファイルのビューを返す (ivf.bin がなければ None)。
    /// 粗セントロイドは open 時に検証・復元済みなので軽量。
    pub fn ivf_view(&self) -> Option<IvfView<'_>> {
        let ivf = self.ivf.as_ref()?;
        let content = ivf.mapped.content();
        Some(IvfView {
            centroids: &ivf.centroids,
            dim: ivf.dim,
            nlist: ivf.nlist,
            offsets: &content[ivf.offsets_start..ivf.entries_start],
            entries: &content[ivf.entries_start..],
        })
    }

    /// このセグメントが IVF 転置ファイルを持つか。
    pub fn has_ivf(&self) -> bool {
        self.ivf.is_some()
    }

    /// IVF-PQ のビューを返す (ivfpq.bin がなければ None)。
    pub fn ivfpq_view(&self) -> Option<IvfPqView<'_>> {
        let x = self.ivfpq.as_ref()?;
        let content = x.mapped.content();
        Some(IvfPqView {
            centroids: &x.centroids,
            codebook: &x.codebook,
            dim: x.dim,
            nlist: x.nlist,
            offsets: &content[x.offsets_start..x.entries_start],
            entries: &content[x.entries_start..x.codes_start],
            codes: &content[x.codes_start..],
        })
    }

    /// このセグメントが IVF-PQ を持つか。
    pub fn has_ivfpq(&self) -> bool {
        self.ivfpq.is_some()
    }

    /// SQ8 量子化ベクトルのビューを返す (vectors_sq8.bin がなければ None)。
    pub fn sq8_view(&self) -> Result<Option<Sq8View<'_>>> {
        let Some(mapped) = &self.sq8 else {
            return Ok(None);
        };
        let content = mapped.content();
        if content.len() < SQ8_HEADER_LEN {
            return Err(corrupted("sq8 header too short"));
        }
        let dim = u32::from_le_bytes(content[8..12].try_into().unwrap()) as usize;
        let count = u64::from_le_bytes(content[12..20].try_into().unwrap()) as usize;
        let min = f32::from_le_bytes(content[20..24].try_into().unwrap());
        let max = f32::from_le_bytes(content[24..28].try_into().unwrap());
        if dim != self.dim || count != self.count {
            return Err(corrupted("sq8 dim/count mismatch with segment"));
        }
        let data = &content[SQ8_HEADER_LEN..];
        if data.len() != count * dim {
            return Err(corrupted("sq8 data size mismatch"));
        }
        Ok(Some(Sq8View {
            dim,
            params: hamane_core::sq8::Sq8Params { min, max },
            data,
        }))
    }

    /// このセグメントが SQ8 量子化ベクトルを持つか。
    pub fn has_sq8(&self) -> bool {
        self.sq8.is_some()
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
        if let Some(q) = &self.sq8 {
            q.verify_checksum(FILE_SQ8)?;
        }
        if let Some(pq) = &self.pq {
            pq.mapped.verify_checksum(FILE_PQ)?;
        }
        if let Some(ivf) = &self.ivf {
            ivf.mapped.verify_checksum(FILE_IVF)?;
        }
        if let Some(x) = &self.ivfpq {
            x.mapped.verify_checksum(FILE_IVFPQ)?;
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
