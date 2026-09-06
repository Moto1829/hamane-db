# 1104: OPQ のベンチとドキュメント

- Status: DONE (2026-09-06)
- Milestone: M11
- Depends: 1103
- Design: docs/design/opq.md §6

## ゴール

SIFT で PQ と OPQ の recall / QPS / 構築時間を対比し、docs/benchmarks.md と
仕様書に記録する。

## やったこと

- [x] `hamane-bench` に `--config opq` / `--config ivfopq` を追加
- [x] `--rotate-input` を追加 (固定の乱数直交回転を入力に掛ける。距離を保存する
      ので正解は不変。「次元順に意味がないデータ」の模擬)
- [x] SIFT 200k で PQ vs OPQ を計測 (recall@10、QPS、構築時間、ディスク)
- [x] docs/benchmarks.md に M11 節を追記
- [x] docs/spec (configuration / search / file-format / limits) を M10・M11 に
      追随させ、README の機能一覧を更新

## 完了条件

- [x] 同一 m で OPQ の recall が PQ 以上 (回転入力で 0.9584 → 0.9699。
      素の SIFT では同等。構築は +35〜54%)
- [x] ベンチ手順が docs/benchmarks.md から再現できる

## 実装メモ

- **ベンチが実装バグを 2 つ検出した**: 最初の計測で OPQ の recall が PQ より
  低く (0.9606 vs 0.9838)、調べると (1) 極分解が実データ規模の行列で収束せず
  学習が 1 歩も進まない、(2) 乱数初期値が構造化データに不適、の 2 点だった。
  ユニットテストの合成データだけでは両方とも見逃していた (todo 1102 の実装メモ)
- **素の SIFT では OPQ に利得がない**。128 次元が「16 セル × 8 方位」の並びで、
  PQ の位置分割が既にこの構造に合っているため。`--rotate-input` で構造を壊すと
  PQ は 2.5 ポイント落ち、OPQ がその半分近くを取り戻す = OPQ の適用領域は
  「次元順に意味がないデータ」(多くの埋め込みモデル)
- QPS は PQ と同等 (回転はクエリごとに d² の行列ベクトル積 1 回のみ)。
  ディスク増は opq.bin (dim² × 4B) だけ
