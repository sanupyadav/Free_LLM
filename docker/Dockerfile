# Freebuff2API Rust edition - multi-stage build
FROM rust:1.95-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs && echo '' > src/lib.rs && \
    cargo build --release 2>/dev/null || true

COPY src/ src/
COPY browser-extension/ browser-extension/
COPY tests/fixtures/freebuff-models.snapshot.json tests/fixtures/
# Full source is now in place: the skeleton cache layer is handled by the RUN above,
# so this does a full build directly (covers both lib + binary)
RUN touch src/main.rs src/lib.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
# v0.10.2: container health check (gateway /healthz)
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 CMD curl -sf http://127.0.0.1:47821/healthz >/dev/null || exit 1
COPY --from=builder /build/target/release/freebuff2api /usr/local/bin/freebuff2api
ENV PORT=47821
VOLUME ["/data"]
EXPOSE 47821
# No /data/config.json (e.g. Render) → config comes purely from env vars (LISTEN_ADDR, AUTH_TOKENS, API_KEYS...)
ENTRYPOINT ["sh", "-c", "if [ -f /data/config.json ]; then exec freebuff2api --config /data/config.json; else exec freebuff2api; fi"]
