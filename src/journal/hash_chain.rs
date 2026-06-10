use sha2::{Digest, Sha256};

pub fn event_hash(previous_hash: Option<&str>, sequence: i64, kind: &str, payload: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(previous_hash.unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(sequence.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(kind.as_bytes());
    hasher.update(b"|");
    hasher.update(payload.as_bytes());
    hex::encode(hasher.finalize())
}
