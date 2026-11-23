//! Verification of the signed GeoSquare V2 registry database.
//!
//! The registry sidecar signs the raw SHA-256 digest of the SQLite database.
//! This module checks the digest, the Ed25519 signature, and the trusted key.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    pub key_id: String,
    pub sha256: String,
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn string_field<'a>(value: &'a serde_json::Value, name: &str) -> Result<&'a str, String> {
    value
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("registry signature field {name:?} must be a string"))
}

/// Verify a registry database against its signature sidecar and trust map.
pub fn verify_bytes(
    database: &[u8],
    signature_json: &str,
    trust_json: &str,
) -> Result<Verification, String> {
    let sidecar: serde_json::Value = serde_json::from_str(signature_json)
        .map_err(|error| format!("registry signature is not valid JSON: {error}"))?;
    let algorithm = string_field(&sidecar, "algorithm")?;
    if algorithm != "Ed25519" {
        return Err(format!(
            "unsupported registry signature algorithm {algorithm:?}"
        ));
    }
    let key_id = string_field(&sidecar, "key_id")?.to_string();
    let signature_text = string_field(&sidecar, "value")?;
    let declared_digest = string_field(&sidecar, "sha256")?;

    let digest = Sha256::digest(database);
    let digest_hex = hex_digest(&digest);
    if declared_digest != digest_hex {
        return Err(format!(
            "registry SHA-256 mismatch: sidecar={declared_digest}, actual={digest_hex}"
        ));
    }

    let signature_bytes = STANDARD
        .decode(signature_text)
        .map_err(|error| format!("registry signature is not valid base64: {error}"))?;
    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| "registry signature must contain 64 bytes".to_string())?;
    let signature = Signature::from_bytes(&signature_array);

    let trust: serde_json::Value = serde_json::from_str(trust_json)
        .map_err(|error| format!("registry trust map is not valid JSON: {error}"))?;
    let public_text = trust
        .get(&key_id)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("registry key {key_id:?} is not trusted"))?;
    let public_bytes = STANDARD
        .decode(public_text)
        .map_err(|error| format!("registry public key is not valid base64: {error}"))?;
    let public_array: [u8; 32] = public_bytes
        .try_into()
        .map_err(|_| "registry Ed25519 public key must contain 32 bytes".to_string())?;
    let public_key = VerifyingKey::from_bytes(&public_array)
        .map_err(|error| format!("registry public key is invalid: {error}"))?;
    public_key
        .verify_strict(&digest, &signature)
        .map_err(|_| "registry signature verification failed".to_string())?;

    Ok(Verification {
        key_id,
        sha256: digest_hex,
    })
}

/// Read and verify a registry database from three local files.
pub fn verify_paths(
    database_path: impl AsRef<Path>,
    signature_path: impl AsRef<Path>,
    trust_path: impl AsRef<Path>,
) -> Result<Verification, String> {
    let database_path = database_path.as_ref();
    let signature_path = signature_path.as_ref();
    let trust_path = trust_path.as_ref();
    let database = fs::read(database_path).map_err(|error| {
        format!(
            "cannot read registry database {}: {error}",
            database_path.display()
        )
    })?;
    let signature = fs::read_to_string(signature_path).map_err(|error| {
        format!(
            "cannot read registry signature {}: {error}",
            signature_path.display()
        )
    })?;
    let trust = fs::read_to_string(trust_path).map_err(|error| {
        format!(
            "cannot read registry trust map {}: {error}",
            trust_path.display()
        )
    })?;
    verify_bytes(&database, &signature, &trust)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    fn artifacts() -> (Vec<u8>, String, String) {
        let database = b"signed registry bytes".to_vec();
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let digest = Sha256::digest(&database);
        let signature = signing_key.sign(&digest);
        let key_id = "test-key";
        let sidecar = json!({
            "algorithm": "Ed25519",
            "key_id": key_id,
            "value": STANDARD.encode(signature.to_bytes()),
            "sha256": hex_digest(&digest),
        });
        let trust = json!({key_id: STANDARD.encode(signing_key.verifying_key().to_bytes())});
        (database, sidecar.to_string(), trust.to_string())
    }

    #[test]
    fn verifies_digest_signature_and_trusted_key() {
        let (database, signature, trust) = artifacts();
        let result = verify_bytes(&database, &signature, &trust).unwrap();
        assert_eq!(result.key_id, "test-key");
        assert_eq!(result.sha256, hex_digest(&Sha256::digest(database)));
    }

    #[test]
    fn rejects_tampered_database() {
        let (mut database, signature, trust) = artifacts();
        database[0] ^= 1;
        let error = verify_bytes(&database, &signature, &trust).unwrap_err();
        assert!(error.contains("SHA-256 mismatch"), "{error}");
    }

    #[test]
    fn rejects_untrusted_key() {
        let (database, signature, _) = artifacts();
        let error = verify_bytes(&database, &signature, "{}").unwrap_err();
        assert!(error.contains("not trusted"), "{error}");
    }
}
