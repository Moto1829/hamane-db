//! Store: manifest / WAL / セグメント / memtable を束ねる永続化装置
//! (docs/design/storage.md §5–7, docs/design/query.md §1)。
//!
//! - 書き込みは内部 Mutex で直列化し、WAL append + sync 後に memtable へ反映する
//! - 読み取りは `view()` でスナップショット (LiveView) を取り、ロック外で行う
//! - フラッシュは全 collection を一括で行い、WAL を世代交代させる
//!   (collection 単位のフラッシュにすると WAL の削除可否判定が複雑になるため)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hamane_core::{HamaneError, Id, Metric, Result};

use hamane_index::HnswParams;

use crate::format::corrupted;
use crate::manifest::{CollectionEntry, Manifest, SegmentEntry, CURRENT_FILE};
use crate::memtable::{Memtable, MemtableSnapshot, StoredRecord};
use crate::segment::{segment_dir_name, IndexBuildSpec, Segment, SegmentWriter};
use crate::wal::{list_wal_files, wal_file_name, SyncPolicy, WalReader, WalRecord, WalWriter};

/// Store の実行時オプション (manifest には永続化されない)。
#[derive(Debug, Clone, Copy)]
pub struct StoreOptions {
    pub sync: SyncPolicy,
    /// memtable がこのバイト数を超えたら自動フラッシュ
    pub flush_threshold_bytes: usize,
    /// フラッシュ時に構築する HNSW のパラメータ (seed はセグメント ID で上書き)
    pub hnsw: HnswParams,
    /// この行数未満のセグメントは HNSW を作らない (Flat で十分)
    pub hnsw_min_rows: usize,
    /// collection のセグメント数がこの値以上になったら自動コンパクション
    pub compaction_threshold: usize,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            sync: SyncPolicy::Always,
            flush_threshold_bytes: 64 * 1024 * 1024,
            hnsw: HnswParams::default(),
            hnsw_min_rows: 1024,
            compaction_threshold: 4,
        }
    }
}

/// collection の永続化メタ情報 (Store が返す)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionInfo {
    pub collection_id: u32,
    pub dim: u32,
    pub metric: Metric,
}

struct CollectionState {
    name: String,
    dim: u32,
    metric: Metric,
    memtable: Memtable,
    /// seg_id 降順 (新しい順)
    segments: Vec<Arc<Segment>>,
}

struct StoreState {
    manifest: Manifest,
    /// アクティブ WAL とその seq。in-memory モードでは None
    wal: Option<(u64, WalWriter)>,
    collections: HashMap<u32, CollectionState>,
    names: HashMap<String, u32>,
}

/// 検索・点参照用のスナップショット (docs/design/storage.md §7)。
///
/// source rank: 0 = memtable, 1 = 最新セグメント, 2 = その次…
pub struct LiveView {
    pub memtable: MemtableSnapshot,
    /// seg_id 降順
    pub segments: Vec<Arc<Segment>>,
}

impl LiveView {
    /// rank のソースで見つかった id が、より新しいソースに
    /// 上書き・削除されていないか判定する。
    pub fn is_live(&self, id: Id, source_rank: usize) -> bool {
        if source_rank > 0 && (self.memtable.get(id).is_some() || self.memtable.is_deleted(id)) {
            return false;
        }
        for seg in self.segments.iter().take(source_rank.saturating_sub(1)) {
            if seg.contains(id) || seg.is_tombstoned(id) {
                return false;
            }
        }
        true
    }

    /// 点参照。memtable → セグメント降順の優先解決。
    pub fn get(&self, id: Id) -> Option<StoredRecord> {
        if self.memtable.is_deleted(id) {
            return None;
        }
        if let Some(rec) = self.memtable.get(id) {
            return Some(rec.clone());
        }
        for seg in &self.segments {
            if seg.is_tombstoned(id) {
                return None;
            }
            if let Some(row) = seg.row_of(id) {
                return Some(StoredRecord {
                    vector: seg.vector(row).to_vec(),
                    metadata: seg.metadata(row).ok()?,
                });
            }
        }
        None
    }

    /// live なレコード数。全セグメント走査のため O(総行数)。
    pub fn live_len(&self) -> usize {
        let mut n = self.memtable.len();
        for (i, seg) in self.segments.iter().enumerate() {
            let rank = i + 1;
            for row in 0..seg.len() as u32 {
                if self.is_live(seg.id(row), rank) {
                    n += 1;
                }
            }
        }
        n
    }
}

/// データベース 1 個分の永続化装置。`Send + Sync`。
pub struct Store {
    db_dir: Option<PathBuf>,
    options: StoreOptions,
    state: Mutex<StoreState>,
}

fn wal_dir(db_dir: &Path) -> PathBuf {
    db_dir.join("wal")
}

fn collections_dir(db_dir: &Path) -> PathBuf {
    db_dir.join("collections")
}

fn collection_dir(db_dir: &Path, collection_id: u32) -> PathBuf {
    collections_dir(db_dir).join(collection_id.to_string())
}

impl Store {
    /// 実行時オプション (フラッシュ閾値・HNSW パラメータ等)。
    pub fn options(&self) -> &StoreOptions {
        &self.options
    }

    /// 永続化なしの Store (従来の in-memory モード)。
    pub fn in_memory() -> Self {
        Self {
            db_dir: None,
            options: StoreOptions::default(),
            state: Mutex::new(StoreState {
                manifest: Manifest::default(),
                wal: None,
                collections: HashMap::new(),
                names: HashMap::new(),
            }),
        }
    }

    /// ディレクトリを開く (なければ初期化)。docs/design/storage.md §5 の手順。
    pub fn open(db_dir: &Path, options: StoreOptions) -> Result<Self> {
        std::fs::create_dir_all(db_dir)?;
        std::fs::create_dir_all(wal_dir(db_dir))?;
        std::fs::create_dir_all(collections_dir(db_dir))?;

        // 1. 空なら初期化
        if !db_dir.join(CURRENT_FILE).exists() {
            Manifest::default().store(db_dir)?;
        }

        // 2. manifest を読む
        let manifest = Manifest::load(db_dir)?;

        // 3. セグメントを開く
        let mut collections = HashMap::new();
        let mut names = HashMap::new();
        for entry in &manifest.collections {
            let dir = collection_dir(db_dir, entry.collection_id);
            let mut segments = Vec::new();
            for seg in &entry.segments {
                segments.push(Arc::new(Segment::open(&dir, seg.seg_id)?));
            }
            segments.sort_by_key(|s| std::cmp::Reverse(s.seg_id()));
            names.insert(entry.name.clone(), entry.collection_id);
            collections.insert(
                entry.collection_id,
                CollectionState {
                    name: entry.name.clone(),
                    dim: entry.dim,
                    metric: entry.metric,
                    memtable: Memtable::new(),
                    segments,
                },
            );
        }

        let mut state = StoreState {
            manifest,
            wal: None,
            collections,
            names,
        };

        // 4. WAL リプレイ (manifest.wal_seq より新しいもの)
        let mut max_seq = state.manifest.wal_seq;
        for (seq, path) in list_wal_files(&wal_dir(db_dir))? {
            if seq <= state.manifest.wal_seq {
                continue;
            }
            max_seq = max_seq.max(seq);
            let replay = WalReader::read_all(&path)?;
            // 部分書き込みを切り詰める
            if replay.valid_len < std::fs::metadata(&path)?.len() {
                let file = std::fs::OpenOptions::new().write(true).open(&path)?;
                file.set_len(replay.valid_len)?;
                file.sync_data()?;
            }
            for record in replay.records {
                Self::apply_record(&mut state, record)?;
            }
        }

        // 5. 掃除: 反映済み WAL、古い manifest、.tmp、参照されないディレクトリ
        for (seq, path) in list_wal_files(&wal_dir(db_dir))? {
            if seq <= state.manifest.wal_seq {
                std::fs::remove_file(&path)?;
            }
        }
        Manifest::gc(db_dir)?;
        Self::cleanup_collection_dirs(db_dir, &state.manifest)?;

        // 6. 新しいアクティブ WAL (既存はそのまま残し、次のフラッシュで削除)
        let new_seq = max_seq + 1;
        let writer =
            WalWriter::create(&wal_dir(db_dir).join(wal_file_name(new_seq)), options.sync)?;
        state.wal = Some((new_seq, writer));

        Ok(Self {
            db_dir: Some(db_dir.to_path_buf()),
            options,
            state: Mutex::new(state),
        })
    }

    /// manifest に載っていないセグメント/collection ディレクトリを削除する
    /// (フラッシュ途中のクラッシュで残った孤児)。
    fn cleanup_collection_dirs(db_dir: &Path, manifest: &Manifest) -> Result<()> {
        let root = collections_dir(db_dir);
        for entry in std::fs::read_dir(&root)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(cid) = name.parse::<u32>() else {
                std::fs::remove_dir_all(&path)?;
                continue;
            };
            let Some(col) = manifest.collections.iter().find(|c| c.collection_id == cid) else {
                std::fs::remove_dir_all(&path)?;
                continue;
            };
            // collection 内の孤児セグメント
            for seg_entry in std::fs::read_dir(&path)? {
                let seg_path = seg_entry?.path();
                let Some(seg_name) = seg_path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let referenced = col
                    .segments
                    .iter()
                    .any(|s| segment_dir_name(s.seg_id) == seg_name);
                if !referenced {
                    std::fs::remove_dir_all(&seg_path)?;
                }
            }
        }
        Ok(())
    }

    /// WAL レコードをインメモリ状態に適用する (リプレイ用)。
    fn apply_record(state: &mut StoreState, record: WalRecord) -> Result<()> {
        match record {
            WalRecord::CreateCollection {
                collection_id,
                name,
                dim,
                metric,
            } => {
                state.names.insert(name.clone(), collection_id);
                state.collections.insert(
                    collection_id,
                    CollectionState {
                        name,
                        dim,
                        metric,
                        memtable: Memtable::new(),
                        segments: Vec::new(),
                    },
                );
                state.manifest.next_collection_id =
                    state.manifest.next_collection_id.max(collection_id + 1);
            }
            WalRecord::DropCollection { collection_id } => {
                if let Some(col) = state.collections.remove(&collection_id) {
                    state.names.remove(&col.name);
                }
            }
            WalRecord::Upsert {
                collection_id,
                id,
                vector,
                metadata,
            } => {
                let col = state
                    .collections
                    .get_mut(&collection_id)
                    .ok_or_else(|| corrupted("WAL upsert for unknown collection"))?;
                col.memtable.upsert(id, StoredRecord { vector, metadata });
            }
            WalRecord::Delete { collection_id, id } => {
                let col = state
                    .collections
                    .get_mut(&collection_id)
                    .ok_or_else(|| corrupted("WAL delete for unknown collection"))?;
                col.memtable.delete(id);
            }
        }
        Ok(())
    }

    /// WAL に書いて sync してから状態に反映する。
    fn log_and_apply(&self, state: &mut StoreState, record: WalRecord) -> Result<()> {
        if let Some((_, wal)) = state.wal.as_mut() {
            wal.append(&record)?;
            wal.sync()?;
        }
        Self::apply_record(state, record)
    }

    // -----------------------------------------------------------------------
    // collection 管理
    // -----------------------------------------------------------------------

    pub fn create_collection(
        &self,
        name: &str,
        dim: u32,
        metric: Metric,
    ) -> Result<CollectionInfo> {
        let mut state = self.state.lock().expect("lock poisoned");
        if state.names.contains_key(name) {
            return Err(HamaneError::CollectionExists(name.to_owned()));
        }
        let collection_id = state.manifest.next_collection_id;
        self.log_and_apply(
            &mut state,
            WalRecord::CreateCollection {
                collection_id,
                name: name.to_owned(),
                dim,
                metric,
            },
        )?;
        Ok(CollectionInfo {
            collection_id,
            dim,
            metric,
        })
    }

    pub fn drop_collection(&self, name: &str) -> Result<()> {
        let mut state = self.state.lock().expect("lock poisoned");
        let Some(&collection_id) = state.names.get(name) else {
            return Err(HamaneError::CollectionNotFound(name.to_owned()));
        };
        self.log_and_apply(&mut state, WalRecord::DropCollection { collection_id })
    }

    pub fn collection_info(&self, name: &str) -> Result<CollectionInfo> {
        let state = self.state.lock().expect("lock poisoned");
        let Some(&collection_id) = state.names.get(name) else {
            return Err(HamaneError::CollectionNotFound(name.to_owned()));
        };
        let col = &state.collections[&collection_id];
        Ok(CollectionInfo {
            collection_id,
            dim: col.dim,
            metric: col.metric,
        })
    }

    pub fn collection_names(&self) -> Vec<String> {
        let state = self.state.lock().expect("lock poisoned");
        let mut names: Vec<String> = state.names.keys().cloned().collect();
        names.sort();
        names
    }

    // -----------------------------------------------------------------------
    // 書き込み
    // -----------------------------------------------------------------------

    pub fn upsert(&self, collection_id: u32, id: Id, record: StoredRecord) -> Result<()> {
        self.upsert_batch(collection_id, vec![(id, record)])
    }

    /// 複数レコードを 1 回の WAL sync でまとめて書く。
    pub fn upsert_batch(&self, collection_id: u32, records: Vec<(Id, StoredRecord)>) -> Result<()> {
        let mut state = self.state.lock().expect("lock poisoned");
        Self::check_collection(&state, collection_id)?;
        if let Some((_, wal)) = state.wal.as_mut() {
            for (id, record) in &records {
                wal.append(&WalRecord::Upsert {
                    collection_id,
                    id: *id,
                    vector: record.vector.clone(),
                    metadata: record.metadata.clone(),
                })?;
            }
            wal.sync()?;
        }
        let col = state.collections.get_mut(&collection_id).unwrap();
        for (id, record) in records {
            col.memtable.upsert(id, record);
        }
        self.maybe_flush(&mut state)
    }

    pub fn delete(&self, collection_id: u32, id: Id) -> Result<()> {
        let mut state = self.state.lock().expect("lock poisoned");
        Self::check_collection(&state, collection_id)?;
        self.log_and_apply(&mut state, WalRecord::Delete { collection_id, id })?;
        self.maybe_flush(&mut state)
    }

    fn check_collection(state: &StoreState, collection_id: u32) -> Result<()> {
        if !state.collections.contains_key(&collection_id) {
            return Err(HamaneError::CollectionNotFound(format!(
                "collection_id={collection_id}"
            )));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 読み取り
    // -----------------------------------------------------------------------

    /// 検索用スナップショットを取る。ロックは取得中のみ保持する。
    pub fn view(&self, collection_id: u32) -> Result<LiveView> {
        let state = self.state.lock().expect("lock poisoned");
        let col = state.collections.get(&collection_id).ok_or_else(|| {
            HamaneError::CollectionNotFound(format!("collection_id={collection_id}"))
        })?;
        Ok(LiveView {
            memtable: col.memtable.snapshot(),
            segments: col.segments.clone(),
        })
    }

    // -----------------------------------------------------------------------
    // フラッシュ (docs/design/storage.md §6)
    // -----------------------------------------------------------------------

    fn maybe_flush(&self, state: &mut StoreState) -> Result<()> {
        if self.db_dir.is_none() {
            return Ok(());
        }
        let over = state
            .collections
            .values()
            .any(|c| c.memtable.approx_bytes() >= self.options.flush_threshold_bytes);
        if over {
            self.flush_locked(state)?;
        }
        Ok(())
    }

    /// 全 collection の memtable をセグメントへ書き出し、WAL を世代交代する。
    pub fn flush(&self) -> Result<()> {
        let mut state = self.state.lock().expect("lock poisoned");
        self.flush_locked(&mut state)
    }

    fn flush_locked(&self, state: &mut StoreState) -> Result<()> {
        let Some(db_dir) = self.db_dir.clone() else {
            return Ok(()); // in-memory は何もしない
        };
        let flushed_wal_seq = state.wal.as_ref().map(|(seq, _)| *seq).unwrap_or(0);

        // 1–3. memtable が空でない collection をセグメントとして書き出す
        let mut flushed: Vec<(u32, u64)> = Vec::new(); // (collection_id, seg_id)
        let mut new_entries: HashMap<u32, SegmentEntry> = HashMap::new();
        for (cid, col) in state.collections.iter() {
            if col.memtable.is_empty() {
                continue;
            }
            let dir = collection_dir(&db_dir, *cid);
            std::fs::create_dir_all(&dir)?;
            let seg_id = state.manifest.next_seg_id + flushed.len() as u64;
            let spec = IndexBuildSpec {
                metric: col.metric,
                params: self.options.hnsw,
                min_rows: self.options.hnsw_min_rows,
            };
            let meta = SegmentWriter::write(&dir, seg_id, &col.memtable, Some(spec))?;
            new_entries.insert(
                *cid,
                SegmentEntry {
                    seg_id,
                    record_count: meta.record_count,
                    tombstone_count: meta.tombstone_count,
                },
            );
            flushed.push((*cid, seg_id));
        }

        // 4. 新 manifest を書いて CURRENT を切り替える
        let mut manifest = Manifest {
            gen: state.manifest.gen + 1,
            next_collection_id: state.manifest.next_collection_id,
            next_seg_id: state.manifest.next_seg_id + flushed.len() as u64,
            wal_seq: flushed_wal_seq,
            collections: Vec::new(),
        };
        let mut cids: Vec<u32> = state.collections.keys().copied().collect();
        cids.sort_unstable();
        for cid in cids {
            let col = &state.collections[&cid];
            let mut segments: Vec<SegmentEntry> = col
                .segments
                .iter()
                .map(|s| SegmentEntry {
                    seg_id: s.seg_id(),
                    record_count: s.len() as u64,
                    tombstone_count: s.tombstone_count() as u64,
                })
                .collect();
            if let Some(entry) = new_entries.get(&cid) {
                segments.push(*entry);
            }
            segments.sort_by_key(|s| s.seg_id);
            manifest.collections.push(CollectionEntry {
                collection_id: cid,
                name: col.name.clone(),
                dim: col.dim,
                metric: col.metric,
                segments,
            });
        }
        manifest.store(&db_dir)?;
        state.manifest = manifest;

        // 5. インメモリ状態を更新: 新セグメントを開き、memtable をクリア
        for (cid, seg_id) in &flushed {
            let col = state.collections.get_mut(cid).unwrap();
            let seg = Arc::new(Segment::open(&collection_dir(&db_dir, *cid), *seg_id)?);
            col.segments.insert(0, seg); // 降順の先頭 = 最新
            col.memtable = Memtable::new();
        }

        // 6. WAL 世代交代: 新 WAL を作り、反映済みの旧 WAL を削除
        let new_seq = flushed_wal_seq + 1;
        let writer = WalWriter::create(
            &wal_dir(&db_dir).join(wal_file_name(new_seq)),
            self.options.sync,
        )?;
        state.wal = Some((new_seq, writer));
        for (seq, path) in list_wal_files(&wal_dir(&db_dir))? {
            if seq <= flushed_wal_seq {
                std::fs::remove_file(&path)?;
            }
        }
        Manifest::gc(&db_dir)?;
        // drop 済み collection のディレクトリを掃除
        Self::cleanup_collection_dirs(&db_dir, &state.manifest)?;

        // セグメントが増えすぎた collection をコンパクション
        let over: Vec<u32> = state
            .collections
            .iter()
            .filter(|(_, c)| c.segments.len() >= self.options.compaction_threshold)
            .map(|(cid, _)| *cid)
            .collect();
        for cid in over {
            self.compact_collection_locked(state, cid)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // コンパクション (todos/401)
    // -----------------------------------------------------------------------

    /// 全 collection の複数セグメントを 1 個に統合する (v0 は full merge)。
    /// 上書き・tombstone を物理適用するため、ディスク使用量が live データに収束する。
    pub fn compact(&self) -> Result<()> {
        let mut state = self.state.lock().expect("lock poisoned");
        let targets: Vec<u32> = state
            .collections
            .iter()
            .filter(|(_, c)| c.segments.len() >= 2)
            .map(|(cid, _)| *cid)
            .collect();
        for cid in targets {
            self.compact_collection_locked(&mut state, cid)?;
        }
        Ok(())
    }

    /// collection の全セグメントを newest-wins で 1 個に統合する。
    ///
    /// 全セグメントを統合するため、より古いデータは存在せず tombstone は破棄できる
    /// (memtable の削除マーカーは memtable 側に残り、読み取りで引き続き優先される)。
    /// size-tiered な部分マージは将来の最適化 (manifest がセグメントの
    /// 年代順リストを持てば可能)。
    fn compact_collection_locked(&self, state: &mut StoreState, cid: u32) -> Result<()> {
        let Some(db_dir) = self.db_dir.clone() else {
            return Ok(());
        };
        let col = state.collections.get(&cid).expect("collection exists");
        if col.segments.len() < 2 {
            return Ok(());
        }

        // newest → oldest に走査し、live な行だけを集める
        let mut merged = Memtable::new();
        let mut seen: std::collections::HashSet<Id> = std::collections::HashSet::new();
        let mut deleted: std::collections::HashSet<Id> = std::collections::HashSet::new();
        for seg in &col.segments {
            for row in 0..seg.len() as u32 {
                let id = seg.id(row);
                if seen.insert(id) && !deleted.contains(&id) {
                    merged.upsert(
                        id,
                        StoredRecord {
                            vector: seg.vector(row).to_vec(),
                            metadata: seg.metadata(row)?,
                        },
                    );
                }
            }
            // このセグメントの tombstone はこれより古いセグメントにのみ効く
            for_each_tombstone(seg, |id| {
                deleted.insert(id);
            });
        }

        // 新セグメントとして書き出し (最大の seg_id より新しい = 読み取り優先順は不変)
        let dir = collection_dir(&db_dir, cid);
        let seg_id = state.manifest.next_seg_id;
        let spec = IndexBuildSpec {
            metric: col.metric,
            params: self.options.hnsw,
            min_rows: self.options.hnsw_min_rows,
        };
        SegmentWriter::write(&dir, seg_id, &merged.snapshot(), Some(spec))?;

        // manifest を更新 (この collection のセグメントを 1 個に置き換え)
        let mut manifest = state.manifest.clone();
        manifest.gen += 1;
        manifest.next_seg_id = seg_id + 1;
        let entry = manifest
            .collections
            .iter_mut()
            .find(|c| c.collection_id == cid)
            .expect("collection in manifest");
        let old_seg_ids: Vec<u64> = entry.segments.iter().map(|s| s.seg_id).collect();
        entry.segments = vec![SegmentEntry {
            seg_id,
            record_count: merged.len() as u64,
            tombstone_count: 0,
        }];
        manifest.store(&db_dir)?;
        state.manifest = manifest;

        // インメモリ状態の差し替え。旧セグメントのファイルは削除してよい
        // (検索中の Arc<Segment> は mmap を保持しており、Unix では unlink 後も安全)
        let col = state.collections.get_mut(&cid).unwrap();
        col.segments = vec![Arc::new(Segment::open(&dir, seg_id)?)];
        for old in old_seg_ids {
            let path = dir.join(segment_dir_name(old));
            if path.exists() {
                std::fs::remove_dir_all(&path)?;
            }
        }
        Manifest::gc(&db_dir)?;
        Ok(())
    }
}

/// セグメントの tombstone を列挙するヘルパ。
fn for_each_tombstone(seg: &Segment, mut f: impl FnMut(Id)) {
    for i in 0..seg.tombstone_count() as u64 {
        f(seg.tombstone_at(i as usize));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hamane_core::Metadata;

    fn rec(v: Vec<f32>) -> StoredRecord {
        StoredRecord {
            vector: v,
            metadata: Metadata::new(),
        }
    }

    fn open(dir: &Path) -> Store {
        Store::open(dir, StoreOptions::default()).unwrap()
    }

    #[test]
    fn create_upsert_reopen_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let info = {
            let store = open(dir.path());
            let info = store.create_collection("docs", 2, Metric::L2).unwrap();
            store
                .upsert(info.collection_id, 1, rec(vec![1.0, 2.0]))
                .unwrap();
            store
                .upsert(info.collection_id, 2, rec(vec![3.0, 4.0]))
                .unwrap();
            store.delete(info.collection_id, 2).unwrap();
            info
        }; // drop = クリーンでないシャットダウン相当 (フラッシュしない)

        let store = open(dir.path());
        let got = store.collection_info("docs").unwrap();
        assert_eq!(got, info);
        let view = store.view(info.collection_id).unwrap();
        assert_eq!(view.get(1).unwrap().vector, vec![1.0, 2.0]);
        assert!(view.get(2).is_none()); // delete もリプレイされる
        assert_eq!(view.live_len(), 1);
    }

    #[test]
    fn flush_then_reopen_without_wal() {
        let dir = tempfile::tempdir().unwrap();
        let cid = {
            let store = open(dir.path());
            let info = store.create_collection("docs", 2, Metric::L2).unwrap();
            for i in 0..10u64 {
                store
                    .upsert(info.collection_id, i, rec(vec![i as f32, 0.0]))
                    .unwrap();
            }
            store.flush().unwrap();
            info.collection_id
        };

        // フラッシュ後は WAL リプレイなしで全件見える
        let store = open(dir.path());
        let view = store.view(cid).unwrap();
        assert_eq!(view.segments.len(), 1);
        assert!(view.memtable.is_empty());
        assert_eq!(view.live_len(), 10);
        assert_eq!(view.get(7).unwrap().vector, vec![7.0, 0.0]);
    }

    #[test]
    fn segment_plus_wal_composition() {
        let dir = tempfile::tempdir().unwrap();
        let cid = {
            let store = open(dir.path());
            let info = store.create_collection("docs", 1, Metric::L2).unwrap();
            store.upsert(info.collection_id, 1, rec(vec![1.0])).unwrap();
            store.upsert(info.collection_id, 2, rec(vec![2.0])).unwrap();
            store.flush().unwrap();
            // フラッシュ後の追加書き込み (WAL のみ)
            store
                .upsert(info.collection_id, 1, rec(vec![10.0]))
                .unwrap(); // 上書き
            store.delete(info.collection_id, 2).unwrap(); // セグメント行の削除
            store.upsert(info.collection_id, 3, rec(vec![3.0])).unwrap(); // 新規
            info.collection_id
        };

        let store = open(dir.path());
        let view = store.view(cid).unwrap();
        assert_eq!(view.get(1).unwrap().vector, vec![10.0]); // memtable が勝つ
        assert!(view.get(2).is_none()); // tombstone が効く
        assert_eq!(view.get(3).unwrap().vector, vec![3.0]);
        assert_eq!(view.live_len(), 2);
    }

    #[test]
    fn newest_wins_across_segments() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        let cid = info.collection_id;

        store.upsert(cid, 1, rec(vec![1.0])).unwrap();
        store.upsert(cid, 2, rec(vec![2.0])).unwrap();
        store.flush().unwrap(); // seg A
        store.upsert(cid, 1, rec(vec![100.0])).unwrap(); // seg B で上書き
        store.delete(cid, 2).unwrap(); // seg B の tombstone
        store.flush().unwrap(); // seg B

        let view = store.view(cid).unwrap();
        assert_eq!(view.segments.len(), 2);
        assert_eq!(view.get(1).unwrap().vector, vec![100.0]);
        assert!(view.get(2).is_none());
        assert_eq!(view.live_len(), 1);

        // is_live: 古いセグメント (rank 2) の id=1 は新セグメントに shadow される
        assert!(!view.is_live(1, 2));
        assert!(view.is_live(1, 1));
    }

    #[test]
    fn threshold_triggers_auto_flush() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path(),
            StoreOptions {
                flush_threshold_bytes: 64, // 極小
                ..Default::default()
            },
        )
        .unwrap();
        let info = store.create_collection("docs", 8, Metric::L2).unwrap();
        for i in 0..10u64 {
            store
                .upsert(info.collection_id, i, rec(vec![0.5; 8]))
                .unwrap();
        }
        let view = store.view(info.collection_id).unwrap();
        assert!(!view.segments.is_empty(), "auto flush must have happened");
        assert_eq!(view.live_len(), 10);
    }

    #[test]
    fn drop_collection_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = open(dir.path());
            let info = store.create_collection("a", 1, Metric::L2).unwrap();
            store.upsert(info.collection_id, 1, rec(vec![1.0])).unwrap();
            store.flush().unwrap();
            store.drop_collection("a").unwrap();
            store.create_collection("b", 2, Metric::Dot).unwrap();
        }
        let store = open(dir.path());
        assert_eq!(store.collection_names(), vec!["b"]);
        assert!(store.collection_info("a").is_err());
        // 再作成しても古いデータは見えない
        let info = store.create_collection("a", 1, Metric::L2).unwrap();
        assert_eq!(store.view(info.collection_id).unwrap().live_len(), 0);
    }

    #[test]
    fn empty_and_existing_dir_open() {
        let dir = tempfile::tempdir().unwrap();
        // 新規ディレクトリ
        let store = open(dir.path());
        assert!(store.collection_names().is_empty());
        drop(store);
        // 既存 (空 DB) を再度開く
        let store = open(dir.path());
        assert!(store.collection_names().is_empty());
    }

    #[test]
    fn compaction_merges_and_applies_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        let cid = info.collection_id;

        // seg A: id 0..10 / seg B: id 5 上書き + id 3 削除 / seg C: id 20 追加
        for i in 0..10u64 {
            store.upsert(cid, i, rec(vec![i as f32])).unwrap();
        }
        store.flush().unwrap();
        store.upsert(cid, 5, rec(vec![500.0])).unwrap();
        store.delete(cid, 3).unwrap();
        store.flush().unwrap();
        store.upsert(cid, 20, rec(vec![20.0])).unwrap();
        store.flush().unwrap();

        let before: Vec<(u64, Option<Vec<f32>>)> = (0..25u64)
            .map(|id| (id, store.view(cid).unwrap().get(id).map(|r| r.vector)))
            .collect();

        store.compact().unwrap();

        let view = store.view(cid).unwrap();
        assert_eq!(view.segments.len(), 1, "compaction must leave one segment");
        assert_eq!(view.segments[0].tombstone_count(), 0, "tombstones dropped");
        assert_eq!(view.live_len(), 10); // 0..10 (3 削除) + 20
        for (id, expected) in before {
            assert_eq!(view.get(id).map(|r| r.vector), expected, "id={id}");
        }

        // 再 open しても同じ
        drop(view);
        drop(store);
        let store = open(dir.path());
        let view = store.view(cid).unwrap();
        assert_eq!(view.segments.len(), 1);
        assert_eq!(view.live_len(), 10);
        assert_eq!(view.get(5).unwrap().vector, vec![500.0]);
        assert!(view.get(3).is_none());
    }

    #[test]
    fn auto_compaction_bounds_segment_count_and_disk() {
        fn dir_size(path: &Path) -> u64 {
            let mut total = 0;
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.path().is_dir() {
                    total += dir_size(&entry.path());
                } else {
                    total += entry.metadata().unwrap().len();
                }
            }
            total
        }

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path(),
            StoreOptions {
                compaction_threshold: 4,
                ..Default::default()
            },
        )
        .unwrap();
        let info = store.create_collection("docs", 4, Metric::L2).unwrap();
        let cid = info.collection_id;

        // 同じ 100 id を上書きし続けながら何度もフラッシュする
        let mut max_segments = 0;
        let mut sizes = Vec::new();
        for round in 0..20u64 {
            for i in 0..100u64 {
                store.upsert(cid, i, rec(vec![round as f32; 4])).unwrap();
            }
            store.flush().unwrap();
            let view = store.view(cid).unwrap();
            max_segments = max_segments.max(view.segments.len());
            sizes.push(dir_size(dir.path()));
        }

        assert!(
            max_segments <= 4,
            "segment count must stay bounded: {max_segments}"
        );
        // ディスク使用量が発散しない: 後半の最大値が前半の最大値の 2 倍以内
        let first_half_max = *sizes[..10].iter().max().unwrap();
        let second_half_max = *sizes[10..].iter().max().unwrap();
        assert!(
            second_half_max <= first_half_max * 2,
            "disk must converge: first={first_half_max}, second={second_half_max}"
        );
        // live データは常に 100 件
        assert_eq!(store.view(cid).unwrap().live_len(), 100);
    }

    #[test]
    fn search_view_survives_concurrent_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        let cid = info.collection_id;
        for i in 0..10u64 {
            store.upsert(cid, i, rec(vec![i as f32])).unwrap();
            store.flush().unwrap(); // セグメントを積む (自動コンパクション込み)
        }
        // 検索用スナップショットを保持したままコンパクション
        let view = store.view(cid).unwrap();
        store.compact().unwrap();
        // 旧セグメントのファイルは消えているが、mmap 保持中の view は使える
        assert_eq!(view.live_len(), 10);
        assert_eq!(view.get(7).unwrap().vector, vec![7.0]);
        // 新しい view はコンパクション後の 1 セグメント
        assert_eq!(store.view(cid).unwrap().segments.len(), 1);
    }

    #[test]
    fn in_memory_mode_works_without_files() {
        let store = Store::in_memory();
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        store.upsert(info.collection_id, 1, rec(vec![1.0])).unwrap();
        store.flush().unwrap(); // no-op
        let view = store.view(info.collection_id).unwrap();
        assert_eq!(view.live_len(), 1);
        assert!(view.segments.is_empty());
    }
}
