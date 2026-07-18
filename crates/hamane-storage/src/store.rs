//! Store: manifest / WAL / セグメント / memtable を束ねる永続化装置
//! (docs/design/storage.md §5–7, docs/design/query.md §1)。
//!
//! - 書き込みは内部 Mutex で直列化し、WAL append + sync 後に memtable へ反映する
//! - 読み取りは `view()` でスナップショット (LiveView) を取り、ロック外で行う
//! - フラッシュ (セグメント化 + HNSW 構築) とコンパクションは専用の
//!   メンテナンススレッドで実行する (todo 504)。書き込みスレッドは
//!   「active memtable → フラッシュ待ち (pending) への切り替え + 新 WAL 作成」
//!   という短い臨界区間しか持たないため、フラッシュ中も書き込みは停止しない
//! - セグメント集合を変更するのはメンテナンススレッドのみ (drop_collection を除く)。
//!   フラッシュとコンパクションは同一スレッド上で直列に実行される

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};

use hamane_core::{HamaneError, Id, Metric, Result};

use hamane_index::HnswParams;

use crate::format::corrupted;
use crate::manifest::{CollectionEntry, Manifest, SegmentEntry, CURRENT_FILE};
use crate::memtable::{Memtable, StoredRecord};
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
    /// SQ8 量子化 (todo 602)。有効にすると HNSW 探索の距離計算が u8 になり
    /// メモリ帯域を節約する。結果は f32 で再ランクされ recall を保つ
    pub sq8: bool,
    /// セグメント並列検索の並列度 (todo 801)。0 = 自動 (論理コア数)、
    /// 1 = 逐次。プールは Database 全体で共有され、初回の複数セグメント
    /// 検索まで worker スレッドは起動しない
    pub search_threads: usize,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            sync: SyncPolicy::Always,
            flush_threshold_bytes: 64 * 1024 * 1024,
            hnsw: HnswParams::default(),
            hnsw_min_rows: 1024,
            compaction_threshold: 4,
            sq8: false,
            search_threads: 0,
        }
    }
}

impl StoreOptions {
    /// 値の妥当性を検証する (todo 507)。Store::open が呼ぶ。
    fn validate(&self) -> Result<()> {
        let bad = |msg: &str| Err(HamaneError::InvalidConfig(msg.into()));
        if self.flush_threshold_bytes == 0 {
            return bad("flush_threshold_bytes must be > 0");
        }
        if self.compaction_threshold < 2 {
            return bad("compaction_threshold must be >= 2");
        }
        if let SyncPolicy::EveryN(0) = self.sync {
            return bad("SyncPolicy::EveryN(0) is invalid");
        }
        let h = &self.hnsw;
        if h.m == 0 || h.m0 == 0 {
            return bad("hnsw.m and hnsw.m0 must be > 0");
        }
        if h.m < 2 {
            return bad("hnsw.m must be >= 2 (ml = 1/ln(m) requires m > 1)");
        }
        if h.ef_construction == 0 || h.ef_search == 0 {
            return bad("hnsw.ef_construction and hnsw.ef_search must be > 0");
        }
        Ok(())
    }
}

/// collection の永続化メタ情報 (Store が返す)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionInfo {
    pub collection_id: u32,
    pub dim: u32,
    pub metric: Metric,
}

/// セグメント 1 個の要約 (デバッグ・info 表示用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentStats {
    pub seg_id: u64,
    pub record_count: usize,
    pub tombstone_count: usize,
    pub has_hnsw: bool,
}

struct CollectionState {
    name: String,
    dim: u32,
    metric: Metric,
    memtable: Memtable,
    /// seg_id 降順 (新しい順)
    segments: Vec<Arc<Segment>>,
    /// live なレコード数。書き込みごとに差分維持する (todo 503)。
    /// フラッシュ・コンパクションでは不変 (データの置き場所が変わるだけ)
    live_len: usize,
    /// 文字列 ID → 内部 ID の辞書 (todo 601)。`_ext_id` メタデータから
    /// open 時に再構築され、以降は upsert で維持される
    ext_ids: HashMap<String, Id>,
    /// 文字列 ID 用の内部採番 (EXT_ID_BASE から単調増加)
    next_ext_id: Id,
}

impl CollectionState {
    fn new(name: String, dim: u32, metric: Metric) -> Self {
        Self {
            name,
            dim,
            metric,
            memtable: Memtable::new(),
            segments: Vec::new(),
            live_len: 0,
            ext_ids: HashMap::new(),
            next_ext_id: hamane_core::EXT_ID_BASE,
        }
    }

    /// upsert されたレコードの `_ext_id` メタデータを辞書に反映する
    /// (WAL リプレイでも呼ばれるため、辞書は復旧後も一貫する)。
    fn track_ext_id(&mut self, id: Id, metadata: &hamane_core::Metadata) {
        if let Some(hamane_core::MetaValue::Str(ext)) = metadata.get(hamane_core::EXT_ID_META_KEY) {
            self.ext_ids.insert(ext.clone(), id);
            self.next_ext_id = self.next_ext_id.max(id + 1);
        }
    }
}

/// セグメント群から live レコード数と文字列 ID 辞書を再構築する (open 時)。
fn scan_segments(segments: &[Arc<Segment>]) -> Result<(usize, HashMap<String, Id>, Id)> {
    let view = LiveView {
        memtables: Vec::new(),
        segments: segments.to_vec(),
        live_len: 0,
    };
    let mut n = 0;
    let mut ext_ids = HashMap::new();
    let mut next_ext_id = hamane_core::EXT_ID_BASE;
    for (i, seg) in view.segments.iter().enumerate() {
        for row in 0..seg.len() as u32 {
            let id = seg.id(row);
            if view.is_live(id, i) {
                n += 1;
                // 文字列 ID の行 (採番領域) のみメタデータをデコードする
                if id >= hamane_core::EXT_ID_BASE {
                    if let Some(hamane_core::MetaValue::Str(ext)) =
                        seg.metadata(row)?.get(hamane_core::EXT_ID_META_KEY)
                    {
                        ext_ids.insert(ext.clone(), id);
                        next_ext_id = next_ext_id.max(id + 1);
                    }
                }
            }
        }
    }
    Ok((n, ext_ids, next_ext_id))
}

/// フラッシュ待ちの世代 (rotate で active memtable から切り替わった不変スナップショット)。
struct PendingFlush {
    /// この世代までのデータが入っている WAL seq (フラッシュ完了で削除可能になる)
    wal_seq: u64,
    /// collection ごとの不変 memtable (空の collection は含まない)
    memtables: HashMap<u32, Arc<Memtable>>,
}

struct StoreState {
    manifest: Manifest,
    /// アクティブ WAL とその seq。in-memory モードでは None
    wal: Option<(u64, WalWriter)>,
    collections: HashMap<u32, CollectionState>,
    names: HashMap<String, u32>,
    /// フラッシュ待ちの世代 (高々 1 個)。メンテナンススレッドが消化する
    pending_flush: Option<PendingFlush>,
    /// メンテナンススレッドの直近の失敗。次の flush()/compact() 呼び出しに返す
    maint_error: Option<String>,
}

impl StoreState {
    /// id が現在 live か (active → pending → セグメント降順の優先解決)。
    fn is_id_live(&self, cid: u32, id: Id) -> bool {
        let col = &self.collections[&cid];
        if col.memtable.is_deleted(id) {
            return false;
        }
        if col.memtable.get(id).is_some() {
            return true;
        }
        if let Some(pending) = &self.pending_flush {
            if let Some(mt) = pending.memtables.get(&cid) {
                if mt.is_deleted(id) {
                    return false;
                }
                if mt.get(id).is_some() {
                    return true;
                }
            }
        }
        for seg in &col.segments {
            if seg.is_tombstoned(id) {
                return false;
            }
            if seg.contains(id) {
                return true;
            }
        }
        false
    }
}

/// 検索・点参照用のスナップショット (docs/design/storage.md §7)。
///
/// source rank: 0 = memtable, 1 = 最新セグメント, 2 = その次…
pub struct LiveView {
    /// 新しい順の memtable 列。[0] = active、[1] = フラッシュ待ち (あれば)
    memtables: Vec<Arc<Memtable>>,
    /// seg_id 降順
    pub segments: Vec<Arc<Segment>>,
    /// スナップショット時点の live レコード数 (Store が差分維持した値)
    live_len: usize,
}

impl LiveView {
    /// 検索対象の memtable 列 (新しい順)。[0] = active、以降フラッシュ待ち。
    pub fn memtables(&self) -> &[Arc<Memtable>] {
        &self.memtables
    }

    /// rank のソースで見つかった id が、より新しいソースに
    /// 上書き・削除されていないか判定する。
    ///
    /// rank: 0..memtables().len() が memtable 列 (新しい順)、
    /// それ以降がセグメント (seg_id 降順)。
    pub fn is_live(&self, id: Id, source_rank: usize) -> bool {
        let mt_count = self.memtables.len();
        for mt in &self.memtables[..source_rank.min(mt_count)] {
            if mt.get(id).is_some() || mt.is_deleted(id) {
                return false;
            }
        }
        if source_rank > mt_count {
            for seg in &self.segments[..source_rank - mt_count] {
                if seg.contains(id) || seg.is_tombstoned(id) {
                    return false;
                }
            }
        }
        true
    }

    /// 点参照。memtable 列 → セグメント降順の優先解決。
    pub fn get(&self, id: Id) -> Option<StoredRecord> {
        for mt in &self.memtables {
            if mt.is_deleted(id) {
                return None;
            }
            if let Some(rec) = mt.get(id) {
                return Some(rec.clone());
            }
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

    /// live なレコード数 (O(1)。Store が書き込みごとに差分維持した値)。
    pub fn live_len(&self) -> usize {
        self.live_len
    }

    /// セグメント構成の要約 (新しい順)。
    pub fn segment_stats(&self) -> Vec<SegmentStats> {
        self.segments
            .iter()
            .map(|s| SegmentStats {
                seg_id: s.seg_id(),
                record_count: s.len(),
                tombstone_count: s.tombstone_count(),
                has_hnsw: s.has_hnsw(),
            })
            .collect()
    }
}

/// メンテナンススレッドへの要求。
enum MaintMsg {
    /// pending_flush を消化する (完了は pending_flush == None + Condvar で観測)
    Flush,
    /// フラッシュ後にコンパクションを実行し、完了を ack で通知する
    Compact { ack: mpsc::Sender<()> },
    /// スレッド終了 (残りの pending はフラッシュしてから)
    Shutdown,
}

/// Store の共有部。メンテナンススレッドと API 呼び出しの両方から参照される。
struct Shared {
    db_dir: Option<PathBuf>,
    options: StoreOptions,
    state: Mutex<StoreState>,
    /// pending_flush の消化完了 (またはエラー記録) で notify される
    flush_done: Condvar,
    /// プロセス排他ロック (todo 702)。fd が開いている間 flock を保持する
    _process_lock: Option<std::fs::File>,
    /// レプリカモード (todo 903)。true なら書き込み API を拒否する
    follower: bool,
}

/// データベース 1 個分の永続化装置。`Send + Sync`。
pub struct Store {
    shared: Arc<Shared>,
    maint_tx: Option<mpsc::Sender<MaintMsg>>,
    maint_join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Some(tx) = self.maint_tx.take() {
            let _ = tx.send(MaintMsg::Shutdown);
        }
        if let Some(join) = self.maint_join.lock().expect("lock poisoned").take() {
            let _ = join.join();
        }
    }
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

/// `<db_dir>/LOCK` に排他 flock をかける (todo 702)。
/// 別プロセス (または同一プロセスの別 Store) が保持していれば Locked を返す。
/// flock はプロセス終了・クラッシュで自動解放されるため残骸の問題はない。
/// 返された File が生きている間ロックが保持される。
#[cfg(unix)]
fn acquire_process_lock(db_dir: &Path) -> Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let path = db_dir.join("LOCK");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        return Ok(file);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Err(HamaneError::Locked(db_dir.display().to_string()))
    } else {
        Err(HamaneError::Io(err))
    }
}

/// 非 unix はロックなし (ベストエフォート。多重 open の防止は保証されない)。
#[cfg(not(unix))]
fn acquire_process_lock(db_dir: &Path) -> Result<std::fs::File> {
    let path = db_dir.join("LOCK");
    Ok(std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?)
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

impl Store {
    /// 実行時オプション (フラッシュ閾値・HNSW パラメータ等)。
    pub fn options(&self) -> &StoreOptions {
        &self.shared.options
    }

    /// データベースディレクトリ (in-memory なら None)。
    pub fn db_dir(&self) -> Option<&Path> {
        self.shared.db_dir.as_deref()
    }

    /// 永続化なしの Store (従来の in-memory モード)。メンテナンススレッドは持たない。
    pub fn in_memory() -> Self {
        Self {
            shared: Arc::new(Shared {
                db_dir: None,
                options: StoreOptions::default(),
                state: Mutex::new(StoreState {
                    manifest: Manifest::default(),
                    wal: None,
                    collections: HashMap::new(),
                    names: HashMap::new(),
                    pending_flush: None,
                    maint_error: None,
                }),
                flush_done: Condvar::new(),
                _process_lock: None,
                follower: false,
            }),
            maint_tx: None,
            maint_join: Mutex::new(None),
        }
    }

    /// ディレクトリを開く (なければ初期化)。docs/design/storage.md §5 の手順。
    pub fn open(db_dir: &Path, options: StoreOptions) -> Result<Self> {
        options.validate()?;
        std::fs::create_dir_all(db_dir)?;
        let process_lock = acquire_process_lock(db_dir)?;
        std::fs::create_dir_all(wal_dir(db_dir))?;
        std::fs::create_dir_all(collections_dir(db_dir))?;

        let (mut state, max_seq) = Self::load_state(db_dir)?;

        // 6. 新しいアクティブ WAL (既存はそのまま残し、次のフラッシュで削除)
        let new_seq = max_seq + 1;
        let writer =
            WalWriter::create(&wal_dir(db_dir).join(wal_file_name(new_seq)), options.sync)?;
        state.wal = Some((new_seq, writer));

        // 7. メンテナンススレッド起動
        let shared = Arc::new(Shared {
            db_dir: Some(db_dir.to_path_buf()),
            options,
            state: Mutex::new(state),
            flush_done: Condvar::new(),
            _process_lock: Some(process_lock),
            follower: false,
        });
        let (tx, rx) = mpsc::channel();
        let maint_shared = Arc::clone(&shared);
        let join = std::thread::Builder::new()
            .name("hamane-maint".into())
            .spawn(move || maint_shared.maintenance_loop(rx))
            .expect("failed to spawn maintenance thread");

        Ok(Self {
            shared,
            maint_tx: Some(tx),
            maint_join: Mutex::new(Some(join)),
        })
    }

    /// レプリカとして開く (todo 903、docs/design/replication.md §4)。
    ///
    /// 書き込み API は `ReadOnlyReplica` を返し、アクティブ WAL の作成・
    /// メンテナンススレッド (自動フラッシュ・コンパクション) を行わない。
    /// 状態の更新は同期ループ (904) が `apply_wal_frames` /
    /// `switch_generation` で行う。flock は通常どおり取る。
    pub fn open_follower(db_dir: &Path, options: StoreOptions) -> Result<Self> {
        options.validate()?;
        std::fs::create_dir_all(db_dir)?;
        let process_lock = acquire_process_lock(db_dir)?;
        std::fs::create_dir_all(wal_dir(db_dir))?;
        std::fs::create_dir_all(collections_dir(db_dir))?;

        let (state, _max_seq) = Self::load_state(db_dir)?;
        Ok(Self {
            shared: Arc::new(Shared {
                db_dir: Some(db_dir.to_path_buf()),
                options,
                state: Mutex::new(state),
                flush_done: Condvar::new(),
                _process_lock: Some(process_lock),
                follower: true,
            }),
            maint_tx: None,
            maint_join: Mutex::new(None),
        })
    }

    /// ディレクトリからインメモリ状態を構築する (open / switch_generation 共用)。
    /// storage.md §5 の手順 1〜5。戻り値は (状態, リプレイした最大 WAL seq)。
    fn load_state(db_dir: &Path) -> Result<(StoreState, u64)> {
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
            // manifest は年代順 (古→新)。内部リストは新→古で保持する
            segments.reverse();
            names.insert(entry.name.clone(), entry.collection_id);
            // 初期 live_len と文字列 ID 辞書はセグメントの全走査で確定し、以降は差分維持
            let (live_len, ext_ids, next_ext_id) = scan_segments(&segments)?;
            let mut col = CollectionState::new(entry.name.clone(), entry.dim, entry.metric);
            col.segments = segments;
            col.live_len = live_len;
            col.ext_ids = ext_ids;
            col.next_ext_id = next_ext_id;
            collections.insert(entry.collection_id, col);
        }

        let mut state = StoreState {
            manifest,
            wal: None,
            collections,
            names,
            pending_flush: None,
            maint_error: None,
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
        cleanup_collection_dirs(db_dir, &state.manifest)?;

        Ok((state, max_seq))
    }

    /// 書き込み API の共通ガード (todo 903)。
    fn ensure_writable(&self) -> Result<()> {
        if self.shared.follower {
            Err(HamaneError::ReadOnlyReplica)
        } else {
            Ok(())
        }
    }

    /// follower モード専用 (todo 903): fetch した WAL フレーム列を状態に適用する。
    ///
    /// `bytes` は WAL ファイルの magic を除いたフレーム境界から始まること。
    /// 適用した完全なフレームのバイト数を返す。末尾の不完全なフレーム
    /// (長さ不足・CRC 不一致) は「まだ届いていない」として残し、呼び出し側が
    /// 続きを連結して再度渡す (ローカル復旧の停止規則と同じ。storage.md §2)。
    ///
    /// 呼び出しは同期ループの単一スレッドから行うこと (`switch_generation` と
    /// 並行に呼んではならない)。
    pub fn apply_wal_frames(&self, bytes: &[u8]) -> Result<usize> {
        if !self.shared.follower {
            return Err(HamaneError::InvalidConfig(
                "apply_wal_frames requires a follower store".into(),
            ));
        }
        let mut state = self.shared.state.lock().expect("lock poisoned");
        let mut pos = 0;
        while let crate::format::Frame::Ok { body, consumed } =
            crate::format::read_frame(&bytes[pos..])
        {
            // フレームは完全なので decode 失敗は真の破損
            let record = WalRecord::decode(body)?;
            Self::apply_record(&mut state, record)?;
            pos += consumed;
        }
        Ok(pos)
    }

    /// follower モード専用 (todo 903): ディスク上の CURRENT が指す世代へ
    /// 状態を差し替える。世代が進んでいなければ何もせず false。
    ///
    /// 同期ループが新しい manifest・セグメントのファイルを配置し CURRENT を
    /// 切り替えた後に呼ぶ。旧 LiveView を持つ読者は Arc 経由で旧世代を
    /// 安全に読み終えられる (コンパクション後の削除と同じ性質)。
    pub fn switch_generation(&self) -> Result<bool> {
        if !self.shared.follower {
            return Err(HamaneError::InvalidConfig(
                "switch_generation requires a follower store".into(),
            ));
        }
        let db_dir = self.shared.db_dir.as_deref().expect("follower has db_dir");
        let disk_gen = Manifest::load(db_dir)?.gen;
        {
            let state = self.shared.state.lock().expect("lock poisoned");
            if disk_gen == state.manifest.gen {
                return Ok(false);
            }
        }
        // 構築 (mmap + セグメント走査 + WAL リプレイ) はロック外で行い、
        // 差し替えだけロックする
        let (new_state, _) = Self::load_state(db_dir)?;
        *self.shared.state.lock().expect("lock poisoned") = new_state;
        Ok(true)
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
                state
                    .collections
                    .insert(collection_id, CollectionState::new(name, dim, metric));
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
                if !state.collections.contains_key(&collection_id) {
                    return Err(corrupted("WAL upsert for unknown collection"));
                }
                let was_live = state.is_id_live(collection_id, id);
                let col = state.collections.get_mut(&collection_id).unwrap();
                if !was_live {
                    col.live_len += 1;
                }
                col.track_ext_id(id, &metadata);
                col.memtable.upsert(id, StoredRecord { vector, metadata });
            }
            WalRecord::Delete { collection_id, id } => {
                if !state.collections.contains_key(&collection_id) {
                    return Err(corrupted("WAL delete for unknown collection"));
                }
                let was_live = state.is_id_live(collection_id, id);
                let col = state.collections.get_mut(&collection_id).unwrap();
                if was_live {
                    col.live_len -= 1;
                }
                col.memtable.delete(id);
            }
        }
        Ok(())
    }

    /// WAL に書いて sync してから状態に反映する。
    ///
    /// SyncPolicy::Batch では SyncToken を返す。呼び出し側は state ロックを
    /// 解放した後に `token.wait()` を呼ぶこと (それまで呼び出し元に Ok を
    /// 返してはならない = ack 済みは常に永続)。
    fn log_and_apply(
        &self,
        state: &mut StoreState,
        record: WalRecord,
    ) -> Result<Option<crate::wal::SyncToken>> {
        let mut token = None;
        if let Some((_, wal)) = state.wal.as_mut() {
            wal.append(&record)?;
            token = wal.sync()?;
        }
        Self::apply_record(state, record)?;
        Ok(token)
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
        self.ensure_writable()?;
        let token = {
            let mut state = self.shared.state.lock().expect("lock poisoned");
            if state.names.contains_key(name) {
                return Err(HamaneError::CollectionExists(name.to_owned()));
            }
            let collection_id = state.manifest.next_collection_id;
            let token = self.log_and_apply(
                &mut state,
                WalRecord::CreateCollection {
                    collection_id,
                    name: name.to_owned(),
                    dim,
                    metric,
                },
            )?;
            (token, collection_id)
        };
        if let Some(t) = token.0 {
            t.wait()?;
        }
        Ok(CollectionInfo {
            collection_id: token.1,
            dim,
            metric,
        })
    }

    pub fn drop_collection(&self, name: &str) -> Result<()> {
        self.ensure_writable()?;
        let token = {
            let mut state = self.shared.state.lock().expect("lock poisoned");
            let Some(&collection_id) = state.names.get(name) else {
                return Err(HamaneError::CollectionNotFound(name.to_owned()));
            };
            let token =
                self.log_and_apply(&mut state, WalRecord::DropCollection { collection_id })?;
            // フラッシュ待ちの世代からも外す (メンテナンススレッドが書かないように)
            if let Some(pending) = state.pending_flush.as_mut() {
                pending.memtables.remove(&collection_id);
            }
            token
        };
        if let Some(t) = token {
            t.wait()?;
        }
        Ok(())
    }

    pub fn collection_info(&self, name: &str) -> Result<CollectionInfo> {
        let state = self.shared.state.lock().expect("lock poisoned");
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
        let state = self.shared.state.lock().expect("lock poisoned");
        let mut names: Vec<String> = state.names.keys().cloned().collect();
        names.sort();
        names
    }

    // -----------------------------------------------------------------------
    // 書き込み
    // -----------------------------------------------------------------------

    /// 文字列 ID を内部 ID に解決する (未登録なら None)。
    pub fn resolve_ext_id(&self, collection_id: u32, ext: &str) -> Result<Option<Id>> {
        let state = self.shared.state.lock().expect("lock poisoned");
        Self::check_collection(&state, collection_id)?;
        Ok(state.collections[&collection_id].ext_ids.get(ext).copied())
    }

    /// RecordId (u64 or 文字列) のバッチ upsert。文字列 ID はロック内で
    /// 解決・採番され、`_ext_id` メタデータが自動付与される (todo 601)。
    pub fn upsert_batch_records(
        &self,
        collection_id: u32,
        records: Vec<(hamane_core::RecordId, StoredRecord)>,
    ) -> Result<()> {
        self.ensure_writable()?;
        // 文字列 ID の解決・採番と _ext_id 注入 (ロック内で原子的に)
        let resolved: Vec<(Id, StoredRecord)> = {
            let mut state = self.shared.state.lock().expect("lock poisoned");
            Self::check_collection(&state, collection_id)?;
            let col = state.collections.get_mut(&collection_id).unwrap();
            records
                .into_iter()
                .map(|(rid, mut record)| {
                    let id = match rid {
                        hamane_core::RecordId::Num(n) => n,
                        hamane_core::RecordId::Str(s) => {
                            let id = *col.ext_ids.entry(s.clone()).or_insert_with(|| {
                                let id = col.next_ext_id;
                                col.next_ext_id += 1;
                                id
                            });
                            record.metadata.insert(
                                hamane_core::EXT_ID_META_KEY.into(),
                                hamane_core::MetaValue::Str(s),
                            );
                            id
                        }
                    };
                    (id, record)
                })
                .collect()
        };
        self.upsert_batch(collection_id, resolved)
    }

    /// RecordId (u64 or 文字列) の削除。判定と削除は 1 臨界区間で原子的。
    pub fn delete_record(&self, collection_id: u32, rid: &hamane_core::RecordId) -> Result<bool> {
        self.ensure_writable()?;
        let id = {
            let state = self.shared.state.lock().expect("lock poisoned");
            Self::check_collection(&state, collection_id)?;
            match rid {
                hamane_core::RecordId::Num(n) => *n,
                hamane_core::RecordId::Str(s) => {
                    match state.collections[&collection_id].ext_ids.get(s) {
                        Some(&id) => id,
                        None => return Ok(false),
                    }
                }
            }
        };
        self.delete(collection_id, id)
    }

    pub fn upsert(&self, collection_id: u32, id: Id, record: StoredRecord) -> Result<()> {
        self.upsert_batch(collection_id, vec![(id, record)])
    }

    /// 複数レコードを 1 回の WAL sync でまとめて書く。
    pub fn upsert_batch(&self, collection_id: u32, records: Vec<(Id, StoredRecord)>) -> Result<()> {
        self.ensure_writable()?;
        let token = {
            let mut state = self.shared.state.lock().expect("lock poisoned");
            Self::check_collection(&state, collection_id)?;
            let mut token = None;
            if let Some((_, wal)) = state.wal.as_mut() {
                for (id, record) in &records {
                    wal.append(&WalRecord::Upsert {
                        collection_id,
                        id: *id,
                        vector: record.vector.clone(),
                        metadata: record.metadata.clone(),
                    })?;
                }
                token = wal.sync()?;
            }
            for (id, record) in records {
                let was_live = state.is_id_live(collection_id, id);
                let col = state.collections.get_mut(&collection_id).unwrap();
                if !was_live {
                    col.live_len += 1;
                }
                col.track_ext_id(id, &record.metadata);
                col.memtable.upsert(id, record);
            }
            self.maybe_flush(state)?;
            token
        };
        // group commit の fsync 待ちはロックの外 (他スレッドの書き込みと相乗り)
        if let Some(t) = token {
            t.wait()?;
        }
        Ok(())
    }

    /// 削除。呼び出し前に id が live だったかを返す (1 臨界区間で判定・適用)。
    pub fn delete(&self, collection_id: u32, id: Id) -> Result<bool> {
        self.ensure_writable()?;
        let (existed, token) = {
            let mut state = self.shared.state.lock().expect("lock poisoned");
            Self::check_collection(&state, collection_id)?;
            let existed = state.is_id_live(collection_id, id);
            let token = self.log_and_apply(&mut state, WalRecord::Delete { collection_id, id })?;
            self.maybe_flush(state)?;
            (existed, token)
        };
        if let Some(t) = token {
            t.wait()?;
        }
        Ok(existed)
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
        let state = self.shared.state.lock().expect("lock poisoned");
        let col = state.collections.get(&collection_id).ok_or_else(|| {
            HamaneError::CollectionNotFound(format!("collection_id={collection_id}"))
        })?;
        let mut memtables = vec![Arc::new(col.memtable.snapshot())];
        if let Some(pending) = &state.pending_flush {
            if let Some(mt) = pending.memtables.get(&collection_id) {
                memtables.push(Arc::clone(mt));
            }
        }
        Ok(LiveView {
            memtables,
            segments: col.segments.clone(),
            live_len: col.live_len,
        })
    }

    // -----------------------------------------------------------------------
    // フラッシュ (docs/design/storage.md §6)
    // -----------------------------------------------------------------------

    /// 書き込み後の自動フラッシュ判定。閾値超過なら rotate してメンテナンス
    /// スレッドに通知する (待たない)。前のフラッシュが進行中で active が
    /// 閾値の 4 倍まで膨らんだ場合のみ、消化を待つ (backpressure)。
    fn maybe_flush(&self, mut state: std::sync::MutexGuard<'_, StoreState>) -> Result<()> {
        if self.shared.db_dir.is_none() {
            return Ok(());
        }
        let threshold = self.shared.options.flush_threshold_bytes;
        let over = state
            .collections
            .values()
            .any(|c| c.memtable.approx_bytes() >= threshold);
        if !over {
            return Ok(());
        }
        if state.pending_flush.is_some() {
            let hard_limit = threshold.saturating_mul(4);
            let too_big = state
                .collections
                .values()
                .any(|c| c.memtable.approx_bytes() >= hard_limit);
            if !too_big {
                return Ok(()); // 前のフラッシュ完了後に改めて rotate される
            }
            // backpressure: エラー時は待たない (WAL があるためデータは安全)
            while state.pending_flush.is_some() && state.maint_error.is_none() {
                state = self.shared.flush_done.wait(state).expect("lock poisoned");
            }
        }
        if self.shared.rotate(&mut state)? {
            if let Some(tx) = &self.maint_tx {
                let _ = tx.send(MaintMsg::Flush);
            }
        }
        Ok(())
    }

    /// 全 collection の memtable をセグメントへ書き出し、WAL を世代交代する。
    /// 完了まで待つ (同期セマンティクス)。実行はメンテナンススレッド上。
    pub fn flush(&self) -> Result<()> {
        self.ensure_writable()?;
        let Some(tx) = &self.maint_tx else {
            return Ok(()); // in-memory は no-op
        };
        // 前の世代が残っていても必ず消化されるよう、待つ前に送っておく
        let _ = tx.send(MaintMsg::Flush);
        {
            let state = self.shared.state.lock().expect("lock poisoned");
            // 前の世代の消化を待つ (エラーはここで返す)
            let mut state = self.wait_pending_clear(state)?;
            if self.shared.rotate(&mut state)? {
                let _ = tx.send(MaintMsg::Flush);
            }
        }
        let state = self.shared.state.lock().expect("lock poisoned");
        drop(self.wait_pending_clear(state)?);
        Ok(())
    }

    /// pending_flush が消化されるまで待つ。メンテナンス失敗はエラーとして返す
    /// (エラーは一度返したらクリアされ、再度 flush() すると再試行される)。
    fn wait_pending_clear<'a>(
        &self,
        mut state: std::sync::MutexGuard<'a, StoreState>,
    ) -> Result<std::sync::MutexGuard<'a, StoreState>> {
        loop {
            if let Some(err) = state.maint_error.take() {
                return Err(HamaneError::Io(std::io::Error::other(format!(
                    "background maintenance failed: {err}"
                ))));
            }
            if state.pending_flush.is_none() {
                return Ok(state);
            }
            state = self.shared.flush_done.wait(state).expect("lock poisoned");
        }
    }

    // -----------------------------------------------------------------------
    // バックアップ (todo 703)
    // -----------------------------------------------------------------------

    /// 一貫性のあるバックアップを dest ディレクトリに取る。
    /// 復元は dest を通常どおり `Store::open` するだけ。
    ///
    /// 手順: 先に flush で未フラッシュ分をセグメント化した後、state ロックを
    /// 保持したまま manifest と全セグメントをコピーする。**コピー中の書き込みは
    /// 待たされる** (コピーは純粋な I/O で、HNSW 構築よりはるかに短い)。
    /// flush とロック取得のわずかな間に入った書き込みはバックアップに含まれない
    /// (バックアップは常に manifest 世代として一貫)。
    pub fn backup(&self, dest: &Path) -> Result<()> {
        self.ensure_writable()?;
        let Some(db_dir) = self.shared.db_dir.clone() else {
            return Err(HamaneError::InvalidConfig(
                "cannot backup an in-memory database".into(),
            ));
        };
        if dest.exists() && std::fs::read_dir(dest)?.next().is_some() {
            return Err(HamaneError::InvalidConfig(format!(
                "backup destination is not empty: {}",
                dest.display()
            )));
        }
        // 未フラッシュ分をセグメント化 (完了まで待つ)
        self.flush()?;
        std::fs::create_dir_all(dest)?;
        std::fs::create_dir_all(dest.join("wal"))?;
        std::fs::create_dir_all(collections_dir(dest))?;

        // ロック保持中はセグメント集合が変わらない (書き込み・コンパクション停止)
        let state = self.shared.state.lock().expect("lock poisoned");
        let manifest_name = crate::manifest::manifest_file_name(state.manifest.gen);
        std::fs::copy(db_dir.join(&manifest_name), dest.join(&manifest_name))?;
        for col in &state.manifest.collections {
            let src_col = collection_dir(&db_dir, col.collection_id);
            let dst_col = collection_dir(dest, col.collection_id);
            for seg in &col.segments {
                let name = segment_dir_name(seg.seg_id);
                let src_seg = src_col.join(&name);
                let dst_seg = dst_col.join(&name);
                std::fs::create_dir_all(&dst_seg)?;
                for entry in std::fs::read_dir(&src_seg)? {
                    let entry = entry?;
                    std::fs::copy(entry.path(), dst_seg.join(entry.file_name()))?;
                }
            }
        }
        // CURRENT は最後に書く (dest が常に完全な世代を指すように)
        std::fs::write(dest.join(CURRENT_FILE), format!("{manifest_name}\n"))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // コンパクション (todos/401)
    // -----------------------------------------------------------------------

    /// 全 collection の複数セグメントを 1 個に統合する (full merge)。
    /// 上書き・tombstone を物理適用するため、ディスク使用量が live データに収束する。
    /// 実行はメンテナンススレッド上 (完了まで待つ)。
    pub fn compact(&self) -> Result<()> {
        self.ensure_writable()?;
        let Some(tx) = &self.maint_tx else {
            return Ok(()); // in-memory は no-op
        };
        {
            let mut state = self.shared.state.lock().expect("lock poisoned");
            self.shared.rotate(&mut state)?;
        }
        let (ack_tx, ack_rx) = mpsc::channel();
        let _ = tx.send(MaintMsg::Compact { ack: ack_tx });
        let _ = ack_rx.recv(); // メンテスレッド終了時は RecvErr — エラーは下で拾う
        let mut state = self.shared.state.lock().expect("lock poisoned");
        if let Some(err) = state.maint_error.take() {
            return Err(HamaneError::Io(std::io::Error::other(format!(
                "background maintenance failed: {err}"
            ))));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// メンテナンススレッド (todo 504)
// ---------------------------------------------------------------------------

impl Shared {
    /// メンテナンススレッドの本体。フラッシュとコンパクションを直列に処理する。
    /// セグメント集合の変更はこのスレッドに閉じるため、フラッシュ・
    /// コンパクション間の競合はない。
    fn maintenance_loop(self: Arc<Self>, rx: mpsc::Receiver<MaintMsg>) {
        while let Ok(msg) = rx.recv() {
            match msg {
                MaintMsg::Flush => {
                    self.run_maintenance(false);
                }
                MaintMsg::Compact { ack } => {
                    self.run_maintenance(true);
                    let _ = ack.send(());
                }
                MaintMsg::Shutdown => {
                    // 残った pending を消化してから終了 (失敗しても WAL がある)
                    self.run_maintenance(false);
                    break;
                }
            }
        }
    }

    /// フラッシュ + コンパクション (自動 or 全体) を 1 サイクル実行する。
    fn run_maintenance(&self, compact_all: bool) {
        let result = self.flush_pending().and_then(|_| {
            let threshold = if compact_all {
                2
            } else {
                self.options.compaction_threshold
            };
            let targets: Vec<u32> = {
                let state = self.state.lock().expect("lock poisoned");
                state
                    .collections
                    .iter()
                    .filter(|(_, c)| c.segments.len() >= threshold)
                    .map(|(cid, _)| *cid)
                    .collect()
            };
            let mode = if compact_all {
                CompactMode::Full
            } else {
                CompactMode::Partial
            };
            for cid in targets {
                self.compact_collection(cid, mode)?;
            }
            Ok(())
        });
        if let Err(e) = result {
            let mut state = self.state.lock().expect("lock poisoned");
            state.maint_error = Some(e.to_string());
        }
        self.flush_done.notify_all();
    }

    /// active memtable 群を pending に切り替え、新しい WAL を開く (短い臨界区間)。
    /// pending が既にあるとき・全 memtable が空のときは何もしない。
    fn rotate(&self, state: &mut StoreState) -> Result<bool> {
        let Some(db_dir) = &self.db_dir else {
            return Ok(false);
        };
        if state.pending_flush.is_some() {
            return Ok(false);
        }
        if state.collections.values().all(|c| c.memtable.is_empty()) {
            return Ok(false);
        }
        let old_seq = state.wal.as_ref().map(|(s, _)| *s).unwrap_or(0);
        let new_seq = old_seq + 1;
        let writer = WalWriter::create(
            &wal_dir(db_dir).join(wal_file_name(new_seq)),
            self.options.sync,
        )?;
        let mut memtables = HashMap::new();
        for (cid, col) in state.collections.iter_mut() {
            if !col.memtable.is_empty() {
                memtables.insert(*cid, Arc::new(std::mem::take(&mut col.memtable)));
            }
        }
        state.wal = Some((new_seq, writer));
        state.pending_flush = Some(PendingFlush {
            wal_seq: old_seq,
            memtables,
        });
        Ok(true)
    }

    /// pending_flush をセグメントとして書き出し、manifest を更新して消化する。
    /// セグメント書き込み (HNSW 構築込み) は state ロックの外で行う。
    fn flush_pending(&self) -> Result<()> {
        let Some(db_dir) = self.db_dir.clone() else {
            return Ok(());
        };
        // 1. 対象を読む (Arc clone) + seg_id 採番
        let (flushed_wal_seq, work) = {
            let state = self.state.lock().expect("lock poisoned");
            let Some(pending) = &state.pending_flush else {
                return Ok(());
            };
            let mut cids: Vec<u32> = pending.memtables.keys().copied().collect();
            cids.sort_unstable();
            let mut next = state.manifest.next_seg_id;
            let mut work: Vec<(u32, Arc<Memtable>, Metric, u64)> = Vec::new();
            for cid in cids {
                // pending 中に drop された collection は書かない
                let Some(col) = state.collections.get(&cid) else {
                    continue;
                };
                work.push((cid, Arc::clone(&pending.memtables[&cid]), col.metric, next));
                next += 1;
            }
            (pending.wal_seq, work)
        };

        // 2. ロック外でセグメントを書く (時間がかかる)
        let mut entries: Vec<(u32, SegmentEntry)> = Vec::new();
        for (cid, memtable, metric, seg_id) in &work {
            let dir = collection_dir(&db_dir, *cid);
            std::fs::create_dir_all(&dir)?;
            let spec = IndexBuildSpec {
                metric: *metric,
                params: self.options.hnsw,
                min_rows: self.options.hnsw_min_rows,
                sq8: self.options.sq8,
            };
            let meta = SegmentWriter::write(&dir, *seg_id, memtable, Some(spec))?;
            entries.push((
                *cid,
                SegmentEntry {
                    seg_id: *seg_id,
                    record_count: meta.record_count,
                    tombstone_count: meta.tombstone_count,
                },
            ));
        }

        // 3. ロック内で manifest 更新 + セグメント open + pending クリア + WAL 削除
        let mut state = self.state.lock().expect("lock poisoned");
        let mut manifest = Manifest {
            gen: state.manifest.gen + 1,
            next_collection_id: state.manifest.next_collection_id,
            next_seg_id: work
                .iter()
                .map(|(_, _, _, sid)| sid + 1)
                .max()
                .unwrap_or(state.manifest.next_seg_id),
            wal_seq: flushed_wal_seq,
            collections: Vec::new(),
        };
        let mut cids: Vec<u32> = state.collections.keys().copied().collect();
        cids.sort_unstable();
        for cid in cids {
            let col = &state.collections[&cid];
            // 内部リストは新→古。manifest は年代順 (古→新) なので反転し、
            // 今回フラッシュした最新セグメントを末尾に足す
            let mut segments: Vec<SegmentEntry> = col
                .segments
                .iter()
                .rev()
                .map(|s| SegmentEntry {
                    seg_id: s.seg_id(),
                    record_count: s.len() as u64,
                    tombstone_count: s.tombstone_count() as u64,
                })
                .collect();
            if let Some((_, entry)) = entries.iter().find(|(c, _)| *c == cid) {
                segments.push(*entry);
            }
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
        for (cid, entry) in &entries {
            if let Some(col) = state.collections.get_mut(cid) {
                let seg = Arc::new(Segment::open(&collection_dir(&db_dir, *cid), entry.seg_id)?);
                col.segments.insert(0, seg); // 降順の先頭 = 最新
            }
        }
        state.pending_flush = None;
        for (seq, path) in list_wal_files(&wal_dir(&db_dir))? {
            if seq <= flushed_wal_seq {
                std::fs::remove_file(&path)?;
            }
        }
        Manifest::gc(&db_dir)?;
        cleanup_collection_dirs(&db_dir, &state.manifest)?;
        Ok(())
    }

    /// collection の全セグメントを newest-wins で 1 個に統合する。
    /// マージと書き込みは state ロックの外で行う (この間セグメント集合は
    /// このスレッドしか変更しないため安全)。
    ///
    /// - `Full`: 全セグメントを 1 個に統合し、tombstone を破棄する
    /// - `Partial`: 最新側から「同規模の連続 run」だけをマージする
    ///   (universal compaction 風、todo 506)。run より古いセグメントが残る場合、
    ///   run 内の tombstone は新セグメントに引き継ぐ
    fn compact_collection(&self, cid: u32, mode: CompactMode) -> Result<()> {
        let Some(db_dir) = self.db_dir.clone() else {
            return Ok(());
        };
        // 1. マージ対象 run を選ぶ (ロック内)。内部リストは新→古
        let (run, older_remains, metric, seg_id) = {
            let state = self.state.lock().expect("lock poisoned");
            let Some(col) = state.collections.get(&cid) else {
                return Ok(()); // drop 済み
            };
            let segs = &col.segments;
            if segs.len() < 2 {
                return Ok(());
            }
            let run_len = match mode {
                CompactMode::Full => segs.len(),
                CompactMode::Partial => {
                    // 最新から「次 (より古い) が run 合計の 4 倍以下」の間つなげる
                    let mut len = 1;
                    let mut total = segs[0].len() as u64;
                    while len < segs.len() {
                        let next = segs[len].len() as u64;
                        if next <= total.saturating_mul(4).max(4) {
                            total += next;
                            len += 1;
                        } else {
                            break;
                        }
                    }
                    if len < self.options.compaction_threshold.max(2) {
                        return Ok(()); // マージに値する run がない
                    }
                    len
                }
            };
            (
                segs[..run_len].to_vec(),
                run_len < segs.len(),
                col.metric,
                state.manifest.next_seg_id,
            )
        };

        // 2. ロック外: newest → oldest に走査して live な行だけを集め、書き出す
        let mut merged = Memtable::new();
        let mut seen: std::collections::HashSet<Id> = std::collections::HashSet::new();
        let mut deleted: std::collections::HashSet<Id> = std::collections::HashSet::new();
        for seg in &run {
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
        // run より古いセグメントが残る場合、tombstone を引き継ぐ
        // (live 値がある id は除く — 同一セグメント内では tombstone が行に勝つため)
        if older_remains {
            for id in &deleted {
                if merged.get(*id).is_none() {
                    merged.delete(*id);
                }
            }
        }
        let merged_len = merged.len() as u64;
        let merged_tombstones = merged.deletes().count() as u64;
        let dir = collection_dir(&db_dir, cid);
        let new_segment = if merged.is_empty() {
            None // 全行が削除済みで tombstone も不要 → セグメントなしに縮退
        } else {
            let spec = IndexBuildSpec {
                metric,
                params: self.options.hnsw,
                min_rows: self.options.hnsw_min_rows,
                sq8: self.options.sq8,
            };
            SegmentWriter::write(&dir, seg_id, &merged.snapshot(), Some(spec))?;
            Some(SegmentEntry {
                seg_id,
                record_count: merged_len,
                tombstone_count: merged_tombstones,
            })
        };

        // 3. ロック内: manifest 更新 + セグメント差し替え
        let mut state = self.state.lock().expect("lock poisoned");
        if !state.collections.contains_key(&cid) {
            return Ok(()); // マージ中に drop された。孤児セグメントは掃除に任せる
        }
        let mut manifest = state.manifest.clone();
        manifest.gen += 1;
        manifest.next_seg_id = manifest.next_seg_id.max(seg_id + 1);
        {
            let col = &state.collections[&cid];
            let entry = manifest
                .collections
                .iter_mut()
                .find(|c| c.collection_id == cid)
                .expect("collection in manifest");
            // 年代順 (古→新): [run より古い残り (反転)] + [マージ結果]
            let mut segments: Vec<SegmentEntry> = col.segments[run.len()..]
                .iter()
                .rev()
                .map(|s| SegmentEntry {
                    seg_id: s.seg_id(),
                    record_count: s.len() as u64,
                    tombstone_count: s.tombstone_count() as u64,
                })
                .collect();
            if let Some(e) = new_segment {
                segments.push(e);
            }
            entry.segments = segments;
        }
        manifest.store(&db_dir)?;
        state.manifest = manifest;

        // インメモリ状態の差し替え。旧セグメントのファイルは削除してよい
        // (検索中の Arc<Segment> は mmap を保持しており、Unix では unlink 後も安全)
        let col = state.collections.get_mut(&cid).unwrap();
        let mut new_list = Vec::with_capacity(col.segments.len());
        if new_segment.is_some() {
            new_list.push(Arc::new(Segment::open(&dir, seg_id)?));
        }
        new_list.extend_from_slice(&col.segments[run.len()..]);
        col.segments = new_list;
        for old in run.iter().map(|s| s.seg_id()) {
            let path = dir.join(segment_dir_name(old));
            if path.exists() {
                std::fs::remove_dir_all(&path)?;
            }
        }
        Manifest::gc(&db_dir)?;
        Ok(())
    }
}

/// コンパクションの範囲。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompactMode {
    /// 全セグメントを 1 個に統合 (明示的な compact())
    Full,
    /// 最新側の同規模 run のみ (フラッシュ後の自動コンパクション)
    Partial,
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

    /// クラッシュ相当のシャットダウン: Drop の Shutdown フラッシュを走らせずに
    /// 終了する (メンテスレッドはチャネル閉鎖で静かに抜ける)。
    /// プロセスロック (todo 702) は解放されるため、同一プロセス内で再 open できる。
    fn simulate_crash(mut store: Store) {
        store.maint_tx.take();
        if let Some(join) = store.maint_join.lock().unwrap().take() {
            let _ = join.join();
        }
        drop(store);
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
        assert!(view.memtables()[0].is_empty());
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

    /// pending (フラッシュ待ち) 状態での読み書きの意味論 (todo 504)。
    /// rotate を直接呼んで pending が残った状態を決定的に作る。
    #[test]
    fn pending_flush_read_write_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        let cid = info.collection_id;
        store.upsert(cid, 1, rec(vec![1.0])).unwrap();
        store.upsert(cid, 2, rec(vec![2.0])).unwrap();

        // 手動 rotate (メンテナンスには通知しない = pending が残り続ける)
        {
            let mut state = store.shared.state.lock().unwrap();
            assert!(store.shared.rotate(&mut state).unwrap());
        }

        // pending 中も読み書きできる
        let view = store.view(cid).unwrap();
        assert_eq!(view.memtables().len(), 2, "active + pending");
        assert_eq!(view.live_len(), 2);
        assert_eq!(view.get(1).unwrap().vector, vec![1.0]);

        store.upsert(cid, 1, rec(vec![10.0])).unwrap(); // pending の値を上書き
        assert!(store.delete(cid, 2).unwrap()); // pending の値を削除
        store.upsert(cid, 3, rec(vec![3.0])).unwrap(); // 新規

        let view = store.view(cid).unwrap();
        assert_eq!(
            view.get(1).unwrap().vector,
            vec![10.0],
            "active が pending に勝つ"
        );
        assert!(view.get(2).is_none(), "active の削除が pending に勝つ");
        assert_eq!(view.live_len(), 2);

        // 明示 flush で pending + active の両方が消化される
        store.flush().unwrap();
        let view = store.view(cid).unwrap();
        assert!(view.memtables()[0].is_empty());
        assert_eq!(view.live_len(), 2);
        assert_eq!(view.get(1).unwrap().vector, vec![10.0]);
        assert!(view.get(2).is_none());

        // 再 open でも同じ
        drop(view);
        drop(store);
        let store = open(dir.path());
        let view = store.view(cid).unwrap();
        assert_eq!(view.live_len(), 2);
        assert_eq!(view.get(1).unwrap().vector, vec![10.0]);
        assert!(view.get(2).is_none());
        assert_eq!(view.get(3).unwrap().vector, vec![3.0]);
    }

    /// pending 中のクラッシュ相当: WAL が複数残った状態からの復旧では
    /// seq 順のリプレイで新しい書き込みが勝つこと (todo 504)。
    #[test]
    fn crash_with_multiple_wals_replays_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let cid = {
            let store = open(dir.path());
            let info = store.create_collection("docs", 1, Metric::L2).unwrap();
            let cid = info.collection_id;
            store.upsert(cid, 1, rec(vec![1.0])).unwrap();
            // rotate で WAL 世代交代 (メンテ未消化のまま)
            {
                let mut state = store.shared.state.lock().unwrap();
                store.shared.rotate(&mut state).unwrap();
            }
            store.upsert(cid, 1, rec(vec![2.0])).unwrap(); // 新 WAL に上書き
            store.upsert(cid, 4, rec(vec![4.0])).unwrap();
            // WAL が 2 本ある状態を確認
            let wals = crate::wal::list_wal_files(&dir.path().join("wal")).unwrap();
            assert_eq!(wals.len(), 2);
            simulate_crash(store);
            cid
        };

        // WAL 2 本の状態をファイル操作で再構築して復旧経路を直接検証
        let dir2 = tempfile::tempdir().unwrap();
        let cid2 = {
            let store = open(dir2.path());
            let info = store.create_collection("docs", 1, Metric::L2).unwrap();
            store.upsert(info.collection_id, 1, rec(vec![1.0])).unwrap();
            simulate_crash(store);
            info.collection_id
        };
        // seq=2 の WAL を手で作り、上書きを追記する
        {
            let mut w = crate::wal::WalWriter::create(
                &dir2.path().join("wal").join(crate::wal::wal_file_name(2)),
                SyncPolicy::Always,
            )
            .unwrap();
            w.append(&WalRecord::Upsert {
                collection_id: cid2,
                id: 1,
                vector: vec![2.0],
                metadata: Default::default(),
            })
            .unwrap();
            w.append(&WalRecord::Upsert {
                collection_id: cid2,
                id: 4,
                vector: vec![4.0],
                metadata: Default::default(),
            })
            .unwrap();
            w.sync().unwrap();
        }
        let store = open(dir2.path());
        let info = store.collection_info("docs").unwrap();
        let view = store.view(info.collection_id).unwrap();
        assert_eq!(
            view.get(1).unwrap().vector,
            vec![2.0],
            "seq 順で新 WAL が勝つ"
        );
        assert_eq!(view.get(4).unwrap().vector, vec![4.0]);
        assert_eq!(view.live_len(), 2);
        let _ = cid;
    }

    /// SyncPolicy::Batch: Ok が返った書き込みはクラッシュ後も残る (todo 505)。
    #[test]
    fn batch_sync_acked_writes_survive() {
        let dir = tempfile::tempdir().unwrap();
        let cid = {
            let store = Store::open(
                dir.path(),
                StoreOptions {
                    sync: SyncPolicy::Batch,
                    ..Default::default()
                },
            )
            .unwrap();
            let info = store.create_collection("docs", 1, Metric::L2).unwrap();
            // 並行 8 スレッドで書く (group commit の共有経路を通す)
            let cid = info.collection_id;
            std::thread::scope(|s| {
                for t in 0..8u64 {
                    let store = &store;
                    s.spawn(move || {
                        for i in 0..25u64 {
                            store.upsert(cid, t * 100 + i, rec(vec![i as f32])).unwrap();
                        }
                    });
                }
            });
            simulate_crash(store); // クラッシュ相当 (Shutdown フラッシュを走らせない)
            cid
        };
        // ack 済みの 200 件すべてが WAL から復元される
        let store = open(dir.path());
        let view = store.view(cid).unwrap();
        assert_eq!(view.live_len(), 200);
        for t in 0..8u64 {
            for i in 0..25u64 {
                assert_eq!(view.get(t * 100 + i).unwrap().vector, vec![i as f32]);
            }
        }
    }

    /// 部分コンパクション (todo 506): 小さい run だけがマージされ、
    /// 古い大セグメントとの newest-wins・tombstone 引き継ぎが保たれる。
    #[test]
    fn partial_compaction_preserves_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path(),
            StoreOptions {
                compaction_threshold: 4,
                ..Default::default()
            },
        )
        .unwrap();
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        let cid = info.collection_id;

        // 大セグメント: id 0..1000
        for i in 0..1000u64 {
            store.upsert(cid, i, rec(vec![i as f32])).unwrap();
        }
        store.flush().unwrap();
        assert_eq!(store.view(cid).unwrap().segments.len(), 1);

        // 小セグメント×4 (4 個目のフラッシュで自動部分コンパクション発動)
        // 小セグメント群には「大セグメント行の上書き」と「削除」を混ぜる
        store.upsert(cid, 5, rec(vec![9995.0])).unwrap(); // 上書き
        for i in 2000..2009u64 {
            store.upsert(cid, i, rec(vec![i as f32])).unwrap();
        }
        store.flush().unwrap();
        store.delete(cid, 7).unwrap(); // 大セグメント行の削除 (tombstone)
        for i in 2010..2019u64 {
            store.upsert(cid, i, rec(vec![i as f32])).unwrap();
        }
        store.flush().unwrap();
        for i in 2020..2030u64 {
            store.upsert(cid, i, rec(vec![i as f32])).unwrap();
        }
        store.flush().unwrap();
        for i in 2030..2040u64 {
            store.upsert(cid, i, rec(vec![i as f32])).unwrap();
        }
        store.flush().unwrap();

        // run (小 4 個) だけがマージされ、大セグメントは残る
        let view = store.view(cid).unwrap();
        assert_eq!(view.segments.len(), 2, "small run merged, big segment kept");
        // 意味論: 上書き・削除・元データすべて正しい
        assert_eq!(view.get(5).unwrap().vector, vec![9995.0], "上書きが勝つ");
        assert!(view.get(7).is_none(), "引き継がれた tombstone が効く");
        assert_eq!(
            view.get(3).unwrap().vector,
            vec![3.0],
            "大セグメントの行は健在"
        );
        assert_eq!(view.get(2035).unwrap().vector, vec![2035.0]);
        assert_eq!(view.live_len(), 1000 - 1 + 38); // 1000 − 削除 1 + 追加 38

        // 再 open (v2 manifest の年代順リスト) でも同じ
        drop(view);
        drop(store);
        let store = open(dir.path());
        let view = store.view(cid).unwrap();
        assert_eq!(view.segments.len(), 2);
        assert_eq!(view.get(5).unwrap().vector, vec![9995.0]);
        assert!(view.get(7).is_none());
        assert_eq!(view.live_len(), 1037);

        // 明示 compact は full merge: 1 個になり tombstone も破棄
        store.compact().unwrap();
        let view = store.view(cid).unwrap();
        assert_eq!(view.segments.len(), 1);
        assert_eq!(view.segments[0].tombstone_count(), 0);
        assert!(view.get(7).is_none());
        assert_eq!(view.live_len(), 1037);
    }

    /// StoreOptions の値検証 (todo 507)。
    #[test]
    fn invalid_options_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cases: Vec<StoreOptions> = vec![
            StoreOptions {
                flush_threshold_bytes: 0,
                ..Default::default()
            },
            StoreOptions {
                compaction_threshold: 1,
                ..Default::default()
            },
            StoreOptions {
                sync: SyncPolicy::EveryN(0),
                ..Default::default()
            },
            StoreOptions {
                hnsw: HnswParams {
                    m: 0,
                    ..Default::default()
                },
                ..Default::default()
            },
            StoreOptions {
                hnsw: HnswParams {
                    ef_search: 0,
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        for options in cases {
            assert!(
                matches!(
                    Store::open(dir.path(), options),
                    Err(HamaneError::InvalidConfig(_))
                ),
                "must reject: {options:?}"
            );
        }
    }

    /// プロセス排他ロック (todo 702): 二重 open は Locked、解放後は再 open 可能。
    #[cfg(unix)]
    #[test]
    fn double_open_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let second = Store::open(dir.path(), StoreOptions::default());
        assert!(
            matches!(second, Err(HamaneError::Locked(_))),
            "second open must fail with Locked"
        );
        drop(store);
        // 解放後は開ける
        let reopened = Store::open(dir.path(), StoreOptions::default());
        assert!(reopened.is_ok());
    }

    /// バックアップ (todo 703): バックアップ時点のスナップショットが取れ、
    /// その後の書き込みは含まれない。バックアップは通常の open で復元できる。
    #[test]
    fn backup_is_consistent_snapshot() {
        let src = tempfile::tempdir().unwrap();
        let dest_root = tempfile::tempdir().unwrap();
        let dest = dest_root.path().join("backup");

        let store = open(src.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        let cid = info.collection_id;
        for i in 0..10u64 {
            store.upsert(cid, i, rec(vec![i as f32])).unwrap();
        }
        store.flush().unwrap();
        store.upsert(cid, 100, rec(vec![100.0])).unwrap(); // 未フラッシュ分

        store.backup(&dest).unwrap();

        // バックアップ後の書き込みはバックアップに含まれない
        store.upsert(cid, 200, rec(vec![200.0])).unwrap();
        drop(store);

        let restored = open(&dest);
        let view = restored.view(cid).unwrap();
        assert_eq!(
            view.live_len(),
            11,
            "flushed 10 + unflushed 1 at backup time"
        );
        assert_eq!(view.get(100).unwrap().vector, vec![100.0]);
        assert!(view.get(200).is_none(), "post-backup write must be absent");
        // CRC 込みで全セグメントが健全
        for seg in &view.segments {
            seg.verify_checksums().unwrap();
        }

        // 空でない dest はエラー
        let store = open(src.path());
        assert!(store.backup(&dest).is_err());
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

    // -----------------------------------------------------------------------
    // follower モード (todo 903)
    // -----------------------------------------------------------------------

    /// src 配下を dst へ再帰コピーする。既存ファイルは CURRENT 以外
    /// 上書きしない (mmap 中のセグメントを truncate しないため。
    /// 実際の puller も「ないファイルだけ fetch + CURRENT 切り替え」で動く)
    fn sync_dir(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dst.join(entry.file_name());
            if entry.path().is_dir() {
                sync_dir(&entry.path(), &to);
            } else if !to.exists() || entry.file_name() == CURRENT_FILE {
                std::fs::copy(entry.path(), &to).unwrap();
            }
        }
    }

    #[test]
    fn follower_rejects_writes_but_serves_reads() {
        let dir = tempfile::tempdir().unwrap();
        let cid = {
            let store = open(dir.path());
            let info = store.create_collection("docs", 2, Metric::L2).unwrap();
            store
                .upsert(info.collection_id, 1, rec(vec![1.0, 2.0]))
                .unwrap();
            info.collection_id
        }; // 未フラッシュ分は WAL 経由で follower 側に再生される

        let f = Store::open_follower(dir.path(), StoreOptions::default()).unwrap();
        // 読み取りは通常どおり
        assert_eq!(f.collection_names(), vec!["docs".to_string()]);
        let view = f.view(cid).unwrap();
        assert_eq!(view.live_len(), 1);
        assert_eq!(view.get(1).unwrap().vector, vec![1.0, 2.0]);
        // 書き込みはすべて ReadOnlyReplica
        let deny = |r: Result<()>| {
            assert!(matches!(r, Err(HamaneError::ReadOnlyReplica)), "{r:?}");
        };
        deny(f.create_collection("x", 2, Metric::L2).map(|_| ()));
        deny(f.drop_collection("docs"));
        deny(f.upsert(cid, 9, rec(vec![0.0, 0.0])));
        deny(f.delete(cid, 1).map(|_| ()));
        deny(f.flush());
        deny(f.compact());
        deny(f.backup(&dir.path().join("b")));
    }

    #[test]
    fn normal_store_rejects_follower_apis() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        assert!(matches!(
            store.apply_wal_frames(&[]),
            Err(HamaneError::InvalidConfig(_))
        ));
        assert!(matches!(
            store.switch_generation(),
            Err(HamaneError::InvalidConfig(_))
        ));
    }

    #[test]
    fn follower_replays_wal_tail_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let cid = {
            let store = open(dir.path());
            let info = store.create_collection("docs", 2, Metric::L2).unwrap();
            store
                .upsert(info.collection_id, 1, rec(vec![1.0, 2.0]))
                .unwrap();
            store.flush().unwrap();
            // WAL tail に残す分 (simulate_crash でフラッシュさせない)
            store
                .upsert(info.collection_id, 2, rec(vec![3.0, 4.0]))
                .unwrap();
            store.delete(info.collection_id, 1).unwrap();
            let cid = info.collection_id;
            simulate_crash(store);
            cid
        };
        let f = Store::open_follower(dir.path(), StoreOptions::default()).unwrap();
        let view = f.view(cid).unwrap();
        assert_eq!(view.live_len(), 1);
        assert!(view.get(1).is_none(), "WAL tail の delete が効いている");
        assert_eq!(view.get(2).unwrap().vector, vec![3.0, 4.0]);
    }

    #[test]
    fn apply_wal_frames_stops_at_frame_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let cid = {
            let store = open(dir.path());
            store
                .create_collection("docs", 2, Metric::L2)
                .unwrap()
                .collection_id
        };
        let f = Store::open_follower(dir.path(), StoreOptions::default()).unwrap();

        // primary が書く WAL と同じバイト列を WalWriter で作る
        let scratch = tempfile::tempdir().unwrap();
        let wal_path = scratch.path().join("frames.wal");
        {
            let mut w = WalWriter::create(&wal_path, SyncPolicy::Always).unwrap();
            for (id, v) in [(10u64, [1.0f32, 0.0]), (11, [0.0, 1.0]), (12, [1.0, 1.0])] {
                w.append(&WalRecord::Upsert {
                    collection_id: cid,
                    id,
                    vector: v.to_vec(),
                    metadata: Metadata::new(),
                })
                .unwrap();
                w.sync().unwrap();
            }
            w.append(&WalRecord::Delete {
                collection_id: cid,
                id: 11,
            })
            .unwrap();
            w.sync().unwrap();
        }
        let bytes = std::fs::read(&wal_path).unwrap();
        let frames = &bytes[crate::format::MAGIC_WAL.len()..];

        // フレーム途中で切ったチャンク → 完全な分だけ消費される
        let cut = frames.len() - 3;
        let consumed = f.apply_wal_frames(&frames[..cut]).unwrap();
        assert!(consumed < cut, "末尾の不完全フレームは持ち越し");
        // 続きを連結して再適用 (puller と同じ扱い)
        let consumed2 = f.apply_wal_frames(&frames[consumed..]).unwrap();
        assert_eq!(consumed + consumed2, frames.len());

        let view = f.view(cid).unwrap();
        assert_eq!(view.live_len(), 2);
        assert_eq!(view.get(10).unwrap().vector, vec![1.0, 0.0]);
        assert!(view.get(11).is_none(), "delete フレームも適用される");
        assert_eq!(view.get(12).unwrap().vector, vec![1.0, 1.0]);
    }

    #[test]
    fn switch_generation_adopts_new_manifest() {
        let primary_dir = tempfile::tempdir().unwrap();
        let replica_dir = tempfile::tempdir().unwrap();

        let cid = {
            let store = open(primary_dir.path());
            let info = store.create_collection("docs", 2, Metric::L2).unwrap();
            store
                .upsert(info.collection_id, 1, rec(vec![1.0, 2.0]))
                .unwrap();
            store.flush().unwrap(); // gen 前進 (Drop はアクティブ memtable を flush しない)
            info.collection_id
        };
        sync_dir(primary_dir.path(), replica_dir.path());
        // LOCK はコピー先で意味を持たないが flock は fd 単位なので問題ない
        let f = Store::open_follower(replica_dir.path(), StoreOptions::default()).unwrap();
        assert!(!f.switch_generation().unwrap(), "世代が同じなら no-op");
        let old_view = f.view(cid).unwrap();
        assert_eq!(old_view.live_len(), 1);

        // primary が次の世代を作る
        {
            let store = open(primary_dir.path());
            store.upsert(cid, 2, rec(vec![3.0, 4.0])).unwrap();
            store.flush().unwrap();
        }
        sync_dir(primary_dir.path(), replica_dir.path());

        assert!(f.switch_generation().unwrap());
        let view = f.view(cid).unwrap();
        assert_eq!(view.live_len(), 2);
        assert_eq!(view.get(2).unwrap().vector, vec![3.0, 4.0]);
        // 旧 LiveView は切り替え後も安全に読める (Arc 保持)
        assert_eq!(old_view.live_len(), 1);
        assert_eq!(old_view.get(1).unwrap().vector, vec![1.0, 2.0]);
        assert!(!f.switch_generation().unwrap());
    }
}
