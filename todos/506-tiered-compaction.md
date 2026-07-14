# 506: size-tiered 部分コンパクション

- Status: DONE (2026-07-14)
- Milestone: M5
- Depends: 504 (バックグラウンド化とセット)
- Design: DESIGN.md §3, todos/README.md M4 実装メモ (full merge の暫定を解消)

## ゴール

現状の full merge (全セグメント統合) は書き込み総量が O(n²/閾値) になる。
サイズ階層ごとの部分マージにして write amplification を抑える。

## やること

- [ ] manifest の `CollectionEntry.segments` を「seg_id 昇順」制約から
      「年代順リスト (古い→新しい)」に変更 (フォーマット version bump v2)。
      部分マージの結果セグメント (新しい seg_id) をマージ元の位置に挿入できる
      ようにする
- [ ] 旧フォーマット (v1) の読み込み互換を維持 (v1 は昇順 = 年代順なのでそのまま)
- [ ] tier 分け: record_count を ×4 区切りで階層化し、同 tier に 4 個
      たまったら **年代的に連続する** その 4 個をマージ
- [ ] tombstone の引き継ぎ規則: マージ範囲より古いセグメントが残る場合、
      範囲内の tombstone は新セグメントに引き継ぐ (401 の設計メモどおり)
- [ ] proptest (211) の Compact を部分マージ経路が通る形に強化
- [ ] 長時間書き込みテスト (401 の収束テスト) で write amplification を
      full merge と比較計測

## 完了条件

- 収束テストで総書き込みバイト数が full merge 比で減っている (計測を記録)
- newest-wins の意味論が部分マージ後も保たれる (proptest green)
- v1 で作った DB が開ける (互換テスト)
