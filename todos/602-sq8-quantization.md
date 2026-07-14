# 602: スカラー量子化 (SQ8)

- Status: DONE (2026-07-14)
- Milestone: M6
- Depends: 307 (計測基盤)
- Design: DESIGN.md §4 将来拡張, docs/design/index.md §1

## ゴール

f32 ベクトルを次元ごとの min/max で u8 に量子化し、ディスク/メモリを
1/4 にしつつ、f32 での再ランキングで精度を保つ。

## やること

- [ ] セグメントに `vectors_sq8.bin` を追加 (ヘッダに次元ごとの min/max、
      本体は count × dim × u8)。元の vectors.bin も残す (再ランク・再構築用)
- [ ] u8 同士の距離カーネル (L2/dot) を hamane-core に追加 (SIMD:
      NEON `vdotq` 系 / AVX2 `maddubs` 系。まずスカラーで正しく)
- [ ] HNSW 探索を SQ8 距離で行い、上位 `k × rerank_factor` (既定 4) を
      f32 で再ランクして top-k を返す 2 段階検索
- [ ] `CollectionConfig` ではなく `StoreOptions.quantization: Option<Sq8Config>`
      で有効化 (既定 off。フォーマット互換を保つ)
- [ ] SIFT1M で recall / QPS / ディスクサイズを on/off 比較して
      docs/benchmarks.md に記録

## 完了条件

- SQ8 on で recall@10 ≥ 0.95 を維持 (再ランクあり)
- ベクトル部分のディスク約 1/4、検索スループット向上を計測で確認
- off の場合の既存動作・フォーマットに一切影響なし (全テスト green)
