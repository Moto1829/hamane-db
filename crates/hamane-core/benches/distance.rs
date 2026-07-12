//! 距離カーネルのベンチマーク (todos/402)。
//! SIMD ディスパッチ版とスカラー版を次元別に比較する。
//!
//! 実行: `cargo bench -p hamane-core`

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use hamane_core::{dot, dot_scalar, l2_squared, l2_squared_scalar};
use std::hint::black_box;

fn vectors(dim: usize) -> (Vec<f32>, Vec<f32>) {
    let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.37).sin()).collect();
    let b: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.73).cos()).collect();
    (a, b)
}

fn bench_distance(c: &mut Criterion) {
    for dim in [64usize, 128, 768, 1536] {
        let (a, b) = vectors(dim);
        let mut group = c.benchmark_group(format!("dim{dim}"));
        group.bench_function(BenchmarkId::new("l2_simd", dim), |bencher| {
            bencher.iter(|| l2_squared(black_box(&a), black_box(&b)))
        });
        group.bench_function(BenchmarkId::new("l2_scalar", dim), |bencher| {
            bencher.iter(|| l2_squared_scalar(black_box(&a), black_box(&b)))
        });
        group.bench_function(BenchmarkId::new("dot_simd", dim), |bencher| {
            bencher.iter(|| dot(black_box(&a), black_box(&b)))
        });
        group.bench_function(BenchmarkId::new("dot_scalar", dim), |bencher| {
            bencher.iter(|| dot_scalar(black_box(&a), black_box(&b)))
        });
        group.finish();
    }
}

criterion_group!(benches, bench_distance);
criterion_main!(benches);
