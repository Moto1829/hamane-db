# 504: バックグラウンドフラッシュ・コンパクション

- Status: DONE (2026-07-14)
- Milestone: M5 (完了ゲート)
- Depends: 501 (構築短縮後でも本質的に必要)
- Design: docs/design/storage.md §6 (「バックグラウンド化は M4」とした残件)

## ゴール

フラッシュとコンパクション (= HNSW 構築を含む、大規模では分単位の処理) が
書き込みスレッド上で同期実行されており、**その間すべての書き込みが停止する**。
これを専用スレッドに逃し、書き込みが止まらないようにする。

現状最大の運用上の問題。1M 規模では閾値到達のたびに書き込みが 20 分以上
ブロックされる。

## やること

- [ ] immutable memtable の導入 (storage.md §6 の元設計):
      閾値到達時はアクティブ memtable を immutable に切り替えて新 WAL を開くだけ
      にし (短い臨界区間)、セグメント書き出しはメンテナンススレッドが行う
- [ ] `StoreState` を「書き込み状態 (active memtable + WAL)」と
      「世代状態 (immutable + segments + manifest)」に分離し、
      LiveView は immutable memtable もソースに含める
      (rank: active=0, immutable=1, segments=2..)
- [ ] コンパクションも同じメンテナンススレッドで実行
      (フラッシュ → 閾値判定 → コンパクションの直列パイプライン)
- [ ] エラー処理: メンテナンス失敗は次回リトライ。WAL が残っている限り
      データは失われないことをテストで確認
- [ ] `Database::close()` (または Drop) でメンテナンススレッドを flush して join
- [ ] 書き込み停止時間の計測: フラッシュを跨ぐ upsert のレイテンシ p99 を
      ベンチに追加

## 完了条件

- 1M 規模のフラッシュ/コンパクション中も upsert p99 < 10ms
- クラッシュ耐性テスト (210 相当) がバックグラウンド化後も green
  (immutable memtable が WAL に残っている状態の復旧を含む)
- proptest (211) に「フラッシュ中の並行書き込み」相当の操作を追加して green
