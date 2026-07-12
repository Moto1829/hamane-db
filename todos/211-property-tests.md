# 211: プロパティテスト (参照実装比較)

- Status: DONE (2026-07-12)
- Milestone: M2
- Depends: 209
- Design: DESIGN.md §8

## ゴール

ランダムな操作系列で実装と単純な参照モデルの結果が一致することを
proptest で継続的に検証する。

## やること

- [ ] 参照モデル: `HashMap<Id, (Vec<f32>, Metadata)>` + 全探索検索
- [ ] 操作 enum: Upsert / Delete / Flush / Reopen / Search{k, filter} /
      Get(id) を proptest strategy で生成 (dim 小さめ、id は狭い範囲で衝突させる)
- [ ] 系列実行後 (および途中の Search/Get ごと) に実装とモデルを比較
  - Search は「返る id 集合と順序」が一致すること
    (スコア同点の順序は id タイブレークで決定的: hamane-index の HeapEntry 参照)
- [ ] 失敗時の最小化 (proptest の shrink) が効くよう操作を独立に保つ

## 完了条件

- 1000 ケース × 数十操作が CI で安定して green
- 意図的にバグを入れる (例: tombstone 判定を反転) と即座に fail する
