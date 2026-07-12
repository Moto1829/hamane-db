#!/bin/sh
# SIFT1M (TexMex corpus) をダウンロードして data/sift/ に展開する。
# 約 161MB のダウンロード、展開後 ~500MB。
set -eu

cd "$(dirname "$0")/.."
mkdir -p data
cd data

if [ -f sift/sift_base.fvecs ]; then
    echo "data/sift/ は既に存在します"
    exit 0
fi

echo "downloading sift.tar.gz (~161MB)..."
if ! curl -fL --retry 3 -o sift.tar.gz ftp://ftp.irisa.fr/local/texmex/corpus/sift.tar.gz; then
    echo "ftp が失敗したため Hugging Face ミラーを試します..."
    mkdir -p sift
    for f in sift_base.fvecs sift_query.fvecs sift_groundtruth.ivecs sift_learn.fvecs; do
        curl -fL --retry 3 -o "sift/$f" \
            "https://huggingface.co/datasets/qbo-odp/sift1m/resolve/main/$f"
    done
    echo "done (mirror)"
    exit 0
fi

tar xzf sift.tar.gz
rm sift.tar.gz
echo "done: data/sift/"
ls -lh sift/
