# 201: hamane-storage クレート雛形とフォーマット基盤

- Status: DONE (2026-07-12)
- Milestone: M2
- Depends: なし
- Design: docs/design/storage.md §1

## ゴール

全オンディスクフォーマットが共有する符号化プリミティブを 1 箇所に実装し、
以降のタスク (WAL・セグメント・manifest) が同じ道具を使えるようにする。

## やること

- [ ] `crates/hamane-storage` を workspace に追加 (deps: hamane-core, crc32c, memmap2)
- [ ] `format` モジュール:
  - magic 定数群 (`HAMANEW\x01` 等) と検証関数
  - リトルエンディアン読み書きヘルパ (u32/u64/f32 slice/string)
  - CRC32C フレーミング (`write_framed` / `read_framed`)
- [ ] `MetaValue` / `Metadata` のバイナリ encode/decode (storage.md §1 の tag 形式)
- [ ] `Metric` ↔ u8 の変換 (0=L2, 1=Cosine, 2=Dot)
- [ ] `HamaneError::Corrupted(String)` を hamane-core に追加

## 完了条件

- encode → decode のラウンドトリップテストが全型で green
- 破損バイト列 (CRC 不一致・未知 tag・切り詰め) が `Corrupted` になるテスト
