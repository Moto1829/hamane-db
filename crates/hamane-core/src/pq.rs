//! 直積量子化 (PQ, todo 1002)。
//!
//! dim 次元ベクトルを `m` 本のサブベクトル (各 `dsub = dim / m` 次元) に分割し、
//! サブベクトルごとに独立した `ksub = 2^nbits` 個のセントロイド (コードブック) を
//! 学習する。1 本のベクトルは `m` 個のセントロイド ID (nbits=8 なので u8) で表す。
//!
//! 検索は **ADC** (Asymmetric Distance Computation): クエリは量子化せず f32 の
//! まま、サブベクトルと全セントロイドの距離を先に表 (LUT) に載せ、各行は
//! `m` 回の表引き加算で距離を推定する。推定は SQ8 と同じ 2 段階検索の 1 段目に
//! 使い、上位候補を f32 で再ランクして精度を保つ。
//!
//! k-means は IVF の粗量子化 (todo 1004) と共有できるよう汎用関数として切り出す。

use crate::metric::{dot_scalar, l2_squared_scalar, Metric};
use crate::{HamaneError, Result};

/// サブコード幅 (ビット) の既定。8 = 1 サブベクトル 1 バイト・k*=256。
pub const NBITS: u8 = 8;
/// 既定 nbits でのサブコードブックのセントロイド数 (= 2^NBITS)。
pub const KSUB: usize = 1 << NBITS;

/// `nbits` に対するサブコードブックのセントロイド数 (todo 1201)。
#[inline]
pub const fn ksub(nbits: u8) -> usize {
    1usize << nbits
}

/// 1 行のコードが占めるバイト数。nbits=4 は 2 サブコードで 1 バイト。
#[inline]
pub const fn code_bytes(m: usize, nbits: u8) -> usize {
    (m * nbits as usize).div_ceil(8)
}

/// 実装済みのサブコード幅か (4 と 8 のみ)。
#[inline]
pub const fn is_supported_nbits(nbits: u8) -> bool {
    nbits == 4 || nbits == 8
}

/// コードブック学習の既定サンプル上限 (これを超える件数は間引いて学習する)。
pub const DEFAULT_TRAIN_SAMPLE: usize = 65536;
/// k-means の既定反復上限。
pub const DEFAULT_MAX_ITER: usize = 25;

/// `dim` からサブベクトル数 `m` の既定値を決める。
///
/// dim の約数のうち `dsub = dim/m ≥ MIN_DSUB` かつ `m ≤ MAX_M` を満たす **最大の
/// m** (= 最小の dsub) を返す。約数が見つからなければ `None` (ユーザが m を明示)。
///
/// `dsub` の探索を `dim/2` までに限ることで `m ≥ 2` を保証する (m=1 は PQ として
/// 無意味 = 全次元 1 コードブック)。素数次元などは `None` になる。
///
/// 例: 768 → 96 (dsub=8)、128 → 32 (dsub=4)、100 → 25 (dsub=4)、97 → None。
pub fn choose_m(dim: usize) -> Option<usize> {
    const MIN_DSUB: usize = 4;
    const MAX_M: usize = 96;
    for dsub in MIN_DSUB..=dim / 2 {
        if dim.is_multiple_of(dsub) && dim / dsub <= MAX_M {
            return Some(dim / dsub);
        }
    }
    None
}

/// 決定的な擬似乱数 (LCG)。seed 固定で再現可能な学習のため、外部 rand に依存しない。
/// OPQ (opq.rs) の乱数直交行列でも使う。
pub(crate) struct Lcg(u64);

impl Lcg {
    pub(crate) fn new(seed: u64) -> Self {
        // seed 0 でも縮退しないよう定数を混ぜる
        Lcg(seed ^ 0x9E3779B97F4A7C15)
    }

    #[inline]
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    /// [0, 1) の一様乱数。
    #[inline]
    pub(crate) fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// [0, n) の一様整数 (n > 0)。
    #[inline]
    fn next_below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// `points` (各 `dim` 次元) を k-means で `k` クラスタに分割し、セントロイドを返す。
///
/// - 初期化は k-means++ (距離² に比例した確率で種を選ぶ)
/// - Lloyd 反復を `max_iter` 回、または重心がほぼ動かなくなるまで
/// - 空クラスタは最大クラスタの最遠点を新セントロイドにして回避
/// - `points.len() < k` の縮退時も panic せず、存在する点でセントロイドを埋める
///
/// 戻り値は `k × dim` の平坦化ベクトル (`out[c * dim + d]`)。距離は L2 で測る
/// (Cosine は正規化済みなので L2 最近傍 = 角度最近傍。Dot も再構成誤差最小化は L2)。
pub fn kmeans(points: &[&[f32]], dim: usize, k: usize, seed: u64, max_iter: usize) -> Vec<f32> {
    let n = points.len();
    let mut centroids = vec![0.0f32; k * dim];
    if n == 0 || k == 0 {
        return centroids;
    }
    let mut rng = Lcg::new(seed);

    // --- k-means++ 初期化 ---
    let first = rng.next_below(n);
    centroids[..dim].copy_from_slice(&points[first][..dim]);
    // 各点の最近セントロイドまでの距離² (種選択の重み)
    let mut nearest = vec![f32::INFINITY; n];
    for (i, p) in points.iter().enumerate() {
        nearest[i] = l2_squared_scalar(&p[..dim], &points[first][..dim]);
    }
    for c in 1..k {
        let total: f64 = nearest.iter().map(|&d| d as f64).sum();
        let chosen = if total <= 0.0 {
            // 全点が既存セントロイドと一致 (重複データ)。任意の点で埋める
            rng.next_below(n)
        } else {
            let target = rng.next_unit() * total;
            let mut acc = 0.0f64;
            let mut idx = n - 1;
            for (i, &d) in nearest.iter().enumerate() {
                acc += d as f64;
                if acc >= target {
                    idx = i;
                    break;
                }
            }
            idx
        };
        centroids[c * dim..(c + 1) * dim].copy_from_slice(&points[chosen][..dim]);
        for (i, p) in points.iter().enumerate() {
            let d = l2_squared_scalar(&p[..dim], &points[chosen][..dim]);
            if d < nearest[i] {
                nearest[i] = d;
            }
        }
    }

    // --- Lloyd 反復 ---
    let mut assign = vec![0u32; n];
    let mut sums = vec![0.0f32; k * dim];
    let mut counts = vec![0u32; k];
    for _ in 0..max_iter {
        // 割り当て
        let mut changed = false;
        for (i, p) in points.iter().enumerate() {
            let mut best = 0usize;
            let mut best_d = f32::INFINITY;
            for c in 0..k {
                let d = l2_squared_scalar(&p[..dim], &centroids[c * dim..(c + 1) * dim]);
                if d < best_d {
                    best_d = d;
                    best = c;
                }
            }
            if assign[i] != best as u32 {
                assign[i] = best as u32;
                changed = true;
            }
        }

        // 更新 (割り当て点の平均)
        sums.iter_mut().for_each(|x| *x = 0.0);
        counts.iter_mut().for_each(|x| *x = 0);
        for (i, p) in points.iter().enumerate() {
            let c = assign[i] as usize;
            counts[c] += 1;
            let base = c * dim;
            for d in 0..dim {
                sums[base + d] += p[d];
            }
        }
        let mut moved = 0.0f32;
        for c in 0..k {
            if counts[c] == 0 {
                // 空クラスタ: 最大クラスタの最遠点を種にする
                if let Some(i) =
                    farthest_point_of_biggest(points, dim, &centroids, &assign, &counts)
                {
                    centroids[c * dim..(c + 1) * dim].copy_from_slice(&points[i][..dim]);
                }
                continue;
            }
            let inv = 1.0 / counts[c] as f32;
            let base = c * dim;
            for d in 0..dim {
                let nv = sums[base + d] * inv;
                let delta = nv - centroids[base + d];
                moved += delta * delta;
                centroids[base + d] = nv;
            }
        }

        if !changed || moved < 1e-8 {
            break;
        }
    }

    centroids
}

/// `point` に最も近いセントロイドの ID を返す (L2)。`centroids` は `k × dim` の
/// 平坦化。PQ の符号化と IVF の粗量子化割り当てで共有する。
pub fn nearest(point: &[f32], centroids: &[f32], dim: usize, k: usize) -> u32 {
    let mut best = 0u32;
    let mut best_d = f32::INFINITY;
    for c in 0..k {
        let d = l2_squared_scalar(point, &centroids[c * dim..(c + 1) * dim]);
        if d < best_d {
            best_d = d;
            best = c as u32;
        }
    }
    best
}

/// `vectors` が `limit` を超える場合、等間隔ストライドで決定的に間引く
/// (k-means の学習コストを抑える)。`limit == 0` または以下ならそのまま。
pub fn subsample<'a>(vectors: &[&'a [f32]], limit: usize) -> Vec<&'a [f32]> {
    if limit > 0 && vectors.len() > limit {
        let stride = vectors.len() / limit;
        vectors
            .iter()
            .step_by(stride)
            .take(limit)
            .copied()
            .collect()
    } else {
        vectors.to_vec()
    }
}

/// 最も点数の多いクラスタの、重心から最遠の点のインデックスを返す。
/// 空クラスタの再シードに使う。該当点がなければ `None`。
fn farthest_point_of_biggest(
    points: &[&[f32]],
    dim: usize,
    centroids: &[f32],
    assign: &[u32],
    counts: &[u32],
) -> Option<usize> {
    let biggest = (0..counts.len()).max_by_key(|&c| counts[c])?;
    let base = biggest * dim;
    let mut far = None;
    let mut far_d = -1.0f32;
    for (i, p) in points.iter().enumerate() {
        if assign[i] as usize != biggest {
            continue;
        }
        let d = l2_squared_scalar(&p[..dim], &centroids[base..base + dim]);
        if d > far_d {
            far_d = d;
            far = Some(i);
        }
    }
    far
}

/// 学習済みの直積量子化コードブック。
#[derive(Debug, Clone, PartialEq)]
pub struct PqCodebook {
    m: usize,
    dsub: usize,
    /// サブコード幅 (4 か 8)。k* = 2^nbits
    nbits: u8,
    /// レイアウト: `centroids[(j * k* + c) * dsub + d]`
    centroids: Vec<f32>,
}

impl PqCodebook {
    /// サブベクトル数。
    #[inline]
    pub fn m(&self) -> usize {
        self.m
    }
    /// サブベクトル次元 (= dim / m)。
    #[inline]
    pub fn dsub(&self) -> usize {
        self.dsub
    }
    /// 元ベクトルの次元。
    #[inline]
    pub fn dim(&self) -> usize {
        self.m * self.dsub
    }
    /// サブコード幅 (ビット)。
    #[inline]
    pub fn nbits(&self) -> u8 {
        self.nbits
    }
    /// サブコードブックのセントロイド数 (= 2^nbits)。
    #[inline]
    pub fn ksub(&self) -> usize {
        ksub(self.nbits)
    }
    /// 1 行のコードが占めるバイト数 (nbits=4 なら m/2)。
    #[inline]
    pub fn code_bytes(&self) -> usize {
        code_bytes(self.m, self.nbits)
    }
    /// セントロイド本体 (直列化用)。`m × k* × dsub` の平坦化。
    #[inline]
    pub fn centroids(&self) -> &[f32] {
        &self.centroids
    }

    /// 直列化されたセントロイドからコードブックを復元する (mmap ロード用、todo 1003)。
    pub fn from_centroids(m: usize, dsub: usize, nbits: u8, centroids: Vec<f32>) -> Result<Self> {
        if !is_supported_nbits(nbits) {
            return Err(HamaneError::Corrupted(format!(
                "pq unsupported nbits: {nbits}"
            )));
        }
        let expected = m * ksub(nbits) * dsub;
        if centroids.len() != expected {
            return Err(HamaneError::Corrupted(format!(
                "pq codebook size mismatch: expected {expected}, got {}",
                centroids.len()
            )));
        }
        Ok(Self {
            m,
            dsub,
            nbits,
            centroids,
        })
    }

    /// 学習データからコードブックを学習する。
    ///
    /// - `dim % m != 0` は `InvalidConfig`
    /// - 件数が `train_sample` を超えたら決定的に間引く (等間隔ストライド)
    /// - サブベクトルごとに `kmeans` を回す (seed はサブベクトル番号で分離)
    /// - `nbits` は 4 か 8 (それ以外は `InvalidConfig`)
    pub fn train(
        vectors: &[&[f32]],
        dim: usize,
        m: usize,
        nbits: u8,
        seed: u64,
        train_sample: usize,
        max_iter: usize,
    ) -> Result<Self> {
        if m == 0 || !dim.is_multiple_of(m) {
            return Err(HamaneError::InvalidConfig(format!(
                "pq requires dim ({dim}) divisible by m ({m})"
            )));
        }
        if !is_supported_nbits(nbits) {
            return Err(HamaneError::InvalidConfig(format!(
                "pq supports nbits 4 or 8, got {nbits}"
            )));
        }
        let dsub = dim / m;
        let ksub = ksub(nbits);

        // 学習用サンプル (件数が多ければ等間隔で間引く)
        let sample: Vec<&[f32]> = if vectors.len() > train_sample && train_sample > 0 {
            let stride = vectors.len() / train_sample;
            vectors
                .iter()
                .step_by(stride)
                .take(train_sample)
                .copied()
                .collect()
        } else {
            vectors.to_vec()
        };

        let mut centroids = vec![0.0f32; m * ksub * dsub];
        // サブベクトル j ごとに独立に学習
        let mut sub: Vec<&[f32]> = Vec::with_capacity(sample.len());
        for j in 0..m {
            sub.clear();
            for v in &sample {
                sub.push(&v[j * dsub..(j + 1) * dsub]);
            }
            // seed をサブベクトルごとにずらして相関を避ける
            let sub_seed = seed ^ (j as u64).wrapping_mul(0x9E3779B97F4A7C15);
            let cb = kmeans(&sub, dsub, ksub, sub_seed, max_iter);
            centroids[j * ksub * dsub..(j + 1) * ksub * dsub].copy_from_slice(&cb);
        }

        Ok(Self {
            m,
            dsub,
            nbits,
            centroids,
        })
    }

    /// サブベクトル `j` のセントロイド `c` のスライス。
    #[inline]
    fn centroid(&self, j: usize, c: usize) -> &[f32] {
        let base = (j * self.ksub() + c) * self.dsub;
        &self.centroids[base..base + self.dsub]
    }

    /// コード列からサブベクトル `j` のセントロイド ID を取り出す。
    /// nbits=4 は 1 バイトに 2 個 (偶数 j が下位ニブル)。
    #[inline]
    fn sub_code(&self, code: &[u8], j: usize) -> usize {
        match self.nbits {
            4 => {
                let byte = code[j / 2];
                if j.is_multiple_of(2) {
                    (byte & 0x0f) as usize
                } else {
                    (byte >> 4) as usize
                }
            }
            _ => code[j] as usize,
        }
    }

    /// 1 本のベクトルを `code_bytes()` バイトのコードに符号化する
    /// (各サブベクトルの L2 最近傍)。nbits=4 は 2 サブコードを 1 バイトに詰める。
    pub fn encode(&self, vector: &[f32], out: &mut Vec<u8>) {
        debug_assert_eq!(vector.len(), self.dim());
        let ksub = self.ksub();
        let mut pending = 0u8; // nbits=4 のとき偶数 j の下位ニブルを保持
        for j in 0..self.m {
            let vsub = &vector[j * self.dsub..(j + 1) * self.dsub];
            let mut best = 0u8;
            let mut best_d = f32::INFINITY;
            for c in 0..ksub {
                let d = l2_squared_scalar(vsub, self.centroid(j, c));
                if d < best_d {
                    best_d = d;
                    best = c as u8;
                }
            }
            if self.nbits == 4 {
                if j.is_multiple_of(2) {
                    pending = best & 0x0f;
                    // m が奇数なら最後のニブルは上位を 0 埋めして出す
                    if j + 1 == self.m {
                        out.push(pending);
                    }
                } else {
                    out.push(pending | (best << 4));
                }
            } else {
                out.push(best);
            }
        }
    }

    /// コードから元ベクトルを復元する (各サブベクトルをセントロイドで置換)。
    /// OPQ の交互最適化 (todo 1102) と量子化誤差の計測に使う。
    pub fn decode_into(&self, code: &[u8], out: &mut [f32]) {
        debug_assert_eq!(code.len(), self.code_bytes());
        debug_assert_eq!(out.len(), self.dim());
        for j in 0..self.m {
            let c = self.centroid(j, self.sub_code(code, j));
            out[j * self.dsub..(j + 1) * self.dsub].copy_from_slice(c);
        }
    }

    /// クエリと全セントロイドの距離を先計算した LUT を作る。
    ///
    /// 各エントリは **「小さいほど近い」に正規化済み** (L2 は距離²、Dot/Cosine は
    /// 内積の符号反転)。したがって `distance_key` は符号を気にせず表引き加算でよい。
    ///
    /// - nbits=8: `m × 256` のサブベクトル別テーブル
    /// - nbits=4: **バイト単位の合成テーブル `(m/2) × 256`** (todo 1204)。
    ///   4bit は 1 バイトに 2 サブコードが入るので、そのバイト値をそのまま引けば
    ///   2 サブベクトルぶんの距離が 1 回で得られる。表引き回数が半分になり、
    ///   ニブル展開も消える (同じコード長の 8bit と同じループ回数になる)
    pub fn build_lut(&self, query: &[f32], metric: Metric) -> Vec<f32> {
        let base = self.build_sub_lut(query, metric);
        if self.nbits != 4 {
            return base;
        }
        // 合成: byte b = (hi << 4) | lo → lut[2i][lo] + lut[2i+1][hi]
        let pairs = self.m / 2;
        let mut fused = vec![0.0f32; pairs * 256 + if self.m % 2 == 1 { 16 } else { 0 }];
        for i in 0..pairs {
            let lo_row = &base[2 * i * 16..2 * i * 16 + 16];
            let hi_row = &base[(2 * i + 1) * 16..(2 * i + 1) * 16 + 16];
            let out = &mut fused[i * 256..(i + 1) * 256];
            for (b, slot) in out.iter_mut().enumerate() {
                *slot = lo_row[b & 0x0f] + hi_row[b >> 4];
            }
        }
        // m が奇数なら最後の 1 サブベクトルは従来どおり 16 エントリで持つ
        if self.m % 2 == 1 {
            let last = &base[(self.m - 1) * 16..self.m * 16];
            fused[pairs * 256..pairs * 256 + 16].copy_from_slice(last);
        }
        fused
    }

    /// サブベクトル別の生 LUT (`m × k*`)。合成前の形。
    fn build_sub_lut(&self, query: &[f32], metric: Metric) -> Vec<f32> {
        debug_assert_eq!(query.len(), self.dim());
        let ksub = self.ksub();
        let mut lut = vec![0.0f32; self.m * ksub];
        for j in 0..self.m {
            let qsub = &query[j * self.dsub..(j + 1) * self.dsub];
            let row = &mut lut[j * ksub..(j + 1) * ksub];
            match metric {
                Metric::L2 => {
                    for (c, slot) in row.iter_mut().enumerate() {
                        *slot = l2_squared_scalar(qsub, self.centroid(j, c));
                    }
                }
                Metric::Cosine | Metric::Dot => {
                    for (c, slot) in row.iter_mut().enumerate() {
                        *slot = -dot_scalar(qsub, self.centroid(j, c));
                    }
                }
            }
        }
        lut
    }

    /// LUT とコードから距離キー (小さいほど近い) を推定する。
    #[inline]
    pub fn distance_key(&self, lut: &[f32], code: &[u8]) -> f32 {
        debug_assert_eq!(code.len(), self.code_bytes());
        if self.nbits == 4 {
            // バイト値をそのまま引く (2 サブベクトルぶんの合成テーブル)
            let pairs = self.m / 2;
            let mut sum = 0.0f32;
            for (i, &byte) in code.iter().take(pairs).enumerate() {
                sum += lut[i * 256 + byte as usize];
            }
            if self.m % 2 == 1 {
                // 端数のサブベクトルは下位ニブルだけを 16 エントリ表から引く
                sum += lut[pairs * 256 + (code[pairs] & 0x0f) as usize];
            }
            return sum;
        }
        let ksub = self.ksub();
        let mut sum = 0.0f32;
        for j in 0..self.m {
            sum += lut[j * ksub + code[j] as usize];
        }
        sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// クラスタ性のある決定的データを生成する (PQ が効く前提のデータ)。
    fn clustered_data(n: usize, dim: usize, n_clusters: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut rng = Lcg::new(seed);
        // クラスタ中心
        let centers: Vec<Vec<f32>> = (0..n_clusters)
            .map(|_| {
                (0..dim)
                    .map(|_| (rng.next_unit() as f32) * 10.0 - 5.0)
                    .collect()
            })
            .collect();
        (0..n)
            .map(|_| {
                let c = &centers[rng.next_below(n_clusters)];
                c.iter()
                    .map(|&x| x + (rng.next_unit() as f32) * 0.4 - 0.2)
                    .collect()
            })
            .collect()
    }

    fn as_slices(v: &[Vec<f32>]) -> Vec<&[f32]> {
        v.iter().map(|x| x.as_slice()).collect()
    }

    #[test]
    fn choose_m_cases() {
        assert_eq!(choose_m(768), Some(96)); // dsub=8
        assert_eq!(choose_m(128), Some(32)); // dsub=4
        assert_eq!(choose_m(100), Some(25)); // dsub=4
        assert_eq!(choose_m(64), Some(16)); // dsub=4
                                            // 素数次元は約数が {1, dim} のみ → dsub≥4 & m≤96 を満たさない
        assert_eq!(choose_m(97), None);
    }

    #[test]
    fn train_rejects_indivisible_dim() {
        let data = clustered_data(50, 10, 4, 1);
        let slices = as_slices(&data);
        assert!(PqCodebook::train(&slices, 10, 3, NBITS, 0, 1000, 10).is_err());
    }

    #[test]
    fn adc_approximates_l2() {
        let dim = 64;
        let m = 16;
        let data = clustered_data(2000, dim, 32, 7);
        let slices = as_slices(&data);
        let cb = PqCodebook::train(&slices, dim, m, NBITS, 0, 65536, 25).unwrap();

        // 各行を符号化
        let codes: Vec<Vec<u8>> = data
            .iter()
            .map(|v| {
                let mut c = Vec::new();
                cb.encode(v, &mut c);
                c
            })
            .collect();

        // クエリ 20 本で ADC 推定と f32 実距離の相対誤差を確認
        let queries = clustered_data(20, dim, 32, 99);
        let mut worst = 0.0f32;
        for q in &queries {
            let lut = cb.build_lut(q, Metric::L2);
            for (v, code) in data.iter().zip(&codes) {
                let exact = l2_squared_scalar(q, v);
                let approx = cb.distance_key(&lut, code);
                if exact > 1.0 {
                    worst = worst.max((exact - approx).abs() / exact);
                }
            }
        }
        // クラスタデータ + 残差小なので 15% 以内に収まるはず
        assert!(worst < 0.15, "worst relative error = {worst}");
    }

    #[test]
    fn adc_candidates_contain_true_neighbors() {
        // 2 段階検索を模す: ADC で k×RERANK 候補を集め、その中に f32 の真の
        // top-k がどれだけ入るか (= f32 再ランク後の recall 上限)。
        let dim = 32;
        let m = 8;
        let data = clustered_data(1000, dim, 16, 3);
        let slices = as_slices(&data);
        let cb = PqCodebook::train(&slices, dim, m, NBITS, 0, 65536, 25).unwrap();
        let codes: Vec<Vec<u8>> = data
            .iter()
            .map(|v| {
                let mut c = Vec::new();
                cb.encode(v, &mut c);
                c
            })
            .collect();

        let queries = clustered_data(30, dim, 16, 55);
        let mut hit = 0usize;
        let k = 10;
        let fetch = k * 4; // PQ_RERANK = 4
        for q in &queries {
            // f32 の正解 top-k
            let mut exact: Vec<(f32, usize)> = data
                .iter()
                .enumerate()
                .map(|(i, v)| (l2_squared_scalar(q, v), i))
                .collect();
            exact.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let truth: std::collections::HashSet<usize> =
                exact.iter().take(k).map(|&(_, i)| i).collect();
            // ADC の上位 fetch 候補
            let lut = cb.build_lut(q, Metric::L2);
            let mut adc: Vec<(f32, usize)> = codes
                .iter()
                .enumerate()
                .map(|(i, c)| (cb.distance_key(&lut, c), i))
                .collect();
            adc.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            hit += adc
                .iter()
                .take(fetch)
                .filter(|&&(_, i)| truth.contains(&i))
                .count();
        }
        // oversample した候補集合には真の近傍がほぼ入る (再ランクで拾える)
        let recall = hit as f32 / (queries.len() * k) as f32;
        assert!(
            recall >= 0.9,
            "adc recall@10 within {fetch} candidates = {recall}"
        );
    }

    #[test]
    fn dot_lut_orders_like_exact_dot() {
        let dim = 48;
        let m = 12;
        let data = clustered_data(800, dim, 16, 11);
        let slices = as_slices(&data);
        let cb = PqCodebook::train(&slices, dim, m, NBITS, 0, 65536, 25).unwrap();
        let codes: Vec<Vec<u8>> = data
            .iter()
            .map(|v| {
                let mut c = Vec::new();
                cb.encode(v, &mut c);
                c
            })
            .collect();
        let q = &clustered_data(1, dim, 16, 22)[0];
        let lut = cb.build_lut(q, Metric::Dot);
        // distance_key は -dot 推定。実 dot が大きいほど key は小さいはず
        let mut pairs: Vec<(f32, f32)> = data
            .iter()
            .zip(&codes)
            .map(|(v, c)| (dot_scalar(q, v), cb.distance_key(&lut, c)))
            .collect();
        pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap()); // 実 dot 降順
                                                              // 上位の平均 key < 下位の平均 key (単調性の粗い確認)
        let top: f32 = pairs.iter().take(50).map(|p| p.1).sum::<f32>() / 50.0;
        let bottom: f32 = pairs.iter().rev().take(50).map(|p| p.1).sum::<f32>() / 50.0;
        assert!(
            top < bottom,
            "top key {top} should be < bottom key {bottom}"
        );
    }

    #[test]
    fn training_is_deterministic() {
        let data = clustered_data(500, 32, 8, 4);
        let slices = as_slices(&data);
        let a = PqCodebook::train(&slices, 32, 8, NBITS, 42, 65536, 25).unwrap();
        let b = PqCodebook::train(&slices, 32, 8, NBITS, 42, 65536, 25).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn from_centroids_roundtrip() {
        let data = clustered_data(300, 16, 8, 5);
        let slices = as_slices(&data);
        let cb = PqCodebook::train(&slices, 16, 4, NBITS, 1, 65536, 25).unwrap();
        let restored =
            PqCodebook::from_centroids(cb.m(), cb.dsub(), cb.nbits(), cb.centroids().to_vec())
                .unwrap();
        assert_eq!(cb, restored);
        // サイズ不整合はエラー
        assert!(PqCodebook::from_centroids(4, 4, NBITS, vec![0.0; 3]).is_err());
    }

    #[test]
    fn duplicate_data_does_not_panic() {
        // 全点同一 (空クラスタ多発) でも学習が完了する
        let data: Vec<Vec<f32>> = (0..100).map(|_| vec![1.0f32; 16]).collect();
        let slices = as_slices(&data);
        let cb = PqCodebook::train(&slices, 16, 4, NBITS, 0, 65536, 25).unwrap();
        let mut code = Vec::new();
        cb.encode(&data[0], &mut code);
        assert_eq!(code.len(), 4);
    }

    #[test]
    fn kmeans_handles_fewer_points_than_k() {
        // 点数 < k の縮退でも panic しない
        let pts = vec![vec![0.0f32, 0.0], vec![1.0f32, 1.0]];
        let slices = as_slices(&pts);
        let c = kmeans(&slices, 2, 8, 0, 10);
        assert_eq!(c.len(), 8 * 2);
    }

    /// 一様分布データ (粗量子化で残差の分散が確実に縮むデータ)。
    fn uniform_data(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut rng = Lcg::new(seed);
        (0..n)
            .map(|_| {
                (0..dim)
                    .map(|_| (rng.next_unit() as f32) * 10.0 - 5.0)
                    .collect()
            })
            .collect()
    }

    /// コード列から復元したベクトルと元ベクトルの平均二乗誤差。
    fn reconstruction_mse(cb: &PqCodebook, vectors: &[Vec<f32>], bases: Option<&[&[f32]]>) -> f64 {
        let mut sum = 0.0f64;
        let mut code = Vec::new();
        for (i, v) in vectors.iter().enumerate() {
            // 基準ベクトル (IVF-PQ の粗セントロイド) があれば残差を符号化する
            let target: Vec<f32> = match bases {
                Some(b) => v.iter().zip(b[i]).map(|(x, c)| x - c).collect(),
                None => v.clone(),
            };
            code.clear();
            cb.encode(&target, &mut code);
            for j in 0..cb.m() {
                let sub = &target[j * cb.dsub()..(j + 1) * cb.dsub()];
                sum += l2_squared_scalar(sub, cb.centroid(j, code[j] as usize)) as f64;
            }
        }
        sum / vectors.len() as f64
    }

    #[test]
    fn residual_pq_has_lower_error_than_raw_pq() {
        // 同じ m で「生ベクトル PQ」と「粗量子化の残差 PQ」の量子化誤差を比べる
        // (todo 1005 の完了条件)。粗セントロイドを引いた分だけ分散が縮むので、
        // 残差版の誤差は必ず小さくなる。
        const N: usize = 800;
        const DIM: usize = 8;
        const M: usize = 2;
        const NLIST: usize = 64;
        const MAX_ITER: usize = 8;

        let data = uniform_data(N, DIM, 7);
        let slices = as_slices(&data);

        // 生ベクトルで学習した PQ
        let raw = PqCodebook::train(&slices, DIM, M, NBITS, 1, 65536, MAX_ITER).unwrap();
        let raw_mse = reconstruction_mse(&raw, &data, None);

        // 粗 k-means → 残差空間で学習した PQ (IVF-PQ と同じ手順)
        let centroids = kmeans(&slices, DIM, NLIST, 2, MAX_ITER);
        let bases: Vec<&[f32]> = data
            .iter()
            .map(|v| {
                let c = nearest(v, &centroids, DIM, NLIST) as usize;
                &centroids[c * DIM..(c + 1) * DIM]
            })
            .collect();
        let residuals: Vec<Vec<f32>> = data
            .iter()
            .zip(&bases)
            .map(|(v, c)| v.iter().zip(c.iter()).map(|(x, cc)| x - cc).collect())
            .collect();
        let res_slices = as_slices(&residuals);
        let res = PqCodebook::train(&res_slices, DIM, M, NBITS, 1, 65536, MAX_ITER).unwrap();
        let res_mse = reconstruction_mse(&res, &data, Some(&bases));

        assert!(
            res_mse < raw_mse,
            "residual pq mse {res_mse} should be below raw pq mse {raw_mse}"
        );
    }

    #[test]
    fn nbits4_packs_two_subcodes_per_byte() {
        const DIM: usize = 16;
        const M: usize = 8;
        let data = clustered_data(400, DIM, 6, 4);
        let slices = as_slices(&data);
        let cb4 = PqCodebook::train(&slices, DIM, M, 4, 0, 65536, 25).unwrap();
        assert_eq!(cb4.nbits(), 4);
        assert_eq!(cb4.ksub(), 16);
        assert_eq!(cb4.code_bytes(), M / 2, "2 サブコードで 1 バイト");

        let mut code = Vec::new();
        cb4.encode(&data[0], &mut code);
        assert_eq!(code.len(), M / 2);

        // ラウンドトリップ: 復元 → 再符号化で同じコードになる
        let mut recon = vec![0.0f32; DIM];
        cb4.decode_into(&code, &mut recon);
        let mut code2 = Vec::new();
        cb4.encode(&recon, &mut code2);
        assert_eq!(code, code2);

        // LUT の表引きが復元ベクトルの実距離と整合する (L2)
        let q = &data[1];
        let lut = cb4.build_lut(q, Metric::L2);
        let est = cb4.distance_key(&lut, &code);
        let exact = l2_squared_scalar(q, &recon);
        assert!(
            (est - exact).abs() / exact.max(1e-6) < 1e-3,
            "adc {est} vs exact {exact}"
        );
    }

    #[test]
    fn nbits4_is_half_the_size_and_coarser() {
        const DIM: usize = 16;
        const M: usize = 8;
        let data = clustered_data(600, DIM, 12, 9);
        let slices = as_slices(&data);
        let mse = |nbits: u8| -> f64 {
            let cb = PqCodebook::train(&slices, DIM, M, nbits, 0, 65536, 25).unwrap();
            let mut code = Vec::new();
            let mut recon = vec![0.0f32; DIM];
            let mut sum = 0.0f64;
            for v in &data {
                code.clear();
                cb.encode(v, &mut code);
                cb.decode_into(&code, &mut recon);
                sum += l2_squared_scalar(v, &recon) as f64;
            }
            sum / data.len() as f64
        };
        let (e8, e4) = (mse(8), mse(4));
        assert!(e4 >= e8, "4bit ({e4}) は 8bit ({e8}) より粗いはず");
        assert_eq!(code_bytes(M, 4) * 2, code_bytes(M, 8));
    }

    #[test]
    fn unsupported_nbits_is_rejected() {
        let data = clustered_data(50, 8, 2, 1);
        let slices = as_slices(&data);
        for nbits in [0u8, 1, 6, 16] {
            assert!(
                PqCodebook::train(&slices, 8, 2, nbits, 0, 65536, 5).is_err(),
                "nbits {nbits} must be rejected"
            );
        }
        assert!(PqCodebook::from_centroids(2, 4, 6, vec![0.0; 2 * 64 * 4]).is_err());
    }

    #[test]
    fn fused_byte_lut_matches_subvector_lut() {
        // nbits=4 の合成 LUT (バイト単位 256 エントリ) が、サブベクトル別の
        // 16 エントリ表を 2 回引いた値と一致すること (todo 1204)
        for (dim, m) in [(16usize, 8usize), (16, 4), (12, 3)] {
            let data = clustered_data(300, dim, 5, 17);
            let slices = as_slices(&data);
            let cb = PqCodebook::train(&slices, dim, m, 4, 2, 65536, 20).unwrap();
            let q = &data[7];
            for metric in [Metric::L2, Metric::Dot] {
                let fused = cb.build_lut(q, metric);
                let sub = cb.build_sub_lut(q, metric);
                let mut code = Vec::new();
                for v in data.iter().take(20) {
                    code.clear();
                    cb.encode(v, &mut code);
                    let got = cb.distance_key(&fused, &code);
                    // 参照: サブベクトル別テーブルをニブル展開して足す
                    let mut want = 0.0f32;
                    for j in 0..m {
                        want += sub[j * 16 + cb.sub_code(&code, j)];
                    }
                    assert!(
                        (got - want).abs() <= want.abs() * 1e-5 + 1e-5,
                        "dim={dim} m={m} {metric:?}: fused {got} vs sub {want}"
                    );
                }
            }
        }
    }
}
