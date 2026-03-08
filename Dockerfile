# --- Build stage ---
FROM rust:1.83-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release && strip target/release/fulcrum-rust

# --- Runtime stage ---
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/fulcrum-rust /usr/local/bin/fulcrum-rust

ENV FULCRUM_URL=127.0.0.1
ENV FULCRUM_PORT=50001
ENV FULCRUM_TLS=false
ENV FULCRUM_POOL_SIZE=4
ENV PORT=3000
ENV NETWORK=mainnet

EXPOSE 3000

CMD ["fulcrum-rust"]
