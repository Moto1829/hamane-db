# hamane-server の最小イメージ (todo 802)。
# alpine (musl) で静的リンクし、scratch に置く。実行時依存ゼロ。
#
#   docker build -t hamane-db .
#   docker run -p 8080:8080 -v hamane-data:/data -e HAMANE_API_KEY=secret hamane-db

FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p hamane-server \
    --config 'profile.release.strip="symbols"' \
    && mkdir -p /out/data

FROM scratch
COPY --from=builder /src/target/release/hamane-server /hamane-server
# 非 root で書けるデータディレクトリ (named volume の初期所有権もここから継承)
COPY --from=builder --chown=65534:65534 /out/data /data

ENV HAMANE_DB=/data
ENV HAMANE_LISTEN=0.0.0.0:8080
EXPOSE 8080
USER 65534:65534
VOLUME ["/data"]
# scratch には curl がないため自前の --healthcheck (/health を確認) を使う
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s \
    CMD ["/hamane-server", "--healthcheck"]
ENTRYPOINT ["/hamane-server"]
