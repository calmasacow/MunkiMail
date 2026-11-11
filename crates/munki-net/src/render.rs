//! HTML sanitizing and tracker blocking.
//!
//! Any remote fetch made while rendering tells the sender you opened the
//! mail, when, and from what IP. So the default is: no remote fetches at
//! all. Images are kept only if they are inline (`cid:`) or embedded
//! (`data:`); everything else is dropped and counted, so the UI can offer
//! "load remote content" as an explicit choice.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    /// Default. Strip every remote resource.
    Block,
    /// The user chose to load remote content for this message or sender.
    Allow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub html: String,
    /// Remote resources removed. Non-zero = show the "load images" bar.
    pub blocked: usize,
}

/// Sanitize untrusted mail HTML for display.
///
/// Scripts, forms, event handlers, iframes, and `style` blocks are always
/// removed, regardless of `remote` — `Allow` only relaxes image/link
/// fetching, never script execution.
pub fn sanitize(html: &str, remote: Remote) -> Rendered {
    let blocked = Arc::new(AtomicUsize::new(0));
    let counter = blocked.clone();

    let mut b = ammonia::Builder::default();
    b.url_schemes(["http", "https", "mailto", "cid", "data"].into())
        // Links open in the system browser; never leak the referrer.
        .link_rel(Some("noopener noreferrer nofollow"))
        // `style` attributes can carry `background: url(...)` beacons.
        // Keep layout-only properties; drop anything that can fetch.
        .add_generic_attributes(["style"])
        .filter_style_properties(
            [
                "color",
                "background-color",
                "font-family",
                "font-size",
                "font-weight",
                "font-style",
                "text-align",
                "text-decoration",
                "line-height",
                "margin",
                "margin-top",
                "margin-bottom",
                "margin-left",
                "margin-right",
                "padding",
                "padding-top",
                "padding-bottom",
                "padding-left",
                "padding-right",
                "width",
                "max-width",
                "height",
                "border",
                "border-collapse",
                "vertical-align",
                "display",
            ]
            .into(),
        )
        .attribute_filter(move |el, attr, value| {
            let is_fetch = matches!(
                (el, attr),
                ("img", "src") | ("img", "srcset") | ("video", "poster") | ("source", "src")
            );
            if !is_fetch {
                return Some(value.into());
            }
            let v = value.trim_start().to_ascii_lowercase();
            if v.starts_with("cid:") || v.starts_with("data:image/") {
                return Some(value.into());
            }
            if remote == Remote::Allow && (v.starts_with("https:") || v.starts_with("http:")) {
                return Some(value.into());
            }
            counter.fetch_add(1, Ordering::Relaxed);
            None
        });

    let html = b.clean(html).to_string();
    Rendered {
        html,
        blocked: blocked.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_pixel_is_removed_and_counted() {
        let r = sanitize(
            r#"<p>hi</p><img src="https://t.example/open?id=42" width=1 height=1>"#,
            Remote::Block,
        );
        assert!(!r.html.contains("t.example"), "{}", r.html);
        assert_eq!(r.blocked, 1);
        assert!(r.html.contains("<p>hi</p>"));
    }

    #[test]
    fn inline_and_embedded_images_survive() {
        let r = sanitize(
            r#"<img src="cid:logo@x"><img src="data:image/png;base64,iVBOR">"#,
            Remote::Block,
        );
        assert!(r.html.contains("cid:logo@x"));
        assert!(r.html.contains("data:image/png"));
        assert_eq!(r.blocked, 0);
    }

    #[test]
    fn css_url_beacons_are_stripped() {
        let r = sanitize(
            r#"<div style="color: red; background: url(https://t.example/b.gif)">x</div>"#,
            Remote::Block,
        );
        assert!(!r.html.contains("t.example"), "{}", r.html);
        assert!(
            r.html.contains("color"),
            "harmless styling kept: {}",
            r.html
        );
    }

    #[test]
    fn style_blocks_are_removed_entirely() {
        let r = sanitize(
            "<style>@import url(https://t.example/a.css)</style><p>x</p>",
            Remote::Allow,
        );
        assert!(!r.html.contains("t.example"), "{}", r.html);
    }

    #[test]
    fn scripts_and_handlers_never_survive_even_when_allowed() {
        let r = sanitize(
            r#"<script>alert(1)</script><img src="https://a.example/x.png" onerror="alert(2)"><a href="javascript:alert(3)">x</a>"#,
            Remote::Allow,
        );
        assert!(!r.html.contains("alert"), "{}", r.html);
        assert!(
            r.html.contains("https://a.example/x.png"),
            "allowed image kept"
        );
    }

    #[test]
    fn links_do_not_leak_referrer() {
        let r = sanitize(r#"<a href="https://example.com">x</a>"#, Remote::Block);
        assert!(r.html.contains("noreferrer"), "{}", r.html);
    }
}
