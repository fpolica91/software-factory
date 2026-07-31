use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use ts_rs::TS;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            Serialize,
            Deserialize,
            JsonSchema,
            TS,
        )]
        #[serde(transparent)]
        #[ts(type = "string")]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
    };
}

string_id!(JobId);
string_id!(OperationId);
string_id!(AttemptId);
string_id!(WorkflowRunId);
string_id!(TaskRunExternalId);
string_id!(FactoryRequestId);
string_id!(ThreadId);
string_id!(TurnId);
string_id!(ItemId);

/// JSON-RPC request identifier used by the Codex app-server wire protocol.
///
/// App-server accepts either a string or a signed integer and requires the
/// response to echo the same representation. Keeping this separate from
/// [`FactoryRequestId`] prevents a numeric upstream request id from being
/// coerced into a string at the Factory boundary.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(untagged)]
pub enum FactoryRpcRequestId {
    String(String),
    Integer(#[ts(type = "number")] i64),
}

impl fmt::Display for FactoryRpcRequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(value) => formatter.write_str(value),
            Self::Integer(value) => value.fmt(formatter),
        }
    }
}

impl From<String> for FactoryRpcRequestId {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for FactoryRpcRequestId {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<i64> for FactoryRpcRequestId {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}
