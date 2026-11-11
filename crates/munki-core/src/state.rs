//! Versioned, per-entry last-write-wins settings stored in the mail account.
use crate::{Address, SenderVerdict};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_STATE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageSettings {
    pub subject: Option<String>,
    pub thread_id: Option<String>,
    pub reply_later: bool,
    pub set_aside: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StateValue {
    Sender(SenderVerdict),
    Message(MessageSettings),
}
impl StateValue {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Sender(_) => "sender",
            Self::Message(_) => "message",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateEntry {
    pub key: String,
    pub revision: i64,
    pub device: String,
    pub value: StateValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSnapshot {
    pub format: u32,
    pub entries: Vec<StateEntry>,
}
impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            format: 1,
            entries: Vec::new(),
        }
    }
}
impl StateSnapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.format != 1 {
            return Err("unsupported triage state format");
        }
        for entry in &self.entries {
            if entry.revision < 0
                || entry.revision == i64::MAX
                || entry.device.len() != 32
                || !entry.device.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("invalid triage state revision or device");
            }
            if entry.key.is_empty()
                || entry.key.len() > 1024
                || entry.key.chars().any(char::is_control)
            {
                return Err("invalid state key");
            }
            match &entry.value {
                StateValue::Sender(verdict) => {
                    if Address::new(&entry.key).as_str() != entry.key
                        || entry.key.matches('@').count() != 1
                        || entry.key.starts_with('@')
                        || entry.key.ends_with('@')
                    {
                        return Err("invalid sender key");
                    }
                    if *verdict == SenderVerdict::Allow(crate::Destination::Screener) {
                        return Err("approval must leave Buffers");
                    }
                }
                StateValue::Message(settings) => {
                    if settings
                        .subject
                        .as_ref()
                        .is_some_and(|s| s.len() > 4096 || s.chars().any(char::is_control))
                        || settings.thread_id.as_ref().is_some_and(|s| {
                            s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control)
                        })
                    {
                        return Err("invalid message settings");
                    }
                }
            }
        }
        Ok(())
    }

    /// Independent edits survive; equal revisions use device ID, then value.
    /// ponytail: clock/device LWW resolves conflicts; expose conflicting edits
    /// if real users need collaborative, simultaneous editing of one setting.
    pub fn merge(&mut self, other: Self) {
        let mut entries = BTreeMap::<(String, String), StateEntry>::new();
        for entry in self.entries.drain(..).chain(other.entries) {
            let key = (entry.value.kind().to_owned(), entry.key.clone());
            if entries.get(&key).is_none_or(|old| {
                (
                    entry.revision,
                    &entry.device,
                    serde_json::to_string(&entry.value).unwrap(),
                ) > (
                    old.revision,
                    &old.device,
                    serde_json::to_string(&old.value).unwrap(),
                )
            }) {
                entries.insert(key, entry);
            }
        }
        self.entries = entries.into_values().collect();
    }
}
