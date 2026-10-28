#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{client, collect, request};
use quick_translate_backends::{
    Chunk, DeeplBackend, DeeplEndpoints, DeeplHost, DeeplOptions, Error, Translator, Usage,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use url::Url;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Servers {
    free: MockServer,
    pro: MockServer,
}

impl Servers {
    async fn start() -> Self {
        Self {
            free: MockServer::start().await,
            pro: MockServer::start().await,
        }
    }

    fn options(&self, key: &str) -> DeeplOptions {
        DeeplOptions {
            endpoints: DeeplEndpoints {
                free: Url::parse(&self.free.uri()).unwrap(),
                pro: Url::parse(&self.pro.uri()).unwrap(),
            },
            retry_base_delay: Duration::from_millis(5),
            ..DeeplOptions::new(key)
        }
    }

    fn backend(&self, key: &str) -> DeeplBackend {
        DeeplBackend::new(client(), self.options(key))
    }
}

fn ok_translation(text: &str, detected: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "translations": [{"detected_source_language": detected, "text": text}]
    }))
}

async fn translate(
    b: &DeeplBackend,
    text: &str,
    src: Option<&str>,
    tgt: &str,
) -> Result<Vec<Chunk>, Error> {
    let stream = b
        .translate(request(text, src, tgt), CancellationToken::new())
        .await?;
    let (chunks, err) = collect(stream).await;
    err.map_or(Ok(chunks), Err)
}

#[tokio::test]
async fn auto_detect_omits_source_lang_and_reports_detected() {
    let s = Servers::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/translate"))
        .and(header("authorization", "DeepL-Auth-Key k1:fx"))
        .and(header("content-type", "application/json"))
        .and(body_json(
            json!({"text": ["Guten Morgen"], "target_lang": "EN-US"}),
        ))
        .respond_with(ok_translation("Good morning", "DE"))
        .expect(1)
        .mount(&s.free)
        .await;

    let chunks = translate(&s.backend("k1:fx"), "Guten Morgen", None, "EN-US")
        .await
        .unwrap();
    assert_eq!(
        chunks,
        [
            Chunk::Detected("DE".into()),
            Chunk::Delta("Good morning".into())
        ]
    );
}

#[tokio::test]
async fn explicit_source_lang_is_sent() {
    let s = Servers::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/translate"))
        .and(body_json(
            json!({"text": ["a\nb\nc"], "target_lang": "TR", "source_lang": "EN"}),
        ))
        .respond_with(ok_translation("x\ny\nz", "EN"))
        .expect(1)
        .mount(&s.pro)
        .await;

    let chunks = translate(&s.backend("pro-key"), "a\nb\nc", Some("EN"), "TR")
        .await
        .unwrap();
    assert_eq!(common::text_of(&chunks), "x\ny\nz");
}

#[tokio::test]
async fn fx_key_uses_free_host_and_other_keys_use_pro() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ok_translation("free", "EN"))
        .expect(1)
        .mount(&s.free)
        .await;
    Mock::given(path("/v2/translate"))
        .respond_with(ok_translation("pro", "EN"))
        .expect(1)
        .mount(&s.pro)
        .await;

    let free = translate(&s.backend("abc:fx"), "t", None, "DE")
        .await
        .unwrap();
    let pro = translate(&s.backend("abc"), "t", None, "DE").await.unwrap();
    assert_eq!(common::text_of(&free), "free");
    assert_eq!(common::text_of(&pro), "pro");
}

#[tokio::test]
async fn forbidden_on_pro_switches_to_free_once_and_caches_it() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&s.pro)
        .await;
    Mock::given(path("/v2/translate"))
        .respond_with(ok_translation("ok", "DE"))
        .expect(2)
        .mount(&s.free)
        .await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    let b = s
        .backend("no-suffix-but-free")
        .on_host_change(move |h| seen2.lock().unwrap().push(h));
    assert_eq!(b.host(), DeeplHost::Pro);

    assert_eq!(
        common::text_of(&translate(&b, "t", None, "EN-GB").await.unwrap()),
        "ok"
    );
    assert_eq!(b.host(), DeeplHost::Free);
    assert_eq!(*seen.lock().unwrap(), [DeeplHost::Free]);

    // The second request goes straight to the cached host (pro expects exactly one hit).
    translate(&b, "t", None, "EN-GB").await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cached_host_is_used_first() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ok_translation("ok", "DE"))
        .expect(1)
        .mount(&s.free)
        .await;
    let opts = DeeplOptions {
        cached_host: Some(DeeplHost::Free),
        ..s.options("pro-looking")
    };
    translate(&DeeplBackend::new(client(), opts), "t", None, "EN-GB")
        .await
        .unwrap();
}

#[tokio::test]
async fn forbidden_on_both_hosts_is_invalid_key() {
    let s = Servers::start().await;
    for server in [&s.free, &s.pro] {
        Mock::given(path("/v2/translate"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "Forbidden"})))
            .expect(1)
            .mount(server)
            .await;
    }
    let b = s.backend("bad:fx");
    assert_eq!(translate(&b, "t", None, "DE").await, Err(Error::InvalidKey));
    assert_eq!(
        b.host(),
        DeeplHost::Free,
        "host unchanged after a failed fallback"
    );
}

#[tokio::test]
async fn quota_exceeded_456() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ResponseTemplate::new(456))
        .expect(1)
        .mount(&s.free)
        .await;
    assert_eq!(
        translate(&s.backend("k:fx"), "t", None, "DE").await,
        Err(Error::QuotaExceeded)
    );
}

#[tokio::test]
async fn rate_limited_twice_then_succeeds() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(2)
        .expect(2)
        .with_priority(1)
        .mount(&s.free)
        .await;
    Mock::given(path("/v2/translate"))
        .respond_with(ok_translation("finally", "EN"))
        .expect(1)
        .mount(&s.free)
        .await;
    let chunks = translate(&s.backend("k:fx"), "t", None, "DE")
        .await
        .unwrap();
    assert_eq!(common::text_of(&chunks), "finally");
}

#[tokio::test]
async fn rate_limited_gives_up_after_three_retries_with_backoff() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ResponseTemplate::new(429))
        .expect(4)
        .mount(&s.free)
        .await;
    let opts = DeeplOptions {
        retry_base_delay: Duration::from_millis(20),
        ..s.options("k:fx")
    };
    let started = Instant::now();
    let res = translate(&DeeplBackend::new(client(), opts), "t", None, "DE").await;
    assert_eq!(res, Err(Error::RateLimited));
    // 20 + 40 + 80 ms of exponential backoff.
    assert!(
        started.elapsed() >= Duration::from_millis(140),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn cancel_during_backoff() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&s.free)
        .await;
    let opts = DeeplOptions {
        retry_base_delay: Duration::from_secs(30),
        ..s.options("k:fx")
    };
    let b = DeeplBackend::new(client(), opts);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        c2.cancel();
    });
    let started = Instant::now();
    let res = b.translate(request("t", None, "DE"), cancel).await.err();
    assert_eq!(res, Some(Error::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn bad_request_shows_deepl_message() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"message": "Value for 'target_lang' not supported."})),
        )
        .mount(&s.free)
        .await;
    assert_eq!(
        translate(&s.backend("k:fx"), "t", None, "XX").await,
        Err(Error::Service(
            "DeepL: Value for 'target_lang' not supported.".into()
        ))
    );
}

#[tokio::test]
async fn server_error_without_body() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&s.free)
        .await;
    assert_eq!(
        translate(&s.backend("k:fx"), "t", None, "DE").await,
        Err(Error::Service("DeepL returned HTTP 503".into()))
    );
}

#[tokio::test]
async fn malformed_success_body() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&s.free)
        .await;
    assert!(matches!(
        translate(&s.backend("k:fx"), "t", None, "DE").await,
        Err(Error::Service(_))
    ));
}

#[tokio::test]
async fn missing_detected_language_yields_only_delta() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"translations": [{"text": "x"}]})),
        )
        .mount(&s.free)
        .await;
    let chunks = translate(&s.backend("k:fx"), "t", Some("DE"), "EN-US")
        .await
        .unwrap();
    assert_eq!(chunks, [Chunk::Delta("x".into())]);
}

#[tokio::test]
async fn timeout_is_reported() {
    let s = Servers::start().await;
    Mock::given(path("/v2/translate"))
        .respond_with(ok_translation("late", "EN").set_delay(Duration::from_secs(5)))
        .mount(&s.free)
        .await;
    let opts = DeeplOptions {
        timeout: Duration::from_millis(200),
        ..s.options("k:fx")
    };
    let started = Instant::now();
    assert_eq!(
        translate(&DeeplBackend::new(client(), opts), "t", None, "DE").await,
        Err(Error::Timeout)
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn network_failure_is_connection_error() {
    let (l, addr) = common::listener().await;
    drop(l);
    let url = Url::parse(&format!("http://{addr}")).unwrap();
    let opts = DeeplOptions {
        endpoints: DeeplEndpoints {
            free: url.clone(),
            pro: url,
        },
        ..DeeplOptions::new("k:fx")
    };
    let res = translate(&DeeplBackend::new(client(), opts), "t", None, "DE").await;
    assert!(matches!(res, Err(Error::Connection(_))), "{res:?}");
}

#[tokio::test]
async fn empty_key_is_not_configured() {
    let s = Servers::start().await;
    let b = s.backend("  ");
    assert_eq!(
        translate(&b, "t", None, "DE").await,
        Err(Error::NotConfigured)
    );
    assert_eq!(b.usage().await, Err(Error::NotConfigured));
    assert!(s.free.received_requests().await.unwrap().is_empty());
    assert!(s.pro.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn usage_reports_quota() {
    let s = Servers::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/usage"))
        .and(header("authorization", "DeepL-Auth-Key k:fx"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"character_count": 120000, "character_limit": 1000000})),
        )
        .expect(1)
        .mount(&s.free)
        .await;
    assert_eq!(
        s.backend("k:fx").usage().await,
        Ok(Usage {
            character_count: 120_000,
            character_limit: 1_000_000
        })
    );
}

#[tokio::test]
async fn usage_with_invalid_key() {
    let s = Servers::start().await;
    for server in [&s.free, &s.pro] {
        Mock::given(path("/v2/usage"))
            .respond_with(ResponseTemplate::new(403))
            .mount(server)
            .await;
    }
    assert_eq!(s.backend("bad").usage().await, Err(Error::InvalidKey));
}

#[tokio::test]
async fn usage_host_fallback_and_quota_exhausted() {
    let s = Servers::start().await;
    Mock::given(path("/v2/usage"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&s.free)
        .await;
    Mock::given(path("/v2/usage"))
        .respond_with(ResponseTemplate::new(456))
        .mount(&s.pro)
        .await;
    let b = s.backend("k:fx");
    assert_eq!(b.usage().await, Err(Error::QuotaExceeded));
    assert_eq!(
        b.host(),
        DeeplHost::Free,
        "only a success switches the cached host"
    );
}

#[tokio::test]
async fn warm_up_reuses_the_connection() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncWriteExt;

    let (l, addr) = common::listener().await;
    let conns = Arc::new(AtomicUsize::new(0));
    let reqs = Arc::new(Mutex::new(Vec::new()));
    {
        let (conns, reqs) = (Arc::clone(&conns), Arc::clone(&reqs));
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = l.accept().await.unwrap();
                conns.fetch_add(1, Ordering::SeqCst);
                let reqs = Arc::clone(&reqs);
                tokio::spawn(async move {
                    while let Some(req) = common::read_request(&mut sock).await {
                        let line = req.lines().next().unwrap_or_default().to_owned();
                        reqs.lock().unwrap().push(line.clone());
                        let body = if line.starts_with("HEAD") {
                            String::new()
                        } else {
                            json!({"translations": [{"text": "hi", "detected_source_language": "DE"}]})
                                .to_string()
                        };
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                            if line.starts_with("HEAD") {
                                0
                            } else {
                                body.len()
                            }
                        );
                        sock.write_all(head.as_bytes()).await.unwrap();
                        sock.write_all(body.as_bytes()).await.unwrap();
                    }
                });
            }
        });
    }
    let url = Url::parse(&format!("http://{addr}")).unwrap();
    let opts = DeeplOptions {
        endpoints: DeeplEndpoints {
            free: url.clone(),
            pro: url,
        },
        ..DeeplOptions::new("k:fx")
    };
    let b = DeeplBackend::new(client(), opts);
    Translator::warm_up(&b).await;
    assert_eq!(conns.load(Ordering::SeqCst), 1);
    let chunks = translate(&b, "Hallo", None, "EN-US").await.unwrap();
    assert_eq!(common::text_of(&chunks), "hi");
    assert_eq!(
        conns.load(Ordering::SeqCst),
        1,
        "translation reused the warm connection"
    );
    let reqs = reqs.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].starts_with("HEAD / "));
    assert!(reqs[1].starts_with("POST /v2/translate "));
}
