//! Serde helpers.

pub mod base64_bytes {
    use std::sync::Arc;

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(b: &Arc<[u8]>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(b))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<[u8]>, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        STANDARD
            .decode(s.as_bytes())
            .map(Into::into)
            .map_err(D::Error::custom)
    }
}
