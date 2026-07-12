/// Collection 作成時に固定する距離関数。
///
/// コサインは挿入時にベクトルを正規化して内積に還元するため、
/// 実際の距離カーネルは L2 と内積の 2 種のみ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    /// ユークリッド距離 (小さいほど近い)
    L2,
    /// コサイン類似度 (大きいほど近い)。挿入時・検索時に正規化する
    Cosine,
    /// 内積 (大きいほど近い)
    Dot,
}

impl Metric {
    /// 「小さいほど近い」に統一した比較キーを返す。
    /// L2 は距離の 2 乗、Cosine/Dot は内積の符号反転。
    #[inline]
    pub fn distance_key(self, a: &[f32], b: &[f32]) -> f32 {
        match self {
            Metric::L2 => l2_squared(a, b),
            Metric::Cosine | Metric::Dot => -dot(a, b),
        }
    }

    /// 比較キーから利用者へ返すスコアに変換する。
    /// L2 はユークリッド距離、Cosine は類似度、Dot は内積そのもの。
    #[inline]
    pub fn score_from_key(self, key: f32) -> f32 {
        match self {
            Metric::L2 => key.sqrt(),
            Metric::Cosine | Metric::Dot => -key,
        }
    }

    /// score_from_key の逆変換。複数ソースの検索結果をマージする際に使う。
    #[inline]
    pub fn key_from_score(self, score: f32) -> f32 {
        match self {
            Metric::L2 => score * score,
            Metric::Cosine | Metric::Dot => -score,
        }
    }

    /// 挿入・検索時にベクトルの正規化が必要か。
    #[inline]
    pub fn requires_normalization(self) -> bool {
        matches!(self, Metric::Cosine)
    }
}

/// ユークリッド距離の 2 乗。
///
/// 4 本のアキュムレータに分けて依存チェーンを切り、自動ベクトル化を促す。
/// aarch64 は NEON 固定、x86_64 は AVX2+FMA を実行時判定 (キャッシュされ低コスト)、
/// それ以外はスカラー実装にフォールバックする。
#[inline]
pub fn l2_squared(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    #[cfg(target_arch = "aarch64")]
    {
        // Safety: NEON は aarch64 で常に利用可能
        return unsafe { neon::l2_squared(a, b) };
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            // Safety: 直前で AVX2+FMA の存在を確認済み
            return unsafe { avx2::l2_squared(a, b) };
        }
    }
    #[allow(unreachable_code)]
    l2_squared_scalar(a, b)
}

/// 内積。ディスパッチは `l2_squared` と同様。
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    #[cfg(target_arch = "aarch64")]
    {
        // Safety: NEON は aarch64 で常に利用可能
        return unsafe { neon::dot(a, b) };
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            // Safety: 直前で AVX2+FMA の存在を確認済み
            return unsafe { avx2::dot(a, b) };
        }
    }
    #[allow(unreachable_code)]
    dot_scalar(a, b)
}

/// スカラー実装 (全プラットフォームの正解基準。4 レーン展開で自動ベクトル化を促す)。
#[inline]
pub fn l2_squared_scalar(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = [0.0f32; 4];
    let chunks = a.len() / 4;
    for i in 0..chunks {
        let base = i * 4;
        for lane in 0..4 {
            let d = a[base + lane] - b[base + lane];
            acc[lane] += d * d;
        }
    }
    let mut sum = acc[0] + acc[1] + acc[2] + acc[3];
    for i in chunks * 4..a.len() {
        let d = a[i] - b[i];
        sum += d * d;
    }
    sum
}

/// スカラー実装の内積。
#[inline]
pub fn dot_scalar(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = [0.0f32; 4];
    let chunks = a.len() / 4;
    for i in 0..chunks {
        let base = i * 4;
        for lane in 0..4 {
            acc[lane] += a[base + lane] * b[base + lane];
        }
    }
    let mut sum = acc[0] + acc[1] + acc[2] + acc[3];
    for i in chunks * 4..a.len() {
        sum += a[i] * b[i];
    }
    sum
}

/// NEON 実装 (aarch64)。unsafe はこのモジュール内に閉じ込める。
#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        let mut acc0 = vdupq_n_f32(0.0);
        let mut acc1 = vdupq_n_f32(0.0);
        let chunks = a.len() / 8;
        for i in 0..chunks {
            let pa = a.as_ptr().add(i * 8);
            let pb = b.as_ptr().add(i * 8);
            acc0 = vfmaq_f32(acc0, vld1q_f32(pa), vld1q_f32(pb));
            acc1 = vfmaq_f32(acc1, vld1q_f32(pa.add(4)), vld1q_f32(pb.add(4)));
        }
        let mut sum = vaddvq_f32(acc0) + vaddvq_f32(acc1);
        for i in chunks * 8..a.len() {
            sum += a[i] * b[i];
        }
        sum
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn l2_squared(a: &[f32], b: &[f32]) -> f32 {
        let mut acc0 = vdupq_n_f32(0.0);
        let mut acc1 = vdupq_n_f32(0.0);
        let chunks = a.len() / 8;
        for i in 0..chunks {
            let pa = a.as_ptr().add(i * 8);
            let pb = b.as_ptr().add(i * 8);
            let d0 = vsubq_f32(vld1q_f32(pa), vld1q_f32(pb));
            let d1 = vsubq_f32(vld1q_f32(pa.add(4)), vld1q_f32(pb.add(4)));
            acc0 = vfmaq_f32(acc0, d0, d0);
            acc1 = vfmaq_f32(acc1, d1, d1);
        }
        let mut sum = vaddvq_f32(acc0) + vaddvq_f32(acc1);
        for i in chunks * 8..a.len() {
            let d = a[i] - b[i];
            sum += d * d;
        }
        sum
    }
}

/// AVX2+FMA 実装 (x86_64、実行時ディスパッチ)。
#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    #[inline]
    unsafe fn hsum(v: __m256) -> f32 {
        let hi = _mm256_extractf128_ps(v, 1);
        let lo = _mm256_castps256_ps128(v);
        let sum4 = _mm_add_ps(lo, hi);
        let sum2 = _mm_add_ps(sum4, _mm_movehl_ps(sum4, sum4));
        let sum1 = _mm_add_ss(sum2, _mm_shuffle_ps(sum2, sum2, 1));
        _mm_cvtss_f32(sum1)
    }

    #[inline]
    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        let mut acc = _mm256_setzero_ps();
        let chunks = a.len() / 8;
        for i in 0..chunks {
            let va = _mm256_loadu_ps(a.as_ptr().add(i * 8));
            let vb = _mm256_loadu_ps(b.as_ptr().add(i * 8));
            acc = _mm256_fmadd_ps(va, vb, acc);
        }
        let mut sum = hsum(acc);
        for i in chunks * 8..a.len() {
            sum += a[i] * b[i];
        }
        sum
    }

    #[inline]
    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn l2_squared(a: &[f32], b: &[f32]) -> f32 {
        let mut acc = _mm256_setzero_ps();
        let chunks = a.len() / 8;
        for i in 0..chunks {
            let va = _mm256_loadu_ps(a.as_ptr().add(i * 8));
            let vb = _mm256_loadu_ps(b.as_ptr().add(i * 8));
            let d = _mm256_sub_ps(va, vb);
            acc = _mm256_fmadd_ps(d, d, acc);
        }
        let mut sum = hsum(acc);
        for i in chunks * 8..a.len() {
            let d = a[i] - b[i];
            sum += d * d;
        }
        sum
    }
}

/// L2 ノルムが 1 になるよう正規化する。ゼロベクトル・非有限値は false を返す。
pub fn normalize(v: &mut [f32]) -> bool {
    let norm_sq = dot(v, v);
    if !norm_sq.is_finite() || norm_sq == 0.0 {
        return false;
    }
    let inv = norm_sq.sqrt().recip();
    for x in v.iter_mut() {
        *x *= inv;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "expected {b}, got {a}");
    }

    #[test]
    fn l2_squared_matches_naive() {
        // 端数処理 (len % 4 != 0) を含む長さで検証する
        for len in [1, 3, 4, 7, 16, 129] {
            let a: Vec<f32> = (0..len).map(|i| i as f32 * 0.5).collect();
            let b: Vec<f32> = (0..len).map(|i| (len - i) as f32 * 0.25).collect();
            let naive: f32 = a.iter().zip(&b).map(|(x, y)| (x - y) * (x - y)).sum();
            assert_close(l2_squared(&a, &b), naive);
        }
    }

    #[test]
    fn dot_matches_naive() {
        for len in [1, 3, 4, 7, 16, 129] {
            let a: Vec<f32> = (0..len).map(|i| (i as f32).sin()).collect();
            let b: Vec<f32> = (0..len).map(|i| (i as f32).cos()).collect();
            let naive: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
            assert_close(dot(&a, &b), naive);
        }
    }

    #[test]
    fn normalize_unit_norm() {
        let mut v = vec![3.0, 4.0];
        assert!(normalize(&mut v));
        assert_close(v[0], 0.6);
        assert_close(v[1], 0.8);
        assert_close(dot(&v, &v), 1.0);
    }

    #[test]
    fn normalize_rejects_zero_and_nan() {
        let mut zero = vec![0.0; 8];
        assert!(!normalize(&mut zero));
        let mut nan = vec![1.0, f32::NAN];
        assert!(!normalize(&mut nan));
    }

    #[test]
    fn simd_matches_scalar() {
        // 簡易 LCG による決定的な擬似乱数で SIMD とスカラーの一致を検証する
        // (相対誤差 1e-4。加算順が違うため完全一致はしない)
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / (1u64 << 31) as f32) - 1.0
        };
        for len in [1usize, 3, 7, 8, 9, 15, 16, 63, 64, 129, 768, 1537] {
            let a: Vec<f32> = (0..len).map(|_| next()).collect();
            let b: Vec<f32> = (0..len).map(|_| next()).collect();
            for (simd, scalar) in [
                (dot(&a, &b), dot_scalar(&a, &b)),
                (l2_squared(&a, &b), l2_squared_scalar(&a, &b)),
            ] {
                let tolerance = scalar.abs().max(1.0) * 1e-4;
                assert!(
                    (simd - scalar).abs() <= tolerance,
                    "len={len}: simd={simd}, scalar={scalar}"
                );
            }
        }
    }

    #[test]
    fn distance_key_ordering() {
        // 「小さいキー = より近い」が全メトリックで成り立つこと
        let q = vec![1.0, 0.0];
        let near = vec![0.9, 0.1];
        let far = vec![-1.0, 0.0];
        for metric in [Metric::L2, Metric::Cosine, Metric::Dot] {
            assert!(metric.distance_key(&q, &near) < metric.distance_key(&q, &far));
        }
    }

    #[test]
    fn score_roundtrip() {
        let a = vec![1.0, 2.0, 2.0];
        let b = vec![0.0, 0.0, 0.0];
        // L2: key は 2 乗距離、score は距離
        let key = Metric::L2.distance_key(&a, &b);
        assert_close(Metric::L2.score_from_key(key), 3.0);
        // Dot: score は内積そのもの
        let key = Metric::Dot.distance_key(&a, &a);
        assert_close(Metric::Dot.score_from_key(key), 9.0);
    }
}
