use bytes::Bytes;
use serde_json::Value;

pub(crate) fn encode(event: &Value) -> Bytes {
    let payload = serde_json::to_string(event).expect("Responses event is serializable");
    Bytes::from(format!("data: {payload}\n\n"))
}

pub(crate) fn done() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}
