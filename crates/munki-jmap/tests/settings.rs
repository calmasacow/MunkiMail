use munki_jmap::{Client, Credentials};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
};
const MAIL: &str = "urn:ietf:params:jmap:mail";
const RAW: &str = "From: me@example.org\r\nMessage-ID: <state@example.org>\r\nContent-Type: text/plain\r\n\r\nstate snapshot";

fn boxes() -> Value {
    json!([{"id":"root","name":"MunkiMail","parentId":null,"role":null},{"id":"settings","name":".state","parentId":"root","role":null}])
}

#[tokio::test]
async fn lazy_append_retry_and_load_settings() {
    exercise_append(false).await;
    exercise_append(true).await;
}

async fn exercise_append(fail_import: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server_origin = origin.clone();
    let server = std::thread::spawn(move || {
        for step in 0..if fail_import { 8 } else { 14 } {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                stream.read_exact(&mut b).unwrap();
                header.push(b[0]);
            }
            let header = String::from_utf8(header).unwrap();
            assert!(header
                .to_lowercase()
                .contains("authorization: bearer secret"));
            let n: usize = header
                .lines()
                .find_map(|s| {
                    s.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse().unwrap())
                })
                .unwrap_or(0);
            let mut bytes = vec![0; n];
            stream.read_exact(&mut bytes).unwrap();
            let body = if step == 0 {
                assert!(header.starts_with("GET /.well-known/jmap "));
                json!({"apiUrl":format!("{server_origin}/api"),"uploadUrl":format!("{server_origin}/upload/{{accountId}}"),"downloadUrl":format!("{server_origin}/download/{{accountId}}/{{blobId}}"),"capabilities":{"urn:ietf:params:jmap:core":{},MAIL:{}},"primaryAccounts":{MAIL:"remote"},"accounts":{"remote":{"accountCapabilities":{MAIL:{}}}}}).to_string()
            } else if step == 6 {
                assert!(header.starts_with("POST /upload/remote "));
                assert!(header
                    .to_lowercase()
                    .contains("content-type: message/rfc822"));
                assert_eq!(bytes, RAW.as_bytes());
                json!({"accountId":"remote","blobId":"blob","size":RAW.len(),"type":"message/rfc822"}).to_string()
            } else if step == 13 {
                assert!(header.starts_with("GET /download/remote/blob "));
                RAW.to_owned()
            } else {
                assert!(header.starts_with("POST /api "));
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                let call = &body["methodCalls"][0];
                assert_eq!(call[1]["accountId"], "remote");
                let (method, mut result) = match step {
                    1 | 2 => ("Mailbox/get", json!({"list":[]})),
                    3 => {
                        assert_eq!(
                            call[1]["create"]["mailbox"],
                            json!({"name":"MunkiMail","parentId":null})
                        );
                        ("Mailbox/set", json!({"created":{"mailbox":{"id":"root"}}}))
                    }
                    4 => {
                        assert_eq!(
                            call[1]["create"]["mailbox"],
                            json!({"name":".state","parentId":"root"})
                        );
                        (
                            "Mailbox/set",
                            json!({"created":{"mailbox":{"id":"settings"}}}),
                        )
                    }
                    5 | 9 | 11 => {
                        assert_eq!(call[1]["filter"]["inMailbox"], "settings");
                        if step != 11 {
                            assert_eq!(
                                call[1]["filter"]["header"],
                                json!(["Message-ID", "<state@example.org>"])
                            );
                        }
                        (
                            "Email/query",
                            json!({"queryState":"q","ids":if step==5 {json!([])}else{json!(["state-email"])},"total":if step==5 {0}else{1}}),
                        )
                    }
                    7 => {
                        assert_eq!(
                            call[1]["emails"]["message"],
                            json!({"blobId":"blob","mailboxIds":{"settings":true},"keywords":{"$seen":true}})
                        );
                        (
                            "Email/import",
                            if fail_import {
                                json!({"notCreated":{"message":{"type":"forbidden"}}})
                            } else {
                                json!({"created":{"message":{"id":"state-email"}}})
                            },
                        )
                    }
                    8 | 10 => ("Mailbox/get", json!({"list":boxes()})),
                    12 => {
                        assert_eq!(call[1]["ids"], json!(["state-email"]));
                        (
                            "Email/get",
                            json!({"list":[{"id":"state-email","blobId":"blob"}],"notFound":[]}),
                        )
                    }
                    _ => unreachable!("{step}"),
                };
                assert_eq!(call[0], method);
                result["accountId"] = json!("remote");
                json!({"methodResponses":[[method,result,"m"]]}).to_string()
            };
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        }
    });
    let client = Client::discover(&origin, Credentials::Bearer("secret".into()), None)
        .await
        .unwrap();
    assert!(client.load_settings().await.unwrap().is_empty());
    if fail_import {
        let error = client
            .append_to_mailbox("MunkiMail/.state", "state@example.org", RAW.as_bytes())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("forbidden"));
        server.join().unwrap();
        return;
    }
    client
        .append_to_mailbox("MunkiMail/.state", "<state@example.org>", RAW.as_bytes())
        .await
        .unwrap();
    // Replay sees existing Message-ID: no second upload or import.
    client
        .append_to_mailbox("MunkiMail/.state", "state@example.org", RAW.as_bytes())
        .await
        .unwrap();
    assert_eq!(
        client.load_settings().await.unwrap(),
        vec![RAW.as_bytes().to_vec()]
    );
    server.join().unwrap();
}

#[tokio::test]
async fn oversized_session_and_foreign_transfer_endpoints_rejected() {
    for kind in ["oversized", "uploadUrl", "downloadUrl"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server_origin = origin.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                stream.read_exact(&mut b).unwrap();
                bytes.push(b[0]);
            }
            if kind == "oversized" {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 33554433\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            } else {
                let mut session = json!({"apiUrl":format!("{server_origin}/api"),"capabilities":{"urn:ietf:params:jmap:core":{},MAIL:{}},"primaryAccounts":{MAIL:"remote"},"accounts":{"remote":{"accountCapabilities":{MAIL:{}}}}});
                session[kind] = json!("https://foreign.invalid/{accountId}/{blobId}");
                let body = session.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let error = Client::discover(&origin, Credentials::Bearer("secret".into()), None)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains(if kind == "oversized" {
                "exceeds"
            } else {
                "cross-origin"
            }),
            "{error}"
        );
        server.join().unwrap();
    }
}
