#![allow(clippy::unwrap_used)]

//! Shared client behavior: total timeout, connect timeout, cancellation, no redirects.

mod common;

use std::time::{Duration, Instant};

use common::{collect, request};
use quick_translate_backends::http::build_client_with;
use quick_translate_backends::{
    DeeplBackend, DeeplEndpoints, DeeplOptions, Error, OpenAiBackend, OpenAiOptions, Translator,
    build_client,
};
use tokio_util::sync::CancellationToken;
use url::Url;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn deepl_at(
    uri: &str,
    client: quick_translate_backends::HttpClient,
    timeout: Duration,
) -> DeeplBackend {
    let url = Url::parse(uri).unwrap();
    let opts = DeeplOptions {
        endpoints: DeeplEndpoints {
            free: url.clone(),
            pro: url,
        },
        timeout,
        ..DeeplOptions::new("k:fx")
    };
    DeeplBackend::new(client, opts)
}

#[tokio::test]
async fn client_total_timeout_applies() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let client = build_client(Duration::from_millis(200)).unwrap();
    let resp = client
        .post(format!("{}/v1/chat/completions", server.uri()))
        .send()
        .await;
    assert!(resp.unwrap_err().is_timeout());
}

#[tokio::test]
async fn backend_timeout_overrides_client_timeout() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    // Client default is long; the backend's configured timeout wins.
    let opts = OpenAiOptions {
        timeout: Duration::from_millis(200),
        ..OpenAiOptions::new(format!("{}/v1", server.uri()), "m")
    };
    let b = OpenAiBackend::new(build_client(Duration::from_secs(60)).unwrap(), opts);
    let started = Instant::now();
    let res = b
        .translate(request("t", None, "DE"), CancellationToken::new())
        .await;
    assert_eq!(res.err(), Some(Error::Timeout));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn connect_timeout_is_bounded() {
    // TEST-NET-1 is never routed: the connect either hangs until the connect timeout or fails
    // fast when the machine has no route at all.
    let client = build_client_with(Duration::from_millis(300), Duration::from_secs(30)).unwrap();
    let b = deepl_at("http://192.0.2.1:9", client, Duration::from_secs(30));
    let started = Instant::now();
    let err = b
        .translate(request("t", None, "DE"), CancellationToken::new())
        .await
        .err();
    assert!(
        matches!(err, Some(Error::Timeout | Error::Connection(_))),
        "{err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn cancel_before_response_aborts_quickly() {
    let server = MockServer::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .mount(&server)
        .await;
    let b = deepl_at(&server.uri(), common::client(), Duration::from_secs(30));
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c2.cancel();
    });
    let started = Instant::now();
    let err = b.translate(request("t", None, "DE"), cancel).await.err();
    assert_eq!(err, Some(Error::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn already_cancelled_token_sends_nothing() {
    let server = MockServer::start().await;
    let b = deepl_at(&server.uri(), common::client(), Duration::from_secs(30));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        b.translate(request("t", None, "DE"), cancel).await.err(),
        Some(Error::Cancelled)
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn dropping_the_future_aborts_the_request() {
    let (l, addr) = common::listener().await;
    let server = tokio::spawn(async move {
        let (mut sock, _) = l.accept().await.unwrap();
        common::read_request(&mut sock).await.unwrap();
        common::peer_closes(&mut sock, Duration::from_secs(5)).await
    });
    let b = deepl_at(
        &format!("http://{addr}"),
        common::client(),
        Duration::from_secs(30),
    );
    let fut = b.translate(request("t", None, "DE"), CancellationToken::new());
    let _ = tokio::time::timeout(Duration::from_millis(200), fut).await;
    assert!(server.await.unwrap(), "server saw the connection close");
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let elsewhere = MockServer::start().await;
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(307).insert_header(
            "location",
            format!("{}/v1/chat/completions", elsewhere.uri()),
        ))
        .mount(&server)
        .await;
    let b = OpenAiBackend::new(
        common::client(),
        OpenAiOptions::new(format!("{}/v1", server.uri()), "m"),
    );
    let res = match b
        .translate(request("secret text", None, "DE"), CancellationToken::new())
        .await
    {
        Ok(s) => collect(s).await.1,
        Err(e) => Some(e),
    };
    assert_eq!(
        res,
        Some(Error::Service("The endpoint returned HTTP 307".into()))
    );
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
}

/// Real rustls handshake against DeepL with a bogus key. Needs network: `--ignored`.
#[tokio::test]
#[ignore = "needs network access"]
async fn live_tls_to_deepl_rejects_bogus_key() {
    let b = DeeplBackend::new(
        build_client(Duration::from_secs(15)).unwrap(),
        DeeplOptions::new("00000000-0000-0000-0000-000000000000:fx"),
    );
    assert_eq!(b.usage().await, Err(Error::InvalidKey));
}
