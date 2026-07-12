//! HNSW (Hierarchical Navigable Small World) インデックス
//! (docs/design/index.md, Malkov & Yashunin 2016)。
//!
//! - 距離は `Metric::distance_key` (小さいほど近い) のみを使い、メトリック非依存
//! - node ID はベクトルソースの行番号 (u32) と一致する
//! - seed 固定で構築は決定的
//! - 直列化は mmap で zero-copy ロードできる CSR レイアウト (`serialize` / `HnswView`)

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use hamane_core::{HamaneError, Metric, Result};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// hnsw.bin の magic (hamane-storage のセグメント規約と同じ形式)。
pub const MAGIC_HNSW: [u8; 8] = *b"HAMANEH\x01";

const HEADER_LEN: usize = 64;
/// レベル抽選の上限 (これを超える層は実用上現れない)
const MAX_LEVEL_CAP: u8 = 31;

/// HNSW のパラメータ。
#[derive(Debug, Clone, Copy)]
pub struct HnswParams {
    /// 層 1 以上の最大接続数
    pub m: usize,
    /// 層 0 の最大接続数 (慣例的に 2m)
    pub m0: usize,
    /// 構築時の候補リスト幅
    pub ef_construction: usize,
    /// 検索時の候補リスト幅の既定値 (クエリごとに上書き可)
    pub ef_search: usize,
    /// レベル抽選の乱数 seed (固定で構築が決定的になる)
    pub seed: u64,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 200,
            ef_search: 64,
            seed: 0,
        }
    }
}

/// 行番号でベクトルを引けるソース。memtable / セグメントの両方を抽象化する。
pub trait VectorSource {
    fn len(&self) -> u32;
    fn vector(&self, row: u32) -> &[f32];
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `Vec<Vec<f32>>` などスライス列のアダプタ (テスト・小規模構築用)。
pub struct SliceSource<'a>(pub &'a [Vec<f32>]);

impl VectorSource for SliceSource<'_> {
    fn len(&self) -> u32 {
        self.0.len() as u32
    }
    fn vector(&self, row: u32) -> &[f32] {
        &self.0[row as usize]
    }
}

/// グラフ構造への読み取りアクセス。ビルダーと mmap ビューが共に実装する。
pub trait HnswGraph {
    fn node_count(&self) -> u32;
    fn max_level(&self) -> u8;
    fn entry_point(&self) -> Option<u32>;
    /// node の最上位レベル
    fn level_of(&self, node: u32) -> u8;
    /// level における node の隣接リスト
    fn neighbors(&self, level: u8, node: u32) -> &[u32];
}

// ---------------------------------------------------------------------------
// 探索の共通部品
// ---------------------------------------------------------------------------

/// (距離キー, node)。キー昇順 + node 昇順で全順序 (キーは有限値のみ)。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Keyed {
    key: f32,
    node: u32,
}

impl Eq for Keyed {}
impl PartialOrd for Keyed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Keyed {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key
            .partial_cmp(&other.key)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.node.cmp(&other.node))
    }
}

struct Visited(Vec<u64>);

impl Visited {
    fn new(n: u32) -> Self {
        Self(vec![0; (n as usize).div_ceil(64)])
    }
    /// 未訪問なら訪問済みにして true
    fn insert(&mut self, node: u32) -> bool {
        let (w, b) = (node as usize / 64, node as usize % 64);
        let unseen = self.0[w] & (1 << b) == 0;
        self.0[w] |= 1 << b;
        unseen
    }
}

/// 1 層内の貪欲探索 (Algorithm 2)。entry_points から始めて ef 幅で探索し、
/// 近い順 (キー昇順) の候補を返す。
///
/// `mask` は**結果への採用のみ**を制限する。グラフの走査は全ノードを通す
/// (走査まで絞るとグラフが分断され再現率が崩れるため。docs/design/index.md §1)。
#[allow(clippy::too_many_arguments)]
fn search_layer<G: HnswGraph, S: VectorSource + ?Sized>(
    graph: &G,
    source: &S,
    metric: Metric,
    query: &[f32],
    entry_points: &[Keyed],
    level: u8,
    ef: usize,
    mask: Option<&dyn Fn(u32) -> bool>,
) -> Vec<Keyed> {
    let mut visited = Visited::new(source.len());
    // candidates: 近い順に取り出す min-heap / results: 遠いものから溢れる max-heap
    let mut candidates: BinaryHeap<std::cmp::Reverse<Keyed>> = BinaryHeap::new();
    let mut results: BinaryHeap<Keyed> = BinaryHeap::new();

    let accepts = |node: u32| mask.map(|f| f(node)).unwrap_or(true);

    for &ep in entry_points {
        if visited.insert(ep.node) {
            candidates.push(std::cmp::Reverse(ep));
            if accepts(ep.node) {
                results.push(ep);
            }
        }
    }

    while let Some(std::cmp::Reverse(current)) = candidates.pop() {
        // 最も近い候補が結果の最遠より遠ければ、これ以上改善しない
        if results.len() >= ef {
            if let Some(furthest) = results.peek() {
                if current.key > furthest.key {
                    break;
                }
            }
        }
        for &nb in graph.neighbors(level, current.node) {
            if !visited.insert(nb) {
                continue;
            }
            let key = metric.distance_key(query, source.vector(nb));
            let furthest_key = results.peek().map(|k| k.key).unwrap_or(f32::INFINITY);
            if results.len() < ef || key < furthest_key {
                candidates.push(std::cmp::Reverse(Keyed { key, node: nb }));
                if accepts(nb) {
                    results.push(Keyed { key, node: nb });
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
    }
    results.into_sorted_vec()
}

/// 最上層から target_level+1 層まで ef=1 で降下し、entry point を更新する。
fn greedy_descend<G: HnswGraph, S: VectorSource + ?Sized>(
    graph: &G,
    source: &S,
    metric: Metric,
    query: &[f32],
    mut ep: Keyed,
    from_level: u8,
    to_level_exclusive: u8,
) -> Keyed {
    let mut lc = from_level;
    while lc > to_level_exclusive {
        loop {
            let mut improved = false;
            for &nb in graph.neighbors(lc, ep.node) {
                let key = metric.distance_key(query, source.vector(nb));
                if key < ep.key {
                    ep = Keyed { key, node: nb };
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
        lc -= 1;
    }
    ep
}

/// クエリ時に上位層 (level ≥ 1) の降下で保持する候補幅。
/// 純粋な貪欲 (ef=1) はクラスタが強く分離したデータで誤ったクラスタに
/// 捕まったまま層 0 に降りてしまうため、少数の候補を並行して保持する。
const EF_UPPER_LAYERS: usize = 8;

/// k 近傍探索 (Algorithm 5)。結果は (row, 距離キー) を近い順で返す。
pub fn search_hnsw<G: HnswGraph, S: VectorSource + ?Sized>(
    graph: &G,
    source: &S,
    metric: Metric,
    query: &[f32],
    k: usize,
    ef: usize,
    mask: Option<&dyn Fn(u32) -> bool>,
) -> Vec<(u32, f32)> {
    let Some(entry) = graph.entry_point() else {
        return Vec::new();
    };
    if k == 0 {
        return Vec::new();
    }
    let mut eps = vec![Keyed {
        key: metric.distance_key(query, source.vector(entry)),
        node: entry,
    }];
    for lc in (1..=graph.max_level()).rev() {
        eps = search_layer(
            graph,
            source,
            metric,
            query,
            &eps,
            lc,
            EF_UPPER_LAYERS,
            None,
        );
    }
    let ef = ef.max(k);
    let mut found = search_layer(graph, source, metric, query, &eps, 0, ef, mask);
    found.truncate(k);
    found.into_iter().map(|kd| (kd.node, kd.key)).collect()
}

// ---------------------------------------------------------------------------
// 構築
// ---------------------------------------------------------------------------

/// インメモリの HNSW ビルダー (docs/design/index.md §1)。
pub struct HnswBuilder {
    params: HnswParams,
    metric: Metric,
    ml: f64,
    levels: Vec<u8>,
    /// neighbors[node][level] → 隣接 node 列
    neighbors: Vec<Vec<Vec<u32>>>,
    entry: Option<u32>,
    max_level: u8,
}

impl HnswGraph for HnswBuilder {
    fn node_count(&self) -> u32 {
        self.levels.len() as u32
    }
    fn max_level(&self) -> u8 {
        self.max_level
    }
    fn entry_point(&self) -> Option<u32> {
        self.entry
    }
    fn level_of(&self, node: u32) -> u8 {
        self.levels[node as usize]
    }
    fn neighbors(&self, level: u8, node: u32) -> &[u32] {
        self.neighbors[node as usize]
            .get(level as usize)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

impl HnswBuilder {
    /// source の全行 (row 0..len) からグラフを構築する。seed 固定で決定的。
    pub fn build<S: VectorSource + ?Sized>(source: &S, metric: Metric, params: HnswParams) -> Self {
        let mut rng = StdRng::seed_from_u64(params.seed);
        let mut builder = Self {
            params,
            metric,
            ml: 1.0 / (params.m as f64).ln(),
            levels: Vec::with_capacity(source.len() as usize),
            neighbors: Vec::with_capacity(source.len() as usize),
            entry: None,
            max_level: 0,
        };
        for row in 0..source.len() {
            let level = builder.sample_level(&mut rng);
            builder.insert(source, row, level);
        }
        builder
    }

    fn sample_level(&self, rng: &mut StdRng) -> u8 {
        let u: f64 = rng.random::<f64>(); // [0, 1)
        let level = (-(1.0 - u).ln() * self.ml) as u32;
        level.min(MAX_LEVEL_CAP as u32) as u8
    }

    fn m_at(&self, level: u8) -> usize {
        if level == 0 {
            self.params.m0
        } else {
            self.params.m
        }
    }

    fn insert<S: VectorSource + ?Sized>(&mut self, source: &S, node: u32, level: u8) {
        debug_assert_eq!(
            node as usize,
            self.levels.len(),
            "rows must be inserted in order"
        );
        self.levels.push(level);
        self.neighbors
            .push((0..=level).map(|_| Vec::new()).collect());

        let Some(entry) = self.entry else {
            self.entry = Some(node);
            self.max_level = level;
            return;
        };

        let query = source.vector(node);
        let mut ep = Keyed {
            key: self.metric.distance_key(query, source.vector(entry)),
            node: entry,
        };
        // 挿入レベルより上は貪欲降下のみ
        if self.max_level > level {
            ep = greedy_descend(self, source, self.metric, query, ep, self.max_level, level);
        }

        // level..0 の各層で候補を探し、ヒューリスティックで接続する
        let top = level.min(self.max_level);
        let mut eps = vec![ep];
        for lc in (0..=top).rev() {
            let candidates = search_layer(
                self,
                source,
                self.metric,
                query,
                &eps,
                lc,
                self.params.ef_construction,
                None,
            );
            let m = self.m_at(lc);
            let extended = self.extend_candidates(source, query, lc, &candidates);
            let selected = self.select_heuristic(source, &extended, m);
            for &nb in &selected {
                self.neighbors[node as usize][lc as usize].push(nb);
                self.neighbors[nb as usize][lc as usize].push(node);
                // 接続超過の刈り込み
                if self.neighbors[nb as usize][lc as usize].len() > m {
                    self.prune(source, nb, lc, m);
                }
            }
            eps = candidates;
        }

        if level > self.max_level {
            self.max_level = level;
            self.entry = Some(node);
        }
    }

    /// 候補集合を隣接ノードで拡張する (Algorithm 4 の extendCandidates)。
    /// クラスタが強く分離したデータで、クラスタ間ブリッジとなるエッジを
    /// 拾いやすくする (論文 §4 で extremely clustered data 向けと明記)。
    fn extend_candidates(
        &self,
        source: &(impl VectorSource + ?Sized),
        query: &[f32],
        level: u8,
        candidates: &[Keyed],
    ) -> Vec<Keyed> {
        let mut seen: std::collections::HashSet<u32> = candidates.iter().map(|c| c.node).collect();
        let mut extended = candidates.to_vec();
        for c in candidates {
            for &nb in self.neighbors(level, c.node) {
                if seen.insert(nb) {
                    extended.push(Keyed {
                        key: self.metric.distance_key(query, source.vector(nb)),
                        node: nb,
                    });
                }
            }
        }
        extended.sort();
        extended
    }

    /// ヒューリスティック隣接選択 (Algorithm 4)。candidates はキー昇順。
    /// 「既選択のどの要素よりもクエリに近い候補のみ採用」し、
    /// m に満たない分は近い順で補充する。
    fn select_heuristic<S: VectorSource + ?Sized>(
        &self,
        source: &S,
        candidates: &[Keyed],
        m: usize,
    ) -> Vec<u32> {
        let mut selected: Vec<Keyed> = Vec::with_capacity(m);
        for &c in candidates {
            if selected.len() >= m {
                break;
            }
            let cv = source.vector(c.node);
            let diverse = selected
                .iter()
                .all(|s| self.metric.distance_key(cv, source.vector(s.node)) > c.key);
            if diverse {
                selected.push(c);
            }
        }
        if selected.len() < m {
            for &c in candidates {
                if selected.len() >= m {
                    break;
                }
                if !selected.iter().any(|s| s.node == c.node) {
                    selected.push(c);
                }
            }
        }
        selected.into_iter().map(|kd| kd.node).collect()
    }

    /// node の level における隣接リストを m 本にヒューリスティックで刈り込む。
    fn prune<S: VectorSource + ?Sized>(&mut self, source: &S, node: u32, level: u8, m: usize) {
        let base = source.vector(node);
        let mut candidates: Vec<Keyed> = self.neighbors[node as usize][level as usize]
            .iter()
            .map(|&nb| Keyed {
                key: self.metric.distance_key(base, source.vector(nb)),
                node: nb,
            })
            .collect();
        candidates.sort();
        candidates.dedup_by_key(|kd| kd.node);
        let selected = self.select_heuristic(source, &candidates, m);
        self.neighbors[node as usize][level as usize] = selected;
    }

    // -----------------------------------------------------------------------
    // 直列化 (docs/design/index.md §3)
    // -----------------------------------------------------------------------

    /// CSR レイアウトに直列化する。返り値は magic を含む本体
    /// (チェックサム footer は呼び出し側が付ける)。
    pub fn serialize(&self) -> Vec<u8> {
        let n = self.node_count();
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_HNSW);
        buf.extend_from_slice(&n.to_le_bytes());
        buf.push(self.max_level);
        buf.extend_from_slice(&[0u8; 3]); // pad
        buf.extend_from_slice(&self.entry.unwrap_or(u32::MAX).to_le_bytes());
        buf.extend_from_slice(&(self.params.m as u32).to_le_bytes());
        buf.extend_from_slice(&(self.params.m0 as u32).to_le_bytes());
        buf.resize(HEADER_LEN, 0);

        // levels (u8 × n, 4 バイト境界に pad して以降の u32 配列を整列させる)
        buf.extend_from_slice(&self.levels);
        while buf.len() % 4 != 0 {
            buf.push(0);
        }

        // 層ごとの CSR
        for level in 0..=self.max_level {
            let node_ids: Vec<u32> = (0..n)
                .filter(|&v| self.levels[v as usize] >= level)
                .collect();
            buf.extend_from_slice(&(node_ids.len() as u32).to_le_bytes());
            for &v in &node_ids {
                buf.extend_from_slice(&v.to_le_bytes());
            }
            let mut offset = 0u32;
            buf.extend_from_slice(&offset.to_le_bytes());
            for &v in &node_ids {
                offset += self.neighbors(level, v).len() as u32;
                buf.extend_from_slice(&offset.to_le_bytes());
            }
            for &v in &node_ids {
                for &nb in self.neighbors(level, v) {
                    buf.extend_from_slice(&nb.to_le_bytes());
                }
            }
        }
        buf
    }
}

// ---------------------------------------------------------------------------
// mmap ビュー
// ---------------------------------------------------------------------------

struct LevelView<'a> {
    node_ids: &'a [u32],
    offsets: &'a [u32],
    neighbor_ids: &'a [u32],
}

/// 直列化済みグラフの zero-copy ビュー。`serialize` の出力 (footer なし) を読む。
pub struct HnswView<'a> {
    node_count: u32,
    max_level: u8,
    entry: Option<u32>,
    levels: &'a [u8],
    per_level: Vec<LevelView<'a>>,
}

fn corrupted(msg: &str) -> HamaneError {
    HamaneError::Corrupted(format!("hnsw: {msg}"))
}

/// buf から u32 スライスを切り出す (アラインメント・範囲を検証)。
fn u32_slice<'a>(buf: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u32]> {
    let bytes = buf
        .get(*pos..*pos + len * 4)
        .ok_or_else(|| corrupted("out of range"))?;
    if !(bytes.as_ptr() as usize).is_multiple_of(4) {
        return Err(corrupted("misaligned u32 array"));
    }
    *pos += len * 4;
    // Safety: 範囲・アラインメント検証済み。u32 は任意のビットパターンが有効
    Ok(unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u32, len) })
}

impl<'a> HnswView<'a> {
    pub fn open(buf: &'a [u8]) -> Result<Self> {
        if buf.len() < HEADER_LEN || buf[..8] != MAGIC_HNSW {
            return Err(corrupted("bad magic or short header"));
        }
        let node_count = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let max_level = buf[12];
        let entry_raw = u32::from_le_bytes(buf[16..20].try_into().unwrap());
        let entry = (entry_raw != u32::MAX).then_some(entry_raw);

        let mut pos = HEADER_LEN;
        let levels = buf
            .get(pos..pos + node_count as usize)
            .ok_or_else(|| corrupted("levels out of range"))?;
        pos += node_count as usize;
        pos = pos.next_multiple_of(4);

        let mut per_level = Vec::with_capacity(max_level as usize + 1);
        for _ in 0..=max_level {
            let count = *u32_slice(buf, &mut pos, 1)?.first().unwrap() as usize;
            if count > node_count as usize {
                return Err(corrupted("level node count exceeds total"));
            }
            let node_ids = u32_slice(buf, &mut pos, count)?;
            let offsets = u32_slice(buf, &mut pos, count + 1)?;
            let total = *offsets.last().unwrap() as usize;
            let neighbor_ids = u32_slice(buf, &mut pos, total)?;
            per_level.push(LevelView {
                node_ids,
                offsets,
                neighbor_ids,
            });
        }
        if pos != buf.len() {
            return Err(corrupted("trailing bytes"));
        }
        if let Some(e) = entry {
            if e >= node_count {
                return Err(corrupted("entry point out of range"));
            }
        }
        Ok(Self {
            node_count,
            max_level,
            entry,
            levels,
            per_level,
        })
    }
}

impl HnswGraph for HnswView<'_> {
    fn node_count(&self) -> u32 {
        self.node_count
    }
    fn max_level(&self) -> u8 {
        self.max_level
    }
    fn entry_point(&self) -> Option<u32> {
        self.entry
    }
    fn level_of(&self, node: u32) -> u8 {
        self.levels[node as usize]
    }
    fn neighbors(&self, level: u8, node: u32) -> &[u32] {
        let Some(lv) = self.per_level.get(level as usize) else {
            return &[];
        };
        let Ok(idx) = lv.node_ids.binary_search(&node) else {
            return &[];
        };
        let start = lv.offsets[idx] as usize;
        let end = lv.offsets[idx + 1] as usize;
        &lv.neighbor_ids[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vectors(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..n)
            .map(|_| (0..dim).map(|_| rng.random::<f32>() * 2.0 - 1.0).collect())
            .collect()
    }

    fn build(vecs: &[Vec<f32>], params: HnswParams) -> HnswBuilder {
        HnswBuilder::build(&SliceSource(vecs), Metric::L2, params)
    }

    #[test]
    fn structure_invariants() {
        let vecs = random_vectors(200, 8, 1);
        let g = build(&vecs, HnswParams::default());
        let n = g.node_count();
        assert_eq!(n, 200);

        // 接続数上限と、隣接が全て有効ノード
        for v in 0..n {
            for level in 0..=g.level_of(v) {
                let nbs = g.neighbors(level, v);
                let cap = if level == 0 { 32 } else { 16 };
                assert!(
                    nbs.len() <= cap,
                    "node {v} level {level}: {} > {cap}",
                    nbs.len()
                );
                for &nb in nbs {
                    assert!(nb < n);
                    assert!(g.level_of(nb) >= level, "neighbor must exist at the level");
                    assert_ne!(nb, v, "no self loops");
                }
            }
        }

        // 層 0 で entry point から全ノードに到達可能
        let entry = g.entry_point().unwrap();
        let mut seen = vec![false; n as usize];
        let mut queue = vec![entry];
        seen[entry as usize] = true;
        while let Some(v) = queue.pop() {
            for &nb in g.neighbors(0, v) {
                if !seen[nb as usize] {
                    seen[nb as usize] = true;
                    queue.push(nb);
                }
            }
        }
        assert!(
            seen.iter().all(|&s| s),
            "graph must be connected at level 0"
        );
    }

    #[test]
    fn deterministic_build() {
        let vecs = random_vectors(150, 8, 2);
        let a = build(&vecs, HnswParams::default());
        let b = build(&vecs, HnswParams::default());
        assert_eq!(a.serialize(), b.serialize());
        // seed を変えると一般に変わる (レベル抽選が変わる)
        let c = build(
            &vecs,
            HnswParams {
                seed: 42,
                ..Default::default()
            },
        );
        assert_ne!(a.serialize(), c.serialize());
    }

    #[test]
    fn exhaustive_ef_matches_flat() {
        // ef = n なら Flat と完全一致するはず (探索の健全性)
        let vecs = random_vectors(300, 8, 3);
        let src = SliceSource(&vecs);
        let g = build(&vecs, HnswParams::default());
        let query = &vecs[7];
        let hnsw: Vec<u32> = search_hnsw(&g, &src, Metric::L2, query, 10, 300, None)
            .into_iter()
            .map(|(r, _)| r)
            .collect();

        let mut flat: Vec<(f32, u32)> = (0..300u32)
            .map(|r| (Metric::L2.distance_key(query, &vecs[r as usize]), r))
            .collect();
        flat.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let flat: Vec<u32> = flat.into_iter().take(10).map(|(_, r)| r).collect();
        assert_eq!(hnsw, flat);
    }

    #[test]
    fn filter_mask_excludes_rows() {
        let vecs = random_vectors(200, 4, 4);
        let src = SliceSource(&vecs);
        let g = build(&vecs, HnswParams::default());
        let mask = |row: u32| row.is_multiple_of(2);
        let hits = search_hnsw(&g, &src, Metric::L2, &vecs[0], 20, 200, Some(&mask));
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|(r, _)| r % 2 == 0));
    }

    #[test]
    fn edge_cases() {
        // 空グラフ
        let vecs: Vec<Vec<f32>> = Vec::new();
        let g = build(&vecs, HnswParams::default());
        let src = SliceSource(&vecs);
        assert!(search_hnsw(&g, &src, Metric::L2, &[0.0], 5, 64, None).is_empty());

        // 1 ノード
        let vecs = vec![vec![1.0, 2.0]];
        let g = build(&vecs, HnswParams::default());
        let src = SliceSource(&vecs);
        let hits = search_hnsw(&g, &src, Metric::L2, &[1.0, 2.0], 5, 64, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 0);

        // k > ノード数
        let vecs = random_vectors(5, 4, 5);
        let g = build(&vecs, HnswParams::default());
        let src = SliceSource(&vecs);
        assert_eq!(
            search_hnsw(&g, &src, Metric::L2, &vecs[0], 100, 64, None).len(),
            5
        );

        // k = 0
        assert!(search_hnsw(&g, &src, Metric::L2, &vecs[0], 0, 64, None).is_empty());
    }

    #[test]
    fn serialize_view_roundtrip() {
        let vecs = random_vectors(250, 8, 6);
        let src = SliceSource(&vecs);
        let g = build(&vecs, HnswParams::default());
        let buf = g.serialize();
        let view = HnswView::open(&buf).unwrap();

        assert_eq!(view.node_count(), g.node_count());
        assert_eq!(view.max_level(), g.max_level());
        assert_eq!(view.entry_point(), g.entry_point());
        for v in 0..g.node_count() {
            assert_eq!(view.level_of(v), g.level_of(v));
            for level in 0..=g.level_of(v) {
                assert_eq!(view.neighbors(level, v), g.neighbors(level, v));
            }
        }

        // ビュー越しの探索がビルダーと同一結果
        let query = &vecs[3];
        let a = search_hnsw(&g, &src, Metric::L2, query, 10, 64, None);
        let b = search_hnsw(&view, &src, Metric::L2, query, 10, 64, None);
        assert_eq!(a, b);
    }

    #[test]
    fn view_rejects_corruption() {
        let vecs = random_vectors(50, 4, 7);
        let g = build(&vecs, HnswParams::default());
        let buf = g.serialize();

        // magic 破壊
        let mut bad = buf.clone();
        bad[0] ^= 0xFF;
        assert!(HnswView::open(&bad).is_err());
        // 切り詰め
        assert!(HnswView::open(&buf[..buf.len() - 3]).is_err());
        // 末尾に余分なバイト
        let mut bad = buf.clone();
        bad.extend_from_slice(&[0; 4]);
        assert!(HnswView::open(&bad).is_err());
    }
}
