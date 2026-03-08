pub mod address;
pub mod config;
pub mod electrum;
pub mod handlers;
pub mod pool;

pub use config::Config;
pub use pool::ElectrumPool;

use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: Arc<ElectrumPool>,
    pub network: String,
}

pub async fn health_check(State(state): State<AppState>) -> Json<Value> {
    let connected = state.pool.is_connected().await;
    Json(json!({
        "status": if connected { "electrumx" } else { "disconnected" },
        "fulcrum": connected,
    }))
}
