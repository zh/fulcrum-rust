# fulcrum-rust

REST API gateway for Fulcrum/ElectrumX on Bitcoin Cash. Written in Rust with Axum. Drop-in replacement for the JavaScript [fulcrum-api](https://github.com/Permissionless-Software-Foundation/fulcrum-api) — same endpoints, same JSON shape.

## What it does

- Exposes a REST API that translates HTTP requests into Electrum JSON-RPC calls
- **CashToken support** via Electrum protocol 1.5 — UTXO responses include `token_data` (category, amount, NFT capability/commitment) for token-bearing outputs
- Supports single-address GET routes and bulk POST routes (up to 20 items)
- Converts CashAddr addresses to Electrum scripthashes — both regular (q/p-prefix) and **token-aware (z/r-prefix)** addresses
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
| `GET /utxos/{address}` | Unspent transaction outputs (includes `token_data` for CashTokens) |
| `GET /balance/{address}` | Confirmed and unconfirmed balance |
| `GET /transactions/{address}` | Transaction history |
| `GET /unconfirmed/{address}` | Mempool transactions |

Addresses can use either regular (q/p) or token-aware (z/r) prefix — both produce identical results since they share the same hash160.

```bash
# Regular address
curl http://localhost:3000/v1/electrumx/balance/bitcoincash:qp3sn6vlwz28ntmf3wmr7trr96qtt6sgm5mzm97yg

# Token-aware address (same hash160, same result)
curl http://localhost:3000/v1/electrumx/utxos/bitcoincash:zp3sn6vlwz28ntmf3wmr7trr96qtt6sgm5xtga74af
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

### CashToken Data

With Electrum protocol 1.5, UTXO responses automatically include `token_data` for outputs that carry CashTokens. Plain BCH outputs have no `token_data` field.

**Fungible token:**
```json
{
  "height": 945296,
  "tx_hash": "b86f39fc...",
  "tx_pos": 0,
  "value": 1000,
  "token_data": {
    "category": "ea38c6a264d5653220ffe691f424a80491b0c4c80dd70bbdd70d4ebe453b202b",
    "amount": "30"
  }
}
```

**NFT:**
```json
{
  "height": 945296,
  "tx_hash": "43daa726...",
  "tx_pos": 1,
  "value": 1000,
  "token_data": {
    "category": "909427e2f7b0a75098d99cdb8c9b4aa65748853ec7caf1e2b2d0443c65e9c2a9",
    "amount": "0",
    "nft": {
      "capability": "none",
      "commitment": "98"
    }
  }
}
```

**Plain BCH (no token):**
```json
{
  "height": 945697,
  "tx_hash": "94bbbf1d...",
  "tx_pos": 0,
  "value": 464500
}
```

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
- `address` — CashAddr to Electrum scripthash conversion (P2PKH/P2SH, including token-aware z/r-prefix)
- `electrum` — JSON-RPC client with TCP/TLS, auto-reconnect, protocol 1.5 handshake (CashToken support)
- `pool` — round-robin connection pool with atomic counter
- `handlers` — 14 HTTP route handlers

**Middleware stack:** CORS (allow all origins) → request tracing → 300s timeout → trailing slash normalization.

## Development

```bash
cargo test          # 63 tests (unit + integration with mock Electrum server)
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

Multi-stage build: `rust:1.85-alpine` (builder, static musl linking) → `gcr.io/distroless/static-debian12` (runtime, ~2MB).

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
