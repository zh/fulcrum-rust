use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Debug)]
pub enum AddressError {
    InvalidAddress(String),
    InvalidNetwork { expected: String, got: String },
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAddress(msg) => write!(f, "invalid address: {msg}"),
            Self::InvalidNetwork { expected, got } => {
                write!(f, "invalid network: expected {expected}, got {got}")
            }
        }
    }
}

impl std::error::Error for AddressError {}

/// Convert a cashaddr (e.g. `bitcoincash:qr...`) to the Electrum scripthash format.
///
/// Algorithm (matches JS `electrumx.js:1375-1413`):
/// 1. Decode cashaddr → hash_type + hash160 bytes
/// 2. Build output script:
///    - P2PKH: `OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG`
///    - P2SH:  `OP_HASH160 <20-byte-hash> OP_EQUAL`
/// 3. SHA256(script) → reverse bytes → hex
pub fn address_to_scripthash(addr: &str) -> Result<String, AddressError> {
    let decoded = bitcoincash_addr::Address::decode(addr)
        .map_err(|e| AddressError::InvalidAddress(format!("{e:?}")))?;

    let hash = &decoded.body;

    let script = match decoded.hash_type {
        bitcoincash_addr::HashType::Key => {
            // P2PKH: OP_DUP OP_HASH160 0x14 <hash160> OP_EQUALVERIFY OP_CHECKSIG
            let mut s = Vec::with_capacity(25);
            s.push(0x76); // OP_DUP
            s.push(0xa9); // OP_HASH160
            s.push(0x14); // push 20 bytes
            s.extend_from_slice(hash);
            s.push(0x88); // OP_EQUALVERIFY
            s.push(0xac); // OP_CHECKSIG
            s
        }
        bitcoincash_addr::HashType::Script => {
            // P2SH: OP_HASH160 0x14 <hash160> OP_EQUAL
            let mut s = Vec::with_capacity(23);
            s.push(0xa9); // OP_HASH160
            s.push(0x14); // push 20 bytes
            s.extend_from_slice(hash);
            s.push(0x87); // OP_EQUAL
            s
        }
    };

    let sha = Sha256::digest(&script);
    let mut reversed: Vec<u8> = sha.to_vec();
    reversed.reverse();

    Ok(hex::encode(reversed))
}

/// Validate that the decoded address network matches the configured network string.
pub fn validate_network(addr: &str, network: &str) -> Result<(), AddressError> {
    let decoded = bitcoincash_addr::Address::decode(addr)
        .map_err(|e| AddressError::InvalidAddress(format!("{e:?}")))?;

    let expected = match network {
        "mainnet" => bitcoincash_addr::Network::Main,
        "testnet" | "testnet3" => bitcoincash_addr::Network::Test,
        "regtest" => bitcoincash_addr::Network::Regtest,
        other => {
            return Err(AddressError::InvalidNetwork {
                expected: other.to_string(),
                got: format!("{:?}", decoded.network),
            })
        }
    };

    if decoded.network != expected {
        return Err(AddressError::InvalidNetwork {
            expected: network.to_string(),
            got: format!("{:?}", decoded.network),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p2pkh_scripthash() {
        // Test vector from fulcrum-api JS test suite
        let addr = "bitcoincash:qpr270a5sxphltdmggtj07v4nskn9gmg9yx4m5h7s4";
        let result = address_to_scripthash(addr).unwrap();
        assert_eq!(
            result,
            "bce4d5f2803bd1ed7c1ba00dcb3edffcbba50524af7c879d6bb918d04f138965"
        );
    }

    #[test]
    fn p2sh_scripthash() {
        // Test vector from fulcrum-api JS test suite
        let addr = "bitcoincash:pz0z7u9p96h2p6hfychxdrmwgdlzpk5luc5yks2wxq";
        let result = address_to_scripthash(addr).unwrap();
        assert_eq!(
            result,
            "8bc2235c8e7d5634d9ec429fc0171f2c58e728d4f1e2fb7e440e313133cfa4f0"
        );
    }

    #[test]
    fn invalid_address_returns_error() {
        let result = address_to_scripthash("not_a_valid_address");
        assert!(result.is_err());
        match result.unwrap_err() {
            AddressError::InvalidAddress(_) => {}
            other => panic!("expected InvalidAddress, got: {other}"),
        }
    }

    #[test]
    fn validate_mainnet_address() {
        let addr = "bitcoincash:qr6m7j9njldwwzlg9v7v53unlr4jkmx6eylep8ekg2";
        assert!(validate_network(addr, "mainnet").is_ok());
    }

    #[test]
    fn reject_testnet_on_mainnet() {
        // Encode a known hash as a testnet address
        use bitcoincash_addr::{Address, HashType, Network, Scheme};
        let hash = hex::decode("F5BF48B397DAE70BE82B3CCA4793F8EB2B6CDAC9").unwrap();
        let addr = Address::new(hash, Scheme::CashAddr, HashType::Key, Network::Test);
        let addr_str = addr.encode().unwrap();
        let result = validate_network(&addr_str, "mainnet");
        assert!(result.is_err());
        match result.unwrap_err() {
            AddressError::InvalidNetwork { .. } => {}
            other => panic!("expected InvalidNetwork, got: {other}"),
        }
    }

    #[test]
    fn error_display() {
        let e = AddressError::InvalidAddress("bad addr".into());
        assert_eq!(e.to_string(), "invalid address: bad addr");

        let e = AddressError::InvalidNetwork {
            expected: "mainnet".into(),
            got: "Test".into(),
        };
        assert_eq!(e.to_string(), "invalid network: expected mainnet, got Test");
    }
}
