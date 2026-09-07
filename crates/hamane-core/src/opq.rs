//! OPQ (回転付き直積量子化, todo 1102)。
//!
//! PQ はベクトルを位置で機械的に分割するため、サブベクトル間の分散の偏りや
//! 次元間の相関があると誤差が大きい。OPQ は直交行列 `R` を学習し、`x' = R x`
//! に回転してから PQ する。直交変換は L2 距離と内積を保存するので **検索の
//! 意味は変わらず、量子化誤差だけが下がる**。
//!
//! 学習は `R` とコードブックの交互最適化 (docs/design/opq.md §1):
//!
//! 1. 回転済みデータで PQ コードブックを学習し、各行を符号化 → 復元する
//! 2. 直交 Procrustes 問題 `max_R tr(R Xᵀ Ŷ)` を解いて `R` を更新する
//!
//! 2 の解は `M = Ŷᵀ X` の直交極因子であり、SVD を持ち込まずとも Newton 反復
//! `Q ← ½(Q + Q⁻ᵀ)` で求まる。必要なのは d×d の逆行列だけ。
//!
//! 縮退入力 (特異行列・全点同一など) では恒等行列にフォールバックし、学習を
//! 絶対に失敗させない (pq.rs の m 自動決定と同じ方針)。

use crate::pq::{Lcg, PqCodebook};
use crate::{HamaneError, Result};

/// 交互最適化の反復数。
pub const OPQ_ITERS: usize = 8;
/// 回転学習に使う行数の上限 (PQ 本学習の DEFAULT_TRAIN_SAMPLE とは別に絞る)。
pub const OPQ_TRAIN_SAMPLE: usize = 16384;
/// 交互最適化の内側で回す k-means の反復数 (最終学習は通常の反復数に戻す)。
pub const OPQ_INNER_ITER: usize = 5;

/// 極分解 Newton 反復の上限。二次収束なので通常 10 回未満で収まる。
const POLAR_MAX_ITER: usize = 50;
/// 直交性の許容誤差 (`‖RᵀR − I‖_max`)。
const ORTHO_TOL: f32 = 1e-3;

// ---------------------------------------------------------------------------
// d×d 行列ユーティリティ (row-major、`a[i * d + j]` が i 行 j 列)
// ---------------------------------------------------------------------------

/// `c = a × b` (いずれも d×d)。
fn matmul(a: &[f32], b: &[f32], d: usize) -> Vec<f32> {
    let mut c = vec![0.0f32; d * d];
    for i in 0..d {
        for k in 0..d {
            let aik = a[i * d + k];
            if aik == 0.0 {
                continue;
            }
            let brow = &b[k * d..k * d + d];
            let crow = &mut c[i * d..i * d + d];
            for (cj, bj) in crow.iter_mut().zip(brow) {
                *cj += aik * bj;
            }
        }
    }
    c
}

/// 転置。
fn transpose(a: &[f32], d: usize) -> Vec<f32> {
    let mut t = vec![0.0f32; d * d];
    for i in 0..d {
        for j in 0..d {
            t[j * d + i] = a[i * d + j];
        }
    }
    t
}

fn identity(d: usize) -> Vec<f32> {
    let mut m = vec![0.0f32; d * d];
    for i in 0..d {
        m[i * d + i] = 1.0;
    }
    m
}

/// 対称行列の固有分解 (循環 Jacobi 法)。`(固有値, 固有ベクトル行列)` を返す。
///
/// 固有ベクトルは**行**方向 (`v[i * d + j]` = 第 i 固有ベクトルの第 j 成分)。
/// 決定的で外部クレートに依存しない。d は数百までを想定 (O(d³) × 掃引回数)。
fn jacobi_eigen(sym: &[f32], d: usize) -> (Vec<f64>, Vec<f64>) {
    const MAX_SWEEPS: usize = 60;
    let mut a: Vec<f64> = sym.iter().map(|&x| x as f64).collect();
    // v は固有ベクトルを列に持つ回転の蓄積 (最後に転置して行に直す)
    let mut v = vec![0.0f64; d * d];
    for i in 0..d {
        v[i * d + i] = 1.0;
    }
    for _ in 0..MAX_SWEEPS {
        // 非対角成分の大きさ
        let off: f64 = (0..d)
            .flat_map(|i| (0..d).map(move |j| (i, j)))
            .filter(|(i, j)| i != j)
            .map(|(i, j)| a[i * d + j] * a[i * d + j])
            .sum();
        if off < 1e-18 {
            break;
        }
        for p in 0..d {
            for q in p + 1..d {
                let apq = a[p * d + q];
                if apq.abs() < 1e-15 {
                    continue;
                }
                // 回転角: cot(2θ) = (a_qq − a_pp) / (2 a_pq)
                let theta = (a[q * d + q] - a[p * d + p]) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..d {
                    let akp = a[k * d + p];
                    let akq = a[k * d + q];
                    a[k * d + p] = c * akp - s * akq;
                    a[k * d + q] = s * akp + c * akq;
                }
                for k in 0..d {
                    let apk = a[p * d + k];
                    let aqk = a[q * d + k];
                    a[p * d + k] = c * apk - s * aqk;
                    a[q * d + k] = s * apk + c * aqk;
                }
                for k in 0..d {
                    let vkp = v[k * d + p];
                    let vkq = v[k * d + q];
                    v[k * d + p] = c * vkp - s * vkq;
                    v[k * d + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let eigenvalues: Vec<f64> = (0..d).map(|i| a[i * d + i]).collect();
    // 列 → 行に転置して「第 i 固有ベクトル = 行 i」にする
    let mut rows = vec![0.0f64; d * d];
    for i in 0..d {
        for j in 0..d {
            rows[i * d + j] = v[j * d + i];
        }
    }
    (eigenvalues, rows)
}

/// パラメトリック OPQ の初期値 (docs/design/opq.md §1.1)。
///
/// 学習サンプルの共分散を固有分解し、**固有値 (= 主成分の分散) の積が
/// サブベクトル間で均等になるように**固有ベクトルを配る (固有値割り当て)。
/// PQ の歪みは各サブベクトルの分散の幾何平均の和で決まるので、AM-GM より
/// これを均すのが最小化に効く。回転後の次元は無相関にもなる。
fn parametric_init(vectors: &[&[f32]], d: usize, m: usize) -> Vec<f32> {
    if d == 0 || m == 0 || !d.is_multiple_of(m) || vectors.len() < 2 {
        return identity(d);
    }
    // 共分散 (平均引き)。d×d なので d が数百までなら現実的
    let n = vectors.len() as f64;
    let mut mean = vec![0.0f64; d];
    for v in vectors {
        for (acc, x) in mean.iter_mut().zip(v.iter()) {
            *acc += *x as f64;
        }
    }
    for x in mean.iter_mut() {
        *x /= n;
    }
    let mut cov = vec![0.0f32; d * d];
    let mut centered = vec![0.0f64; d];
    for v in vectors {
        for (c, (x, mu)) in centered.iter_mut().zip(v.iter().zip(&mean)) {
            *c = *x as f64 - mu;
        }
        for i in 0..d {
            let ci = centered[i];
            if ci == 0.0 {
                continue;
            }
            for j in 0..d {
                cov[i * d + j] += (ci * centered[j]) as f32;
            }
        }
    }

    let (eigenvalues, eigenvectors) = jacobi_eigen(&cov, d);
    // 固有値降順の並び
    let mut order: Vec<usize> = (0..d).collect();
    order.sort_by(|&a, &b| {
        eigenvalues[b]
            .partial_cmp(&eigenvalues[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // 固有値割り当て: 大きい順に「積が最小のサブベクトル」へ入れる
    // (積は log 和で持つ。分散 0 の固有値は下限でクランプ)
    let dsub = d / m;
    let mut buckets: Vec<Vec<usize>> = vec![Vec::with_capacity(dsub); m];
    let mut log_prod = vec![0.0f64; m];
    for &idx in &order {
        let lambda = eigenvalues[idx].max(1e-12);
        let target = (0..m)
            .filter(|&b| buckets[b].len() < dsub)
            .min_by(|&a, &b| {
                log_prod[a]
                    .partial_cmp(&log_prod[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(0);
        buckets[target].push(idx);
        log_prod[target] += lambda.ln();
    }

    // 行 = 固有ベクトル (サブベクトル順に並べる)
    let mut r = vec![0.0f32; d * d];
    let mut row = 0usize;
    for bucket in &buckets {
        for &idx in bucket {
            for j in 0..d {
                r[row * d + j] = eigenvectors[idx * d + j] as f32;
            }
            row += 1;
        }
    }
    // 数値誤差で直交から外れたら恒等に落とす (安全側)
    if ortho_error(&r, d) < ORTHO_TOL {
        r
    } else {
        identity(d)
    }
}

/// 決定的な乱数直交行列 (Gram-Schmidt 直交化)。
///
/// 交互最適化の初期値。**恒等行列から始めてはいけない**: 軸に沿った分布では
/// 復元 `Ŷ` も軸に沿うため `M = ŶᵀX` がほぼ対角になり、極分解が恒等行列を
/// 返して 1 歩も動かない (恒等は局所最適)。乱数回転はそれ自体が分散を均す
/// 強いベースラインでもある。
fn random_orthogonal(d: usize, seed: u64) -> Vec<f32> {
    let mut rng = Lcg::new(seed);
    // 平均 0・分散 1 に近いガウス近似 (一様乱数 12 個の和 − 6)
    let mut normal = move || (0..12).map(|_| rng.next_unit()).sum::<f64>() - 6.0;
    let mut rows: Vec<Vec<f64>> = (0..d).map(|_| (0..d).map(|_| normal()).collect()).collect();

    // 修正 Gram-Schmidt で行を正規直交化する
    for i in 0..d {
        // rows[i] を rows[0..i] と同時に触るので split_at_mut で分ける
        let (done, rest) = rows.split_at_mut(i);
        let row_i = &mut rest[0];
        for row_j in done.iter() {
            let dot: f64 = row_i.iter().zip(row_j.iter()).map(|(a, b)| a * b).sum();
            for (x, y) in row_i.iter_mut().zip(row_j.iter()) {
                *x -= dot * y;
            }
        }
        let norm: f64 = row_i.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm < 1e-9 {
            // 縮退 (ほぼ従属) は単位ベクトルで埋める
            row_i.iter_mut().enumerate().for_each(|(k, x)| {
                *x = if k == i { 1.0 } else { 0.0 };
            });
        } else {
            for x in row_i.iter_mut() {
                *x /= norm;
            }
        }
    }
    let r: Vec<f32> = rows
        .into_iter()
        .flat_map(|row| row.into_iter().map(|x| x as f32))
        .collect();
    // 直交化が崩れていたら恒等に落とす (安全側)
    if ortho_error(&r, d) < ORTHO_TOL {
        r
    } else {
        identity(d)
    }
}

/// Gauss-Jordan (部分ピボット選択) による逆行列。特異なら `None`。
///
/// 反復のたびに桁落ちしないよう f64 で計算する (d ≤ 数千を想定)。
fn invert(a: &[f32], d: usize) -> Option<Vec<f32>> {
    let mut m: Vec<f64> = a.iter().map(|&x| x as f64).collect();
    let mut inv: Vec<f64> = {
        let mut v = vec![0.0f64; d * d];
        for i in 0..d {
            v[i * d + i] = 1.0;
        }
        v
    };
    for col in 0..d {
        // 部分ピボット選択
        let mut pivot = col;
        let mut best = m[col * d + col].abs();
        for row in col + 1..d {
            let v = m[row * d + col].abs();
            if v > best {
                best = v;
                pivot = row;
            }
        }
        if best < 1e-12 {
            return None; // 特異
        }
        if pivot != col {
            for j in 0..d {
                m.swap(col * d + j, pivot * d + j);
                inv.swap(col * d + j, pivot * d + j);
            }
        }
        // ピボット行を正規化
        let p = m[col * d + col];
        for j in 0..d {
            m[col * d + j] /= p;
            inv[col * d + j] /= p;
        }
        // 他行から消去
        for row in 0..d {
            if row == col {
                continue;
            }
            let f = m[row * d + col];
            if f == 0.0 {
                continue;
            }
            for j in 0..d {
                m[row * d + j] -= f * m[col * d + j];
                inv[row * d + j] -= f * inv[col * d + j];
            }
        }
    }
    Some(inv.into_iter().map(|x| x as f32).collect())
}

/// `‖AᵀA − I‖_max` (直交性のずれ)。
fn ortho_error(a: &[f32], d: usize) -> f32 {
    let at = transpose(a, d);
    let p = matmul(&at, a, d);
    let mut err = 0.0f32;
    for i in 0..d {
        for j in 0..d {
            let target = if i == j { 1.0 } else { 0.0 };
            err = err.max((p[i * d + j] - target).abs());
        }
    }
    err
}

/// 行列の直交極因子 `polar(M) = U Vᵀ` (`M = U S Vᵀ`) を Newton 反復で求める。
///
/// `Q ← ½(Q + Q⁻ᵀ)` は二次収束する。特異で逆行列が取れない場合は対角に微小な
/// リッジを足して 1 度だけ再試行し、それでも駄目なら `None`。
fn polar(m: &[f32], d: usize) -> Option<Vec<f32>> {
    // 実データでも共分散が低ランクになることはある (次元より行数が少ない、
    // 重複が多いなど)。逆行列が取れないたびにリッジを強めて粘る
    let scale = m.iter().map(|x| x.abs()).sum::<f32>() / (d * d) as f32;
    let base_ridge = if scale > 0.0 { scale * 1e-3 } else { 1e-6 };
    let mut ridge = base_ridge;
    let mut q = m.to_vec();

    let invert_or_ridge = |q: &mut Vec<f32>, ridge: &mut f32| -> Option<Vec<f32>> {
        for _ in 0..8 {
            if let Some(inv) = invert(q, d) {
                return Some(inv);
            }
            for i in 0..d {
                q[i * d + i] += *ridge;
            }
            *ridge *= 10.0;
        }
        None
    };

    for _ in 0..POLAR_MAX_ITER {
        let inv = invert_or_ridge(&mut q, &mut ridge)?;
        let inv_t = transpose(&inv, d);
        // Higham のスケーリング γ = (‖Q⁻¹‖_F / ‖Q‖_F)^½。
        // 特異値のばらつきが大きい行列 (実データの ŶᵀX は成分が 1e3〜1e4)
        // では、これが無いと 20 反復では直交まで収束しない
        let norm_q = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_inv = inv.iter().map(|x| x * x).sum::<f32>().sqrt();
        let gamma = if norm_q > 0.0 && norm_inv > 0.0 {
            (norm_inv / norm_q).sqrt()
        } else {
            1.0
        };
        let mut next = vec![0.0f32; d * d];
        let mut delta = 0.0f32;
        for i in 0..d * d {
            next[i] = 0.5 * (gamma * q[i] + inv_t[i] / gamma);
            delta = delta.max((next[i] - q[i]).abs());
        }
        q = next;
        if delta < 1e-6 {
            break;
        }
    }
    (ortho_error(&q, d) < ORTHO_TOL).then_some(q)
}

// ---------------------------------------------------------------------------
// OpqRotation
// ---------------------------------------------------------------------------

/// 学習済みの直交回転行列。`x' = R x` (R は row-major の d×d)。
#[derive(Debug, Clone, PartialEq)]
pub struct OpqRotation {
    dim: usize,
    r: Vec<f32>,
}

impl OpqRotation {
    /// 決定的な乱数直交行列 (ベンチで「次元順に意味のないデータ」を作るため)。
    /// 学習には使わない (`train` が内部で候補として使う)。
    pub fn random_for_bench(dim: usize, seed: u64) -> Self {
        Self {
            dim,
            r: random_orthogonal(dim, seed),
        }
    }

    /// 恒等回転 (実質 PQ と同じ挙動)。
    pub fn identity(dim: usize) -> Self {
        Self {
            dim,
            r: identity(dim),
        }
    }

    /// 次元。
    #[inline]
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// 行列本体 (直列化用、row-major d×d)。
    #[inline]
    pub fn matrix(&self) -> &[f32] {
        &self.r
    }

    /// 直列化された行列から復元する (mmap ロード用、todo 1103)。
    /// サイズと直交性を検証する。
    pub fn from_matrix(dim: usize, r: Vec<f32>) -> Result<Self> {
        if r.len() != dim * dim {
            return Err(HamaneError::Corrupted(format!(
                "opq matrix size mismatch: expected {}, got {}",
                dim * dim,
                r.len()
            )));
        }
        let err = ortho_error(&r, dim);
        if !err.is_finite() || err >= ORTHO_TOL {
            return Err(HamaneError::Corrupted(format!(
                "opq matrix is not orthogonal (max |RᵀR − I| = {err})"
            )));
        }
        Ok(Self { dim, r })
    }

    /// `out = R x` (out は dim 要素に上書きされる)。
    pub fn apply_into(&self, x: &[f32], out: &mut [f32]) {
        debug_assert_eq!(x.len(), self.dim);
        debug_assert_eq!(out.len(), self.dim);
        for (i, o) in out.iter_mut().enumerate() {
            let row = &self.r[i * self.dim..(i + 1) * self.dim];
            let mut sum = 0.0f32;
            for (rj, xj) in row.iter().zip(x) {
                sum += rj * xj;
            }
            *o = sum;
        }
    }

    /// `R x` を新しい `Vec` で返す。
    pub fn apply(&self, x: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; self.dim];
        self.apply_into(x, &mut out);
        out
    }

    /// 回転行列を学習する (交互最適化、docs/design/opq.md §1.1)。
    ///
    /// 初期値によって落ちる局所最適が大きく変わるため、**候補を実際に測って
    /// 一番良いものを採る**:
    ///
    /// 1. 恒等行列から交互最適化 (自然な次元順に意味があるデータ向け。
    ///    SIFT のようにサブベクトルが元から相関しているとこれが最良)
    /// 2. パラメトリック初期値 (PCA + 固有値割り当て、todo 1105) から
    ///    交互最適化。恒等が停留点になる軸に沿った分布で効く
    ///
    /// 最後に「回転なし (= 素の PQ)」の誤差とも比べ、勝てなければ恒等行列を
    /// 返す。**OPQ を有効にして PQ より悪くなることはない**。
    pub fn train(vectors: &[&[f32]], dim: usize, m: usize, nbits: u8, seed: u64) -> Result<Self> {
        if m == 0 || !dim.is_multiple_of(m) {
            return Err(HamaneError::InvalidConfig(format!(
                "opq requires dim ({dim}) divisible by m ({m})"
            )));
        }
        if dim == 0 || vectors.is_empty() {
            return Ok(Self::identity(dim));
        }
        let sample = crate::pq::subsample(vectors, OPQ_TRAIN_SAMPLE);

        // 回転なしの誤差 (これに勝てなければ採用しない)
        let mut best_r = identity(dim);
        let mut best_err = Self::sample_error(&sample, dim, m, nbits, seed, &best_r)?;

        let no_rotation_err = best_err;

        // 1. 恒等初期値から交互最適化
        let r = Self::optimize(&sample, dim, m, nbits, seed, identity(dim))?;
        let err = Self::sample_error(&sample, dim, m, nbits, seed, &r)?;
        if err < best_err {
            best_err = err;
            best_r = r;
        }

        // 2. 恒等が目立って改善しなければ (停留点の疑い)、パラメトリック
        //    初期値からも試す。PCA + 固有値割り当てで分散を均した出発点
        if best_err > no_rotation_err * 0.99 {
            let init = parametric_init(&sample, dim, m);
            let r = Self::optimize(&sample, dim, m, nbits, seed, init)?;
            let err = Self::sample_error(&sample, dim, m, nbits, seed, &r)?;
            if err < best_err {
                best_r = r;
            }
        }

        let rot = Self { dim, r: best_r };
        // 数値誤差で直交性が崩れていたら恒等行列に落とす (安全側)
        if ortho_error(&rot.r, dim) < ORTHO_TOL {
            Ok(rot)
        } else {
            Ok(Self::identity(dim))
        }
    }

    /// 交互最適化を `OPQ_ITERS` 回まわして回転行列を返す。
    /// 極分解に失敗した反復は捨てて直前の `R` を返す (学習は失敗させない)。
    #[allow(clippy::too_many_arguments)]
    fn optimize(
        sample: &[&[f32]],
        dim: usize,
        m: usize,
        nbits: u8,
        seed: u64,
        init: Vec<f32>,
    ) -> Result<Vec<f32>> {
        let n = sample.len();
        let mut r = init;
        let mut rotated = vec![vec![0.0f32; dim]; n];
        let mut recon = vec![vec![0.0f32; dim]; n];
        let mut code = Vec::with_capacity(m);

        for it in 0..OPQ_ITERS {
            // 1. 現在の R でデータを回転する
            let cur = Self { dim, r };
            for (dst, src) in rotated.iter_mut().zip(sample) {
                cur.apply_into(src, dst);
            }
            r = cur.r;

            // 2. 回転後空間でコードブックを学習し、符号化 → 復元する
            let rot_slices: Vec<&[f32]> = rotated.iter().map(|v| v.as_slice()).collect();
            let cb = PqCodebook::train(
                &rot_slices,
                dim,
                m,
                nbits,
                seed ^ (it as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
                OPQ_TRAIN_SAMPLE,
                OPQ_INNER_ITER,
            )?;
            for (dst, src) in recon.iter_mut().zip(&rotated) {
                code.clear();
                cb.encode(src, &mut code);
                cb.decode_into(&code, dst);
            }

            // 3. 直交 Procrustes: M = Ŷᵀ X の極因子が次の R
            //    (Ŷ は復元した回転後ベクトル、X は元ベクトル)
            let mut mmat = vec![0.0f32; dim * dim];
            for (y, x) in recon.iter().zip(sample) {
                for i in 0..dim {
                    let yi = y[i];
                    if yi == 0.0 {
                        continue;
                    }
                    let row = &mut mmat[i * dim..(i + 1) * dim];
                    for (acc, xj) in row.iter_mut().zip(x.iter()) {
                        *acc += yi * xj;
                    }
                }
            }
            match polar(&mmat, dim) {
                Some(next) => r = next,
                // 特異・非収束はこの反復を捨てて直前の R を維持する
                None => break,
            }
        }
        Ok(r)
    }

    /// 回転 `r` を掛けたサンプルの PQ 再構成誤差 (候補比較用)。
    /// 反復中と同じ `OPQ_INNER_ITER` で学習して条件を揃える。
    fn sample_error(
        sample: &[&[f32]],
        dim: usize,
        m: usize,
        nbits: u8,
        seed: u64,
        r: &[f32],
    ) -> Result<f64> {
        let rot = Self { dim, r: r.to_vec() };
        let rotated: Vec<Vec<f32>> = sample.iter().map(|v| rot.apply(v)).collect();
        let slices: Vec<&[f32]> = rotated.iter().map(|v| v.as_slice()).collect();
        let cb = PqCodebook::train(
            &slices,
            dim,
            m,
            nbits,
            seed,
            OPQ_TRAIN_SAMPLE,
            OPQ_INNER_ITER,
        )?;
        let mut code = Vec::with_capacity(m);
        let mut recon = vec![0.0f32; dim];
        let mut sum = 0.0f64;
        for v in &rotated {
            code.clear();
            cb.encode(v, &mut code);
            cb.decode_into(&code, &mut recon);
            sum += crate::metric::l2_squared_scalar(v, &recon) as f64;
        }
        Ok(sum / rotated.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::l2_squared_scalar;
    use crate::pq::DEFAULT_MAX_ITER;
    use crate::pq::NBITS;

    /// 決定的な擬似乱数 (テスト用の簡易 LCG)。
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
        }
        fn unit(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f32) / ((1u64 << 53) as f32)
        }
        /// 平均 0・分散 1 に近い値 (12 個の一様乱数の和 − 6)。
        fn normal(&mut self) -> f32 {
            (0..12).map(|_| self.unit()).sum::<f32>() - 6.0
        }
    }

    /// OPQ が効くデータ: **次元ごとの分散が大きく偏った**フルランクのガウス。
    ///
    /// 前半の次元に分散が集中しているため、位置で切る PQ は前半のサブベクトル
    /// にだけ誤差が集中する (256 セントロイドの割り当てが不公平)。OPQ の回転は
    /// この偏りをならすので誤差が下がる。
    fn variance_skewed(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut rng = Rng::new(seed);
        // 分散を次元順に等比で落とす (先頭 1.0 → 末尾 1/64)
        let scale: Vec<f32> = (0..dim)
            .map(|d| 64.0f32.powf(-(d as f32) / (dim - 1).max(1) as f32))
            .collect();
        (0..n)
            .map(|_| (0..dim).map(|d| rng.normal() * scale[d]).collect())
            .collect()
    }

    fn as_slices(v: &[Vec<f32>]) -> Vec<&[f32]> {
        v.iter().map(|x| x.as_slice()).collect()
    }

    /// 与えたベクトル集合を PQ 符号化したときの平均二乗誤差。
    fn pq_mse(data: &[Vec<f32>], dim: usize, m: usize, seed: u64) -> f64 {
        let slices = as_slices(data);
        let cb = PqCodebook::train(&slices, dim, m, NBITS, seed, 65536, DEFAULT_MAX_ITER).unwrap();
        let mut code = Vec::new();
        let mut recon = vec![0.0f32; dim];
        let mut sum = 0.0f64;
        for v in data {
            code.clear();
            cb.encode(v, &mut code);
            cb.decode_into(&code, &mut recon);
            sum += l2_squared_scalar(v, &recon) as f64;
        }
        sum / data.len() as f64
    }

    #[test]
    fn identity_applies_unchanged() {
        let r = OpqRotation::identity(4);
        let x = [1.0f32, -2.0, 3.5, 0.0];
        assert_eq!(r.apply(&x), x.to_vec());
    }

    #[test]
    fn trained_rotation_is_orthogonal() {
        let data = variance_skewed(500, 8, 11);
        let slices = as_slices(&data);
        let r = OpqRotation::train(&slices, 8, 2, NBITS, 1).unwrap();
        assert!(
            ortho_error(r.matrix(), 8) < ORTHO_TOL,
            "‖RᵀR − I‖ = {}",
            ortho_error(r.matrix(), 8)
        );
        // 直交変換は L2 距離を保存する
        let a = r.apply(&data[0]);
        let b = r.apply(&data[1]);
        let before = l2_squared_scalar(&data[0], &data[1]);
        let after = l2_squared_scalar(&a, &b);
        assert!(
            (before - after).abs() / before.max(1e-6) < 1e-3,
            "distance changed: {before} → {after}"
        );
    }

    #[test]
    fn opq_reduces_quantization_error() {
        // 分散が偏り相関のあるデータでは、回転してから PQ する方が誤差が小さい
        const N: usize = 600;
        const DIM: usize = 8;
        const M: usize = 2;
        let data = variance_skewed(N, DIM, 7);
        let slices = as_slices(&data);

        let raw = pq_mse(&data, DIM, M, 3);

        let r = OpqRotation::train(&slices, DIM, M, NBITS, 3).unwrap();
        let rotated: Vec<Vec<f32>> = data.iter().map(|v| r.apply(v)).collect();
        let opq = pq_mse(&rotated, DIM, M, 3);

        assert!(
            opq < raw,
            "opq mse {opq} should be below plain pq mse {raw}"
        );
    }

    #[test]
    fn train_is_deterministic() {
        let data = variance_skewed(200, 8, 5);
        let slices = as_slices(&data);
        let a = OpqRotation::train(&slices, 8, 2, NBITS, 42).unwrap();
        let b = OpqRotation::train(&slices, 8, 2, NBITS, 42).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn degenerate_inputs_fall_back_to_identity() {
        // 全点同一 → 共分散が特異。panic せず直交行列を返す
        let data: Vec<Vec<f32>> = (0..50).map(|_| vec![1.0f32; 8]).collect();
        let slices = as_slices(&data);
        let r = OpqRotation::train(&slices, 8, 2, NBITS, 0).unwrap();
        assert!(ortho_error(r.matrix(), 8) < ORTHO_TOL);

        // 空入力 / 点数 < ksub でも恒等行列
        let empty: Vec<&[f32]> = Vec::new();
        assert_eq!(
            OpqRotation::train(&empty, 8, 2, NBITS, 0).unwrap(),
            OpqRotation::identity(8)
        );
    }

    #[test]
    fn train_rejects_indivisible_m() {
        let data = variance_skewed(50, 9, 1);
        let slices = as_slices(&data);
        assert!(OpqRotation::train(&slices, 9, 2, NBITS, 0).is_err());
    }

    #[test]
    fn from_matrix_validates() {
        let r = OpqRotation::identity(3);
        assert!(OpqRotation::from_matrix(3, r.matrix().to_vec()).is_ok());
        // サイズ不一致
        assert!(OpqRotation::from_matrix(3, vec![0.0; 8]).is_err());
        // 非直交
        let mut bad = r.matrix().to_vec();
        bad[0] = 2.0;
        assert!(OpqRotation::from_matrix(3, bad).is_err());
    }

    #[test]
    fn polar_of_orthogonal_is_itself() {
        // 平面回転 (直交) の極因子は自分自身
        let (c, s) = (0.6f32, 0.8f32);
        let m = vec![c, -s, s, c];
        let p = polar(&m, 2).unwrap();
        for (a, b) in p.iter().zip(&m) {
            assert!((a - b).abs() < 1e-4, "{p:?} != {m:?}");
        }
    }

    #[test]
    fn invert_detects_singular() {
        // 2 行が同一 → 特異
        let m = vec![1.0f32, 2.0, 1.0, 2.0];
        assert!(invert(&m, 2).is_none());
        let m = vec![2.0f32, 0.0, 0.0, 4.0];
        let inv = invert(&m, 2).unwrap();
        assert!((inv[0] - 0.5).abs() < 1e-6 && (inv[3] - 0.25).abs() < 1e-6);
    }

    /// SIFT のように**サブベクトルが元から相関している**データでは、素の PQ の
    /// 位置分割が既に良い。OPQ はそこで悪化してはいけない (候補比較で担保)。
    #[test]
    fn opq_is_not_worse_on_block_correlated_data() {
        const N: usize = 1500;
        const DIM: usize = 16;
        const M: usize = 4;
        // 4 次元ブロックごとに共通成分を持つ = 位置分割と相性の良いデータ
        let mut rng = Rng::new(3);
        let data: Vec<Vec<f32>> = (0..N)
            .map(|_| {
                let mut v = vec![0.0f32; DIM];
                for b in 0..DIM / 4 {
                    let base = rng.normal() * (1.0 + b as f32);
                    for j in 0..4 {
                        v[b * 4 + j] = base + rng.normal() * 0.3;
                    }
                }
                v
            })
            .collect();
        let slices = as_slices(&data);

        let plain = pq_mse(&data, DIM, M, 3);
        let r = OpqRotation::train(&slices, DIM, M, NBITS, 3).unwrap();
        let rotated: Vec<Vec<f32>> = data.iter().map(|v| r.apply(v)).collect();
        let opq = pq_mse(&rotated, DIM, M, 3);

        // 学習は 16384 行サンプル・少ない k-means 反復で選ぶので厳密な単調性は
        // 保証できない。悪化しても数 % 以内に収まること
        assert!(
            opq <= plain * 1.05,
            "opq mse {opq} must not regress vs plain pq {plain}"
        );
    }

    #[test]
    fn jacobi_eigen_matches_known_matrix() {
        // 対角行列: 固有値はそのまま
        let d = 3;
        let m = vec![3.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 2.0];
        let (mut vals, vecs) = jacobi_eigen(&m, d);
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for (got, want) in vals.iter().zip([1.0, 2.0, 3.0]) {
            assert!((got - want).abs() < 1e-6, "{vals:?}");
        }
        // 固有ベクトル行列は直交
        let v: Vec<f32> = vecs.iter().map(|&x| x as f32).collect();
        assert!(ortho_error(&v, d) < ORTHO_TOL);

        // 非対角あり: 2x2 の [[2,1],[1,2]] は固有値 1, 3
        let (mut vals, vecs) = jacobi_eigen(&[2.0, 1.0, 1.0, 2.0], 2);
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((vals[0] - 1.0).abs() < 1e-6 && (vals[1] - 3.0).abs() < 1e-6);
        let v: Vec<f32> = vecs.iter().map(|&x| x as f32).collect();
        assert!(ortho_error(&v, 2) < ORTHO_TOL);
    }

    #[test]
    fn parametric_init_balances_variance() {
        // 軸に沿って分散が大きく偏ったデータ (恒等が停留点になるケース)
        const DIM: usize = 8;
        const M: usize = 2;
        let data = variance_skewed(600, DIM, 21);
        let slices = as_slices(&data);
        let r = parametric_init(&slices, DIM, M);
        assert!(ortho_error(&r, DIM) < ORTHO_TOL, "init must be orthogonal");

        // 回転後はサブベクトルごとの分散の幾何平均が近づく
        let rot = OpqRotation {
            dim: DIM,
            r: r.clone(),
        };
        let rotated: Vec<Vec<f32>> = data.iter().map(|v| rot.apply(v)).collect();
        let geo_mean = |from: usize| -> f64 {
            let dsub = DIM / M;
            let mut log_sum = 0.0;
            for j in from..from + dsub {
                let mean: f64 =
                    rotated.iter().map(|v| v[j] as f64).sum::<f64>() / rotated.len() as f64;
                let var: f64 = rotated
                    .iter()
                    .map(|v| (v[j] as f64 - mean).powi(2))
                    .sum::<f64>()
                    / rotated.len() as f64;
                log_sum += var.max(1e-12).ln();
            }
            (log_sum / (DIM / M) as f64).exp()
        };
        let (a, b) = (geo_mean(0), geo_mean(DIM / M));
        let ratio = a.max(b) / a.min(b);
        assert!(
            ratio < 2.0,
            "variance geo-means must be balanced: {a} vs {b}"
        );

        // 回転なしの偏り (元データ) より均等であること
        let raw_geo = |from: usize| -> f64 {
            let dsub = DIM / M;
            let mut log_sum = 0.0;
            for j in from..from + dsub {
                let mean: f64 = data.iter().map(|v| v[j] as f64).sum::<f64>() / data.len() as f64;
                let var: f64 = data
                    .iter()
                    .map(|v| (v[j] as f64 - mean).powi(2))
                    .sum::<f64>()
                    / data.len() as f64;
                log_sum += var.max(1e-12).ln();
            }
            (log_sum / dsub as f64).exp()
        };
        let (ra, rb) = (raw_geo(0), raw_geo(DIM / M));
        assert!(
            ratio < ra.max(rb) / ra.min(rb),
            "rotation must reduce the imbalance ({ratio} vs {})",
            ra.max(rb) / ra.min(rb)
        );
    }

    /// 極分解が実データ規模 (成分が 1e3〜1e4) の行列でも直交行列に収束する。
    /// スケーリングが無いと収束せず `None` になり、学習が 1 歩も進まない。
    #[test]
    fn polar_converges_on_large_scale_matrix() {
        let d = 8;
        let mut rng = Rng::new(31);
        // 特異値の散らばりが大きい行列 (対角スケール × 乱数)
        let scale: Vec<f32> = (0..d).map(|i| 10f32.powi(i as i32 % 4)).collect();
        let m: Vec<f32> = (0..d * d)
            .map(|idx| rng.normal() * 1000.0 * scale[idx / d])
            .collect();
        let p = polar(&m, d).expect("polar must converge");
        assert!(ortho_error(&p, d) < ORTHO_TOL);
    }
}
