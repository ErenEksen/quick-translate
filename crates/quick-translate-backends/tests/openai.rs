#![allow(clippy::unwrap_used)]

mod common;

use std::time::{Duration, Instant};

use common::{SSE_HEAD, client, collect, request, sse_delta, text_of, write_chunk};
use futures::StreamExt;
use quick_translate_backends::{Chunk, Error, OpenAiBackend, OpenAiOptions, Translator};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn backend(base: &str, key: &str) -> OpenAiBackend {
    let opts = OpenAiOptions {
        api_key: key.into(),
        ..OpenAiOptions::new(base, "test-model")
    };
    OpenAiBackend::new(client(), opts)
}

fn sse(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

async fn run(b: &OpenAiBackend, text: &str) -> (Vec<Chunk>, Option<Error>) {
    match b
        .translate(request(text, None, "TR"), CancellationToken::new())
        .await
    {
        Ok(stream) => collect(stream).await,
        Err(e) => (Vec::new(), Some(e)),
    }
}

#[tokio::test]
async fn streams_chunked_deltas_until_done() {
    let server = MockServer::start().await;
    let body = format!(
        "{}{}: keep-alive\n\n{}{}data: [DONE]\n\n{}",
        // Role-only first chunk, as OpenAI sends it.
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
        sse_delta("Gün"),
        sse_delta("aydın\n"),
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        sse_delta("IGNORED AFTER DONE"),
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse(&body))
        .expect(1)
        .mount(&server)
        .await;

    let (chunks, err) = run(
        &backend(&format!("{}/v1", server.uri()), ""),
        "Good morning",
    )
    .await;
    assert_eq!(err, None);
    assert_eq!(
        chunks,
        [Chunk::Delta("Gün".into()), Chunk::Delta("aydın\n".into())]
    );
    assert!(
        chunks.iter().all(|c| matches!(c, Chunk::Delta(_))),
        "no Detected chunk"
    );
}

#[tokio::test]
async fn request_body_and_no_auth_header_without_key() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(sse(&format!("{}data: [DONE]\n\n", sse_delta("x"))))
        .mount(&server)
        .await;
    let (_, err) = run(
        &backend(&format!("{}/v1/", server.uri()), ""),
        "line 1\nline 2\nline 3",
    )
    .await;
    assert_eq!(err, None);

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].headers.get("authorization").is_none());
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["temperature"], 0.0);
    assert_eq!(body["messages"][0]["role"], "system");
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("Turkish") && system.contains("translation only"),
        "{system}"
    );
    assert_eq!(
        body["messages"][1],
        json!({"role": "user", "content": "line 1\nline 2\nline 3"})
    );
}

#[tokio::test]
async fn bearer_token_when_key_is_set() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(sse(&format!("{}data: [DONE]\n\n", sse_delta("ok"))))
        .expect(1)
        .mount(&server)
        .await;
    let (chunks, err) = run(&backend(&format!("{}/v1", server.uri()), "sk-test"), "hi").await;
    assert_eq!((text_of(&chunks).as_str(), err), ("ok", None));
}

#[tokio::test]
async fn error_json_for_unknown_model() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"message": "The model `nope` does not exist", "type": "invalid_request_error", "code": "model_not_found"}
        })))
        .mount(&server)
        .await;
    let b = backend(&format!("{}/v1", server.uri()), "");
    let expected = Error::Service("The model `nope` does not exist".into());
    assert_eq!(run(&b, "hi").await.1, Some(expected.clone()));
    assert_eq!(b.test_connection("DE").await, Err(expected));
}

#[tokio::test]
async fn status_mapping() {
    let server = MockServer::start().await;
    Mock::given(path("/a/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "Incorrect API key"}})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/b/chat/completions"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    Mock::given(path("/c/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("<html>oops</html>"))
        .mount(&server)
        .await;
    Mock::given(path("/d/chat/completions"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": "model \"x\" not found, try pulling it first"})),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    assert_eq!(
        run(&backend(&format!("{uri}/a"), "k"), "t").await.1,
        Some(Error::InvalidKey)
    );
    assert_eq!(
        run(&backend(&format!("{uri}/b"), ""), "t").await.1,
        Some(Error::RateLimited)
    );
    assert_eq!(
        run(&backend(&format!("{uri}/c"), ""), "t").await.1,
        Some(Error::Service("The endpoint returned HTTP 500".into()))
    );
    assert_eq!(
        run(&backend(&format!("{uri}/d"), ""), "t").await.1,
        Some(Error::Service(
            "model \"x\" not found, try pulling it first".into()
        ))
    );
}

#[tokio::test]
async fn error_event_mid_stream() {
    let server = MockServer::start().await;
    let body = format!(
        "{}data: {{\"error\":{{\"message\":\"Provider overloaded\",\"code\":502}}}}\n\n",
        sse_delta("partial ")
    );
    Mock::given(path("/v1/chat/completions"))
        .respond_with(sse(&body))
        .mount(&server)
        .await;
    let (chunks, err) = run(&backend(&format!("{}/v1", server.uri()), ""), "t").await;
    assert_eq!(text_of(&chunks), "partial ");
    assert_eq!(err, Some(Error::Service("Provider overloaded".into())));
}

#[tokio::test]
async fn finish_reason_without_done_is_complete() {
    let server = MockServer::start().await;
    let body = format!(
        "{}data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}",
        sse_delta("x")
    );
    Mock::given(path("/v1/chat/completions"))
        .respond_with(sse(&body))
        .mount(&server)
        .await;
    let (chunks, err) = run(&backend(&format!("{}/v1", server.uri()), ""), "t").await;
    assert_eq!((text_of(&chunks).as_str(), err), ("x", None));
}

#[tokio::test]
async fn clean_eof_without_done_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(sse(&sse_delta("half")))
        .mount(&server)
        .await;
    let (chunks, err) = run(&backend(&format!("{}/v1", server.uri()), ""), "t").await;
    assert_eq!(text_of(&chunks), "half");
    assert!(matches!(err, Some(Error::Connection(_))), "{err:?}");
}

#[tokio::test]
async fn test_connection_success() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(sse(&format!(
            "{}{}data: [DONE]\n\n",
            sse_delta("Mer"),
            sse_delta("haba")
        )))
        .expect(1)
        .mount(&server)
        .await;
    let b = backend(&format!("{}/v1", server.uri()), "");
    assert_eq!(b.test_connection("TR").await, Ok("Merhaba".into()));
    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["messages"][1]["content"], "Hello");
}

#[tokio::test]
async fn test_connection_empty_reply() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(sse("data: [DONE]\n\n"))
        .mount(&server)
        .await;
    let b = backend(&format!("{}/v1", server.uri()), "");
    assert!(matches!(
        b.test_connection("TR").await,
        Err(Error::Service(_))
    ));
}

#[tokio::test]
async fn deltas_arrive_progressively() {
    let (l, addr) = common::listener().await;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut sock, _) = l.accept().await.unwrap();
        common::read_request(&mut sock).await.unwrap();
        sock.write_all(SSE_HEAD.as_bytes()).await.unwrap();
        write_chunk(&mut sock, &sse_delta("first ")).await;
        // The second delta is only sent after the client has seen the first.
        rx.await.unwrap();
        // Split one event across two chunks.
        let second = sse_delta("second");
        let (a, b) = second.split_at(10);
        write_chunk(&mut sock, a).await;
        write_chunk(&mut sock, b).await;
        write_chunk(&mut sock, "data: [DONE]\n\n").await;
        sock.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let b = backend(&format!("http://{addr}/v1"), "");
    let mut stream = b
        .translate(request("t", None, "DE"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(stream.next().await, Some(Ok(Chunk::Delta("first ".into()))));
    tx.send(()).unwrap();
    assert_eq!(stream.next().await, Some(Ok(Chunk::Delta("second".into()))));
    assert_eq!(stream.next().await, None);
    server.await.unwrap();
}

#[tokio::test]
async fn mid_stream_disconnect_is_connection_error() {
    let (l, addr) = common::listener().await;
    tokio::spawn(async move {
        let (mut sock, _) = l.accept().await.unwrap();
        common::read_request(&mut sock).await.unwrap();
        sock.write_all(SSE_HEAD.as_bytes()).await.unwrap();
        write_chunk(&mut sock, &sse_delta("before drop")).await;
        // Close without the terminating zero-length chunk.
        drop(sock);
    });
    let b = backend(&format!("http://{addr}/v1"), "");
    let (chunks, err) = run(&b, "t").await;
    assert_eq!(text_of(&chunks), "before drop");
    assert!(matches!(err, Some(Error::Connection(_))), "{err:?}");
}

#[tokio::test]
async fn cancel_mid_stream_aborts_the_request() {
    let (l, addr) = common::listener().await;
    let server = tokio::spawn(async move {
        let (mut sock, _) = l.accept().await.unwrap();
        common::read_request(&mut sock).await.unwrap();
        sock.write_all(SSE_HEAD.as_bytes()).await.unwrap();
        write_chunk(&mut sock, &sse_delta("tok")).await;
        common::peer_closes(&mut sock, Duration::from_secs(5)).await
    });
    let b = backend(&format!("http://{addr}/v1"), "");
    let cancel = CancellationToken::new();
    let mut stream = b
        .translate(request("t", None, "DE"), cancel.clone())
        .await
        .unwrap();
    assert_eq!(stream.next().await, Some(Ok(Chunk::Delta("tok".into()))));
    cancel.cancel();
    assert_eq!(stream.next().await, Some(Err(Error::Cancelled)));
    // The connection is closed while the stream object is still alive.
    assert!(server.await.unwrap(), "server saw the connection close");
    assert_eq!(stream.next().await, None);
}

#[tokio::test]
async fn dropping_the_stream_aborts_the_request() {
    let (l, addr) = common::listener().await;
    let server = tokio::spawn(async move {
        let (mut sock, _) = l.accept().await.unwrap();
        common::read_request(&mut sock).await.unwrap();
        sock.write_all(SSE_HEAD.as_bytes()).await.unwrap();
        write_chunk(&mut sock, &sse_delta("tok")).await;
        common::peer_closes(&mut sock, Duration::from_secs(5)).await
    });
    let b = backend(&format!("http://{addr}/v1"), "");
    let mut stream = b
        .translate(request("t", None, "DE"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(stream.next().await, Some(Ok(Chunk::Delta("tok".into()))));
    drop(stream);
    assert!(server.await.unwrap(), "server saw the connection close");
}

#[tokio::test]
async fn stalled_stream_times_out() {
    let (l, addr) = common::listener().await;
    tokio::spawn(async move {
        let (mut sock, _) = l.accept().await.unwrap();
        common::read_request(&mut sock).await.unwrap();
        sock.write_all(SSE_HEAD.as_bytes()).await.unwrap();
        write_chunk(&mut sock, &sse_delta("slow")).await;
        tokio::time::sleep(Duration::from_secs(10)).await;
    });
    let opts = OpenAiOptions {
        timeout: Duration::from_millis(300),
        ..OpenAiOptions::new(format!("http://{addr}/v1"), "m")
    };
    let b = OpenAiBackend::new(client(), opts);
    let started = Instant::now();
    let (chunks, err) = run(&b, "t").await;
    assert_eq!(text_of(&chunks), "slow");
    assert_eq!(err, Some(Error::Timeout));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn insecure_remote_endpoint_is_refused_without_a_request() {
    let b = OpenAiBackend::new(client(), OpenAiOptions::new("http://example.com/v1", "m"));
    assert_eq!(run(&b, "t").await.1, Some(Error::InsecureEndpoint));
    assert_eq!(b.test_connection("DE").await, Err(Error::InsecureEndpoint));
}

#[tokio::test]
async fn missing_model_is_not_configured() {
    let b = OpenAiBackend::new(client(), OpenAiOptions::new("http://localhost:1/v1", ""));
    assert_eq!(run(&b, "t").await.1, Some(Error::NotConfigured));
}
