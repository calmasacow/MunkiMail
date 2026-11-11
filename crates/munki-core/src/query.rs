//! Search query grammar.
//!
//! Lives in core, with no I/O, so three callers share one definition:
//! the store (which executes it), the UI (which highlights it), and the
//! AI layer (whose only job is to emit this syntax from plain English —
//! it never touches SQL).
//!
//! Grammar: whitespace-separated terms. `key:value` is a filter,
//! `"quoted text"` is a phrase, anything else is a full-text term.
//! Unknown keys are not errors; they fall through to full-text, so a
//! stray colon in a search never produces a wall of red.

use serde::{Deserialize, Serialize};

use crate::{Address, Destination};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    /// Full-text terms and phrases, ANDed together.
    pub text: Vec<String>,
    pub from: Option<Address>,
    pub folder: Option<String>,
    pub destination: Option<Destination>,
    pub has_attachment: Option<bool>,
    pub unread: Option<bool>,
    /// Unix seconds, exclusive upper bound.
    pub before: Option<i64>,
    /// Unix seconds, inclusive lower bound.
    pub after: Option<i64>,
}

impl Query {
    pub fn is_empty(&self) -> bool {
        *self == Query::default()
    }
}

pub fn parse(input: &str) -> Query {
    let mut q = Query::default();
    for tok in tokenize(input) {
        match tok.split_once(':') {
            Some(("from", v)) if !v.is_empty() => q.from = Some(Address::new(v)),
            Some(("has", "attachment" | "attachments" | "file")) => q.has_attachment = Some(true),
            Some(("is", "unread")) => q.unread = Some(true),
            Some(("is", "read")) => q.unread = Some(false),
            Some(("in", v)) => match v {
                "imbox" => q.destination = Some(Destination::Imbox),
                "feed" => q.destination = Some(Destination::Feed),
                "paper" | "paper_trail" | "papertrail" => {
                    q.destination = Some(Destination::PaperTrail)
                }
                "screener" => q.destination = Some(Destination::Screener),
                _ => q.text.push(tok),
            },
            Some(("before", v)) => match parse_date(v) {
                Some(t) => q.before = Some(t),
                None => q.text.push(tok),
            },
            Some(("after", v)) => match parse_date(v) {
                Some(t) => q.after = Some(t),
                None => q.text.push(tok),
            },
            _ => q.text.push(tok),
        }
    }
    q
}

/// Split on whitespace, keeping `"quoted phrases"` whole. An unclosed
/// quote runs to end of input rather than failing: the user is still typing.
fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `YYYY-MM-DD` to unix seconds at UTC midnight.
///
/// Hand-rolled days-from-civil rather than pulling in a date crate for
/// one function. Valid for any Gregorian date; rejects out-of-range
/// month/day so `before:2024-13-01` falls through to full-text.
fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_words_are_full_text() {
        let q = parse("quarterly report");
        assert_eq!(q.text, vec!["quarterly", "report"]);
        assert_eq!(q.from, None);
    }

    #[test]
    fn phrases_stay_whole() {
        assert_eq!(
            parse("\"quarterly report\" draft").text,
            vec!["quarterly report", "draft"]
        );
    }

    #[test]
    fn unclosed_quote_does_not_lose_input() {
        // The user is mid-typing; dropping their text would be worse.
        assert_eq!(parse("\"half typed").text, vec!["half typed"]);
    }

    #[test]
    fn filters_parse() {
        let q = parse("from:Jane@Example.COM has:attachment is:unread in:feed");
        assert_eq!(q.from.unwrap().as_str(), "jane@example.com");
        assert_eq!(q.has_attachment, Some(true));
        assert_eq!(q.unread, Some(true));
        assert_eq!(q.destination, Some(Destination::Feed));
        assert!(q.text.is_empty());
    }

    #[test]
    fn dates_convert_to_unix_utc_midnight() {
        assert_eq!(parse("after:1970-01-01").after, Some(0));
        assert_eq!(parse("before:2024-03-01").before, Some(1_709_251_200));
        assert_eq!(parse("after:2000-02-29").after, Some(951_782_400));
    }

    #[test]
    fn unparseable_filters_fall_through_to_text() {
        // A colon in ordinary text must never be an error.
        for bad in [
            "before:yesterday",
            "after:2024-13-01",
            "in:nowhere",
            "ratio:3:1",
        ] {
            let q = parse(bad);
            assert_eq!(q.text, vec![bad], "{bad}");
            assert_eq!(q.before, None);
            assert_eq!(q.after, None);
        }
    }

    #[test]
    fn empty_query_is_empty() {
        assert!(parse("   ").is_empty());
        assert!(!parse("x").is_empty());
    }
}
