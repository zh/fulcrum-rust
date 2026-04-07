//! Integration tests for the fulcrum-rust REST API.
//!
//! These tests spin up a mock Electrum TCP server and the full Axum HTTP
//! stack, then exercise every endpoint via HTTP to verify response format
//! compatibility with the JavaScript `fulcrum-api`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use fulcrum_rust::{handlers, health_check, AppState, ElectrumPool};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tower::ServiceExt;

/// Mock Electrum JSON-RPC server.
/// Responds to known methods with canned data matching the JS mock fixtures.
async fn mock_electrum_server(listener: TcpListener) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => break,
        };

        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            loop {
                line.clear();
                let n = match reader.read_line(&mut line).await {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n == 0 {
                    break;
                }

                let req: Value = match serde_json::from_str(line.trim()) {
                    Ok(v) => v,
                    Err(_) => break,
                };

                let id = req.get("id").cloned().unwrap_or(Value::Null);
                let method = req["method"].as_str().unwrap_or("");

                let result = match method {
                    "server.version" => json!(["MockFulcrum 2.1.0", "1.5"]),

                    "blockchain.scripthash.listunspent" => json!([
                        {
                            "height": 604392,
                            "tx_hash": "7774e449c5a3065144cefbc4c0c21e6b69c987f095856778ef9f45ddd8ae1a41",
                            "tx_pos": 0,
                            "value": 1000
                        },
                        {
                            "height": 630834,
                            "tx_hash": "4fe60a51e0d8f5134bfd8e5f872d6e502d7f01b28a6afebb27f4438a4f638d53",
                            "tx_pos": 0,
                            "value": 6000,
                            "token_data": {
                                "category": "ea38c6a264d5653220ffe691f424a80491b0c4c80dd70bbdd70d4ebe453b202b",
                                "amount": "100"
                            }
                        }
                    ]),

                    "blockchain.scripthash.get_balance" => json!({
                        "confirmed": 7000,
                        "unconfirmed": 0
                    }),

                    "blockchain.scripthash.get_history" => json!([
                        {
                            "height": 601861,
                            "tx_hash": "6181c669614fa18039a19b23eb06806bfece1f7514ab457c3bb82a40fe171a6d"
                        }
                    ]),

                    "blockchain.scripthash.get_mempool" => json!([
                        {
                            "tx_hash": "45381031132c57b2ff1cbe8d8d3920cf9ed25efd9a0beb764bdb2f24c7d1c7e3",
                            "height": 0,
                            "fee": 24310
                        }
                    ]),

                    "blockchain.transaction.get" => json!({
                        "blockhash": "0000000000000000002aaf94953da3b487317508ebd1003a1d75d6d6ec2e75cc",
                        "blocktime": 1578327094,
                        "confirmations": 31861,
                        "hash": "4db095f34d632a4daf942142c291f1f2abb5ba2e1ccac919d85bdc2f671fb251",
                        "hex": "0200000002abcd",
                        "locktime": 0,
                        "size": 392,
                        "time": 1578327094,
                        "txid": "4db095f34d632a4daf942142c291f1f2abb5ba2e1ccac919d85bdc2f671fb251",
                        "version": 2,
                        "vin": [],
                        "vout": []
                    }),

                    "blockchain.transaction.broadcast" => {
                        let params = &req["params"];
                        if let Some(hex) = params.get(0).and_then(Value::as_str) {
                            if hex.is_empty() {
                                // Return an RPC error for empty hex
                                let resp = json!({
                                    "id": id,
                                    "error": { "code": -1, "message": "TX decode failed" }
                                });
                                let mut msg = serde_json::to_string(&resp).unwrap();
                                msg.push('\n');
                                let _ = writer.write_all(msg.as_bytes()).await;
                                continue;
                            }
                            json!(
                                "4db095f34d632a4daf942142c291f1f2abb5ba2e1ccac919d85bdc2f671fb251"
                            )
                        } else {
                            json!(
                                "4db095f34d632a4daf942142c291f1f2abb5ba2e1ccac919d85bdc2f671fb251"
                            )
                        }
                    }

                    "blockchain.block.headers" => {
                        let count = req["params"].get(1).and_then(Value::as_u64).unwrap_or(1);
                        let header1 = "010000008b52bbd72c2f49569059f559c1b1794de5192e4f7d6d2b03c7482bad0000000083e4f8a9d502ed0c419075c1abb5d56f878a2e9079e5612bfb76a2dc37d9c42741dd6849ffff001d2b909dd6";
                        let header2 = "01000000f528fac1bcb685d0cd6c792320af0300a5ce15d687c7149548904e31000000004e8985a786d864f21e9cbb7cbdf4bc9265fe681b7a0893ac55a8e919ce035c2f85de6849ffff001d385ccb7c";
                        let hex = if count >= 2 {
                            format!("{header1}{header2}")
                        } else {
                            header1.to_string()
                        };
                        json!({
                            "count": count,
                            "hex": hex,
                            "max": 2016
                        })
                    }

                    _ => json!(null),
                };

                let resp = json!({ "id": id, "result": result });
                let mut msg = serde_json::to_string(&resp).unwrap();
                msg.push('\n');
                let _ = writer.write_all(msg.as_bytes()).await;
            }
        });
    }
}

/// Start the mock Electrum server and return its port.
async fn start_mock_electrum() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(mock_electrum_server(listener));
    (port, handle)
}

/// Build the full router (mirrors main.rs routes).
fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/electrumx/", get(health_check))
        .route("/v1/electrumx/utxos/{address}", get(handlers::get_utxos))
        .route(
            "/v1/electrumx/balance/{address}",
            get(handlers::get_balance),
        )
        .route(
            "/v1/electrumx/transactions/{address}",
            get(handlers::get_transactions),
        )
        .route(
            "/v1/electrumx/unconfirmed/{address}",
            get(handlers::get_unconfirmed),
        )
        .route("/v1/electrumx/tx/data/{txid}", get(handlers::get_tx_data))
        .route("/v1/electrumx/tx/broadcast", post(handlers::broadcast_tx))
        .route(
            "/v1/electrumx/block/headers/{height}",
            get(handlers::get_block_headers),
        )
        .route("/v1/electrumx/utxos", post(handlers::utxos_bulk))
        .route("/v1/electrumx/balance", post(handlers::balance_bulk))
        .route(
            "/v1/electrumx/transactions",
            post(handlers::transactions_bulk),
        )
        .route(
            "/v1/electrumx/unconfirmed",
            post(handlers::unconfirmed_bulk),
        )
        .route("/v1/electrumx/tx/data", post(handlers::tx_data_bulk))
        .route(
            "/v1/electrumx/block/headers",
            post(handlers::block_headers_bulk),
        )
        .with_state(state)
}

// ========================================================================
// Test helpers
// ========================================================================

async fn setup() -> Router {
    let (port, _handle) = start_mock_electrum().await;
    let pool = Arc::new(ElectrumPool::new("127.0.0.1", port, false, 2));
    pool.connect_all().await;
    let state = AppState {
        pool,
        network: "mainnet".to_string(),
    };
    build_router(state)
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    (status, json)
}

async fn post_json(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

const ADDR: &str = "bitcoincash:qpr270a5sxphltdmggtj07v4nskn9gmg9yx4m5h7s4";
const TXID: &str = "4db095f34d632a4daf942142c291f1f2abb5ba2e1ccac919d85bdc2f671fb251";

// ========================================================================
// Scripthash conversion (matching JS test vectors)
// ========================================================================

#[test]
fn scripthash_p2pkh_matches_js() {
    let result = fulcrum_rust::address::address_to_scripthash(
        "bitcoincash:qpr270a5sxphltdmggtj07v4nskn9gmg9yx4m5h7s4",
    )
    .unwrap();
    assert_eq!(
        result,
        "bce4d5f2803bd1ed7c1ba00dcb3edffcbba50524af7c879d6bb918d04f138965"
    );
}

#[test]
fn scripthash_p2sh_matches_js() {
    let result = fulcrum_rust::address::address_to_scripthash(
        "bitcoincash:pz0z7u9p96h2p6hfychxdrmwgdlzpk5luc5yks2wxq",
    )
    .unwrap();
    assert_eq!(
        result,
        "8bc2235c8e7d5634d9ec429fc0171f2c58e728d4f1e2fb7e440e313133cfa4f0"
    );
}

// ========================================================================
// Health check
// ========================================================================

#[tokio::test]
async fn health_check_connected() {
    let app = setup().await;
    let (status, json) = get_json(&app, "/v1/electrumx/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "electrumx");
    assert_eq!(json["fulcrum"], true);
}

// ========================================================================
// GET endpoints — UTXOs
// ========================================================================

#[tokio::test]
async fn get_utxos_success() {
    let app = setup().await;
    let (status, json) = get_json(&app, &format!("/v1/electrumx/utxos/{ADDR}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["utxos"].is_array());

    let utxos = json["utxos"].as_array().unwrap();
    assert_eq!(utxos.len(), 2);
    assert_eq!(utxos[0]["height"], 604392);
    assert_eq!(
        utxos[0]["tx_hash"],
        "7774e449c5a3065144cefbc4c0c21e6b69c987f095856778ef9f45ddd8ae1a41"
    );
    assert_eq!(utxos[0]["tx_pos"], 0);
    assert_eq!(utxos[0]["value"], 1000);
    // First UTXO has no token_data
    assert!(utxos[0].get("token_data").is_none());

    // Second UTXO has token_data (CashToken) -- verify passthrough
    assert_eq!(utxos[1]["value"], 6000);
    let token = &utxos[1]["token_data"];
    assert_eq!(
        token["category"],
        "ea38c6a264d5653220ffe691f424a80491b0c4c80dd70bbdd70d4ebe453b202b"
    );
    assert_eq!(token["amount"], "100");
}

#[tokio::test]
async fn get_utxos_invalid_address() {
    let app = setup().await;
    let (status, json) = get_json(&app, "/v1/electrumx/utxos/invalidaddr").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["success"], false);
    assert!(json["error"].as_str().unwrap().contains("invalid address"));
}

#[tokio::test]
async fn get_utxos_network_mismatch() {
    let app = setup().await;
    let (status, json) = get_json(
        &app,
        "/v1/electrumx/utxos/bchtest:qq89kjkeqz9mngp8kl3dpmu43y2wztdjqu500gn4c4",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["success"], false);
    assert!(json["error"].as_str().unwrap().contains("invalid network"));
}

// ========================================================================
// GET endpoints — Balance
// ========================================================================

#[tokio::test]
async fn get_balance_success() {
    let app = setup().await;
    let (status, json) = get_json(&app, &format!("/v1/electrumx/balance/{ADDR}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["balance"]["confirmed"], 7000);
    assert_eq!(json["balance"]["unconfirmed"], 0);
}

#[tokio::test]
async fn get_balance_invalid_address() {
    let app = setup().await;
    let (status, json) = get_json(&app, "/v1/electrumx/balance/invalidaddr").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["success"], false);
}

// ========================================================================
// GET endpoints — Transactions
// ========================================================================

#[tokio::test]
async fn get_transactions_success() {
    let app = setup().await;
    let (status, json) = get_json(&app, &format!("/v1/electrumx/transactions/{ADDR}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["transactions"].is_array());
    assert_eq!(json["transactions"][0]["height"], 601861);
}

// ========================================================================
// GET endpoints — Unconfirmed
// ========================================================================

#[tokio::test]
async fn get_unconfirmed_success() {
    let app = setup().await;
    let (status, json) = get_json(&app, &format!("/v1/electrumx/unconfirmed/{ADDR}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["utxos"].is_array());
    assert_eq!(json["utxos"][0]["fee"], 24310);
}

// ========================================================================
// GET endpoints — Transaction details
// ========================================================================

#[tokio::test]
async fn get_tx_data_success() {
    let app = setup().await;
    let (status, json) = get_json(&app, &format!("/v1/electrumx/tx/data/{TXID}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["details"].is_object());
    assert_eq!(json["details"]["hash"], TXID);
    assert!(json["details"]["vin"].is_array());
    assert!(json["details"]["vout"].is_array());
}

// ========================================================================
// GET endpoints — Block headers
// ========================================================================

#[tokio::test]
async fn get_block_headers_success() {
    let app = setup().await;
    let (status, json) = get_json(&app, "/v1/electrumx/block/headers/42?count=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);

    let headers = json["headers"].as_array().unwrap();
    assert_eq!(headers.len(), 2);
    // Each header is 160 hex chars (80 bytes)
    assert_eq!(headers[0].as_str().unwrap().len(), 160);
    assert_eq!(headers[1].as_str().unwrap().len(), 160);

    // Verify exact header values from JS mock
    assert_eq!(
        headers[0].as_str().unwrap(),
        "010000008b52bbd72c2f49569059f559c1b1794de5192e4f7d6d2b03c7482bad0000000083e4f8a9d502ed0c419075c1abb5d56f878a2e9079e5612bfb76a2dc37d9c42741dd6849ffff001d2b909dd6"
    );
    assert_eq!(
        headers[1].as_str().unwrap(),
        "01000000f528fac1bcb685d0cd6c792320af0300a5ce15d687c7149548904e31000000004e8985a786d864f21e9cbb7cbdf4bc9265fe681b7a0893ac55a8e919ce035c2f85de6849ffff001d385ccb7c"
    );
}

#[tokio::test]
async fn get_block_headers_default_count() {
    let app = setup().await;
    let (status, json) = get_json(&app, "/v1/electrumx/block/headers/42").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    let headers = json["headers"].as_array().unwrap();
    assert_eq!(headers.len(), 1);
}

// ========================================================================
// POST — Broadcast
// ========================================================================

#[tokio::test]
async fn broadcast_tx_success() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/tx/broadcast",
        json!({ "txHex": "0200000001abcd" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["txid"], TXID);
}

// ========================================================================
// POST — Bulk UTXOs
// ========================================================================

#[tokio::test]
async fn utxos_bulk_success() {
    let app = setup().await;
    let (status, json) =
        post_json(&app, "/v1/electrumx/utxos", json!({ "addresses": [ADDR] })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["utxos"].is_array());

    let entry = &json["utxos"][0];
    assert_eq!(entry["address"], ADDR);
    assert!(entry["utxos"].is_array());
    assert_eq!(entry["utxos"][0]["height"], 604392);
}

#[tokio::test]
async fn utxos_bulk_multiple() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/utxos",
        json!({ "addresses": [ADDR, ADDR] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["utxos"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn utxos_bulk_too_large() {
    let app = setup().await;
    let addrs: Vec<String> = (0..21).map(|i| format!("addr{i}")).collect();
    let (status, json) =
        post_json(&app, "/v1/electrumx/utxos", json!({ "addresses": addrs })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("Array too large"));
}

#[tokio::test]
async fn utxos_bulk_invalid_address() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/utxos",
        json!({ "addresses": ["invalidaddr"] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["success"], false);
}

#[tokio::test]
async fn utxos_bulk_network_mismatch() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/utxos",
        json!({ "addresses": ["bchtest:qq89kjkeqz9mngp8kl3dpmu43y2wztdjqu500gn4c4"] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("invalid network"));
}

// ========================================================================
// POST — Bulk Balance
// ========================================================================

#[tokio::test]
async fn balance_bulk_success() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/balance",
        json!({ "addresses": [ADDR] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["balances"].is_array());
    assert_eq!(json["balances"][0]["address"], ADDR);
    assert_eq!(json["balances"][0]["balance"]["confirmed"], 7000);
}

#[tokio::test]
async fn balance_bulk_too_large() {
    let app = setup().await;
    let addrs: Vec<String> = (0..21).map(|i| format!("addr{i}")).collect();
    let (status, json) =
        post_json(&app, "/v1/electrumx/balance", json!({ "addresses": addrs })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("Array too large"));
}

// ========================================================================
// POST — Bulk Transactions
// ========================================================================

#[tokio::test]
async fn transactions_bulk_success() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/transactions",
        json!({ "addresses": [ADDR] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["transactions"].is_array());
    assert_eq!(json["transactions"][0]["address"], ADDR);
    assert!(json["transactions"][0]["transactions"].is_array());
}

// ========================================================================
// POST — Bulk Unconfirmed
// ========================================================================

#[tokio::test]
async fn unconfirmed_bulk_success() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/unconfirmed",
        json!({ "addresses": [ADDR] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["utxos"].is_array());
    assert_eq!(json["utxos"][0]["address"], ADDR);
}

// ========================================================================
// POST — Bulk TX data
// ========================================================================

#[tokio::test]
async fn tx_data_bulk_success() {
    let app = setup().await;
    let (status, json) = post_json(&app, "/v1/electrumx/tx/data", json!({ "txids": [TXID] })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["transactions"].is_array());
    assert_eq!(json["transactions"][0]["txid"], TXID);
    assert!(json["transactions"][0]["details"].is_object());
    assert_eq!(json["transactions"][0]["details"]["hash"], TXID);
}

#[tokio::test]
async fn tx_data_bulk_multiple() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/tx/data",
        json!({ "txids": [TXID, TXID] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["transactions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn tx_data_bulk_too_large() {
    let app = setup().await;
    let txids: Vec<String> = (0..21).map(|i| format!("txid{i}")).collect();
    let (status, json) = post_json(&app, "/v1/electrumx/tx/data", json!({ "txids": txids })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("Array too large"));
}

// ========================================================================
// POST — Bulk Block headers
// ========================================================================

#[tokio::test]
async fn block_headers_bulk_success() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/block/headers",
        json!({ "heights": [{ "height": 42, "count": 2 }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert!(json["headers"].is_array());
    assert_eq!(json["headers"][0]["height"], 42);
    assert!(json["headers"][0]["headers"].is_array());
    assert_eq!(json["headers"][0]["headers"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn block_headers_bulk_multiple() {
    let app = setup().await;
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/block/headers",
        json!({ "heights": [
            { "height": 42, "count": 2 },
            { "height": 100, "count": 1 }
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["headers"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn block_headers_bulk_too_large() {
    let app = setup().await;
    let heights: Vec<Value> = (0..21)
        .map(|i| json!({ "height": i, "count": 1 }))
        .collect();
    let (status, json) = post_json(
        &app,
        "/v1/electrumx/block/headers",
        json!({ "heights": heights }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("Array too large"));
}
