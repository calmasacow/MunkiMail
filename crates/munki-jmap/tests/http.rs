use munki_jmap::{Client, Credentials};
use munki_store::Store;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
};

const MAIL: &str = "urn:ietf:params:jmap:mail";
#[tokio::test]
async fn discovery_import_changes_pending_and_delete() {
    exercise_sync(false).await;
    exercise_sync(true).await;
}

async fn exercise_sync(download: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let api = format!("{origin}/api");
    let server = std::thread::spawn(move || {
        let mut step = 0;
        while step < 11 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                if bytes.ends_with(b"\r\n\r\n") {
                    break bytes.len();
                }
            };
            let headers = String::from_utf8(bytes.clone()).unwrap();
            assert!(headers
                .to_lowercase()
                .contains("authorization: bearer secret"));
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap_or(0);
            bytes.resize(header_end + length, 0);
            socket.read_exact(&mut bytes[header_end..]).unwrap();
            if headers.starts_with("GET /download/remote/blob ") {
                assert!(download);
                let raw = b"From: sender@example.org\r\nSubject: Original\r\nContent-Type: text/plain\r\n\r\nreal body with original attachment MIME";
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    raw.len()
                )
                .unwrap();
                socket.write_all(raw).unwrap();
                continue;
            }
            let result = if step == 0 {
                assert!(headers.starts_with("GET /.well-known/jmap "));
                json!({"downloadUrl": if download { Some(api.replace("/api","/download/{accountId}/{blobId}")) } else {None},"apiUrl":api,"capabilities":{"urn:ietf:params:jmap:core":{},MAIL:{}},"primaryAccounts":{MAIL:"remote"},"accounts":{"remote":{"accountCapabilities":{MAIL:{}}}}})
            } else {
                let request: Value = serde_json::from_slice(&bytes[header_end..]).unwrap();
                assert_eq!(request["using"], json!(["urn:ietf:params:jmap:core", MAIL]));
                let call = &request["methodCalls"][0];
                assert_eq!(call[1]["accountId"], "remote");
                let method = call[0].as_str().unwrap();
                if method == "Mailbox/get" {
                    let body = json!({"methodResponses":[["Mailbox/get",{"accountId":"remote","list":[{"id":"inbox","name":"Inbox","role":"inbox","parentId":null},{"id":"root","name":"MunkiMail","role":null,"parentId":null},{"id":"settings","name":".state","role":null,"parentId":"root"}]},"m"]]}).to_string();
                    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
                    continue;
                }
                let args = match step {
                    1 => {
                        assert_eq!(method, "Email/get");
                        assert_eq!(call[1]["ids"], json!([]));
                        json!({"state":"s1","list":[],"notFound":[]})
                    }
                    2 => {
                        assert_eq!(method, "Email/query");
                        assert_eq!(call[1]["position"], 0);
                        assert_eq!(call[1]["filter"]["conditions"][0]["inMailbox"], "inbox");
                        assert_eq!(
                            call[1]["filter"]["conditions"][1]["conditions"][0]["inMailbox"],
                            "settings"
                        );
                        json!({"queryState":"q","ids":["e1"],"total":1})
                    }
                    3 | 5 | 7 => {
                        assert_eq!(method, "Email/get");
                        assert_eq!(
                            call[1]["ids"],
                            if step == 3 {
                                json!(["e1"])
                            } else {
                                json!(["e1", "internal"])
                            }
                        );
                        assert_eq!(call[1]["fetchAllBodyValues"], true);
                        json!({"state":"s2","notFound":[],"list":[{"id":"e1","blobId":"blob","mailboxIds":{"inbox":true},"messageId":["m@example.org"],"threadId":"t1","from":[{"email":"sender@example.org"}],"subject":"Hello","receivedAt":"2026-01-01T00:00:00Z","keywords":{},"hasAttachment":false,"textBody":[{"partId":"1"}],"bodyValues":{"1":{"value":"real body","isTruncated":false,"isEncodingProblem":false}}}]})
                    }
                    4 | 6 => {
                        assert_eq!(method, "Email/changes");
                        assert_eq!(call[1]["sinceState"], "s1");
                        json!({"oldState":"s1","newState":"s2","hasMoreChanges":false,"created":[],"updated":["e1","internal"],"destroyed":[]})
                    }
                    8 => {
                        assert_eq!(method, "Email/set");
                        assert_eq!(call[1]["update"]["e1"]["keywords/$seen"], true);
                        json!({"updated":{"e1":null}})
                    }
                    9 | 10 => {
                        assert_eq!(method, "Email/changes");
                        assert_eq!(call[1]["sinceState"], "s2");
                        json!({"oldState":"s2","newState":"s3","hasMoreChanges":false,"created":[],"updated":[],"destroyed":["e1"]})
                    }
                    _ => unreachable!(),
                };
                let mut args = args;
                if step == 5 || step == 7 {
                    args["list"].as_array_mut().unwrap().push(json!({"id":"internal","mailboxIds":{"inbox":true,"settings":true},"subject":"private state"}));
                }
                args["accountId"] = json!("remote");
                json!({"methodResponses":[[method,args,"m"]]})
            };
            let body = serde_json::to_vec(&result).unwrap();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
            socket.write_all(&body).unwrap();
            step += 1;
        }
    });
    let client = Client::discover(
        &format!("{origin}/settings"),
        Credentials::Bearer("secret".into()),
        None,
    )
    .await
    .unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let account = store.add_account("me@example.org", "Me", "jmap").unwrap();
    assert_eq!(client.sync(&mut store, account).await.unwrap().imported, 1);
    let local = store.find(account, "JMAP/Inbox", 1).unwrap().unwrap();
    let body: String = store
        .conn()
        .query_row(
            "SELECT body FROM messages_fts WHERE rowid=?1",
            [local],
            |r| r.get(0),
        )
        .unwrap();
    assert!(body.contains("real body"));
    let raw = store.raw_message(account, local).unwrap().unwrap();
    let mime = String::from_utf8(raw).unwrap();
    assert!(mime.contains("real body"));
    assert!(mime.contains(if download {
        "Subject: Original"
    } else {
        "Subject: Hello"
    }));
    assert_eq!(
        Client::server_id(&store, account, local)
            .unwrap()
            .as_deref(),
        Some("e1")
    );
    store.mark_seen(local, true).unwrap();
    assert_eq!(client.sync(&mut store, account).await.unwrap().deferred, 1);
    let seen: bool = store
        .conn()
        .query_row("SELECT seen FROM messages WHERE id=?1", [local], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(seen);
    store
        .conn()
        .execute("DELETE FROM outbox WHERE account=?1", [account])
        .unwrap();
    assert_eq!(client.sync(&mut store, account).await.unwrap().imported, 1);
    assert_eq!(store.find(account, "JMAP/Inbox", 1).unwrap(), Some(local));
    let count: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    client.set_keywords("e1", &[("$seen", true)]).await.unwrap();
    store.mark_seen(local, true).unwrap();
    assert_eq!(client.sync(&mut store, account).await.unwrap().deferred, 1);
    assert!(store.message(account, local).is_ok());
    store
        .conn()
        .execute("DELETE FROM outbox WHERE account=?1", [account])
        .unwrap();
    assert_eq!(client.sync(&mut store, account).await.unwrap().deleted, 1);
    assert_eq!(store.find(account, "JMAP/Inbox", 1).unwrap(), None);
    server.join().unwrap();
}

#[tokio::test]
async fn insecure_and_embedded_credentials_rejected() {
    for url in ["http://example.org", "https://name:password@example.org"] {
        assert!(
            Client::discover(url, Credentials::Bearer("secret".into()), None)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn redirects_and_foreign_api_rejected() {
    for redirect in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let body = json!({"apiUrl":"https://foreign.invalid/api","capabilities":{"urn:ietf:params:jmap:core":{},MAIL:{}},"primaryAccounts":{MAIL:"remote"},"accounts":{"remote":{"accountCapabilities":{MAIL:{}}}}}).to_string();
            if redirect {
                write!(stream,"HTTP/1.1 302 Found\r\nLocation: https://foreign.invalid/session\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            } else {
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        let error = Client::discover(
            &origin,
            Credentials::Basic {
                username: "me".into(),
                password: "secret".into(),
            },
            None,
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(
            error.contains(if redirect { "302" } else { "cross-origin" }),
            "{error}"
        );
        server.join().unwrap();
    }
}
