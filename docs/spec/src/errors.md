# エラーリファレンス

すべての操作は `hamane::Result<T> = Result<T, HamaneError>` を返します。

| バリアント | 発生条件 | 対処 |
|---|---|---|
| `DimensionMismatch { expected, actual }` | ベクトル長が Collection の dim と不一致 (upsert / search) | 入力を修正 |
| `InvalidVector(msg)` | NaN・無限大を含む / Cosine でゼロベクトル | 入力を修正 |
| `CollectionExists(name)` | `create_collection` で同名が既に存在 | `collection(name)` で取得 |
| `CollectionNotFound(name)` | 存在しない Collection への操作 | 名前を確認 |
| `InvalidConfig(msg)` | `dim == 0` など設定不正 | 設定を修正 |
| `Corrupted(msg)` | ファイル破損の検出 (CRC 不一致・フォーマット不正) | 下記 |
| `Locked(path)` | 同じ DB を別プロセス (または二重に) 開こうとした | 先に開いた側を閉じる |
| `ReadOnlyReplica` | read レプリカ (`--replicate-from` で起動) への書き込み | 書き込みは primary に送る ([レプリケーション](replication.md)) |
| `Io(std::io::Error)` | ファイル I/O 失敗 (権限・ディスクフル等) | 環境を確認 |

## エラー後の状態

- **書き込みエラー** (`upsert` / `delete` が Err): その操作は WAL にも
  memtable にも反映されていない。Database は引き続き使用できる
- **`upsert_batch` のエラー**: 検証エラー (次元・NaN) はバッチ全体が
  未反映。I/O エラー時も memtable には未反映
- **フラッシュのエラー**: WAL は残っているため、データは失われない。
  再試行または再 open で復旧できる

## Corrupted について

`Database::open` 時の `Corrupted` は次のいずれかです:

- manifest の破損 → DB を開けない。バックアップからの復旧が必要
- セグメントファイルのヘッダ・サイズ不整合 → 同上
- WAL の**末尾以外**の破損 → 破損位置までのレコードで復旧される
  (エラーにならない。末尾の書きかけはクラッシュの正常な痕跡として扱う)

すべてのファイルは CRC32C を持ち、破損は「検出してエラーを返す」ことが
保証されます (panic しない)。
