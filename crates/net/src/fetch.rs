//! Bounded text/byte fetch helpers, intended for the manifest and
//! its `.minisig`.
//!
//! Pak downloads use [`crate::download`] instead — this module is
//! for *small* responses where buffering the whole body is fine.

use tracing::debug;

use crate::client::Client;
use crate::error::{NetError, Result};

/// Conservative upper bound for a manifest fetch. The signed
/// manifest itself should be far below this; the cap exists so a
/// malicious or misconfigured server cannot stream gigabytes into
/// our buffer before we notice.
pub const DEFAULT_MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

/// GET `url`, validate `2xx`, and read up to `max_bytes` of body
/// into a `String`. Refuses non-UTF-8 bodies.
///
/// # Errors
///
/// - [`NetError::Http`] on transport / TLS / DNS failures.
/// - [`NetError::HttpStatus`] for any non-success response.
/// - [`NetError::ResponseTooLarge`] if `Content-Length` exceeds
///   `max_bytes` (rejected before reading body) or if the streamed
///   body grows past `max_bytes`.
/// - [`NetError::NotUtf8`] if the bytes read are not valid UTF-8.
pub async fn fetch_text(client: &Client, url: &str, max_bytes: u64) -> Result<String> {
    let bytes = fetch_bytes(client, url, max_bytes).await?;
    String::from_utf8(bytes).map_err(|_| NetError::NotUtf8 {
        url: url.to_string(),
    })
}

/// GET `url`, validate `2xx`, and read up to `max_bytes` of body.
///
/// # Errors
///
/// See [`fetch_text`]; identical error set minus `NotUtf8`.
pub async fn fetch_bytes(client: &Client, url: &str, max_bytes: u64) -> Result<Vec<u8>> {
    let response = client.inner.get(url).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(NetError::HttpStatus {
            url: url.to_string(),
            status: status.as_u16(),
        });
    }
    if let Some(declared) = response.content_length() {
        if declared > max_bytes {
            return Err(NetError::ResponseTooLarge {
                url: url.to_string(),
                max: max_bytes,
            });
        }
    }

    // Read the full body but bail if the stream produces more than
    // max_bytes — protects against servers that lie about (or omit)
    // Content-Length.
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let new_len = buf.len() as u64 + chunk.len() as u64;
        if new_len > max_bytes {
            return Err(NetError::ResponseTooLarge {
                url: url.to_string(),
                max: max_bytes,
            });
        }
        buf.extend_from_slice(&chunk);
    }
    debug!(url = %url, bytes = buf.len(), "fetched body");
    Ok(buf)
}

/// POST `body` as JSON with extra request `headers`, validate `2xx`, and
/// read up to `max_bytes` of the (UTF-8) response. Intended for small
/// control endpoints (e.g. the PUG bot's action API), not bulk transfer.
///
/// # Errors
///
/// - [`NetError::Http`] on transport / TLS / DNS failures.
/// - [`NetError::HttpStatus`] for any non-success response.
/// - [`NetError::ResponseTooLarge`] if the body grows past `max_bytes`.
/// - [`NetError::NotUtf8`] if the bytes read are not valid UTF-8.
pub async fn post_json(
    client: &Client,
    url: &str,
    headers: &[(&str, &str)],
    body: String,
    max_bytes: u64,
) -> Result<String> {
    let mut req = client
        .inner
        .post(url)
        .header("content-type", "application/json")
        .body(body);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    read_bounded_utf8(req.send().await?, url, max_bytes).await
}

/// POST `form` as `application/x-www-form-urlencoded` with extra request
/// `headers` (e.g. an `Authorization: Basic …`), validate `2xx`, and read up to
/// `max_bytes` of the (UTF-8) response. Used for the UT4 master server's OAuth
/// token endpoint. `reqwest`'s form serializer handles the percent-encoding.
///
/// # Errors
///
/// Same set as [`post_json`].
pub async fn post_form(
    client: &Client,
    url: &str,
    headers: &[(&str, &str)],
    form: &[(&str, &str)],
    max_bytes: u64,
) -> Result<String> {
    let mut req = client.inner.post(url).form(form);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    read_bounded_utf8(req.send().await?, url, max_bytes).await
}

/// GET `url` with extra request `headers` (e.g. an `Authorization: Bearer …`),
/// validate `2xx`, and read up to `max_bytes` of the (UTF-8) response. Used for
/// the OAuth exchange-code endpoint.
///
/// # Errors
///
/// Same set as [`fetch_text`].
pub async fn fetch_text_with_headers(
    client: &Client,
    url: &str,
    headers: &[(&str, &str)],
    max_bytes: u64,
) -> Result<String> {
    let mut req = client.inner.get(url);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    read_bounded_utf8(req.send().await?, url, max_bytes).await
}

/// Validate a response is `2xx` and read up to `max_bytes` of its body as UTF-8,
/// streaming so a server that lies about `Content-Length` can't overrun the cap.
async fn read_bounded_utf8(
    response: reqwest::Response,
    url: &str,
    max_bytes: u64,
) -> Result<String> {
    let status = response.status();
    if !status.is_success() {
        return Err(NetError::HttpStatus {
            url: url.to_string(),
            status: status.as_u16(),
        });
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(NetError::ResponseTooLarge {
                url: url.to_string(),
                max: max_bytes,
            });
        }
        buf.extend_from_slice(&chunk);
    }
    String::from_utf8(buf).map_err(|_| NetError::NotUtf8 {
        url: url.to_string(),
    })
}
