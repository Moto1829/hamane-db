# 702: プロセス排他ロック

- Status: DONE (2026-07-15)
- Milestone: M7
- Depends: なし
- Design: docs/spec/limits.md「単一プロセス専用 (多重 open は未定義)」の解消

## ゴール

同じ DB ディレクトリを 2 つのプロセス (または同一プロセスで 2 回) が
開いたときの「未定義動作 (データ破損の可能性)」を「明示エラー」に変える。

## やること

- [ ] `<db_dir>/LOCK` に flock (排他・非ブロッキング) をかける。
      取得できなければ `HamaneError::Locked` (新バリアント) を返す
- [ ] unix は libc::flock (advisory)。非 unix はフォールバック
      (ベストエフォートであることを doc に明記)
- [ ] ロックは Store の生存期間中保持し、Drop で解放
      (flock はプロセス終了・クラッシュで自動解放されるため残骸問題なし)
- [ ] テスト: 同一プロセス内の二重 open がエラー / drop 後は再 open 可能

## 完了条件

- 二重 open テスト green
- docs/spec/limits.md から該当制約を「解消済み」へ移動
