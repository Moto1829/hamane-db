# 703: バックアップ API

- Status: DONE (2026-07-15)
- Milestone: M7
- Depends: 702 (バックアップ先の検証と相性)
- Design: docs/spec/limits.md「バックアップ機構はない」の解消

## ゴール

稼働中の DB から一貫性のあるバックアップを取れる `Database::backup(dest)` を
提供する。復元は「バックアップディレクトリを Database::open するだけ」。

## やること

- [ ] `Store::backup(dest_dir)`: flush (未フラッシュ分をセグメント化) した後、
      state ロックを保持して CURRENT / MANIFEST / 全セグメントファイルを
      dest へコピーする
  - ロック保持中は書き込みが待たされる (コピーは I/O のみで HNSW 構築より
    はるかに短い)。この制約を doc に明記
  - WAL はコピーしない (flush 直後なので空。バックアップは manifest 完結)
- [ ] dest が空でない場合はエラー (誤上書き防止)
- [ ] `Database::backup(dest)` を公開、CLI に `hamane backup <db> <dest>` 追加
- [ ] テスト: バックアップ → 元 DB に追記 → バックアップを open すると
      バックアップ時点の内容 (追記なし) が見える / CRC 検証込みで開ける

## 完了条件

- 上記テスト green
- docs/spec (persistence.md / limits.md / cli.md) 更新
