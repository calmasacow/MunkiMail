use super::*;
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

pub fn image_urls(source: &str) -> Result<Vec<String>> {
    ensure!(
        source.len() <= 2_000_000,
        "HTML exceeds native reader limit; use plain text"
    );
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    let mut stack = vec![(dom.document.clone(), 0)];
    let mut count = 0;
    let mut urls = Vec::new();
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        ensure!(
            count <= 20_000 && depth <= 128,
            "HTML structure exceeds native reader limit"
        );
        if let NodeData::Element { name, attrs, .. } = &node.data {
            if &*name.local == "img" {
                if let Some(src) = attrs.borrow().iter().find(|a| &*a.name.local == "src") {
                    let s = src.value.to_string();
                    if urls.len() < 30 && !urls.contains(&s) {
                        urls.push(s);
                    }
                }
            }
        }
        for child in node.children.borrow().iter() {
            stack.push((child.clone(), depth + 1));
        }
    }
    Ok(urls)
}
pub fn render(source: &str, app: &MailApp, cx: &Context<MailApp>) -> Result<AnyElement> {
    image_urls(source)?;
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    let mut count = 0;
    Ok(node(&dom.document, app, cx, 0, &mut count))
}
fn node(
    handle: &Handle,
    app: &MailApp,
    cx: &Context<MailApp>,
    depth: usize,
    count: &mut usize,
) -> AnyElement {
    *count += 1;
    if depth > 128 || *count > 20_000 {
        return div().into_any_element();
    }
    match &handle.data {
        NodeData::Text { contents } => div().child(line(&contents.borrow())).into_any_element(),
        NodeData::Element { name, attrs, .. } => {
            let tag: &str = &name.local;
            if ["script", "style", "iframe", "object", "embed", "head"].contains(&tag) {
                return div().into_any_element();
            }
            let attribute = |key: &str| {
                attrs
                    .borrow()
                    .iter()
                    .find(|a| &*a.name.local == key)
                    .map(|a| a.value.to_string())
            };
            if tag == "img" {
                let src = attribute("src").unwrap_or_default();
                if let Some(image) = app.images.get(&src) {
                    return gpui::img(image.clone())
                        .max_w_full()
                        .max_h(px(500.))
                        .object_fit(gpui::ObjectFit::Contain)
                        .into_any_element();
                }
                return div()
                    .text_sm()
                    .text_color(cx.theme().colors.fg_muted)
                    .child(line(
                        &attribute("alt").unwrap_or_else(|| "Image blocked or unavailable".into()),
                    ))
                    .into_any_element();
            }
            let mut container = div().id(("html-node", *count)).max_w_full();
            if [
                "body",
                "html",
                "div",
                "p",
                "section",
                "article",
                "ul",
                "ol",
                "table",
                "blockquote",
            ]
            .contains(&tag)
            {
                container = container.flex().flex_col().gap_2().mb_2();
            } else {
                container = container.flex().flex_wrap().gap_1();
            }
            if tag == "tr" {
                container = container.flex_row().gap_4();
            }
            if ["h1", "h2", "h3"].contains(&tag) {
                container = container
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .my_2();
            }
            if ["strong", "b", "th"].contains(&tag) {
                container = container.font_weight(gpui::FontWeight::BOLD);
            }
            if ["em", "i"].contains(&tag) {
                container = container.italic();
            }
            if tag == "br" {
                container = container.h(px(12.)).w_full();
            }
            if tag == "a" {
                if let Some(url) = attribute("href") {
                    let weak = cx.entity().downgrade();
                    container = container
                        .text_color(cx.theme().colors.accent)
                        .cursor_pointer()
                        .on_click(move |_, window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.command(
                                    "open_url",
                                    json!({"url":url}),
                                    window,
                                    cx,
                                    |_, _, _, _| {},
                                )
                            });
                        });
                }
            }
            for child in handle.children.borrow().iter() {
                container = container.child(node(child, app, cx, depth + 1, count));
            }
            container.into_any_element()
        }
        _ => div()
            .flex()
            .flex_col()
            .children(
                handle
                    .children
                    .borrow()
                    .iter()
                    .map(|child| node(child, app, cx, depth + 1, count)),
            )
            .into_any_element(),
    }
}
pub fn image(bytes: Vec<u8>) -> Result<Arc<gpui::Image>> {
    ensure!(
        bytes.len() <= 8 * 1024 * 1024,
        "Image exceeds 8 MiB preview limit"
    );
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode()?;
    let mut png = std::io::Cursor::new(Vec::new());
    decoded.write_to(&mut png, image::ImageFormat::Png)?;
    Ok(Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        png.into_inner(),
    )))
}
impl MailApp {
    pub fn load_images(&mut self, cx: &mut Context<Self>) {
        if self.demo || !self.remote {
            return;
        }
        let Some(detail) = &self.detail else {
            return;
        };
        let urls = match image_urls(detail["html"].as_str().unwrap_or("")) {
            Ok(v) => v,
            Err(e) => {
                self.error = e.to_string();
                return;
            }
        };
        let selected = self.selected.clone();
        let worker = self.worker.clone();
        let task = cx.background_executor().spawn(async move {
            worker.runtime.block_on(async move {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(15))
                    .build()?;
                let mut loaded = std::collections::HashMap::new();
                for address in urls {
                    let parsed = url::Url::parse(&address)?;
                    ensure!(
                        ["https", "http"].contains(&parsed.scheme())
                            && parsed.username().is_empty()
                            && parsed.password().is_none(),
                        "Invalid image URL"
                    );
                    let mut response = client.get(parsed).send().await?.error_for_status()?;
                    let mut bytes = Vec::new();
                    while let Some(chunk) = response.chunk().await? {
                        ensure!(
                            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
                            "Remote image exceeds 8 MiB"
                        );
                        bytes.extend_from_slice(&chunk);
                    }
                    loaded.insert(address, image(bytes)?);
                }
                anyhow::Ok(loaded)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.selected != selected || !this.remote {
                    return;
                }
                match result {
                    Ok(images) => this.images = images,
                    Err(e) => this.error = format!("Image: {e:#}"),
                };
                cx.notify();
            });
        })
        .detach();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn html_limits_and_images() {
        assert_eq!(
            image_urls("<p>Hello</p><img src='https://example.test/a.png'>").unwrap(),
            vec!["https://example.test/a.png"]
        );
        assert!(image_urls(&"x".repeat(2_000_001)).is_err());
        assert!(image(vec![0; 8 * 1024 * 1024 + 1]).is_err());
    }
}
