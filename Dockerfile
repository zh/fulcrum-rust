# --- Builder stage ---
FROM rust:1.85-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

# --- Runtime stage ---
FROM gcr.io/distroless/static-debian12

COPY --from=builder /app/target/release/fulcrum-rust /fulcrum-rust

ENV FULCRUM_URL=127.0.0.1
ENV FULCRUM_PORT=50001
ENV FULCRUM_TLS=false
ENV FULCRUM_POOL_SIZE=4
ENV PORT=3000
ENV NETWORK=mainnet

EXPOSE 3000

ENTRYPOINT ["/fulcrum-rust"]
