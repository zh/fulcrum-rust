use crate::electrum::{ElectrumClient, ElectrumError};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::info;

/// A round-robin pool of ElectrumClient connections.
pub struct ElectrumPool {
    clients: Vec<Arc<ElectrumClient>>,
    next: AtomicUsize,
}

impl ElectrumPool {
    /// Create a pool with `size` clients, all pointing to the same Fulcrum server.
    pub fn new(host: &str, port: u16, tls: bool, size: usize) -> Self {
        let size = size.max(1);
        let clients: Vec<Arc<ElectrumClient>> = (0..size)
            .map(|_| Arc::new(ElectrumClient::with_tls(host.to_string(), port, tls)))
            .collect();
        Self {
            clients,
            next: AtomicUsize::new(0),
        }
    }

    /// Connect all clients in the pool. Logs errors but doesn't fail.
    pub async fn connect_all(&self) {
        for (i, client) in self.clients.iter().enumerate() {
            match client.connect().await {
                Ok(()) => info!("pool connection {i}/{} ready", self.clients.len()),
                Err(e) => info!("pool connection {i} failed: {e} (will retry on use)"),
            }
        }
    }

    /// Get the next client via round-robin.
    pub fn get(&self) -> Arc<ElectrumClient> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        self.clients[idx].clone()
    }

    /// Send a request using the next available connection.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, ElectrumError> {
        self.get().request(method, params).await
    }

    /// Check if any connection in the pool is active.
    pub async fn is_connected(&self) -> bool {
        for client in &self.clients {
            if client.is_connected().await {
                return true;
            }
        }
        false
    }

    /// Pool size.
    pub fn size(&self) -> usize {
        self.clients.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_creates_correct_size() {
        let pool = ElectrumPool::new("127.0.0.1", 50001, false, 4);
        assert_eq!(pool.size(), 4);
    }

    #[test]
    fn pool_minimum_size_is_one() {
        let pool = ElectrumPool::new("127.0.0.1", 50001, false, 0);
        assert_eq!(pool.size(), 1);
    }

    #[test]
    fn round_robin_distribution() {
        let pool = ElectrumPool::new("127.0.0.1", 50001, false, 3);
        // Get 6 clients — should cycle through indices 0,1,2,0,1,2
        let ptrs: Vec<usize> = (0..6).map(|_| Arc::as_ptr(&pool.get()) as usize).collect();
        // First 3 should all be different
        assert_ne!(ptrs[0], ptrs[1]);
        assert_ne!(ptrs[1], ptrs[2]);
        // Cycle repeats
        assert_eq!(ptrs[0], ptrs[3]);
        assert_eq!(ptrs[1], ptrs[4]);
        assert_eq!(ptrs[2], ptrs[5]);
    }

    #[tokio::test]
    async fn pool_not_connected_initially() {
        let pool = ElectrumPool::new("127.0.0.1", 59999, false, 2);
        assert!(!pool.is_connected().await);
    }
}
