//! Protocol adapters. Bytes in from the network, store rows out.

pub mod imap;
pub mod parse;
pub mod render;

pub mod compose;
pub mod smtp;

pub mod state;
