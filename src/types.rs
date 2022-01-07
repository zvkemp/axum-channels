use std::{str::FromStr, sync::Arc};

use serde::Serialize;

#[derive(Hash, Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct Token(usize);

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{}>", self.0)
    }
}

impl From<usize> for Token {
    fn from(id: usize) -> Self {
        Token(id)
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct ChannelId {
    raw: Arc<str>,
    delimiter_idx: Option<usize>,
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Channel<{}>", self.raw)
    }
}

impl ChannelId {
    pub fn key(&self) -> Option<&str> {
        self.delimiter_idx.map(|idx| &self.raw[..idx])
    }

    pub fn value(&self) -> Option<&str> {
        self.delimiter_idx.map(|idx| &self.raw[(idx + 1)..])
    }

    pub fn id(&self) -> &str {
        &self.raw
    }
}

impl<'a> From<&'a str> for ChannelId {
    fn from(s: &'a str) -> Self {
        match s.char_indices().find(|(_, char)| *char == ':') {
            Some((index, _)) => ChannelId {
                raw: s.into(),
                delimiter_idx: Some(index),
            },

            None => ChannelId {
                raw: s.into(),
                delimiter_idx: None,
            },
        }
    }
}
