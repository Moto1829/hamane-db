use std::cell::RefCell;
use std::sync::Arc;

use hamane_core::{normalize, Filter, HamaneError, Id, Metadata, Metric, Record, RecordId, Result};
use hamane_index::{search_flat, search_hnsw};
use hamane_storage::{LiveView, Segment, Store, StoredRecord};

/// フィルタ選択率の推定サンプル数と pre/post filter の切り替え閾値
/// (docs/design/index.md §5)。
const FILTER_SAMPLE_SIZE: usize = 1000;
const PRE_FILTER_SELECTIVITY: f64 = 0.05;
const MAX_OVERSAMPLE: f32 = 4.0;
/// SQ8 の 2 段階検索で量子化距離により取得する候補の倍率 (todo 602)
const SQ8_RERANK_FACTOR: usize = 4;

/// Collection 作成時の設定。次元数と距離関数は作成後変更できない。
#[derive(Debug, Clone, Copy)]
pub struct CollectionConfig {
    /// ベクトルの次元数 (必須、> 0)
    pub dim: usize,
    /// 距離関数 (既定: Cosine)
    pub metric: Metric,
}

impl Default for CollectionConfig {
    fn default() -> Self {
        Self {
            dim: 0, // 必須項目。0 のままだと create_collection がエラーを返す
            metric: Metric::Cosine,
        }
    }
}

/// 検索結果 1 件。score の意味はメトリック依存
/// (L2 = 距離 = 小さいほど近い、Cosine/Dot = 類似度 = 大きいほど近い)。
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// 内部レコード ID (u64)
    pub id: Id,
    /// 距離スコア (L2 = 距離、Cosine/Dot = 類似度)
    pub score: f32,
    /// レコードのメタデータ
    pub metadata: Metadata,
}

impl SearchHit {
    /// このレコードが文字列 ID で挿入されていた場合、その文字列 ID。
    pub fn ext_id(&self) -> Option<&str> {
        match self.metadata.get(hamane_core::EXT_ID_META_KEY) {
            Some(hamane_core::MetaValue::Str(s)) => Some(s),
            _ => None,
        }
    }
}

/// 次元数・距離関数を固定したベクトルの集合。
///
/// 単一ライタ・複数リーダ。実体は `Store` が保持し、このハンドルは
/// collection_id を介して操作する。
pub struct Collection {
    name: String,
    config: CollectionConfig,
    collection_id: u32,
    store: Arc<Store>,
}

impl std::fmt::Debug for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collection")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("collection_id", &self.collection_id)
            .finish()
    }
}

impl Collection {
    pub(crate) fn new(
        name: String,
        config: CollectionConfig,
        collection_id: u32,
        store: Arc<Store>,
    ) -> Self {
        Self {
            name,
            config,
            collection_id,
            store,
        }
    }

    /// Collection 名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 作成時の設定 (dim / metric)。
    pub fn config(&self) -> CollectionConfig {
        self.config
    }

    /// live なレコード数 (O(1)。Store が書き込みごとに差分維持)。
    pub fn len(&self) -> usize {
        self.store
            .view(self.collection_id)
            .map(|v| v.live_len())
            .unwrap_or(0)
    }

    /// live なレコードが 1 件もないか。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// レコードを挿入する。同一 ID が存在する場合は置き換える。
    ///
    /// 文字列 ID (`Record::new("uuid", ...)`) は collection 内部の辞書で
    /// u64 に対応づけられ、`_ext_id` メタデータとして永続化される。
    pub fn upsert(&self, record: Record) -> Result<()> {
        self.upsert_batch(vec![record])
    }

    /// 複数レコードを 1 回の WAL sync でまとめて挿入する。
    pub fn upsert_batch(&self, records: Vec<Record>) -> Result<()> {
        let mut prepared = Vec::with_capacity(records.len());
        for record in records {
            let vector = self.prepare_vector(record.vector)?;
            prepared.push((
                record.id,
                StoredRecord {
                    vector,
                    metadata: record.metadata,
                },
            ));
        }
        self.store
            .upsert_batch_records(self.collection_id, prepared)
    }

    /// レコードを削除する。存在した場合 true を返す (判定と削除は原子的)。
    /// u64 と文字列 ID のどちらも受け付ける。
    pub fn delete(&self, id: impl Into<RecordId>) -> Result<bool> {
        self.store.delete_record(self.collection_id, &id.into())
    }

    /// ID でレコードを取得する。Cosine の場合ベクトルは正規化済みの値を返す。
    /// u64 と文字列 ID のどちらも受け付ける。
    pub fn get(&self, id: impl Into<RecordId>) -> Option<Record> {
        let rid = id.into();
        let internal = match &rid {
            RecordId::Num(n) => *n,
            RecordId::Str(s) => self.store.resolve_ext_id(self.collection_id, s).ok()??,
        };
        self.store
            .view(self.collection_id)
            .ok()?
            .get(internal)
            .map(|r| Record {
                id: rid,
                vector: r.vector,
                metadata: r.metadata,
            })
    }

    /// memtable をセグメントへ書き出す (DB 全体のフラッシュ)。
    pub fn flush(&self) -> Result<()> {
        self.store.flush()
    }

    /// セグメント構成の要約 (新しい順)。デバッグ・監視用。
    pub fn segment_stats(&self) -> Result<Vec<hamane_storage::SegmentStats>> {
        Ok(self.store.view(self.collection_id)?.segment_stats())
    }

    /// 検索クエリのビルダーを返す。
    pub fn search<'a>(&'a self, query: &'a [f32]) -> SearchBuilder<'a> {
        SearchBuilder {
            collection: self,
            query,
            k: 10,
            filter: None,
            ef: None,
        }
    }

    /// 次元・数値の検証と、メトリックに応じた正規化。
    fn prepare_vector(&self, mut vector: Vec<f32>) -> Result<Vec<f32>> {
        if vector.len() != self.config.dim {
            return Err(HamaneError::DimensionMismatch {
                expected: self.config.dim,
                actual: vector.len(),
            });
        }
        if vector.iter().any(|x| !x.is_finite()) {
            return Err(HamaneError::InvalidVector(
                "vector contains NaN or infinite values".into(),
            ));
        }
        if self.config.metric.requires_normalization() && !normalize(&mut vector) {
            return Err(HamaneError::InvalidVector(
                "zero vector cannot be used with cosine metric".into(),
            ));
        }
        Ok(vector)
    }

    /// 複数ソース (memtable + セグメント) のマージ検索 (docs/design/query.md §2)。
    ///
    /// - memtable と HNSW なしセグメントは Flat (走査時 live 判定で正確な top-k)
    /// - HNSW ありセグメントは近似探索。フィルタ付きは選択率で pre/post を自動選択
    fn run_search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&Filter>,
        ef: Option<usize>,
    ) -> Result<Vec<SearchHit>> {
        let metric = self.config.metric;
        let query = self.prepare_vector(query.to_vec())?;
        let view: LiveView = self.store.view(self.collection_id)?;
        let ef = ef.unwrap_or(self.store.options().hnsw.ef_search);

        // (比較キー, id) を全ソースから収集。
        // rank: 0..memtables().len() が memtable 列 (active + フラッシュ待ち)、
        // それ以降が新しい順のセグメント
        let mut candidates: Vec<(f32, Id)> = Vec::new();

        for (rank, mt) in view.memtables().iter().enumerate() {
            for h in search_flat(mt.iter(), &query, k, metric, filter) {
                // rank 0 (active) は常に最新。フラッシュ待ちは active に shadow され得る
                if rank == 0 || view.is_live(h.id, rank) {
                    candidates.push((metric.key_from_score(h.score), h.id));
                }
            }
        }

        // セグメント間は並列に検索する (todo 503)。1 個以下ならスレッドを立てない
        let rank_base = view.memtables().len();
        if view.segments.len() <= 1 {
            for (i, seg) in view.segments.iter().enumerate() {
                candidates.extend(self.search_segment(
                    &view,
                    rank_base + i,
                    seg,
                    &query,
                    k,
                    ef,
                    filter,
                )?);
            }
        } else {
            let per_segment: Vec<Result<Vec<(f32, Id)>>> = std::thread::scope(|scope| {
                let (view, query) = (&view, query.as_slice());
                let handles: Vec<_> = view
                    .segments
                    .iter()
                    .enumerate()
                    .map(|(i, seg)| {
                        scope.spawn(move || {
                            self.search_segment(view, rank_base + i, seg, query, k, ef, filter)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("segment search thread panicked"))
                    .collect()
            });
            for result in per_segment {
                candidates.extend(result?);
            }
        }

        // live 判定済みなので id 重複はない。キー昇順 (近い順) に k 件
        candidates.sort_by(|a, b| a.partial_cmp(b).expect("keys are finite"));
        candidates.truncate(k);

        Ok(candidates
            .into_iter()
            .map(|(key, id)| SearchHit {
                id,
                score: metric.score_from_key(key),
                metadata: view.get(id).map(|r| r.metadata).unwrap_or_default(),
            })
            .collect())
    }

    /// セグメント 1 個の検索プラン (docs/design/index.md §4–5)。
    #[allow(clippy::too_many_arguments)]
    fn search_segment(
        &self,
        view: &LiveView,
        rank: usize,
        seg: &Segment,
        query: &[f32],
        k: usize,
        ef: usize,
        filter: Option<&Filter>,
    ) -> Result<Vec<(f32, Id)>> {
        let metric = self.config.metric;
        let n = seg.len() as u32;
        let live = |row: u32| view.is_live(seg.id(row), rank);

        let flat_over =
            |rows: &mut dyn Iterator<Item = u32>, metas: Option<&[Metadata]>| -> Vec<(f32, Id)> {
                let empty = Metadata::new();
                let iter = rows.map(|r| {
                    let meta = metas.map(|m| &m[r as usize]).unwrap_or(&empty);
                    (seg.id(r), seg.vector(r), meta)
                });
                search_flat(iter, query, k, metric, filter)
                    .into_iter()
                    .map(|h| (metric.key_from_score(h.score), h.id))
                    .collect()
            };

        let Some(hview) = seg.hnsw()? else {
            // HNSW なし: Flat (フィルタありならメタデータをデコード)
            let metas = if filter.is_some() {
                Some(seg.decode_all_metadata()?)
            } else {
                None
            };
            return Ok(flat_over(
                &mut (0..n).filter(|&r| live(r)),
                metas.as_deref(),
            ));
        };

        let Some(filter) = filter else {
            // フィルタなし: live マスクのみで HNSW。
            // SQ8 があれば量子化距離で k×RERANK 件探索し、f32 で再ランクする (todo 602)
            if let Some(sq8) = seg.sq8_view()? {
                let query_codes = sq8.quantize_query(query);
                let code_sum: u64 = query_codes.iter().map(|&x| x as u64).sum();
                let dist =
                    |row: u32| -> f32 { sq8.distance_key(metric, &query_codes, code_sum, row) };
                let fetch = k * SQ8_RERANK_FACTOR;
                let hits = hamane_index::search_hnsw_by(
                    &hview,
                    n,
                    &dist,
                    fetch,
                    ef.max(fetch),
                    Some(&live),
                );
                // f32 で再ランクして上位 k
                let mut reranked: Vec<(f32, Id)> = hits
                    .into_iter()
                    .map(|(r, _)| (metric.distance_key(query, seg.vector(r)), seg.id(r)))
                    .collect();
                reranked.sort_by(|a, b| a.partial_cmp(b).expect("keys are finite"));
                reranked.truncate(k);
                return Ok(reranked);
            }
            let hits = search_hnsw(&hview, seg, metric, query, k, ef, Some(&live));
            return Ok(hits.into_iter().map(|(r, key)| (key, seg.id(r))).collect());
        };

        // フィルタあり: 選択率をサンプリングして pre/post を選ぶ
        let stride = (n as usize / FILTER_SAMPLE_SIZE).max(1) as u32;
        let mut sampled = 0usize;
        let mut matched = 0usize;
        let mut row = 0u32;
        while row < n {
            sampled += 1;
            if filter.matches(&seg.metadata(row)?) {
                matched += 1;
            }
            row += stride;
        }
        let selectivity = matched as f64 / sampled.max(1) as f64;

        if selectivity < PRE_FILTER_SELECTIVITY {
            // pre-filter: 一致行だけを Flat で距離計算
            let metas = seg.decode_all_metadata()?;
            let rows: Vec<u32> = (0..n)
                .filter(|&r| live(r) && filter.matches(&metas[r as usize]))
                .collect();
            Ok(flat_over(&mut rows.into_iter(), Some(&metas)))
        } else {
            // post-filter: マスク付き HNSW。メタデータ判定は行単位でメモ化
            let memo: RefCell<Vec<u8>> = RefCell::new(vec![0u8; n as usize]); // 0=未評価 1=可 2=不可
            let mask = |r: u32| -> bool {
                if !live(r) {
                    return false;
                }
                let cached = memo.borrow()[r as usize];
                if cached != 0 {
                    return cached == 1;
                }
                let ok = seg.metadata(r).map(|m| filter.matches(&m)).unwrap_or(false);
                memo.borrow_mut()[r as usize] = if ok { 1 } else { 2 };
                ok
            };
            let oversample = (1.0 / selectivity as f32).clamp(1.0, MAX_OVERSAMPLE);
            let ef = ((ef as f32) * oversample) as usize;
            let hits = search_hnsw(&hview, seg, metric, query, k, ef, Some(&mask));
            Ok(hits.into_iter().map(|(r, key)| (key, seg.id(r))).collect())
        }
    }
}

/// 検索クエリのビルダー。
pub struct SearchBuilder<'a> {
    collection: &'a Collection,
    query: &'a [f32],
    k: usize,
    filter: Option<Filter>,
    ef: Option<usize>,
}

impl<'a> SearchBuilder<'a> {
    /// 取得する件数 (既定 10)。
    pub fn k(mut self, k: usize) -> Self {
        self.k = k;
        self
    }

    /// メタデータフィルタ。
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// HNSW の探索幅 ef_search を上書きする (既定は StoreOptions の値)。
    /// 大きいほど再現率が上がり遅くなる。Flat 検索には影響しない。
    pub fn ef(mut self, ef: usize) -> Self {
        self.ef = Some(ef);
        self
    }

    /// 検索を実行し、近い順に返す。
    pub fn run(self) -> Result<Vec<SearchHit>> {
        self.collection
            .run_search(self.query, self.k, self.filter.as_ref(), self.ef)
    }
}
