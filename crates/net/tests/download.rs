//! Integration tests for [`ncp_net::download`].
//!
//! Each test spins up a [`wiremock::MockServer`] and writes the
//! download into a [`tempfile::TempDir`] so nothing escapes the
//! test sandbox.

use std::fs;

use ncp_manifest::Sha256Digest;
use ncp_net::{download, Client, NetError};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sha256_of(bytes: &[u8]) -> Sha256Digest {
    let mut h = Sha256::new();
    h.update(bytes);
    Sha256Digest::from_bytes(h.finalize().into())
}

#[tokio::test]
async fn download_writes_verified_bytes() {
    let payload: Vec<u8> = (0u8..=255u8).cycle().take(4096).collect();
    let expected_sha = sha256_of(&payload);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/test.pak"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("test.pak");
    let client = Client::new().unwrap();
    let url = format!("{}/test.pak", server.uri());

    let outcome = download(&client, &url, expected_sha, payload.len() as u64, &dest)
        .await
        .unwrap();

    assert_eq!(outcome.bytes, payload.len() as u64);
    assert_eq!(outcome.sha256, expected_sha);

    let on_disk = fs::read(&dest).unwrap();
    assert_eq!(on_disk, payload);
}

#[tokio::test]
async fn download_rejects_404_and_does_not_create_dest() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing.pak"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("missing.pak");
    let client = Client::new().unwrap();
    let url = format!("{}/missing.pak", server.uri());

    let err = download(&client, &url, sha256_of(b""), 0, &dest)
        .await
        .unwrap_err();
    assert!(
        matches!(err, NetError::HttpStatus { status: 404, .. }),
        "got {err:?}"
    );
    assert!(!dest.exists(), "dest must not be created on 404");
}

#[tokio::test]
async fn download_rejects_hash_mismatch_and_cleans_up_partial() {
    let payload: Vec<u8> = b"the real bytes".to_vec();
    let wrong_sha = Sha256Digest::from_bytes([0u8; 32]);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/swapped.pak"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("swapped.pak");
    let client = Client::new().unwrap();
    let url = format!("{}/swapped.pak", server.uri());

    let err = download(&client, &url, wrong_sha, payload.len() as u64, &dest)
        .await
        .unwrap_err();
    assert!(matches!(err, NetError::HashMismatch { .. }), "got {err:?}");
    assert!(!dest.exists(), "dest must not be created on hash mismatch");

    // Temp staging file should have been cleaned up.
    let leftovers: Vec<_> = fs::read_dir(tmp.path()).unwrap().collect();
    assert!(
        leftovers.is_empty(),
        "no staging files should remain after failed download: {leftovers:?}"
    );
}

#[tokio::test]
async fn download_rejects_declared_content_length_mismatch() {
    let payload: Vec<u8> = b"actual bytes".to_vec();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wrong-clen.pak"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("wrong-clen.pak");
    let client = Client::new().unwrap();
    let url = format!("{}/wrong-clen.pak", server.uri());

    // Declare a much larger expected size: server's Content-Length
    // (= payload.len()) won't match.
    let err = download(&client, &url, sha256_of(&payload), 9_999_999, &dest)
        .await
        .unwrap_err();
    assert!(
        matches!(err, NetError::DeclaredSizeMismatch { .. }),
        "got {err:?}"
    );
    assert!(!dest.exists());
}

#[tokio::test]
async fn download_rejects_smaller_than_expected_bodies() {
    // Server returns 100 bytes; expected size is 200. This one is
    // tricky because most servers send a Content-Length, so we end
    // up with DeclaredSizeMismatch first. That's correct behaviour.
    let payload = vec![0u8; 100];
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/short.pak"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("short.pak");
    let client = Client::new().unwrap();
    let url = format!("{}/short.pak", server.uri());

    let err = download(&client, &url, sha256_of(&payload), 200, &dest)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            NetError::DeclaredSizeMismatch { .. } | NetError::SizeMismatch { .. }
        ),
        "expected size error, got {err:?}"
    );
    assert!(!dest.exists());
}

#[tokio::test]
async fn download_overwrites_existing_destination_on_success() {
    let payload: Vec<u8> = b"new contents".to_vec();
    let expected_sha = sha256_of(&payload);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/overwrite.pak"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("overwrite.pak");
    fs::write(&dest, b"OLD CONTENTS").unwrap();
    let client = Client::new().unwrap();
    let url = format!("{}/overwrite.pak", server.uri());

    download(&client, &url, expected_sha, payload.len() as u64, &dest)
        .await
        .unwrap();

    assert_eq!(fs::read(&dest).unwrap(), payload);
}
