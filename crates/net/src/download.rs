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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use ncp_manifest::Sha256Digest;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

/// How often a resumable download / hash pass reports progress — throttled so a
/// multi-GB transfer emits a few updates per second rather than one per chunk.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Stream `url` into `dest` with **resume**, **progress**, and **cancel** —
/// built for very large files (the UT4 installer is ~10 GB), where [`download`]
/// (single-shot, no resume, hashes inline) is unsuitable.
///
/// `dest` is the partial file the caller owns (e.g. `<final>.part`): a dropped
/// connection leaves it in place and a later call resumes from its length via a
/// `Range` request. Reports `(downloaded, total)` through `on_progress`, and
/// stops promptly with [`NetError::Cancelled`] when `cancel` is set (keeping the
/// partial). On success `dest` holds exactly `expected_size` bytes — the caller
/// then verifies it with [`hash_file`] and renames it into place. The size is
/// enforced here; the SHA-256 is **not** (resume can't keep an inline hasher in
/// sync) — that's the caller's separate final pass.
///
/// # Errors
/// Network / `Range` / IO failures, [`NetError::SizeMismatch`] /
/// [`NetError::OversizeBody`] on a wrong byte total, or [`NetError::Cancelled`].
pub async fn download_resumable(
    client: &Client,
    url: &str,
    expected_size: u64,
    dest: &Path,
    cancel: &AtomicBool,
    on_progress: impl Fn(u64, u64),
) -> Result<()> {
    let parent = dest.parent().ok_or(NetError::NoParentDir)?;
    fs::create_dir_all(parent).await?;

    // Resume point: bytes already on disk (0 if absent; restart if somehow longer
    // than expected — a corrupt partial).
    let mut have = fs::metadata(dest).await.map(|m| m.len()).unwrap_or(0);
    if have > expected_size {
        have = 0;
    }
    if have == expected_size {
        on_progress(have, expected_size);
        return Ok(());
    }

    let mut request = client.inner.get(url);
    if have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(NetError::HttpStatus {
            url: url.to_string(),
            status: status.as_u16(),
        });
    }
    // We asked to resume but the server sent the whole file (200, not 206): it
    // ignored the Range, so start over from byte 0.
    let resuming = have > 0 && status.as_u16() == 206;
    let mut total = if resuming { have } else { 0 };
    let mut file = if resuming {
        fs::OpenOptions::new().append(true).open(dest).await?
    } else {
        fs::File::create(dest).await?
    };

    let mut stream = response.bytes_stream();
    let mut last = Instant::now();
    on_progress(total, expected_size);
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            let _ = file.flush().await;
            return Err(NetError::Cancelled);
        }
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
        file.write_all(&chunk).await?;
        if last.elapsed() >= PROGRESS_INTERVAL {
            on_progress(total, expected_size);
            last = Instant::now();
        }
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
    on_progress(total, expected_size);
    Ok(())
}

/// SHA-256 a file in one streaming pass, reporting `(read, total)` progress.
/// Used to verify a resumed download whose bytes were never hashed inline.
///
/// # Errors
/// [`NetError::Io`] on a read failure.
pub async fn hash_file(path: &Path, on_progress: impl Fn(u64, u64)) -> Result<Sha256Digest> {
    let total = fs::metadata(path).await?.len();
    let mut file = fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut read: u64 = 0;
    let mut last = Instant::now();
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        read += n as u64;
        if last.elapsed() >= PROGRESS_INTERVAL {
            on_progress(read, total);
            last = Instant::now();
        }
    }
    on_progress(read, total);
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}
