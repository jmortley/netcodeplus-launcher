//! Streaming pak download with per-chunk SHA-256 + size enforcement
//! and atomic-replace install.
//!
//! The flow:
//!
//! 1. Issue the GET. Reject non-`2xx` and `Content-Length`s that
//!    disagree with the manifest's declared size.
//! 2. Stream the body chunk-by-chunk into a sibling temp file
//!    (`<dest_dir>/.<filename>.partial.<pid>.<nanos>`), updating a
//!    rolling SHA-256 hasher and a byte counter as we go. Abort the
//!    moment the byte counter exceeds the manifest's expected size.
//! 3. After the stream ends, verify the totals: bytes-received and
//!    hash-finalised must match the manifest exactly.
//! 4. `fsync` the temp file, then [`std::fs::rename`] it over `dest`.
//!    On any error before this step, the temp file is unlinked and
//!    `dest` is left untouched.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use ncp_manifest::Sha256Digest;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use crate::client::Client;
use crate::error::{NetError, Result};

/// What [`download`] returns on success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadOutcome {
    /// Total bytes streamed (equal to the manifest's declared size).
    pub bytes: u64,
    /// SHA-256 of the streamed bytes (equal to the manifest's
    /// declared digest).
    pub sha256: Sha256Digest,
}

/// Stream `url` into `dest`, verifying both size and SHA-256 against
/// the manifest's declared values, then atomically replace `dest`
/// with the verified bytes.
///
/// # Errors
///
/// Surfaces every variant of [`NetError`] except `NotUtf8`. The
/// security-critical ones to handle distinctly in callers:
///
/// - [`NetError::HashMismatch`] / [`NetError::SizeMismatch`]
///   indicate a corrupted host or active attack and should be
///   treated as fatal for the affected pak.
/// - [`NetError::OversizeBody`] is the streaming-time analogue —
///   abort and discard.
pub async fn download(
    client: &Client,
    url: &str,
    expected_sha256: Sha256Digest,
    expected_size: u64,
    dest: &Path,
) -> Result<DownloadOutcome> {
    let parent = dest.parent().ok_or(NetError::NoParentDir)?;
    fs::create_dir_all(parent).await?;
    let tmp_path = parent.join(staging_filename(dest));

    let outcome = stream_to_temp(client, url, expected_sha256, expected_size, &tmp_path).await;

    match outcome {
        Ok(outcome) => {
            // Atomic rename. On Windows this fails if `dest` exists
            // and is read-only; fs::rename otherwise replaces.
            if let Err(e) = fs::rename(&tmp_path, dest).await {
                let _ = fs::remove_file(&tmp_path).await;
                return Err(NetError::Io(e));
            }
            debug!(
                url = %url,
                dest = %dest.display(),
                bytes = outcome.bytes,
                "download committed"
            );
            Ok(outcome)
        }
        Err(e) => {
            if let Err(remove_err) = fs::remove_file(&tmp_path).await {
                if remove_err.kind() != std::io::ErrorKind::NotFound {
                    warn!(
                        path = %tmp_path.display(),
                        error = %remove_err,
                        "failed to clean up partial download"
                    );
                }
            }
            Err(e)
        }
    }
}

async fn stream_to_temp(
    client: &Client,
    url: &str,
    expected_sha256: Sha256Digest,
    expected_size: u64,
    tmp_path: &Path,
) -> Result<DownloadOutcome> {
    let response = client.inner.get(url).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(NetError::HttpStatus {
            url: url.to_string(),
            status: status.as_u16(),
        });
    }
    if let Some(declared) = response.content_length() {
        if declared != expected_size {
            return Err(NetError::DeclaredSizeMismatch {
                url: url.to_string(),
                expected: expected_size,
                got: declared,
            });
        }
    }

    let mut file = fs::File::create(tmp_path).await?;
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or(NetError::OversizeBody {
                url: url.to_string(),
                expected: expected_size,
                got_so_far: u64::MAX,
            })?;
        if total > expected_size {
            return Err(NetError::OversizeBody {
                url: url.to_string(),
                expected: expected_size,
                got_so_far: total,
            });
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    file.sync_all().await?;

    if total != expected_size {
        return Err(NetError::SizeMismatch {
            url: url.to_string(),
            expected: expected_size,
            got: total,
        });
    }
    let computed = Sha256Digest::from_bytes(hasher.finalize().into());
    if computed != expected_sha256 {
        return Err(NetError::HashMismatch {
            url: url.to_string(),
            expected: expected_sha256,
            got: computed,
        });
    }

    Ok(DownloadOutcome {
        bytes: total,
        sha256: computed,
    })
}

fn staging_filename(dest: &Path) -> PathBuf {
    let stem = dest
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(".{stem}.partial.{pid}.{nanos}"))
}
