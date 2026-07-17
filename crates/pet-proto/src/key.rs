use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::event::Source;

/// Canonical identity of a tracked session: the harness that owns it plus the
/// harness-native session id. Serialized everywhere (JSON, D-Bus args, CLI)
/// in its canonical string form `source/session`, so it can also be a map key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionKey {
    pub source: Source,
    pub session: String,
}

impl Serialize for SessionKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SessionKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl SessionKey {
    pub fn new(source: Source, session: impl Into<String>) -> Self {
        Self {
            source,
            session: session.into(),
        }
    }
}

impl std::fmt::Display for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.source, self.session)
    }
}

impl std::str::FromStr for SessionKey {
    type Err = crate::event::ValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (source, session) = s
            .split_once('/')
            .ok_or(crate::event::ValidationError::BadSessionKey)?;
        if session.is_empty() {
            return Err(crate::event::ValidationError::BadSessionKey);
        }
        Ok(Self {
            source: source.parse()?,
            session: session.to_string(),
        })
    }
}
