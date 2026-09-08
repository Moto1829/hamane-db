//! RAG (検索拡張生成) の検索側を一通り (todo 1302)。
//!
//! 実行: `cargo run --release --example rag_pipeline`
//!
//! 文書 → チャンク分割 → 埋め込み → 投入 → クエリ埋め込み → 近傍検索 →
//! 出典つきコンテキスト組み立て、までの流れを示します。
//!
//! **埋め込みは外部依存を避けたハッシュベースの疑似実装**です。
//! 実際には OpenAI / Cohere / ローカルモデル (fastembed, candle など) の
//! 出力を `Record::new(id, embedding)` に渡してください。次元は
//! `CollectionConfig.dim` と一致している必要があります。

use hamane::{CollectionConfig, Database, Filter, Metric, Record};

/// 埋め込みの次元 (実際のモデルに合わせる。例: text-embedding-3-small は 1536)
const DIM: usize = 64;

struct Document {
    id: &'static str,
    title: &'static str,
    body: &'static str,
}

fn main() -> hamane::Result<()> {
    let dir = std::env::temp_dir().join(format!("hamane-example-rag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let db = Database::open(&dir)?;
    // 埋め込みの類似度はコサインが定番 (投入時に自動で正規化される)
    let chunks = db.create_collection(
        "chunks",
        CollectionConfig {
            dim: DIM,
            metric: Metric::Cosine,
        },
    )?;

    let docs = corpus();

    // 1. チャンク分割して埋め込み、メタデータに出典を持たせる
    let mut records = Vec::new();
    for doc in &docs {
        for (i, chunk) in split_into_chunks(doc.body, 12).into_iter().enumerate() {
            records.push(
                Record::new(format!("{}#{i}", doc.id), embed(&chunk))
                    .with_meta("doc_id", doc.id)
                    .with_meta("title", doc.title)
                    .with_meta("text", chunk.clone())
                    .with_meta("chunk_index", i as i64),
            );
        }
    }
    println!(
        "{} 文書 → {} チャンクを投入します",
        docs.len(),
        records.len()
    );
    chunks.upsert_batch(records)?;
    chunks.flush()?;

    // 2. 質問を埋め込んで検索する
    let question = "ベクトル検索の再現率はどう評価しますか";
    let hits = chunks.search(&embed(question)).k(3).run()?;

    println!("\n質問: {question}\n");
    println!(
        "(注意: ここでの埋め込みは文字 n-gram のハッシュなので**意味は捉えません**。\n \
         実際の埋め込みモデルに差し替えると、この質問では HNSW の文書が上位に来ます)\n"
    );
    println!("--- 取得したコンテキスト ---");
    let mut context = String::new();
    for (rank, hit) in hits.iter().enumerate() {
        let title = hit.metadata.get("title").map(text).unwrap_or_default();
        let body = hit.metadata.get("text").map(text).unwrap_or_default();
        println!("[{}] {title} (score={:.3})", rank + 1, hit.score);
        println!("    {body}");
        context.push_str(&format!("[{}] {body}\n", rank + 1));
    }

    // 3. 出典つきのプロンプトを組み立てる (ここから先は LLM に渡す)
    let prompt = format!(
        "以下のコンテキストだけを使って質問に答え、必ず [番号] で出典を示してください。\n\n\
         # コンテキスト\n{context}\n# 質問\n{question}\n"
    );
    println!(
        "--- LLM に渡すプロンプト ({} 文字) ---",
        prompt.chars().count()
    );
    println!("{prompt}");

    // 4. 文書単位で絞り込みたいときはメタデータフィルタを使う
    //    (例: 特定の文書だけを対象にした追加質問)
    let scoped = chunks
        .search(&embed(question))
        .k(2)
        .filter(Filter::eq("doc_id", "doc-hnsw"))
        .run()?;
    println!("--- doc-hnsw に絞った再検索 ---");
    for hit in &scoped {
        println!(
            "  {} score={:.3}",
            hit.ext_id().unwrap_or_default(),
            hit.score
        );
    }

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

fn text(v: &hamane::MetaValue) -> String {
    match v {
        hamane::MetaValue::Str(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

fn corpus() -> Vec<Document> {
    vec![
        Document {
            id: "doc-hnsw",
            title: "HNSW による近似最近傍探索",
            body: "HNSW は階層化した近傍グラフを辿って近似的に最近傍を見つける手法です。 \
                   探索幅 ef を大きくすると再現率が上がり、そのぶん遅くなります。 \
                   再現率は総当たりの正解と比べた recall@k で評価します。 \
                   構築時のパラメータ m と ef_construction がグラフの品質を決めます。",
        },
        Document {
            id: "doc-quantization",
            title: "量子化によるメモリ削減",
            body: "スカラー量子化 SQ8 はベクトルを 1 バイトに落として 4 分の 1 に圧縮します。 \
                   直積量子化 PQ はサブベクトルごとにコードブックを学習し、さらに強く圧縮します。 \
                   いずれも量子化距離で候補を絞り、最後に元の f32 で並べ直すので精度を保てます。",
        },
        Document {
            id: "doc-persistence",
            title: "永続化とクラッシュ耐性",
            body: "書き込みはまず WAL に追記され、memtable が閾値を超えるとセグメントになります。 \
                   セグメントは不変で、manifest が世代を指します。 \
                   プロセスが落ちても ack 済みのデータは復旧でき、半端な状態は見えません。",
        },
    ]
}

/// 単語数でざっくり分割する (実際は文境界やトークン数で分けることが多い)。
fn split_into_chunks(body: &str, words_per_chunk: usize) -> Vec<String> {
    let words: Vec<&str> = body.split_whitespace().collect();
    words.chunks(words_per_chunk).map(|w| w.join(" ")).collect()
}

/// **疑似埋め込み**: 文字 n-gram をハッシュして次元に散らす (bag-of-ngrams)。
/// 意味を捉えるものではありませんが、同じ語を含む文が近くなる程度には働きます。
/// 実運用では埋め込みモデルの出力に差し替えてください。
fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    let chars: Vec<char> = text.chars().collect();
    for window in chars.windows(2) {
        let mut hash: u64 = 1469598103934665603;
        for c in window {
            hash ^= *c as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
        v[(hash % DIM as u64) as usize] += 1.0;
    }
    // ゼロベクトルは cosine で使えないので保険
    if v.iter().all(|x| *x == 0.0) {
        v[0] = 1.0;
    }
    v
}
