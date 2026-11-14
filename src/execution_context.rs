#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExecutionContext {
    Client,
    Server,
}

impl ExecutionContext {
    pub(crate) fn opposite(self) -> Self {
        match self {
            Self::Client => Self::Server,
            Self::Server => Self::Client,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
}
