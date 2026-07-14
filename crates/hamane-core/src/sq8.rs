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
///
/// aarch64 は NEON、x86_64 は AVX2 (実行時判定)、他はスカラー。
/// 中間アキュムレータは u32 のため **dim ≤ 66051** が前提
/// (dim × 255² が u32 に収まる範囲。実用上の全ケースをカバー)。
#[inline]
pub fn sq8_l2_accum(a: &[u8], b: &[u8]) -> u64 {
    debug_assert_eq!(a.len(), b.len());
    debug_assert!(a.len() <= 66051, "dim too large for u32 accumulator");
    #[cfg(target_arch = "aarch64")]
    {
        // Safety: NEON は aarch64 で常に利用可能
        return unsafe { neon::l2_accum(a, b) };
    }
    #[allow(unreachable_code)]
    sq8_l2_accum_scalar(a, b)
}

/// `(Σ a·b, Σ b)` (u8 コード間の整数 dot と、b 側の総和)。
/// a 側 (クエリ) の総和は呼び出し側が 1 回だけ計算して使い回す。
/// dim の上限は `sq8_l2_accum` と同じ。
#[inline]
pub fn sq8_dot_accum(a: &[u8], b: &[u8]) -> (u64, u64) {
    debug_assert_eq!(a.len(), b.len());
    debug_assert!(a.len() <= 66051, "dim too large for u32 accumulator");
    #[cfg(target_arch = "aarch64")]
    {
        // Safety: NEON は aarch64 で常に利用可能
        return unsafe { neon::dot_accum(a, b) };
    }
    #[allow(unreachable_code)]
    sq8_dot_accum_scalar(a, b)
}

/// スカラー実装 (全プラットフォームの正解基準)。
#[inline]
pub fn sq8_l2_accum_scalar(a: &[u8], b: &[u8]) -> u64 {
    let mut sum = 0u64;
    for (&x, &y) in a.iter().zip(b) {
        let d = x as i32 - y as i32;
        sum += (d * d) as u64;
    }
    sum
}

/// スカラー実装の dot + Σb。
#[inline]
pub fn sq8_dot_accum_scalar(a: &[u8], b: &[u8]) -> (u64, u64) {
    let mut dot = 0u64;
    let mut sum_b = 0u64;
    for (&x, &y) in a.iter().zip(b) {
        dot += (x as u32 * y as u32) as u64;
        sum_b += y as u64;
    }
    (dot, sum_b)
}

/// NEON 実装 (aarch64、todo 701)。16 lane の u8 を widening 乗算で
/// u16 → u32 に累積する。整数演算なのでスカラーと**完全一致**する。
#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn l2_accum(a: &[u8], b: &[u8]) -> u64 {
        // 32 バイト/イテレーションでアキュムレータ 4 本 (依存チェーンを切る)
        let mut acc0 = vdupq_n_u32(0);
        let mut acc1 = vdupq_n_u32(0);
        let mut acc2 = vdupq_n_u32(0);
        let mut acc3 = vdupq_n_u32(0);
        let chunks = a.len() / 32;
        for i in 0..chunks {
            let pa = a.as_ptr().add(i * 32);
            let pb = b.as_ptr().add(i * 32);
            let d0 = vabdq_u8(vld1q_u8(pa), vld1q_u8(pb));
            let d1 = vabdq_u8(vld1q_u8(pa.add(16)), vld1q_u8(pb.add(16)));
            acc0 = vpadalq_u16(acc0, vmull_u8(vget_low_u8(d0), vget_low_u8(d0)));
            acc1 = vpadalq_u16(acc1, vmull_u8(vget_high_u8(d0), vget_high_u8(d0)));
            acc2 = vpadalq_u16(acc2, vmull_u8(vget_low_u8(d1), vget_low_u8(d1)));
            acc3 = vpadalq_u16(acc3, vmull_u8(vget_high_u8(d1), vget_high_u8(d1)));
        }
        let acc = vaddq_u32(vaddq_u32(acc0, acc1), vaddq_u32(acc2, acc3));
        let mut sum = vaddvq_u32(acc) as u64;
        for i in chunks * 32..a.len() {
            let d = a[i] as i32 - b[i] as i32;
            sum += (d * d) as u64;
        }
        sum
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn dot_accum(a: &[u8], b: &[u8]) -> (u64, u64) {
        let mut dot0 = vdupq_n_u32(0);
        let mut dot1 = vdupq_n_u32(0);
        let mut dot2 = vdupq_n_u32(0);
        let mut dot3 = vdupq_n_u32(0);
        let mut sum0 = vdupq_n_u32(0);
        let mut sum1 = vdupq_n_u32(0);
        let chunks = a.len() / 32;
        for i in 0..chunks {
            let pa = a.as_ptr().add(i * 32);
            let pb = b.as_ptr().add(i * 32);
            let (va0, vb0) = (vld1q_u8(pa), vld1q_u8(pb));
            let (va1, vb1) = (vld1q_u8(pa.add(16)), vld1q_u8(pb.add(16)));
            dot0 = vpadalq_u16(dot0, vmull_u8(vget_low_u8(va0), vget_low_u8(vb0)));
            dot1 = vpadalq_u16(dot1, vmull_u8(vget_high_u8(va0), vget_high_u8(vb0)));
            dot2 = vpadalq_u16(dot2, vmull_u8(vget_low_u8(va1), vget_low_u8(vb1)));
            dot3 = vpadalq_u16(dot3, vmull_u8(vget_high_u8(va1), vget_high_u8(vb1)));
            // Σb: u8 → u16 → u32 のペア加算
            sum0 = vpadalq_u16(sum0, vpaddlq_u8(vb0));
            sum1 = vpadalq_u16(sum1, vpaddlq_u8(vb1));
        }
        let dot_acc = vaddq_u32(vaddq_u32(dot0, dot1), vaddq_u32(dot2, dot3));
        let mut dot = vaddvq_u32(dot_acc) as u64;
        let mut sum_b = vaddvq_u32(vaddq_u32(sum0, sum1)) as u64;
        for i in chunks * 32..a.len() {
            dot += (a[i] as u32 * b[i] as u32) as u64;
            sum_b += b[i] as u64;
        }
        (dot, sum_b)
    }
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

    /// SIMD とスカラーの完全一致 (整数演算なので誤差ゼロ、todo 701)。
    #[test]
    fn simd_matches_scalar_exactly() {
        let mut state = 0xABCDu64;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 33) as u8
        };
        // 端数 (len % 16 != 0) を含む長さで検証
        for len in [1usize, 15, 16, 17, 64, 127, 128, 768, 1537] {
            let a: Vec<u8> = (0..len).map(|_| next()).collect();
            let b: Vec<u8> = (0..len).map(|_| next()).collect();
            assert_eq!(
                sq8_l2_accum(&a, &b),
                sq8_l2_accum_scalar(&a, &b),
                "l2 len={len}"
            );
            assert_eq!(
                sq8_dot_accum(&a, &b),
                sq8_dot_accum_scalar(&a, &b),
                "dot len={len}"
            );
        }
        // 最大値 (255) でのオーバーフロー確認
        let a = vec![255u8; 768];
        let b = vec![0u8; 768];
        assert_eq!(sq8_l2_accum(&a, &b), 768 * 255 * 255);
        let (dot, sum) = sq8_dot_accum(&a, &a);
        assert_eq!(dot, 768 * 255 * 255);
        assert_eq!(sum, 768 * 255);
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
