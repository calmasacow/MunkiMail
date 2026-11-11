use crate::{Record, Store};
use anyhow::{bail, ensure, Context, Result};
use quick_xml::{events::Event as XmlEvent, Reader};
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    header, Method, StatusCode, Url,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    time::Duration,
};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    pub downloaded: usize,
    pub uploaded: usize,
    pub deleted: usize,
    pub conflicts: usize,
}
/// Blocking CalDAV/CardDAV collection sync. No discovery, scheduling, or credentials in SQLite.
/// Use a separate client for each account. Redirects disabled to avoid credential forwarding.
pub struct DavClient {
    client: Client,
    auth: Option<Auth>,
}
enum Auth {
    Basic(String, String),
    Bearer(String),
}
impl DavClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            auth: None,
        })
    }
    pub fn basic(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.auth = Some(Auth::Basic(username.into(), password.into()));
        self
    }
    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = Some(Auth::Bearer(token.into()));
        self
    }
    fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let r = self.client.request(method, url);
        match &self.auth {
            Some(Auth::Basic(u, p)) => r.basic_auth(u, Some(p)),
            Some(Auth::Bearer(t)) => r.bearer_auth(t),
            None => r,
        }
    }
    fn inventory(&self, base: &Url) -> Result<BTreeMap<String, String>> {
        let response=self.request(Method::from_bytes(b"PROPFIND")?,base.clone())
            .header("Depth","1").header(header::CONTENT_TYPE,"application/xml; charset=utf-8")
            .body("<?xml version=\"1.0\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:getetag/><d:resourcetype/></d:prop></d:propfind>").send()?;
        ensure!(
            response.status() == StatusCode::MULTI_STATUS,
            "PROPFIND expected 207, got {}",
            response.status()
        );
        let xml = body(response)?;
        let mut out = BTreeMap::new();
        let mut saw_base = false;
        for item in parse_inventory(&xml)? {
            let url = resource_url(base, &item.href)?;
            if url == *base {
                ensure!(
                    item.collection,
                    "DAV collection response missing resourcetype"
                );
                saw_base = true;
                continue;
            }
            if item.collection {
                continue;
            }
            let etag = item.etag.context(
                "DAV resource missing successful getetag; refusing incomplete inventory",
            )?;
            ensure!(
                out.insert(url.to_string(), etag).is_none(),
                "duplicate DAV href"
            );
        }
        ensure!(
            saw_base,
            "incomplete DAV inventory: missing collection response"
        );
        Ok(out)
    }
    fn get(&self, url: Url) -> Result<Option<(String, String)>> {
        let r = self.request(Method::GET, url).send()?;
        if r.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            r.status() == StatusCode::OK,
            "GET expected 200, got {}",
            r.status()
        );
        let etag = etag(&r)?.context("GET missing ETag")?;
        Ok(Some((body(r)?, etag)))
    }
    fn capture_conflict(&self, store: &Store<'_>, r: &Record, url: Url) -> Result<()> {
        match self.get(url)? {
            Some((raw, etag)) => {
                crate::format::metadata(&raw, store.collection(&r.collection_id)?.kind)?;
                store.conflict(&r.id, Some(&raw), Some(&etag))?;
            }
            None => store.conflict(&r.id, None, None)?,
        }
        Ok(())
    }
    /// Full Depth:1 scan, fetch changes, merge, then conditional writes. Errors leave
    /// pending local edits dirty. Conflicts are never automatically overwritten.
    /// Serialize sync calls per collection; local edits may occur during network I/O.
    pub fn sync(&self, store: &Store<'_>, collection_id: &str) -> Result<SyncReport> {
        let c = store.collection(collection_id)?;
        let base = collection_url(
            c.url
                .as_deref()
                .context("local collection has no DAV URL")?,
        )?;
        let inventory = self.inventory(&base)?;
        let mut report = SyncReport::default();
        let before = store.records(&c.id)?;
        let mut staged = Vec::new();
        for (href, tag) in &inventory {
            let old = before.iter().find(|r| r.href.as_deref() == Some(href));
            if old.is_some_and(|r| r.etag.as_deref() == Some(tag)) {
                continue;
            }
            let (raw, etag) = self
                .get(resource_url(&base, href)?)?
                .context("resource vanished during DAV scan; retry sync")?;
            crate::format::metadata(&raw, c.kind)?;
            staged.push((href.clone(), raw, etag));
        }
        // Validate all downloads before interpreting absent resources as deletions.
        for (href, raw, tag) in staged {
            if store.remote(&c, &href, &raw, &tag)? {
                report.conflicts += 1;
            } else {
                report.downloaded += 1;
            }
        }
        for r in before {
            if r.etag.is_some() && r.href.as_ref().is_some_and(|h| !inventory.contains_key(h)) {
                if store.remote_missing(&r.id)? {
                    report.conflicts += 1;
                } else {
                    report.deleted += 1;
                }
            }
        }
        for mut r in store.records(&c.id)? {
            if !r.dirty || store.has_conflict(&r.id)? {
                continue;
            }
            if r.deleted && r.etag.is_none() {
                store.acknowledge_delete(&r)?;
                report.deleted += 1;
                continue;
            }
            if r.href.is_none() {
                let href = base
                    .join(&format!("{}.{}", uuid::Uuid::new_v4(), c.kind.extension()))?
                    .to_string();
                store.assign_href(&r.id, &href)?;
                r = store.get(&r.id)?;
            }
            let url = resource_url(&base, r.href.as_deref().unwrap())?;
            let method = if r.deleted {
                Method::DELETE
            } else {
                Method::PUT
            };
            let mut request = self.request(method, url.clone());
            if let Some(etag) = &r.etag {
                strong_etag(etag)?;
                request = request.header(header::IF_MATCH, etag);
            } else {
                request = request.header(header::IF_NONE_MATCH, "*");
            }
            if !r.deleted {
                request = request
                    .header(header::CONTENT_TYPE, c.kind.content_type())
                    .body(r.raw.clone());
            }
            let response = request.send()?;
            if matches!(
                response.status(),
                StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT
            ) {
                self.capture_conflict(store, &r, url)?;
                report.conflicts += 1;
                continue;
            }
            if r.deleted {
                ensure!(
                    matches!(
                        response.status(),
                        StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
                    ),
                    "DELETE failed: {}",
                    response.status()
                );
                store.acknowledge_delete(&r)?;
                report.deleted += 1;
            } else {
                ensure!(
                    matches!(
                        response.status(),
                        StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT
                    ),
                    "PUT failed: {}",
                    response.status()
                );
                if let Some(etag) = etag(&response)? {
                    store.acknowledge(&r, &etag)?;
                    report.uploaded += 1;
                } else {
                    // Some servers omit PUT ETags. A follow-up GET must match the sent
                    // bytes before its version can acknowledge the local revision.
                    match self.get(url)? {
                        Some((raw, etag)) if raw == r.raw => {
                            store.acknowledge(&r, &etag)?;
                            report.uploaded += 1;
                        }
                        Some((raw, etag)) => {
                            store.conflict(&r.id, Some(&raw), Some(&etag))?;
                            report.conflicts += 1;
                        }
                        None => {
                            store.conflict(&r.id, None, None)?;
                            report.conflicts += 1;
                        }
                    }
                }
            }
        }
        Ok(report)
    }
}
fn strong_etag(s: &str) -> Result<()> {
    ensure!(
        s.starts_with('"')
            && s.ends_with('"')
            && s.len() >= 2
            && !s[1..s.len() - 1].contains(['"', '\r', '\n']),
        "strong quoted ETag required for conditional DAV writes"
    );
    Ok(())
}
fn etag(r: &Response) -> Result<Option<String>> {
    r.headers()
        .get(header::ETAG)
        .map(|h| Ok(h.to_str()?.to_owned()))
        .transpose()
}
fn body(r: Response) -> Result<String> {
    let mut bytes = Vec::new();
    r.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 16 * 1024 * 1024, "DAV response too large");
    Ok(String::from_utf8(bytes)?)
}
pub(crate) fn collection_url(raw: &str) -> Result<Url> {
    let mut u = Url::parse(raw)?;
    ensure!(
        matches!(u.scheme(), "http" | "https") && u.host_str().is_some(),
        "HTTP(S) DAV URL required"
    );
    ensure!(
        u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none(),
        "DAV URL cannot include credentials, query or fragment"
    );
    if !u.path().ends_with('/') {
        u.set_path(&(u.path().to_owned() + "/"));
    }
    Ok(u)
}
fn resource_url(base: &Url, href: &str) -> Result<Url> {
    let u = base.join(href)?;
    ensure!(
        u.origin() == base.origin()
            && u.path().starts_with(base.path())
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none(),
        "DAV href outside collection"
    );
    // Encoded path separators/dot segments can escape the intended collection on
    // servers that decode before routing. Reject rather than forward credentials.
    let lower = u.path().to_ascii_lowercase();
    ensure!(
        !lower.contains("%2f")
            && !lower.contains("%5c")
            && !lower.contains("%2e")
            && !lower.contains("%25"),
        "ambiguous encoded DAV path"
    );
    Ok(u)
}
#[derive(Default)]
struct InventoryItem {
    href: String,
    etag: Option<String>,
    collection: bool,
}
fn parse_inventory(xml: &str) -> Result<Vec<InventoryItem>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let mut item: Option<InventoryItem> = None;
    let mut status = String::new();
    let mut prop_etag = None;
    let mut prop_collection = false;
    let mut seen_root = false;
    let mut seen = BTreeSet::new();
    loop {
        match reader.read_event()? {
            XmlEvent::Start(e) => {
                let name = String::from_utf8(e.local_name().as_ref().to_vec())?;
                ensure!(
                    name != "error",
                    "DAV error in multistatus; refusing partial inventory"
                );
                if stack.is_empty() {
                    ensure!(name == "multistatus", "expected DAV multistatus");
                    ensure!(!seen_root, "multiple XML roots");
                    seen_root = true;
                }
                if name == "response" {
                    ensure!(item.is_none(), "nested DAV response");
                    item = Some(InventoryItem::default());
                }
                if name == "propstat" {
                    status.clear();
                    prop_etag = None;
                    prop_collection = false;
                }
                if name == "collection" && stack.iter().any(|n| n == "resourcetype") {
                    prop_collection = true;
                }
                stack.push(name);
            }
            XmlEvent::Empty(e) => {
                if e.local_name().as_ref() == b"collection"
                    && stack.iter().any(|n| n == "resourcetype")
                {
                    prop_collection = true;
                }
            }
            XmlEvent::Text(e) => {
                append_xml_text(&e.decode()?, &stack, &mut item, &mut prop_etag, &mut status)
            }
            XmlEvent::CData(e) => {
                append_xml_text(&e.decode()?, &stack, &mut item, &mut prop_etag, &mut status)
            }
            XmlEvent::GeneralRef(e) => {
                let entity = format!("&{};", e.decode()?);
                let text = quick_xml::escape::unescape(&entity)?.into_owned();
                append_xml_text(&text, &stack, &mut item, &mut prop_etag, &mut status);
            }
            XmlEvent::End(e) => {
                let name = String::from_utf8(e.local_name().as_ref().to_vec())?;
                if name == "propstat" {
                    if status.split_whitespace().nth(1) == Some("200") {
                        if let Some(item) = item.as_mut() {
                            if prop_etag.is_some() {
                                item.etag = prop_etag.take();
                            }
                            item.collection |= prop_collection;
                        }
                    } else {
                        ensure!(
                            status.split_whitespace().nth(1).is_some(),
                            "missing propstat status"
                        );
                    }
                }
                if name == "response" {
                    let item = item.take().context("unbalanced DAV response")?;
                    ensure!(
                        !item.href.is_empty() && seen.insert(item.href.clone()),
                        "missing or duplicate href"
                    );
                    out.push(item);
                }
                ensure!(
                    stack.pop().as_deref() == Some(name.as_str()),
                    "unbalanced XML"
                );
            }
            XmlEvent::DocType(_) => bail!("XML DTD unsupported"),
            XmlEvent::Eof => break,
            _ => {}
        }
    }
    ensure!(
        seen_root && stack.is_empty() && item.is_none(),
        "incomplete DAV multistatus"
    );
    Ok(out)
}

fn append_xml_text(
    text: &str,
    stack: &[String],
    item: &mut Option<InventoryItem>,
    etag: &mut Option<String>,
    status: &mut String,
) {
    if let Some(item) = item.as_mut() {
        match stack.last().map(String::as_str) {
            Some("href") => item.href.push_str(text),
            Some("getetag") => etag.get_or_insert_with(String::new).push_str(text),
            Some("status") => status.push_str(text),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_and_xml_fail_closed() -> Result<()> {
        let base = collection_url("https://example.com/cal/")?;
        for href in [
            "https://evil.example/cal/a.ics",
            "/elsewhere/a.ics",
            "../a.ics",
            "a%2fb.ics",
            "a%252fb.ics",
            "a.ics?x=1",
        ] {
            assert!(resource_url(&base, href).is_err(), "{href}");
        }
        assert!(resource_url(&base, "a%20b.ics").is_ok());
        assert!(parse_inventory("<multistatus><response>").is_err());
        assert!(parse_inventory("<!DOCTYPE multistatus><multistatus/>").is_err());
        assert!(strong_etag("W/\"weak\"").is_err());
        let items=parse_inventory("<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/cal/a&amp;b.ics</d:href><d:propstat><d:prop><d:getetag>&quot;a&amp;b&quot;</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>")?;
        assert_eq!(items[0].href, "/cal/a&b.ics");
        assert_eq!(items[0].etag.as_deref(), Some("\"a&b\""));
        Ok(())
    }
}
