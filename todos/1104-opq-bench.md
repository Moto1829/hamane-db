# 1104: OPQ のベンチとドキュメント

- Status: TODO
- Milestone: M11
- Depends: 1103
- Design: docs/design/opq.md §6

## ゴール

SIFT で PQ と OPQ の recall / QPS / 構築時間を対比し、docs/benchmarks.md と
仕様書に記録する。

## やること

- [ ] `hamane-bench` に `--config opq` / `--config ivfopq` を追加
- [ ] SIFT 200k で PQ vs OPQ を計測 (recall@10、QPS、構築時間、ディスク)
- [ ] docs/benchmarks.md に OPQ の行と考察を追記
- [ ] docs/spec/ の量子化章に OPQ を追記、README の機能一覧を更新

## 完了条件

- [ ] 同一 m で OPQ の recall が PQ 以上 (構築時間の増分も記録)
- [ ] ベンチ手順が docs/benchmarks.md から再現できる
