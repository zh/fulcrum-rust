use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::address::{address_to_scripthash, validate_network, AddressError};
use crate::AppState;

const MAX_BULK_SIZE: usize = 20;

// --- Query / body types ---

#[derive(Deserialize)]
pub struct BlockHeadersQuery {
    pub count: Option<u64>,
}

#[derive(Deserialize)]
pub struct TxDetailsQuery {
    pub verbose: Option<bool>,
}

#[derive(Deserialize)]
pub struct AddressesBulk {
    pub addresses: Vec<String>,
}

#[derive(Deserialize)]
pub struct TxidsBulk {
    pub txids: Vec<String>,
    pub verbose: Option<bool>,
}

#[derive(Deserialize)]
pub struct BroadcastBody {
    #[serde(rename = "txHex")]
    pub tx_hex: String,
}

#[derive(Deserialize)]
pub struct HeightsBulk {
    pub heights: Vec<HeightCount>,
}

#[derive(Deserialize)]
pub struct HeightCount {
    pub height: u64,
    pub count: u64,
}

// --- Error helper ---

fn error_response(status: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "success": false, "error": msg })))
}

fn electrum_error(e: crate::electrum::ElectrumError) -> (StatusCode, Json<Value>) {
    use crate::electrum::ElectrumError;
    let status = match &e {
        ElectrumError::Rpc { .. } => StatusCode::BAD_REQUEST,
        ElectrumError::Connection(_) => StatusCode::SERVICE_UNAVAILABLE,
        ElectrumError::Protocol(_) => StatusCode::UNPROCESSABLE_ENTITY,
    };
    warn!("electrum error: {e}");
    error_response(status, &e.to_string())
}

fn addr_error(e: AddressError) -> (StatusCode, Json<Value>) {
    error_response(StatusCode::BAD_REQUEST, &e.to_string())
}

/// Resolve address to scripthash, validating network.
fn resolve_scripthash(addr: &str, network: &str) -> Result<String, (StatusCode, Json<Value>)> {
    validate_network(addr, network).map_err(addr_error)?;
    address_to_scripthash(addr).map_err(addr_error)
}

/// Split block headers hex into 160-char (80-byte) chunks.
fn split_headers_hex(hex: &str) -> Vec<&str> {
    hex.as_bytes()
        .chunks(160)
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .collect()
}

// ========================================================================
// GET handlers — single address/txid/height
// ========================================================================

/// GET /utxos/:address
pub async fn get_utxos(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let scripthash = resolve_scripthash(&address, &state.network)?;
    let result = state
        .pool
        .request("blockchain.scripthash.listunspent", json!([scripthash]))
        .await
        .map_err(electrum_error)?;
    Ok(Json(json!({ "success": true, "utxos": result })))
}

/// GET /balance/:address
pub async fn get_balance(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let scripthash = resolve_scripthash(&address, &state.network)?;
    let result = state
        .pool
        .request("blockchain.scripthash.get_balance", json!([scripthash]))
        .await
        .map_err(electrum_error)?;
    Ok(Json(json!({ "success": true, "balance": result })))
}

/// GET /transactions/:address
pub async fn get_transactions(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let scripthash = resolve_scripthash(&address, &state.network)?;
    let result = state
        .pool
        .request("blockchain.scripthash.get_history", json!([scripthash]))
        .await
        .map_err(electrum_error)?;
    Ok(Json(json!({ "success": true, "transactions": result })))
}

/// GET /unconfirmed/:address
pub async fn get_unconfirmed(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let scripthash = resolve_scripthash(&address, &state.network)?;
    let result = state
        .pool
        .request("blockchain.scripthash.get_mempool", json!([scripthash]))
        .await
        .map_err(electrum_error)?;
    Ok(Json(json!({ "success": true, "utxos": result })))
}

/// GET /tx/data/:txid
pub async fn get_tx_data(
    State(state): State<AppState>,
    Path(txid): Path<String>,
    Query(query): Query<TxDetailsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let verbose = query.verbose.unwrap_or(true);
    let result = state
        .pool
        .request("blockchain.transaction.get", json!([txid, verbose]))
        .await
        .map_err(electrum_error)?;
    Ok(Json(json!({ "success": true, "details": result })))
}

/// GET /block/headers/:height
pub async fn get_block_headers(
    State(state): State<AppState>,
    Path(height): Path<u64>,
    Query(query): Query<BlockHeadersQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let count = query.count.unwrap_or(1);
    let result = state
        .pool
        .request("blockchain.block.headers", json!([height, count]))
        .await
        .map_err(electrum_error)?;

    // Split hex into individual 80-byte header hex strings
    let headers = if let Some(hex_str) = result.get("hex").and_then(Value::as_str) {
        split_headers_hex(hex_str)
    } else {
        vec![]
    };

    Ok(Json(json!({ "success": true, "headers": headers })))
}

// ========================================================================
// POST handlers — broadcast
// ========================================================================

/// POST /tx/broadcast
pub async fn broadcast_tx(
    State(state): State<AppState>,
    Json(body): Json<BroadcastBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let result = state
        .pool
        .request("blockchain.transaction.broadcast", json!([body.tx_hex]))
        .await
        .map_err(electrum_error)?;
    Ok(Json(json!({ "success": true, "txid": result })))
}

// ========================================================================
// POST handlers — bulk address operations
// ========================================================================

fn validate_bulk_size<T>(items: &[T], name: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if items.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            &format!("{name} array must not be empty"),
        ));
    }
    if items.len() > MAX_BULK_SIZE {
        return Err(error_response(StatusCode::BAD_REQUEST, "Array too large."));
    }
    Ok(())
}

/// POST /utxos — bulk
pub async fn utxos_bulk(
    State(state): State<AppState>,
    Json(body): Json<AddressesBulk>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_bulk_size(&body.addresses, "addresses")?;

    let mut results = Vec::with_capacity(body.addresses.len());
    for address in &body.addresses {
        let scripthash = resolve_scripthash(address, &state.network)?;
        let utxos = state
            .pool
            .request("blockchain.scripthash.listunspent", json!([scripthash]))
            .await
            .map_err(electrum_error)?;
        results.push(json!({ "utxos": utxos, "address": address }));
    }

    Ok(Json(json!({ "success": true, "utxos": results })))
}

/// POST /balance — bulk
pub async fn balance_bulk(
    State(state): State<AppState>,
    Json(body): Json<AddressesBulk>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_bulk_size(&body.addresses, "addresses")?;

    let mut results = Vec::with_capacity(body.addresses.len());
    for address in &body.addresses {
        let scripthash = resolve_scripthash(address, &state.network)?;
        let balance = state
            .pool
            .request("blockchain.scripthash.get_balance", json!([scripthash]))
            .await
            .map_err(electrum_error)?;
        results.push(json!({ "balance": balance, "address": address }));
    }

    Ok(Json(json!({ "success": true, "balances": results })))
}

/// POST /transactions — bulk
pub async fn transactions_bulk(
    State(state): State<AppState>,
    Json(body): Json<AddressesBulk>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_bulk_size(&body.addresses, "addresses")?;

    let mut results = Vec::with_capacity(body.addresses.len());
    for address in &body.addresses {
        let scripthash = resolve_scripthash(address, &state.network)?;
        let txs = state
            .pool
            .request("blockchain.scripthash.get_history", json!([scripthash]))
            .await
            .map_err(electrum_error)?;
        results.push(json!({ "transactions": txs, "address": address }));
    }

    Ok(Json(json!({ "success": true, "transactions": results })))
}

/// POST /unconfirmed — bulk
pub async fn unconfirmed_bulk(
    State(state): State<AppState>,
    Json(body): Json<AddressesBulk>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_bulk_size(&body.addresses, "addresses")?;

    let mut results = Vec::with_capacity(body.addresses.len());
    for address in &body.addresses {
        let scripthash = resolve_scripthash(address, &state.network)?;
        let mempool = state
            .pool
            .request("blockchain.scripthash.get_mempool", json!([scripthash]))
            .await
            .map_err(electrum_error)?;
        results.push(json!({ "utxos": mempool, "address": address }));
    }

    Ok(Json(json!({ "success": true, "utxos": results })))
}

/// POST /tx/data — bulk
pub async fn tx_data_bulk(
    State(state): State<AppState>,
    Json(body): Json<TxidsBulk>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_bulk_size(&body.txids, "txids")?;

    let verbose = body.verbose.unwrap_or(true);
    let mut results = Vec::with_capacity(body.txids.len());
    for txid in &body.txids {
        let details = state
            .pool
            .request("blockchain.transaction.get", json!([txid, verbose]))
            .await
            .map_err(electrum_error)?;
        results.push(json!({ "details": details, "txid": txid }));
    }

    Ok(Json(json!({ "success": true, "transactions": results })))
}

/// POST /block/headers — bulk
pub async fn block_headers_bulk(
    State(state): State<AppState>,
    Json(body): Json<HeightsBulk>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_bulk_size(&body.heights, "heights")?;

    let mut results = Vec::with_capacity(body.heights.len());
    for hc in &body.heights {
        let result = state
            .pool
            .request("blockchain.block.headers", json!([hc.height, hc.count]))
            .await
            .map_err(electrum_error)?;

        let headers = if let Some(hex_str) = result.get("hex").and_then(Value::as_str) {
            split_headers_hex(hex_str)
        } else {
            vec![]
        };

        results.push(json!({ "headers": headers, "height": hc.height }));
    }

    Ok(Json(json!({ "success": true, "headers": results })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_response_format() {
        let (status, Json(body)) = error_response(StatusCode::BAD_REQUEST, "test error");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["success"], false);
        assert_eq!(body["error"], "test error");
    }

    #[test]
    fn bulk_size_empty_rejected() {
        let empty: Vec<String> = vec![];
        let result = validate_bulk_size(&empty, "addresses");
        assert!(result.is_err());
    }

    #[test]
    fn bulk_size_over_limit_rejected() {
        let too_many: Vec<String> = (0..21).map(|i| format!("addr{i}")).collect();
        let result = validate_bulk_size(&too_many, "addresses");
        assert!(result.is_err());
        let (status, Json(body)) = result.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "Array too large.");
    }

    #[test]
    fn bulk_size_at_limit_accepted() {
        let exactly_20: Vec<String> = (0..20).map(|i| format!("addr{i}")).collect();
        assert!(validate_bulk_size(&exactly_20, "addresses").is_ok());
    }

    #[test]
    fn split_headers_hex_single() {
        // 160 hex chars = one 80-byte header
        let hex = "a".repeat(160);
        let result = split_headers_hex(&hex);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 160);
    }

    #[test]
    fn split_headers_hex_multiple() {
        // 320 hex chars = two 80-byte headers
        let hex = format!("{}{}", "a".repeat(160), "b".repeat(160));
        let result = split_headers_hex(&hex);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "a".repeat(160));
        assert_eq!(result[1], "b".repeat(160));
    }

    #[test]
    fn split_headers_hex_empty() {
        let result = split_headers_hex("");
        assert!(result.is_empty());
    }
}
