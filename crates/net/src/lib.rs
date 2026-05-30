//! HTTPS fetch + streaming SHA-256 verification for the NetcodePlus
//! launcher.
//!
//! Two narrow concerns:
//!
//! - [`fetch_text`] / [`fetch_bytes`]: bounded full-buffer GET for
//!   small responses (the signed manifest and its `.minisig`).
//! - [`download`]: streaming GET that hashes as it goes, refuses
//!   bodies larger than the manifest's declared size, and atomically
//!   replaces the destination on success.
//!
//! Network trust comes from rustls-tls + a bundled webpki root list
//! (set in [`Client::new`]); content trust comes from the signed
//! manifest, not from TLS, the URL, or the server.

mod client;
mod download;
mod error;
mod fetch;

pub use crate::client::{Client, ClientConfig};
pub use crate::download::{download, DownloadOutcome};
pub use crate::error::{NetError, Result};
pub use crate::fetch::{
    fetch_bytes, fetch_text, fetch_text_with_headers, post_form, post_json,
    DEFAULT_MAX_MANIFEST_BYTES,
};
