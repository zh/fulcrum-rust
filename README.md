# fulcrum-rust

REST API gateway for Fulcrum/ElectrumX on Bitcoin Cash. Written in Rust with Axum. Drop-in replacement for the JavaScript [fulcrum-api](https://github.com/Permissionless-Software-Foundation/fulcrum-api) — same endpoints, same JSON shape.

## What it does

- Exposes a REST API that translates HTTP requests into Electrum JSON-RPC calls
- Supports single-address GET routes and bulk POST routes (up to 20 items)
- Converts CashAddr addresses to Electrum scripthashes (P2PKH and P2SH)
- Connection pooling with round-robin distribution and auto-reconnect
- TLS support with self-signed certificate acceptance for private Fulcrum nodes

## Quick Start

```bash
# Run locally (connects to Fulcrum at 127.0.0.1:50001)
cargo run

# Or with Docker
docker compose up

# Health check
curl http://localhost:3000/v1/electrumx/
```

## Configuration

All settings via environment variables:

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `FULCRUM_URL` | string | `127.0.0.1` | Fulcrum server hostname |
| `FULCRUM_PORT` | u16 | `50001` | Fulcrum server port |
| `FULCRUM_TLS` | bool | `false` | Enable TLS (`true` or `1`) |
| `FULCRUM_POOL_SIZE` | usize | `4` | Number of pooled connections |
| `PORT` | u16 | `3000` | HTTP listen port |
| `NETWORK` | string | `mainnet` | Network: `mainnet`, `testnet3`, `regtest` |

## API Endpoints

Base path: `/v1/electrumx`

### Health Check

```
GET /v1/electrumx/
→ {"success": true, "status": {"electrumx": "connected"}, "fulcrum": true}
```

### GET — Single Address

| Endpoint | Description |
|----------|-------------|
| `GET /utxos/{address}` | Unspent transaction outputs |
| `GET /balance/{address}` | Confirmed and unconfirmed balance |
| `GET /transactions/{address}` | Transaction history |
| `GET /unconfirmed/{address}` | Mempool transactions |

```bash
curl http://localhost:3000/v1/electrumx/balance/bitcoincash:qp3sn6vlwz28ntmf3wmr7trr96qtt6sgm5mzm97yg
```

### GET — Transaction & Block Data

| Endpoint | Description |
|----------|-------------|
| `GET /tx/data/{txid}?verbose=true` | Transaction details (verbose optional) |
| `GET /block/headers/{height}?count=1` | Block headers as 160-char hex strings |

### POST — Broadcast

```bash
curl -X POST http://localhost:3000/v1/electrumx/tx/broadcast \
  -H "Content-Type: application/json" \
  -d '{"txHex": "0200000001..."}'
```

### POST — Bulk Routes (max 20 items)

| Endpoint | Body |
|----------|------|
| `POST /utxos` | `{"addresses": ["bitcoincash:qp...", ...]}` |
| `POST /balance` | `{"addresses": ["bitcoincash:qp...", ...]}` |
| `POST /transactions` | `{"addresses": ["bitcoincash:qp...", ...]}` |
| `POST /unconfirmed` | `{"addresses": ["bitcoincash:qp...", ...]}` |
| `POST /tx/data` | `{"txids": ["abc123...", ...], "verbose": true}` |
| `POST /block/headers` | `{"heights": [{"height": 800000, "count": 1}, ...]}` |

### Error Responses

All errors return consistent JSON:

```json
{"success": false, "error": "description of what went wrong"}
```

| Status | Meaning |
|--------|---------|
| 400 | Bad request (invalid address, missing fields, bulk limit exceeded) |
| 408 | Request timeout (300s) |
| 422 | Unprocessable (network mismatch, invalid format) |
| 500 | Fulcrum connection or RPC error |

## Architecture

```
HTTP Request → Axum (CORS, tracing, timeout, path normalization)
  → Handler (address→scripthash conversion, request validation)
    → ElectrumPool (round-robin connection selection)
      → ElectrumClient (TCP or TLS, JSON-RPC 2.0)
        → Fulcrum server
```

**Modules:**
- `config` — env var parsing and defaults
- `address` — CashAddr to Electrum scripthash conversion (P2PKH/P2SH)
- `electrum` — JSON-RPC client with TCP/TLS, auto-reconnect, version handshake
- `pool` — round-robin connection pool with atomic counter
- `handlers` — 14 HTTP route handlers

**Middleware stack:** CORS (allow all origins) → request tracing → 300s timeout → trailing slash normalization.

## Development

```bash
cargo test          # 66 tests (31 unit + 35 integration with mock Electrum server)
cargo clippy        # Lint
cargo fmt           # Format
```

### Make Targets

| Target | Description |
|--------|-------------|
| `make build` | Debug build |
| `make release` | Release build (LTO, stripped, size-optimized) |
| `make run` | `cargo run` |
| `make test` | `cargo test` |
| `make clean` | `cargo clean` |
| `make cross-arm64` | Cross-compile for aarch64 (requires `cross`) |
| `make static-arm64` | Static musl build for aarch64 |
| `make docker` | `docker build -t fulcrum-rust .` |
| `make docker-run` | `docker compose up` |

## Docker

Multi-stage build: `rust:1.83-slim` (builder) → `debian:bookworm-slim` (runtime, ~80MB).

```bash
# Build and run
docker compose up

# Connects to host Fulcrum via host.docker.internal
```

The `docker-compose.yml` maps `host.docker.internal` to the host gateway, so a Fulcrum instance running on the host is reachable without extra network config.

## Cross-Compilation

For ARM64 targets (e.g., Raspberry Pi 5):

```bash
# Dynamic linking (requires cross: cargo install cross)
make cross-arm64

# Static binary (musl)
make static-arm64
```
