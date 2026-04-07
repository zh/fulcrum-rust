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

// ── Cashaddr type bytes ──────────────────────────────────────────────
// Standard types (handled by bitcoincash-addr crate):
//   0x00 = P2PKH (q-prefix)
//   0x08 = P2SH  (p-prefix)
// CashToken types (NOT handled by the crate, same hash160):
//   0x10 = token-aware P2PKH (z-prefix)
//   0x18 = token-aware P2SH  (r-prefix)
const TYPE_MASK: u8 = 0x78;
const TYPE_P2PKH: u8 = 0x00;
const TYPE_P2SH: u8 = 0x08;
const TYPE_TOKEN_P2PKH: u8 = 0x10;
const TYPE_TOKEN_P2SH: u8 = 0x18;

/// Script type for building the output script.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ScriptType {
    P2PKH,
    P2SH,
}

/// Decoded cashaddr: network prefix, script type, and hash160.
struct DecodedAddr {
    network: String,
    script_type: ScriptType,
    hash: Vec<u8>,
}

/// Decode any cashaddr including CashToken z/r-prefix addresses.
///
/// First tries the `bitcoincash-addr` crate. If it fails (e.g. on token-aware
/// type bytes), falls back to a minimal raw decode that extracts the version
/// byte and hash160 directly.
fn decode_cashaddr(addr: &str) -> Result<DecodedAddr, AddressError> {
    // Fast path: crate handles standard q/p addresses
    if let Ok(decoded) = bitcoincash_addr::Address::decode(addr) {
        let script_type = match decoded.hash_type {
            bitcoincash_addr::HashType::Key => ScriptType::P2PKH,
            bitcoincash_addr::HashType::Script => ScriptType::P2SH,
        };
        let network = match decoded.network {
            bitcoincash_addr::Network::Main => "mainnet",
            bitcoincash_addr::Network::Test => "testnet",
            bitcoincash_addr::Network::Regtest => "regtest",
        };
        return Ok(DecodedAddr {
            network: network.to_string(),
            script_type,
            hash: decoded.body,
        });
    }

    // Slow path: handle token-aware z/r-prefix addresses
    decode_token_cashaddr(addr)
}

// ── Minimal cashaddr bech32 decode for token types ──────────────────

const CHARSET_REV: [Option<u8>; 128] = [
    None,     None,     None,     None,     None,     None,     None,     None,
    None,     None,     None,     None,     None,     None,     None,     None,
    None,     None,     None,     None,     None,     None,     None,     None,
    None,     None,     None,     None,     None,     None,     None,     None,
    None,     None,     None,     None,     None,     None,     None,     None,
    None,     None,     None,     None,     None,     None,     None,     None,
    Some(15), None,     Some(10), Some(17), Some(21), Some(20), Some(26), Some(30),
    Some(7),  Some(5),  None,     None,     None,     None,     None,     None,
    None,     Some(29), None,     Some(24), Some(13), Some(25), Some(9),  Some(8),
    Some(23), None,     Some(18), Some(22), Some(31), Some(27), Some(19), None,
    Some(1),  Some(0),  Some(3),  Some(16), Some(11), Some(28), Some(12), Some(14),
    Some(6),  Some(4),  Some(2),  None,     None,     None,     None,     None,
    None,     Some(29), None,     Some(24), Some(13), Some(25), Some(9),  Some(8),
    Some(23), None,     Some(18), Some(22), Some(31), Some(27), Some(19), None,
    Some(1),  Some(0),  Some(3),  Some(16), Some(11), Some(28), Some(12), Some(14),
    Some(6),  Some(4),  Some(2),  None,     None,     None,     None,     None,
];

fn polymod(v: &[u8]) -> u64 {
    let mut c: u64 = 1;
    for d in v.iter() {
        let c0 = (c >> 35) as u8;
        c = ((c & 0x0007_ffff_ffff) << 5) ^ u64::from(*d);
        if c0 & 0x01 != 0 { c ^= 0x0098_f2bc_8e61; }
        if c0 & 0x02 != 0 { c ^= 0x0079_b76d_99e2; }
        if c0 & 0x04 != 0 { c ^= 0x00f3_3e5f_b3c4; }
        if c0 & 0x08 != 0 { c ^= 0x00ae_2eab_e2a8; }
        if c0 & 0x10 != 0 { c ^= 0x001e_4f43_e470; }
    }
    c ^ 1
}

fn expand_prefix(prefix: &str) -> Vec<u8> {
    let mut ret: Vec<u8> = prefix.chars().map(|c| (c as u8) & 0x1f).collect();
    ret.push(0);
    ret
}

fn convert_bits(data: &[u8], inbits: u8, outbits: u8) -> Vec<u8> {
    let num_bytes = (data.len() * inbits as usize).div_ceil(outbits as usize);
    let mut ret = Vec::with_capacity(num_bytes);
    let mut acc: u16 = 0;
    let mut num: u8 = 0;
    let groupmask = (1 << outbits) - 1;
    for d in data.iter() {
        acc = (acc << inbits) | u16::from(*d);
        num += inbits;
        while num > outbits {
            ret.push((acc >> (num - outbits)) as u8);
            acc &= !(groupmask << (num - outbits));
            num -= outbits;
        }
    }
    // No padding: extract remaining bits if meaningful
    let padding = (data.len() * inbits as usize) % outbits as usize;
    if num as usize > padding {
        ret.push((acc >> padding) as u8);
    }
    ret
}

fn decode_token_cashaddr(addr: &str) -> Result<DecodedAddr, AddressError> {
    let parts: Vec<&str> = addr.split(':').collect();
    if parts.len() != 2 {
        return Err(AddressError::InvalidAddress("missing prefix".into()));
    }
    let prefix = parts[0];
    let payload_str = parts[1];

    let network = match prefix {
        "bitcoincash" => "mainnet",
        "bchtest" => "testnet",
        "bchreg" => "regtest",
        _ => return Err(AddressError::InvalidAddress(format!("unknown prefix: {prefix}"))),
    };

    // Decode base32 payload
    let payload_5_bits: Result<Vec<u8>, _> = payload_str
        .chars()
        .map(|c| {
            let i = c as usize;
            CHARSET_REV
                .get(i)
                .and_then(|v| *v)
                .ok_or_else(|| AddressError::InvalidAddress(format!("invalid char: {c}")))
        })
        .collect();
    let payload_5_bits = payload_5_bits?;

    // Verify checksum
    let checksum = polymod(&[&expand_prefix(prefix), &payload_5_bits[..]].concat());
    if checksum != 0 {
        return Err(AddressError::InvalidAddress("checksum failed".into()));
    }

    // Convert from 5-bit to 8-bit (strip 8 checksum characters)
    let len = payload_5_bits.len();
    let payload = convert_bits(&payload_5_bits[..(len - 8)], 5, 8);

    let version = payload[0];
    let body = &payload[1..];

    if body.len() != 20 {
        return Err(AddressError::InvalidAddress(format!(
            "unexpected hash length: {}",
            body.len()
        )));
    }

    let version_type = version & TYPE_MASK;
    let script_type = match version_type {
        TYPE_P2PKH | TYPE_TOKEN_P2PKH => ScriptType::P2PKH,
        TYPE_P2SH | TYPE_TOKEN_P2SH => ScriptType::P2SH,
        _ => {
            return Err(AddressError::InvalidAddress(format!(
                "unsupported version byte: 0x{version:02x}"
            )))
        }
    };

    Ok(DecodedAddr {
        network: network.to_string(),
        script_type,
        hash: body.to_vec(),
    })
}

// ── Public API ───────────────────────────────────────────────────────

/// Convert a cashaddr (including CashToken z/r-prefix) to Electrum scripthash.
///
/// Algorithm:
/// 1. Decode cashaddr → script_type + hash160 bytes
/// 2. Build output script:
///    - P2PKH: `OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG`
///    - P2SH:  `OP_HASH160 <20-byte-hash> OP_EQUAL`
/// 3. SHA256(script) → reverse bytes → hex
///
/// Token-aware addresses (z/r-prefix) share the same hash160 as their
/// regular counterparts (q/p-prefix), so the scripthash is identical.
pub fn address_to_scripthash(addr: &str) -> Result<String, AddressError> {
    let decoded = decode_cashaddr(addr)?;

    let script = match decoded.script_type {
        ScriptType::P2PKH => {
            let mut s = Vec::with_capacity(25);
            s.push(0x76); // OP_DUP
            s.push(0xa9); // OP_HASH160
            s.push(0x14); // push 20 bytes
            s.extend_from_slice(&decoded.hash);
            s.push(0x88); // OP_EQUALVERIFY
            s.push(0xac); // OP_CHECKSIG
            s
        }
        ScriptType::P2SH => {
            let mut s = Vec::with_capacity(23);
            s.push(0xa9); // OP_HASH160
            s.push(0x14); // push 20 bytes
            s.extend_from_slice(&decoded.hash);
            s.push(0x87); // OP_EQUAL
            s
        }
    };

    let sha = Sha256::digest(&script);
    let mut reversed: Vec<u8> = sha.to_vec();
    reversed.reverse();

    Ok(hex::encode(reversed))
}

/// Validate that the address network matches the configured network string.
/// Supports both regular (q/p) and token-aware (z/r) address prefixes.
pub fn validate_network(addr: &str, network: &str) -> Result<(), AddressError> {
    let decoded = decode_cashaddr(addr)?;

    let expected = match network {
        "mainnet" => "mainnet",
        "testnet" | "testnet3" => "testnet",
        "regtest" => "regtest",
        other => {
            return Err(AddressError::InvalidNetwork {
                expected: other.to_string(),
                got: decoded.network,
            })
        }
    };

    if decoded.network != expected {
        return Err(AddressError::InvalidNetwork {
            expected: network.to_string(),
            got: decoded.network,
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
    fn z_prefix_same_scripthash_as_q_prefix() {
        // z-prefix (token-aware P2PKH) and q-prefix (regular P2PKH)
        // share the same hash160, so scripthash must be identical.
        let q_addr = "bitcoincash:qzx8rpy6ravy6uz5gpchjt83z94srp8y0u2nj0yw7d";
        let z_addr = "bitcoincash:zzx8rpy6ravy6uz5gpchjt83z94srp8y0udep32gp7";

        let q_hash = address_to_scripthash(q_addr).unwrap();
        let z_hash = address_to_scripthash(z_addr).unwrap();
        assert_eq!(q_hash, z_hash);
    }

    #[test]
    fn z_prefix_validates_mainnet() {
        let z_addr = "bitcoincash:zzx8rpy6ravy6uz5gpchjt83z94srp8y0udep32gp7";
        assert!(validate_network(z_addr, "mainnet").is_ok());
    }

    #[test]
    fn z_prefix_rejects_wrong_network() {
        let z_addr = "bitcoincash:zzx8rpy6ravy6uz5gpchjt83z94srp8y0udep32gp7";
        let result = validate_network(z_addr, "testnet");
        assert!(result.is_err());
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
