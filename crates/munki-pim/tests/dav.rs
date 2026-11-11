use munki_pim::{Contact, DavClient, Kind, Resolution, Store};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

#[derive(Default)]
struct State {
    resources: BTreeMap<String, (String, String)>,
    requests: Vec<(String, String, BTreeMap<String, String>)>,
    sequence: usize,
    race_put: bool,
    broken_listing: bool,
    omit_put_etag: bool,
}
struct Server {
    url: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/dav/", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &shared),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            url,
            state,
            stop,
            worker: Some(worker),
        }
    }
    fn set(&self, path: &str, raw: &str) {
        let mut s = self.state.lock().unwrap();
        s.sequence += 1;
        let tag = format!("\"v{}\"", s.sequence);
        s.resources.insert(path.into(), (raw.into(), tag));
    }
    fn raw(&self, path: &str) -> String {
        self.state.lock().unwrap().resources[path].0.clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}
fn serve(mut stream: TcpStream, state: &Mutex<State>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut request = line.split_whitespace();
    let method = request.next().unwrap().to_owned();
    let path = request.next().unwrap().to_owned();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (k, v) = line.split_once(':').unwrap();
        headers.insert(k.to_lowercase(), v.trim().to_owned());
    }
    let len = headers
        .get("content-length")
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(0);
    let mut raw = vec![0; len];
    reader.read_exact(&mut raw).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let mut state = state.lock().unwrap();
    state
        .requests
        .push((method.clone(), path.clone(), headers.clone()));
    let mut etag = None;
    let mut body = String::new();
    let code = match method.as_str() {
        "PROPFIND" => {
            if state.broken_listing {
                body = "<d:multistatus xmlns:d=\"DAV:\"><d:response>".into();
            } else {
                body="<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/dav/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>".into();
                for (href, (_, tag)) in &state.resources {
                    body+=&format!("<d:response><d:href>{}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",escape(href),escape(tag));
                }
                body += "</d:multistatus>";
            }
            207
        }
        "GET" => {
            if let Some((raw, tag)) = state.resources.get(&path) {
                body = raw.clone();
                etag = Some(tag.clone());
                200
            } else {
                404
            }
        }
        "PUT" | "DELETE" => {
            if state.race_put && method == "PUT" {
                state.race_put = false;
                state.sequence += 1;
                let tag = format!("\"v{}\"", state.sequence);
                if let Some(r) = state.resources.get_mut(&path) {
                    r.1 = tag;
                    r.0 = r.0.replace("Remote", "Raced");
                }
            }
            let exists = state.resources.contains_key(&path);
            let matched = if let Some(tag) = headers.get("if-match") {
                state.resources.get(&path).is_some_and(|r| &r.1 == tag)
            } else {
                headers.get("if-none-match").is_some_and(|v| v == "*") && !exists
            };
            if !matched {
                412
            } else if method == "DELETE" {
                state.resources.remove(&path);
                204
            } else {
                state.sequence += 1;
                let tag = format!("\"v{}\"", state.sequence);
                state.resources.insert(path, (raw, tag.clone()));
                if !state.omit_put_etag {
                    etag = Some(tag);
                }
                if exists {
                    204
                } else {
                    201
                }
            }
        }
        _ => 405,
    };
    let extra = etag.map(|t| format!("ETag: {t}\r\n")).unwrap_or_default();
    write!(
        stream,
        "HTTP/1.1 {code} Status\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
}
fn ics(uid: &str, title: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART:20261103T150000Z\r\nDTEND:20261103T160000Z\r\nSUMMARY:{title}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
}

#[test]
fn calendar_download_edit_conditional_conflicts_and_resolutions() -> anyhow::Result<()> {
    let server = Server::new();
    server.set("/dav/meeting.ics", &ics("meeting", "Remote"));
    let conn = Connection::open_in_memory()?;
    let store = Store::new(&conn)?;
    let cal = store.create_collection("Remote", Kind::Calendar, Some(&server.url))?;
    let dav = DavClient::new()?;
    assert_eq!(dav.sync(&store, &cal.id)?.downloaded, 1);
    let r = store.records(&cal.id)?.remove(0);
    assert!(!r.dirty);
    let mut e = r.event()?;
    e.summary = "Local".into();
    store.save_event(&cal.id, Some(&r.id), Some(r.revision), &e)?;
    assert_eq!(dav.sync(&store, &cal.id)?.uploaded, 1);
    assert!(server.raw("/dav/meeting.ics").contains("SUMMARY:Local"));
    assert!(server
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .any(|(m, _, h)| m == "PUT" && h.get("if-match") == Some(&"\"v1\"".into())));
    let r = store.get(&r.id)?;
    e.summary = "Unsynced edit".into();
    store.save_event(&cal.id, Some(&r.id), Some(r.revision), &e)?;
    server.set("/dav/meeting.ics", &ics("meeting", "Remote changed"));
    assert_eq!(dav.sync(&store, &cal.id)?.conflicts, 1);
    assert_eq!(store.get(&r.id)?.event()?.summary, "Unsynced edit");
    assert!(store.conflicts(&cal.id)?[0]
        .remote_raw
        .as_ref()
        .unwrap()
        .contains("Remote changed"));
    let r = store.get(&r.id)?;
    store.resolve(&r.id, r.revision, Resolution::KeepLocal)?;
    assert_eq!(dav.sync(&store, &cal.id)?.uploaded, 1);
    let r = store.get(&r.id)?;
    e.summary = "Another edit".into();
    store.save_event(&cal.id, Some(&r.id), Some(r.revision), &e)?;
    server.set("/dav/meeting.ics", &ics("meeting", "Server wins by choice"));
    dav.sync(&store, &cal.id)?;
    let r = store.get(&r.id)?;
    store.resolve(&r.id, r.revision, Resolution::AcceptRemote)?;
    assert_eq!(store.get(&r.id)?.event()?.summary, "Server wins by choice");
    assert!(!store.get(&r.id)?.dirty);
    Ok(())
}
#[test]
fn write_race_412_never_clobbers_local_and_remote_delete_is_explicit() -> anyhow::Result<()> {
    let server = Server::new();
    server.set("/dav/race.ics", &ics("race", "Remote"));
    let conn = Connection::open_in_memory()?;
    let store = Store::new(&conn)?;
    let dav = DavClient::new()?;
    let cal = store.create_collection("Remote", Kind::Calendar, Some(&server.url))?;
    dav.sync(&store, &cal.id)?;
    let r = store.records(&cal.id)?.remove(0);
    let mut e = r.event()?;
    e.summary = "Preserve me".into();
    store.save_event(&cal.id, Some(&r.id), Some(r.revision), &e)?;
    server.state.lock().unwrap().race_put = true;
    assert_eq!(dav.sync(&store, &cal.id)?.conflicts, 1);
    assert_eq!(store.get(&r.id)?.event()?.summary, "Preserve me");
    assert!(server.raw("/dav/race.ics").contains("Raced"));
    let r = store.get(&r.id)?;
    store.resolve(&r.id, r.revision, Resolution::KeepLocal)?;
    server.state.lock().unwrap().resources.clear();
    assert_eq!(dav.sync(&store, &cal.id)?.conflicts, 1);
    assert!(store.conflicts(&cal.id)?[0].remote_raw.is_none());
    let r = store.get(&r.id)?;
    store.resolve(&r.id, r.revision, Resolution::KeepLocal)?;
    assert_eq!(dav.sync(&store, &cal.id)?.uploaded, 1);
    assert!(server
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .any(|(m, _, h)| m == "PUT" && h.get("if-none-match") == Some(&"*".into())));
    let r = store.get(&r.id)?;
    store.delete(&r.id, r.revision)?;
    assert_eq!(dav.sync(&store, &cal.id)?.deleted, 1);
    assert!(store.get(&r.id).is_err());
    Ok(())
}
#[test]
fn carddav_create_missing_put_etag_bad_inventory_and_clean_remote_deletion() -> anyhow::Result<()> {
    let server = Server::new();
    server.state.lock().unwrap().omit_put_etag = true;
    let conn = Connection::open_in_memory()?;
    let store = Store::new(&conn)?;
    let dav = DavClient::new()?.basic("user", "password");
    let book = store.create_collection("Contacts", Kind::AddressBook, Some(&server.url))?;
    let c = Contact {
        uid: "alice".into(),
        name: "Alice".into(),
        emails: vec!["alice@example.com".into()],
        phones: vec![],
        organization: String::new(),
        note: String::new(),
    };
    let r = store.save_contact(&book.id, None, None, &c)?;
    assert_eq!(dav.sync(&store, &book.id)?.uploaded, 1);
    assert!(!store.get(&r.id)?.dirty);
    let locked = server.state.lock().unwrap();
    assert!(locked
        .requests
        .iter()
        .any(|(m, _, h)| m == "PUT" && h["content-type"].starts_with("text/vcard")));
    assert!(locked
        .requests
        .iter()
        .all(|(_, _, h)| h.contains_key("authorization")));
    drop(locked);
    server.state.lock().unwrap().broken_listing = true;
    assert!(dav.sync(&store, &book.id).is_err());
    assert!(store.get(&r.id).is_ok());
    let mut locked = server.state.lock().unwrap();
    locked.broken_listing = false;
    locked.resources.clear();
    drop(locked);
    assert_eq!(dav.sync(&store, &book.id)?.deleted, 1);
    assert!(store.get(&r.id).is_err());
    Ok(())
}
