# 詳細設計: インデックス層 (hamane-index)

DESIGN.md §4 の詳細化。HNSW の構築・探索・永続化フォーマットを規定する。

- Status: Draft v0.1 (2026-07-12)
- 対応タスク: todos/301〜305

---

## 1. HNSW の構造とパラメータ

Malkov & Yashunin (2016) の標準構成に従う。

| パラメータ | 既定値 | 意味 |
|---|---|---|
| `m` | 16 | 層 1 以上の最大接続数 |
| `m0` | 32 (= 2m) | 層 0 の最大接続数 |
| `ef_construction` | 200 | 構築時の候補リスト幅 |
| `ef_search` | 64 | 検索時の候補リスト幅 (クエリごとに上書き可) |
| `ml` | 1/ln(m) | レベル抽選の係数 |

- ノードのレベル: `level = floor(-ln(uniform(0,1)) * ml)`
- 乱数は seed 固定可能にする (`rand::rngs::StdRng` + `IndexParams.seed: Option<u64>`)。
  テストの再現性とセグメントの決定的構築のため、フラッシュ時は seg_id を seed に使う
- 距離は `Metric::distance_key` (小さいほど近い) のみを使い、メトリック非依存に書く

### インメモリ表現 (構築時)

```rust
struct HnswBuilder {
    dim: usize,
    metric: Metric,
    params: HnswParams,
    levels: Vec<u8>,               // node → 最上位レベル
    neighbors: Vec<Vec<Vec<u32>>>, // node → level → 隣接 node 列
    entry_point: Option<u32>,
    rng: StdRng,
}
```

node ID はセグメントの row (u32) と一致させる。ベクトル本体は保持せず、
`&dyn VectorSource` (`fn vector(&self, row: u32) -> &[f32]`) 経由で参照する。
これによりビルダーは memtable からもセグメント (コンパクション時) からも構築できる。

### 挿入 (Algorithm 1)

1. レベル L を抽選
2. entry_point から最上層〜L+1 層まで greedy 降下 (ef=1)
3. L 層から 0 層まで各層で `search_layer(q, ef_construction)` → 候補から
   **ヒューリスティック選択 (Algorithm 4)** で m 個 (層 0 は m0) を接続
4. 逆向きエッジを張り、接続超過ノードは同じヒューリスティックで刈り込み
5. L が現在の最上層を超えたら entry_point を更新

ヒューリスティック選択は「候補 c について、既選択集合のどの要素よりも q に近い
場合のみ採用」の標準形。`keep_pruned_connections` は v0 では実装しない。

### 探索 (Algorithm 2/5)

- `search(q, k, ef, filter_mask)`: 最上層から 1 層まで ef=1 で降下、
  層 0 で `search_layer(q, max(ef, k))` → 上位 k を返す
- `filter_mask: Option<&dyn Fn(u32) -> bool>`: **結果への採用のみ**をマスクし、
  グラフの走査は全ノードを通す (走査まで絞るとグラフが分断され再現率が崩れるため)
- visited 集合は `Vec<u64>` のビットセット。探索ごとに確保 (v0)。
  world/epoch 方式の再利用は M4 の最適化で検討

---

## 2. 再現率の検証 (todos/303)

- ランダムデータ (一様 + クラスタ混合、n=10k, dim=64/512) で
  Flat の正解に対する recall@10 を測るテストを `hamane-index` に置く
- 既定パラメータで **recall@10 ≥ 0.95** を CI で担保 (seed 固定で決定的に)
- SIFT1M での測定は todos/307 のベンチハーネスで行う (CI 外)

---

## 3. 永続化フォーマット: hnsw.bin

セグメントディレクトリ内。mmap で zero-copy 読み込みできる CSR レイアウト。

```
header (64 B に pad):
  magic[8] = b"HAMANEH\x01"
  node_count u32
  max_level u8
  entry_point u32
  m u32, m0 u32
per-node metadata:
  levels:       node_count × u8      # 各ノードの最上位レベル
per-level CSR (level 0 から max_level まで順に):
  level_node_count u32               # この層に存在するノード数
  node_ids:     level_node_count × u32   # この層に居るノード (昇順)
  offsets:      (level_node_count+1) × u32
  neighbor_ids: offsets[last] × u32
footer: crc32c u32
```

- 読み込み側 `HnswView` は mmap スライスへの参照だけを持ち、探索は
  `neighbors(level, node) -> &[u32]` で辿る
- アラインメント: u32 境界のみ必要。header を 64B に pad して data 部を揃える

---

## 4. Flat との使い分けとセグメント統合 (todos/305)

- セグメントフラッシュ時、`record_count >= hnsw_min_rows` (既定 1024) なら
  HNSW を構築して hnsw.bin を書く。それ未満は Flat スキャンで十分なので書かない
- 検索時の各ソース:
  - memtable → `search_flat`
  - セグメント (hnsw.bin あり) → `HnswView::search`
  - セグメント (hnsw.bin なし) → vectors.bin の全行 `search_flat`
- 各ソースの結果を storage 層の LiveView で newest-wins マージ (storage.md §7)

## 5. フィルタ戦略 (todos/306)

クエリプランナ (hamane クレート内) がソースごとに選択する:

1. **pre-filter**: meta.bin を走査してフィルタ一致行の集合 (ビットセット) を作り、
   一致行だけ Flat で距離計算
2. **post-filter**: HNSW を `ef' = ef × oversample` で探索し、
   filter_mask で一致行のみ採用

選択基準: セグメントから最大 `sample_size = 1000` 行をサンプリングして選択率 s を推定。

- `s < 0.05` → pre-filter (一致が少なく HNSW では k 件集まらない)
- `s >= 0.05` → post-filter、`oversample = clamp(1/s, 1, 4)`

推定はセグメント単位 (不変なのでフィルタ式ごとにキャッシュ可能だが v0 では都度)。
