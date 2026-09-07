# 1201: 4-bit PQ サブコード (hamane-core)

- Status: DONE (2026-09-06)
- Milestone: M12
- Depends: 1002
- Design: docs/design/quantization.md §1.1, §8

## ゴール

`PqCodebook` を nbits でパラメータ化し、4-bit サブコード (k*=16、2 サブコード
= 1 バイト) を扱えるようにする。既定 8 の挙動は完全に据え置き。

## やること

- [x] `PqCodebook { m, dsub, nbits, centroids }` に nbits を持たせる
      (centroids は `m × 2^nbits × dsub`)
- [x] `train(..., nbits)` / `from_centroids(m, dsub, nbits, ...)`
- [x] `encode`: nbits=4 は 2 サブコードを 1 バイトに詰める (下位ニブルが偶数 j)
- [x] `decode_into` / `distance_key`: ニブル展開して表引き
- [x] `ksub(nbits)` / `code_bytes(m, nbits)` を公開 (ストレージ側が使う)
- [x] nbits は 4 と 8 のみ許可 (他は InvalidConfig)

## 完了条件

- [x] 符号化 → 復元のラウンドトリップが nbits=4/8 の両方で一致
- [x] nbits=4 は同 m で nbits=8 より誤差が大きく、コードは半分のバイト数
- [x] 既存の nbits=8 のテストが全て無変更で green

## 実装メモ

- `PqCodebook` に `nbits` を持たせ、`ksub()` / `code_bytes()` / `sub_code()` を
  内部で使う形にした。8bit の経路は分岐 1 つ増えるだけで挙動は不変
- ニブル配置は「偶数 j = 下位、奇数 j = 上位」。m が奇数の場合は最後の
  ニブルを上位 0 埋めで出す (`code_bytes` は ceil)
- OPQ (`OpqRotation::train`) にも nbits を通した。回転は**実際に使うコード幅**に
  対して最適化しないと意味がないため
