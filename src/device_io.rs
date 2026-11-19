//! Bounded binary payloads shared by native device adapters.
use crate::json::quote;

pub const MAX_BYTES: usize = 65_536;
pub const MAX_TIMEOUT_MS: u32 = 30_000;

pub fn validate_timeout(timeout_ms: u32) -> Result<(), String> {
    if !(1..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err("device timeout must be between 1 and 30000 milliseconds".into());
    }
    Ok(())
}

pub fn validate_length(length: usize) -> Result<(), String> {
    if !(1..=MAX_BYTES).contains(&length) {
        return Err("device read length must be between 1 and 65536 bytes".into());
    }
    Ok(())
}

/// Decode a strict RFC 4648 payload with the existing Project Z codec.
pub fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    let limit = z64::encoded_len(MAX_BYTES, z64::Mode::Standard).unwrap();
    if text.len() > limit {
        return Err("device payload exceeds 65536 bytes".into());
    }
    let mut bytes = vec![0; z64::decoded_capacity(text.len()).unwrap()];
    let count = z64::decode(text.as_bytes(), &mut bytes, z64::Mode::Standard)
        .map_err(|error| format!("invalid device base64: {error}"))?;
    if count > MAX_BYTES {
        return Err("device payload exceeds 65536 bytes".into());
    }
    bytes.truncate(count);
    Ok(bytes)
}

pub fn encode_base64(bytes: &[u8]) -> String {
    let mut output = vec![0; z64::encoded_len(bytes.len(), z64::Mode::Standard).unwrap()];
    let count = z64::encode(bytes, &mut output, z64::Mode::Standard).unwrap();
    output.truncate(count);
    // The codec only emits ASCII.
    String::from_utf8(output).unwrap()
}

/// A bounded read may time out after returning some data.
pub struct ReadResult {
    pub bytes: Vec<u8>,
    pub timed_out: bool,
}

impl ReadResult {
    pub fn json(&self) -> String {
        let text = std::str::from_utf8(&self.bytes)
            .map(quote)
            .unwrap_or_else(|_| "null".into());
        format!(
            "{{\"bytes\":{},\"base64\":{},\"text\":{},\"timed_out\":{}}}",
            self.bytes.len(),
            quote(&encode_base64(&self.bytes)),
            text,
            self.timed_out
        )
    }
}
