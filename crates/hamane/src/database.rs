use std::path::Path;
use std::sync::Arc;

use hamane_core::{HamaneError, Result};
use hamane_storage::{Store, StoreOptions};

use crate::collection::{Collection, CollectionConfig};
use crate::pool::SearchPool;

/// データベース本体。Collection の入れ物。
///
/// `open` で永続化 (WAL + セグメント)、`in_memory` で揮発モード。
/// どちらも同じ API で使える (docs/design/query.md §3)。
pub struct Database {
    store: Arc<Store>,
    /// セグメント並列検索用の共有プール (todo 801)。全 Collection で共有
    search_pool: Arc<SearchPool>,
}

impl Database {
    /// インメモリデータベースを作成する (永続化なし)。
    pub fn in_memory() -> Self {
        Self {
            store: Arc::new(Store::in_memory()),
            search_pool: Arc::new(SearchPool::new(0)),
        }
    }

    /// ディレクトリを開く (存在しなければ初期化)。
    /// クラッシュ後は manifest + WAL リプレイで直前の状態を復元する。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, StoreOptions::default())
    }

    /// フラッシュ閾値や fsync ポリシーを指定して開く。
    pub fn open_with_options(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let search_pool = Arc::new(SearchPool::new(options.search_threads));
        Ok(Self {
            store: Arc::new(Store::open(path.as_ref(), options)?),
            search_pool,
        })
    }

    /// Collection を新規作成する。同名が存在する場合はエラー。
    pub fn create_collection(
        &self,
        name: &str,
        config: CollectionConfig,
    ) -> Result<Arc<Collection>> {
        if config.dim == 0 {
            return Err(HamaneError::InvalidConfig("dim must be > 0".into()));
        }
        let info = self
            .store
            .create_collection(name, config.dim as u32, config.metric)?;
        Ok(Arc::new(Collection::new(
            name.to_owned(),
            config,
            info.collection_id,
            Arc::clone(&self.store),
            Arc::clone(&self.search_pool),
        )))
    }

    /// 既存の Collection を取得する。
    pub fn collection(&self, name: &str) -> Result<Arc<Collection>> {
        let info = self.store.collection_info(name)?;
        Ok(Arc::new(Collection::new(
            name.to_owned(),
            CollectionConfig {
                dim: info.dim as usize,
                metric: info.metric,
            },
            info.collection_id,
            Arc::clone(&self.store),
            Arc::clone(&self.search_pool),
        )))
    }

    /// データベースディレクトリ (in-memory なら None)。
    pub fn path(&self) -> Option<&Path> {
        self.store.db_dir()
    }

    /// レプリカ (読み取り専用 follower) として開く (todo 904)。
    ///
    /// 書き込み API は `HamaneError::ReadOnlyReplica` を返す。状態の更新は
    /// 同期ループが `apply_wal_frames` / `switch_generation` で行う
    /// (docs/design/replication.md)。検索・点参照は通常どおり使える。
    pub fn open_replica(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let search_pool = Arc::new(SearchPool::new(options.search_threads));
        Ok(Self {
            store: Arc::new(Store::open_follower(path.as_ref(), options)?),
            search_pool,
        })
    }

    /// レプリカモードか。
    pub fn is_replica(&self) -> bool {
        self.store.is_follower()
    }

    /// 現在の manifest 世代 (監視用。in-memory は常に 0)。
    pub fn manifest_gen(&self) -> u64 {
        self.store.manifest_gen()
    }

    /// レプリカ専用: fetch した WAL フレーム列を適用し、消費バイト数を返す
    /// (同期ループ内部用。docs/design/replication.md §3)。
    pub fn apply_wal_frames(&self, bytes: &[u8]) -> Result<usize> {
        self.store.apply_wal_frames(bytes)
    }

    /// レプリカ専用: ディスク上の新しい世代へ状態を切り替える
    /// (同期ループ内部用)。切り替えたら true。
    pub fn switch_generation(&self) -> Result<bool> {
        self.store.switch_generation()
    }

    /// Collection を削除する。
    pub fn drop_collection(&self, name: &str) -> Result<()> {
        self.store.drop_collection(name)
    }

    /// Collection 名の一覧 (ソート済み)。
    pub fn collection_names(&self) -> Vec<String> {
        self.store.collection_names()
    }

    /// 全 collection の memtable をセグメントへ書き出す (in-memory では no-op)。
    pub fn flush(&self) -> Result<()> {
        self.store.flush()
    }

    /// 複数セグメントを統合し、上書き・削除を物理適用してディスクを回収する。
    /// フラッシュ後にセグメント数が閾値を超えた場合は自動でも実行される。
    pub fn compact(&self) -> Result<()> {
        self.store.compact()
    }

    /// 一貫性のあるバックアップを dest ディレクトリに取る。
    /// 復元は dest を `Database::open` するだけ。dest は空であること。
    /// コピー中の書き込みは待たされる (詳細は仕様書の「永続化」参照)。
    pub fn backup(&self, dest: impl AsRef<Path>) -> Result<()> {
        self.store.backup(dest.as_ref())
    }
}
