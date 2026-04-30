//! Errors for HTTPS fetch and streaming downloads.

use std::io;

use ncp_manifest::Sha256Digest;
use thiserror::Error;

/// Result alias for the [`crate`] crate.
pub type Result<T> = std::result::Result<T, NetError>;

/// Failure modes for fetching manifests and downloading paks.
///
/// All variants are recoverable in the sense that the launcher can
/// surface them to the user and offer "retry" — *except* hash and
/// size mismatches, which indicate either a corrupted host or a
/// targeted attack and should be treated as fatal for the affected
/// pak.
#[derive(Debug, Error)]
pub enum NetError {
    /// Underlying reqwest / hyper / TLS error.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// The server returned a non-success status.
    #[error("HTTP {status} for {url}")]
    HttpStatus {
        /// The URL that failed.
        url: String,
        /// The status code returned.
        status: u16,
    },

    /// Response body was longer than the configured `max_bytes` for
    /// this fetch (manifests have a tight bound; paks have a loose
    /// one tied to `expected_size`).
    #[error("response from {url} exceeded {max} bytes")]
    ResponseTooLarge {
        /// The URL that was being fetched.
        url: String,
        /// The configured maximum.
        max: u64,
    },

    /// Body bytes were not valid UTF-8 when reading a text response
    /// (e.g. a `.minisig` file).
    #[error("response from {url} was not valid UTF-8")]
    NotUtf8 {
        /// The URL whose body failed UTF-8 decoding.
        url: String,
    },

    /// The server's `Content-Length` differed from the size declared
    /// in the manifest. Aborts before any bytes are written.
    #[error("size mismatch for {url}: expected {expected} bytes, server said {got}")]
    DeclaredSizeMismatch {
        /// The URL being downloaded.
        url: String,
        /// Size declared in the manifest.
        expected: u64,
        /// Size in the response's `Content-Length` header.
        got: u64,
    },

    /// The body grew larger than the manifest's expected size while
    /// streaming. Aborts and discards the partial download.
    #[error("body from {url} exceeded expected {expected} bytes (got at least {got_so_far})")]
    OversizeBody {
        /// The URL being downloaded.
        url: String,
        /// Size declared in the manifest.
        expected: u64,
        /// Bytes received before aborting.
        got_so_far: u64,
    },

    /// Streaming finished but the total bytes received did not match
    /// the manifest's expected size.
    #[error("size mismatch for {url}: expected {expected} bytes, got {got}")]
    SizeMismatch {
        /// The URL being downloaded.
        url: String,
        /// Size declared in the manifest.
        expected: u64,
        /// Total bytes actually received.
        got: u64,
    },

    /// The SHA-256 of the streamed bytes did not match the manifest.
    #[error("hash mismatch for {url}: expected {expected}, got {got}")]
    HashMismatch {
        /// The URL being downloaded.
        url: String,
        /// Hash declared in the manifest.
        expected: Sha256Digest,
        /// Hash computed from the streamed bytes.
        got: Sha256Digest,
    },

    /// The destination path has no parent directory, so we cannot
    /// stage an atomic-replace temp file alongside it.
    #[error("download destination has no parent directory")]
    NoParentDir,

    /// Filesystem error during the streaming write or atomic rename.
    #[error("download I/O error: {0}")]
    Io(#[from] io::Error),
}
