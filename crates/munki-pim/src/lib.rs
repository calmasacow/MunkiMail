//! Local-first PIM storage. Blocking DAV calls belong on a worker thread.
mod dav;
mod format;
mod store;
pub use dav::{DavClient, SyncReport};
pub use format::{
    normalize_email, parse_contact, parse_event, rsvp, rsvp_local, Contact, Event, EventTime,
};
pub use store::{Collection, Conflict, Kind, Person, Record, Resolution, Store};
