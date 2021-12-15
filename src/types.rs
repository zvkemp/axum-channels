use std::str::FromStr;

use serde::Serialize;

#[derive(Hash, Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct Token(usize);

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Token({})", self.0)
    }
}

impl From<usize> for Token {
    fn from(id: usize) -> Self {
        Token(id)
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct ChannelId {
    raw: String,
    delimiter_idx: Option<usize>,
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

impl<'a> FromStr for ChannelId {
    type Err = crate::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.char_indices().find(|(_, char)| *char == ':') {
            Some((index, _)) => Ok(ChannelId {
                raw: s.to_string(),
                delimiter_idx: Some(index),
            }),

            None => Ok(ChannelId {
                raw: s.to_string(),
                delimiter_idx: None,
            }),
        }
    }
}
