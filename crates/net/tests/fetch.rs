//! Integration tests for [`ncp_net::fetch_text`] / [`ncp_net::fetch_bytes`].
//!
//! Uses [`wiremock`] to spin up a local async HTTP server per test —
//! no real network, no external services.

use ncp_net::{fetch_bytes, fetch_text, Client, NetError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn fetch_text_returns_body_on_2xx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"hello\":1}"))
        .mount(&server)
        .await;

    let client = Client::new().unwrap();
    let url = format!("{}/manifest.json", server.uri());
    let body = fetch_text(&client, &url, 1024).await.unwrap();
    assert_eq!(body, "{\"hello\":1}");
}

#[tokio::test]
async fn fetch_text_rejects_non_2xx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let client = Client::new().unwrap();
    let url = format!("{}/missing", server.uri());
    let err = fetch_text(&client, &url, 1024).await.unwrap_err();
    assert!(
        matches!(err, NetError::HttpStatus { status: 404, .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn fetch_text_rejects_when_content_length_exceeds_max() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(5000)))
        .mount(&server)
        .await;

    let client = Client::new().unwrap();
    let url = format!("{}/big", server.uri());
    let err = fetch_text(&client, &url, 1000).await.unwrap_err();
    assert!(
        matches!(err, NetError::ResponseTooLarge { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn fetch_bytes_returns_raw_body() {
    let server = MockServer::start().await;
    let payload: Vec<u8> = (0u8..=200u8).collect();
    Mock::given(method("GET"))
        .and(path("/raw"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
        .mount(&server)
        .await;

    let client = Client::new().unwrap();
    let url = format!("{}/raw", server.uri());
    let body = fetch_bytes(&client, &url, 1024).await.unwrap();
    assert_eq!(body, payload);
}

#[tokio::test]
async fn fetch_text_rejects_invalid_utf8() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/binary"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xff, 0xfe, 0xfd]))
        .mount(&server)
        .await;

    let client = Client::new().unwrap();
    let url = format!("{}/binary", server.uri());
    let err = fetch_text(&client, &url, 1024).await.unwrap_err();
    assert!(matches!(err, NetError::NotUtf8 { .. }), "got {err:?}");
}
