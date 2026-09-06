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
const POLAR_MAX_ITER: usize = 20;
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
        let mut next = vec![0.0f32; d * d];
        let mut delta = 0.0f32;
        for i in 0..d * d {
            next[i] = 0.5 * (q[i] + inv_t[i]);
            delta = delta.max((next[i] - q[i]).abs());
        }
        q = next;
        if delta < 1e-7 {
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
    /// - `m` は PQ のサブベクトル数 (`dim % m == 0` が必要)
    /// - 学習は `OPQ_TRAIN_SAMPLE` 行の等間隔サンプルで行う
    /// - 極分解に失敗した反復は捨てて直前の `R` を維持する。1 度も更新できな
    ///   ければ恒等行列を返す (学習は失敗させない)
    pub fn train(vectors: &[&[f32]], dim: usize, m: usize, seed: u64) -> Result<Self> {
        if m == 0 || !dim.is_multiple_of(m) {
            return Err(HamaneError::InvalidConfig(format!(
                "opq requires dim ({dim}) divisible by m ({m})"
            )));
        }
        if dim == 0 || vectors.is_empty() {
            return Ok(Self::identity(dim));
        }
        let sample = crate::pq::subsample(vectors, OPQ_TRAIN_SAMPLE);
        let n = sample.len();

        // 乱数直交行列で初期化する (恒等は局所最適なので動かない)
        let mut r = random_orthogonal(dim, seed);
        let mut rotated = vec![vec![0.0f32; dim]; n];
        let mut recon = vec![vec![0.0f32; dim]; n];
        let mut code = Vec::with_capacity(m);

        for it in 0..OPQ_ITERS {
            // 1. 現在の R でデータを回転する
            let cur = Self { dim, r };
            for (dst, src) in rotated.iter_mut().zip(&sample) {
                cur.apply_into(src, dst);
            }
            r = cur.r;

            // 2. 回転後空間でコードブックを学習し、符号化 → 復元する
            let rot_slices: Vec<&[f32]> = rotated.iter().map(|v| v.as_slice()).collect();
            let cb = PqCodebook::train(
                &rot_slices,
                dim,
                m,
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
            for (y, x) in recon.iter().zip(&sample) {
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

        let rot = Self { dim, r };
        // 数値誤差で直交性が崩れていたら恒等行列に落とす (安全側)
        if ortho_error(&rot.r, dim) < ORTHO_TOL {
            Ok(rot)
        } else {
            Ok(Self::identity(dim))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::l2_squared_scalar;
    use crate::pq::DEFAULT_MAX_ITER;

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
        let cb = PqCodebook::train(&slices, dim, m, seed, 65536, DEFAULT_MAX_ITER).unwrap();
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
        let r = OpqRotation::train(&slices, 8, 2, 1).unwrap();
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

        let r = OpqRotation::train(&slices, DIM, M, 3).unwrap();
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
        let a = OpqRotation::train(&slices, 8, 2, 42).unwrap();
        let b = OpqRotation::train(&slices, 8, 2, 42).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn degenerate_inputs_fall_back_to_identity() {
        // 全点同一 → 共分散が特異。panic せず直交行列を返す
        let data: Vec<Vec<f32>> = (0..50).map(|_| vec![1.0f32; 8]).collect();
        let slices = as_slices(&data);
        let r = OpqRotation::train(&slices, 8, 2, 0).unwrap();
        assert!(ortho_error(r.matrix(), 8) < ORTHO_TOL);

        // 空入力 / 点数 < ksub でも恒等行列
        let empty: Vec<&[f32]> = Vec::new();
        assert_eq!(
            OpqRotation::train(&empty, 8, 2, 0).unwrap(),
            OpqRotation::identity(8)
        );
    }

    #[test]
    fn train_rejects_indivisible_m() {
        let data = variance_skewed(50, 9, 1);
        let slices = as_slices(&data);
        assert!(OpqRotation::train(&slices, 9, 2, 0).is_err());
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
}
