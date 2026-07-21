# ─── builder ───
FROM rust:1.91-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf src/

COPY src src/
RUN cargo build --release

# ─── runtime ───
FROM alpine:3.21

RUN apk add --no-cache \
    aria2 \
    tini \
    tzdata

ENV TZ=Asia/Shanghai

COPY --from=builder /app/target/release/bangumi-rss /usr/local/bin/
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV DATA_DIR=/app/data
ENV ARIA2_RPC_URL=http://localhost:6800/jsonrpc

EXPOSE 7893
VOLUME ["/downloads", "/anime", "/app/data"]

ENTRYPOINT ["tini", "--", "/entrypoint.sh"]
