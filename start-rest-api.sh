#!/bin/bash

export FULCRUM_URL=127.0.0.1
export FULCRUM_PORT=50002
export FULCRUM_TLS=true
export FULCRUM_POOL_SIZE=4
export NETWORK=mainnet
export PORT=3001
# export RUST_LOG=fulcrum_rust=info,tower_http=info
# export RUST_BACKTRACE=full

cargo run --release
