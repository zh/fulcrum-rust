use axum::{
    http::{header, StatusCode},
    routing::{get, post},
    Router, ServiceExt,
};
use fulcrum_rust::{handlers, health_check, AppState, Config, ElectrumPool};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower::Layer;
use tower_http::cors::CorsLayer;
use tower_http::normalize_path::NormalizePathLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fulcrum_rust=info,tower_http=debug".into()),
        )
        .init();

    // Install the rustls crypto provider before any TLS connections.
    // `let _ =` ignores "already installed" errors on reconnect paths.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cfg = Config::from_env();
    info!("config: {cfg}");

    let pool = Arc::new(ElectrumPool::new(
        &cfg.fulcrum_host,
        cfg.fulcrum_port,
        cfg.fulcrum_tls,
        cfg.fulcrum_pool_size,
    ));

    pool.connect_all().await;

    let state = AppState {
        pool,
        network: cfg.network,
    };

    // CORS: match Express cors() defaults — allow all origins
    let cors = CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(vec![header::CONTENT_TYPE, header::ACCEPT]);

    let router = Router::new()
        .route("/v1/electrumx/", get(health_check))
        // Single-address GET routes
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
        // TX data & broadcast
        .route("/v1/electrumx/tx/data/{txid}", get(handlers::get_tx_data))
        .route("/v1/electrumx/tx/broadcast", post(handlers::broadcast_tx))
        // Block headers
        .route(
            "/v1/electrumx/block/headers/{height}",
            get(handlers::get_block_headers),
        )
        // Bulk POST routes
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
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(300),
        ))
        .with_state(state);

    // Strip trailing slashes before route matching so that
    // POST /v1/electrumx/utxos/ matches the /v1/electrumx/utxos route.
    let app = NormalizePathLayer::trim_trailing_slash().layer(router);

    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.port));
    info!("fulcrum-rust listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    // Graceful shutdown on SIGTERM/SIGINT
    axum::serve(
        listener,
        ServiceExt::<axum::http::Request<axum::body::Body>>::into_make_service(app),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .unwrap();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received Ctrl+C, shutting down"),
        _ = terminate => info!("received SIGTERM, shutting down"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use fulcrum_rust::ElectrumPool;
    use tower::ServiceExt;

    fn test_app() -> Router {
        let state = AppState {
            pool: Arc::new(ElectrumPool::new("127.0.0.1", 59999, false, 1)),
            network: "mainnet".into(),
        };
        Router::new()
            .route("/v1/electrumx/", get(health_check))
            .with_state(state)
    }

    #[tokio::test]
    async fn test_health_check_disconnected() {
        let app = test_app();

        let response: axum::http::Response<Body> = app
            .oneshot(
                Request::builder()
                    .uri("/v1/electrumx/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "disconnected");
        assert_eq!(json["fulcrum"], false);
    }
}
