use munki_ai::{AiClient, Config, Error, Provider};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::Duration,
};

fn mock(
    body: Value,
    status: &str,
) -> (String, mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let status = status.to_owned();
    let thread = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).unwrap();
            assert_ne!(n, 0);
            request.extend_from_slice(&buf[..n]);
            if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let len: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if request.len() >= end + 4 + len {
                    break;
                }
            }
        }
        tx.send(String::from_utf8(request).unwrap()).unwrap();
        let body = body.to_string();
        write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    (url, rx, thread)
}
fn config(provider: Provider, url: String) -> Config {
    let mut config = Config::new(provider, url, "test-model");
    config.enabled = true;
    config.consent_to_send_contents = true;
    config
}
fn openai(text: &str) -> Value {
    json!({"choices":[{"finish_reason":"stop","message":{"content":text}}]})
}

#[tokio::test]
async fn disabled_and_no_consent_never_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut client = AiClient::new(
        Config::new(
            Provider::OpenAiCompatible,
            format!("http://{}", listener.local_addr().unwrap()),
            "llama3.2",
        ),
        None,
    )
    .unwrap();
    assert_eq!(client.draft_reply("PRIVATE").await, Err(Error::Disabled));
    client.set_enabled(true);
    assert_eq!(
        client.draft_reply("PRIVATE").await,
        Err(Error::ConsentRequired)
    );
    assert_eq!(
        client.translate_query("PRIVATE").await,
        Err(Error::ConsentRequired)
    );
    assert!(client
        .classify_sender("PRIVATE", "PRIVATE", "PRIVATE")
        .await
        .is_err());
    assert!(listener.accept().is_err());
    let log = serde_json::to_string(&client.take_activity()).unwrap();
    assert!(!log.contains("PRIVATE"));
    assert_eq!(client.take_activity().len(), 0);
}

#[tokio::test]
async fn anthropic_shape_and_draft() {
    let (url, rx, thread) = mock(
        json!({"stop_reason":"end_turn","content":[{"type":"text","text":"Draft only"}]}),
        "200 OK",
    );
    let client = AiClient::new(config(Provider::Anthropic, url), Some("secret-key")).unwrap();
    assert_eq!(
        client.draft_reply("email context").await.unwrap(),
        "Draft only"
    );
    let request = rx.recv().unwrap();
    assert!(request.contains("x-api-key: secret-key"));
    assert!(request.contains("anthropic-version: 2023-06-01"));
    let payload: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        payload["messages"],
        json!([{"role":"user","content":"email context"}])
    );
    assert!(payload["system"].is_string());
    let log = serde_json::to_value(client.take_activity()).unwrap();
    assert_eq!(log[0].as_object().unwrap().len(), 4);
    assert_eq!(log[0]["success"], true);
    assert!(!log.to_string().contains("secret-key"));
    thread.join().unwrap();
}

#[tokio::test]
async fn openai_query_and_classification_privacy() {
    let (url, rx, thread) = mock(
        openai(r#"{"text":["invoice"],"destination":"paper_trail","unread":true}"#),
        "200 OK",
    );
    let client = AiClient::new(config(Provider::OpenAiCompatible, url), Some("secret")).unwrap();
    let query = client.translate_query("unread invoices").await.unwrap();
    assert_eq!(query.destination, Some(munki_core::Destination::PaperTrail));
    let request = rx.recv().unwrap();
    assert!(request.contains("authorization: Bearer secret"));
    let payload: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(payload["messages"][0]["role"], "system");
    assert_eq!(payload["messages"][1]["content"], "unread invoices");
    thread.join().unwrap();
    let (url, rx, thread) = mock(
        openai(r#"{"destination":"feed","reason":"newsletter"}"#),
        "200 OK",
    );
    let client = AiClient::new(config(Provider::OpenAiCompatible, url), None).unwrap();
    let preview = format!("{}PRIVATE_TAIL", "é".repeat(200));
    assert_eq!(
        client
            .classify_sender("a@example.com", "news", &preview)
            .await
            .unwrap()
            .destination,
        munki_core::Destination::Feed
    );
    let request = rx.recv().unwrap();
    assert!(!request.contains("PRIVATE_TAIL"));
    let payload: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let fields: Value =
        serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(fields.as_object().unwrap().len(), 3);
    assert_eq!(fields["preview"].as_str().unwrap().chars().count(), 200);
    thread.join().unwrap();
}

#[tokio::test]
async fn invalid_responses_are_failed_activities() {
    for text in [
        "not json",
        r#"{"destination":"trash"}"#,
        r#"{"before":1,"after":2}"#,
        r#"{"from":"invalid"}"#,
        r#"{"sql":"DELETE"}"#,
        "{}",
    ] {
        let (url, rx, thread) = mock(openai(text), "200 OK");
        let client = AiClient::new(config(Provider::OpenAiCompatible, url), None).unwrap();
        assert_eq!(
            client.translate_query("search").await,
            Err(Error::InvalidResponse)
        );
        assert!(!client.take_activity()[0].success);
        rx.recv().unwrap();
        thread.join().unwrap();
    }
    for body in [
        json!({}),
        json!({"choices":[{"finish_reason":"length","message":{"content":"partial"}}]}),
    ] {
        let (url, rx, thread) = mock(body, "200 OK");
        let client = AiClient::new(config(Provider::OpenAiCompatible, url), None).unwrap();
        assert_eq!(
            client.draft_reply("context").await,
            Err(Error::InvalidResponse)
        );
        rx.recv().unwrap();
        thread.join().unwrap();
    }
    let (url, rx, thread) = mock(json!({"error":"PRIVATE"}), "401 Unauthorized");
    let client = AiClient::new(config(Provider::OpenAiCompatible, url), None).unwrap();
    assert_eq!(client.draft_reply("context").await, Err(Error::Http));
    rx.recv().unwrap();
    thread.join().unwrap();
}

#[test]
fn endpoint_trust_boundary() {
    for endpoint in [
        "http://example.com/v1",
        "https://user:secret@example.com",
        "https://example.com?key=secret",
        "file:///tmp/model",
        "https://example.com/#secret",
    ] {
        assert!(matches!(
            AiClient::new(config(Provider::OpenAiCompatible, endpoint.into()), None),
            Err(Error::InvalidConfig)
        ));
    }
    for endpoint in [
        "https://example.com/custom",
        "http://127.0.0.1:11434/v1/chat/completions",
        "http://[::1]:11434/v1/chat/completions",
    ] {
        assert!(AiClient::new(config(Provider::OpenAiCompatible, endpoint.into()), None).is_ok());
    }
}

#[tokio::test]
async fn timeout_and_invalid_classification() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut cfg = config(
        Provider::OpenAiCompatible,
        format!("http://{}", listener.local_addr().unwrap()),
    );
    cfg.timeout = Duration::from_millis(40);
    let client = AiClient::new(cfg, None).unwrap();
    assert_eq!(client.draft_reply("context").await, Err(Error::Transport));
    assert!(!client.take_activity()[0].success);
    for text in [
        r#"{"destination":"delete","reason":"bad"}"#,
        r#"{"destination":"feed","reason":""}"#,
    ] {
        let (url, rx, thread) = mock(openai(text), "200 OK");
        let client = AiClient::new(config(Provider::OpenAiCompatible, url), None).unwrap();
        assert!(matches!(
            client
                .classify_sender("a@example.com", "news", "preview")
                .await,
            Err(Error::InvalidResponse)
        ));
        rx.recv().unwrap();
        thread.join().unwrap();
    }
}
