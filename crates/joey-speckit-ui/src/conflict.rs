//! SHA-256 content-hash based optimistic concurrency helpers.
//!
//! Every read of a source file is accompanied by a `content_hash`
//! (`"sha256:<hex>"`). Writes must supply the hash they read; if the
//! current on-disk hash doesn't match, the write is rejected with
//! `ConflictError` and the file is left untouched (FR-018).

use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConflictError {
    #[error("file changed on disk since based_on_hash was read")]
    Conflict { current_hash: String },
}

/// Compute the `sha256:<hex>` content hash of the given bytes.
pub fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Verify that `based_on_hash` matches the hash of `current_content`.
/// Returns `Ok(())` if they match, or `Err(ConflictError::Conflict)`
/// carrying the actual current hash otherwise.
pub fn check_conflict(current_content: &str, based_on_hash: &str) -> Result<(), ConflictError> {
    let current = content_hash(current_content);
    if current == based_on_hash {
        Ok(())
    } else {
        Err(ConflictError::Conflict {
            current_hash: current,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_hash_ok() {
        let content = "hello world";
        let hash = content_hash(content);
        assert!(check_conflict(content, &hash).is_ok());
    }

    #[test]
    fn mismatched_hash_conflicts() {
        let content = "hello world";
        let stale_hash = content_hash("something else");
        let err = check_conflict(content, &stale_hash).unwrap_err();
        match err {
            ConflictError::Conflict { current_hash } => {
                assert_eq!(current_hash, content_hash(content));
            }
        }
    }

    #[test]
    fn hash_is_deterministic_and_prefixed() {
        let h1 = content_hash("abc");
        let h2 = content_hash("abc");
        assert_eq!(h1, h2);
        assert!(h1.starts_with("sha256:"));
    }
}
