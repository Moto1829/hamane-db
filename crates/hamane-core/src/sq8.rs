//! スカラー量子化 (SQ8, todo 602)。
//!
//! f32 ベクトルを**全次元共通の min/max** (グローバルスケール) で u8 に量子化する。
//! 次元別スケールにしない理由: 距離計算が純粋な整数演算
//! (`Σ(qa-qb)²` / `Σqa·qb`) に還元でき、スケール補正が距離全体に対する
//! 定数倍・定数加算で済むため。
//!
//! 復元: `x ≈ min + q · s` (s = (max−min)/255)
//! - L2²: `s² · Σ(qa−qb)²`
//! - dot: `d·min² + min·s·(Σqa + Σqb) + s²·Σqa·qb`

/// 量子化パラメータ (全次元共通)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sq8Params {
    /// 全データの最小値
    pub min: f32,
    /// 全データの最大値
    pub max: f32,
}

impl Sq8Params {
    /// 量子化ステップ幅。min == max (全要素同値) でも 0 除算しない。
    #[inline]
    pub fn scale(&self) -> f32 {
        let s = (self.max - self.min) / 255.0;
        if s > 0.0 {
            s
        } else {
            1.0
        }
    }

    /// データ全体から min/max を求める。
    pub fn fit<'a>(vectors: impl Iterator<Item = &'a [f32]>) -> Self {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for v in vectors {
            for &x in v {
                min = min.min(x);
                max = max.max(x);
            }
        }
        if !min.is_finite() || !max.is_finite() {
            // 空データ。任意の有効値
            return Self { min: 0.0, max: 1.0 };
        }
        Self { min, max }
    }

    /// 1 本のベクトルを量子化する。
    pub fn quantize(&self, v: &[f32], out: &mut Vec<u8>) {
        let inv = 1.0 / self.scale();
        out.extend(
            v.iter()
                .map(|&x| ((x - self.min) * inv).round().clamp(0.0, 255.0) as u8),
        );
    }
}

/// `Σ (a−b)²` (u8 コード間の整数 L2²)。
#[inline]
pub fn sq8_l2_accum(a: &[u8], b: &[u8]) -> u64 {
    debug_assert_eq!(a.len(), b.len());
    let mut sum = 0u64;
    for (&x, &y) in a.iter().zip(b) {
        let d = x as i32 - y as i32;
        sum += (d * d) as u64;
    }
    sum
}

/// `(Σ a·b, Σ b)` (u8 コード間の整数 dot と、b 側の総和)。
/// a 側 (クエリ) の総和は呼び出し側が 1 回だけ計算して使い回す。
#[inline]
pub fn sq8_dot_accum(a: &[u8], b: &[u8]) -> (u64, u64) {
    debug_assert_eq!(a.len(), b.len());
    let mut dot = 0u64;
    let mut sum_b = 0u64;
    for (&x, &y) in a.iter().zip(b) {
        dot += (x as u32 * y as u32) as u64;
        sum_b += y as u64;
    }
    (dot, sum_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::{dot_scalar, l2_squared_scalar};

    fn quantize_all(params: &Sq8Params, vecs: &[Vec<f32>]) -> Vec<Vec<u8>> {
        vecs.iter()
            .map(|v| {
                let mut q = Vec::new();
                params.quantize(v, &mut q);
                q
            })
            .collect()
    }

    #[test]
    fn quantized_l2_approximates_f32() {
        // 決定的な擬似乱数
        let mut state = 12345u64;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((state >> 33) as f32 / (1u64 << 31) as f32) * 100.0 - 50.0
        };
        let vecs: Vec<Vec<f32>> = (0..20).map(|_| (0..64).map(|_| next()).collect()).collect();
        let params = Sq8Params::fit(vecs.iter().map(|v| v.as_slice()));
        let quantized = quantize_all(&params, &vecs);

        let s = params.scale();
        for i in 0..vecs.len() {
            for j in (i + 1)..vecs.len() {
                let exact = l2_squared_scalar(&vecs[i], &vecs[j]);
                let approx = s * s * sq8_l2_accum(&quantized[i], &quantized[j]) as f32;
                // 量子化誤差は次元あたり最大 s/2。相対 5% + 小さな絶対項まで許容
                let tolerance = exact * 0.05 + s * s * 64.0;
                assert!(
                    (exact - approx).abs() <= tolerance,
                    "L2² mismatch: exact={exact}, approx={approx}"
                );
            }
        }
    }

    #[test]
    fn quantized_dot_approximates_f32() {
        let mut state = 777u64;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
        };
        let vecs: Vec<Vec<f32>> = (0..20).map(|_| (0..64).map(|_| next()).collect()).collect();
        let params = Sq8Params::fit(vecs.iter().map(|v| v.as_slice()));
        let quantized = quantize_all(&params, &vecs);

        let s = params.scale();
        let d = 64.0f32;
        for i in 0..vecs.len() {
            let sum_a: u64 = quantized[i].iter().map(|&x| x as u64).sum();
            for j in 0..vecs.len() {
                let exact = dot_scalar(&vecs[i], &vecs[j]);
                let (dot_q, sum_b) = sq8_dot_accum(&quantized[i], &quantized[j]);
                let approx = d * params.min * params.min
                    + params.min * s * (sum_a + sum_b) as f32
                    + s * s * dot_q as f32;
                let tolerance = exact.abs() * 0.05 + s * d;
                assert!(
                    (exact - approx).abs() <= tolerance,
                    "dot mismatch: exact={exact}, approx={approx}"
                );
            }
        }
    }

    #[test]
    fn constant_vector_does_not_divide_by_zero() {
        let vecs = [vec![3.0f32; 8]];
        let params = Sq8Params::fit(vecs.iter().map(|v| v.as_slice()));
        let mut q = Vec::new();
        params.quantize(&vecs[0], &mut q);
        assert_eq!(q.len(), 8);
    }
}
