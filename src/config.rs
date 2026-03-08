use std::env;
use std::fmt;

pub struct Config {
    pub fulcrum_host: String,
    pub fulcrum_port: u16,
    pub fulcrum_tls: bool,
    pub fulcrum_pool_size: usize,
    pub port: u16,
    pub network: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self::parse(|key| env::var(key).ok())
    }

    fn parse(get: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            fulcrum_host: get("FULCRUM_URL").unwrap_or_else(|| "127.0.0.1".into()),
            fulcrum_port: get("FULCRUM_PORT")
                .and_then(|p| p.parse().ok())
                .unwrap_or(50001),
            fulcrum_tls: get("FULCRUM_TLS")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            fulcrum_pool_size: get("FULCRUM_POOL_SIZE")
                .and_then(|p| p.parse().ok())
                .unwrap_or(4),
            port: get("PORT").and_then(|p| p.parse().ok()).unwrap_or(3000),
            network: get("NETWORK").unwrap_or_else(|| "mainnet".into()),
        }
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fulcrum={}:{}{} pool={} port={} network={}",
            self.fulcrum_host,
            self.fulcrum_port,
            if self.fulcrum_tls { " (TLS)" } else { "" },
            self.fulcrum_pool_size,
            self.port,
            self.network
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn defaults_when_no_env() {
        let cfg = Config::parse(|_| None);
        assert_eq!(cfg.fulcrum_host, "127.0.0.1");
        assert_eq!(cfg.fulcrum_port, 50001);
        assert!(!cfg.fulcrum_tls);
        assert_eq!(cfg.fulcrum_pool_size, 4);
        assert_eq!(cfg.port, 3000);
        assert_eq!(cfg.network, "mainnet");
    }

    #[test]
    fn parses_all_vars() {
        let cfg = Config::parse(env_from(&[
            ("FULCRUM_URL", "10.0.0.5"),
            ("FULCRUM_PORT", "60001"),
            ("FULCRUM_TLS", "true"),
            ("PORT", "8080"),
            ("NETWORK", "testnet3"),
        ]));
        assert_eq!(cfg.fulcrum_host, "10.0.0.5");
        assert_eq!(cfg.fulcrum_port, 60001);
        assert!(cfg.fulcrum_tls);
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.network, "testnet3");
    }

    #[test]
    fn tls_flag_variations() {
        let cfg = Config::parse(env_from(&[("FULCRUM_TLS", "1")]));
        assert!(cfg.fulcrum_tls);

        let cfg = Config::parse(env_from(&[("FULCRUM_TLS", "false")]));
        assert!(!cfg.fulcrum_tls);

        let cfg = Config::parse(env_from(&[("FULCRUM_TLS", "0")]));
        assert!(!cfg.fulcrum_tls);
    }

    #[test]
    fn invalid_port_falls_back_to_default() {
        let cfg = Config::parse(env_from(&[
            ("FULCRUM_PORT", "not_a_number"),
            ("PORT", "also_bad"),
        ]));
        assert_eq!(cfg.fulcrum_port, 50001);
        assert_eq!(cfg.port, 3000);
    }

    #[test]
    fn display_format() {
        let cfg = Config {
            fulcrum_host: "localhost".into(),
            fulcrum_port: 50001,
            fulcrum_tls: false,
            fulcrum_pool_size: 4,
            port: 3000,
            network: "mainnet".into(),
        };
        assert_eq!(
            cfg.to_string(),
            "fulcrum=localhost:50001 pool=4 port=3000 network=mainnet"
        );
    }

    #[test]
    fn display_format_with_tls() {
        let cfg = Config {
            fulcrum_host: "localhost".into(),
            fulcrum_port: 50002,
            fulcrum_tls: true,
            fulcrum_pool_size: 4,
            port: 3000,
            network: "mainnet".into(),
        };
        assert_eq!(
            cfg.to_string(),
            "fulcrum=localhost:50002 (TLS) pool=4 port=3000 network=mainnet"
        );
    }
}
