# 306: フィルタ戦略 (pre/post 自動選択)

- Status: DONE (2026-07-12)
- Milestone: M3
- Depends: 305
- Design: docs/design/index.md §5

## ゴール

フィルタ付き検索で、選択率に応じて pre-filter / post-filter を
セグメントごとに自動選択する。

## やること

- [ ] 選択率推定: セグメントから等間隔 `sample_size=1000` 行の metadata を
      デコードして一致率 s を計算
- [ ] `s < 0.05` → pre-filter: meta.bin 全走査で一致ビットセット → 一致行のみ Flat
- [ ] `s ≥ 0.05` → post-filter: `ef' = ef × clamp(1/s, 1, 4)` で HNSW +
      filter_mask (一致ビットセットではなく行単位の遅延判定でメタデコードを節約)
- [ ] memtable は従来どおり逐次フィルタ (Flat)
- [ ] 閾値・サンプル数は `HnswParams` ではなく内部定数 (公開しない。調整は M4)

## 完了条件

- 選択率 0.1% / 5% / 50% の合成データで、フィルタ付き recall@10 ≥ 0.9 を維持しつつ
  「pre が選ばれるケース」「post が選ばれるケース」の両経路を通るテスト
- フィルタ一致 0 件で空結果 (panic なし)
