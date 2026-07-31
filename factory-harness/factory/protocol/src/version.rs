use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

pub const FACTORY_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };
pub const SOURCE_CODEX_REVISION: &str = "406dc9239492aff6d295cca5eebe2a548548d42f";
pub const FACTORY_PROTOCOL_SCHEMA_SHA256: &str =
    "c2ac03004607754df606e8bf2b1bfd7c5a6646bbec815cb2f7605dd7c8180e77";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolRange {
    pub minimum: ProtocolVersion,
    pub maximum: ProtocolVersion,
}

impl ProtocolRange {
    pub fn highest_common(self, other: Self) -> Option<ProtocolVersion> {
        if self.minimum.major != self.maximum.major
            || other.minimum.major != other.maximum.major
            || self.minimum.major != other.minimum.major
        {
            return None;
        }

        let minimum_minor = self.minimum.minor.max(other.minimum.minor);
        let maximum_minor = self.maximum.minor.min(other.maximum.minor);
        (minimum_minor <= maximum_minor).then_some(ProtocolVersion {
            major: self.minimum.major,
            minor: maximum_minor,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolManifest {
    pub version: ProtocolVersion,
    pub source_codex_revision: String,
    pub schema_sha256: String,
}

impl ProtocolManifest {
    pub fn current() -> Self {
        Self {
            version: FACTORY_PROTOCOL_VERSION,
            source_codex_revision: SOURCE_CODEX_REVISION.to_string(),
            schema_sha256: FACTORY_PROTOCOL_SCHEMA_SHA256.to_string(),
        }
    }
}
