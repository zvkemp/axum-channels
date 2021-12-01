#[derive(Hash, Debug, Clone, Copy, Eq, PartialEq)]
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

pub type ChannelId = String;
