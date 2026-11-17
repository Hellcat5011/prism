//! KDE Connect network packets: one JSON object per line,
//! `{"id": <ms>, "type": "kdeconnect.<x>", "body": {...}}`, with `payloadSize` and
//! `payloadTransferInfo` when a payload follows on its own socket.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const PROTOCOL_VERSION: i64 = 8;
/// Longest packet line accepted (identity packets with long capability lists stay far below).
pub const MAX_LINE: usize = 1 << 20;

pub const IDENTITY: &str = "kdeconnect.identity";
pub const PAIR: &str = "kdeconnect.pair";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Packet {
    /// Milliseconds; older peers sent it as a string, so it stays a Value.
    #[serde(default)]
    pub id: Value,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub body: Value,
    #[serde(rename = "payloadSize", default, skip_serializing_if = "Option::is_none")]
    pub payload_size: Option<i64>,
    #[serde(rename = "payloadTransferInfo", default, skip_serializing_if = "Option::is_none")]
    pub payload_transfer_info: Option<Value>,
}

impl Packet {
    pub fn new(kind: &str, body: Value) -> Packet {
        Packet { id: json!(crate::identity::unix_ms()), kind: kind.to_string(), body, payload_size: None, payload_transfer_info: None }
    }

    pub fn parse(line: &str) -> Option<Packet> {
        let p: Packet = serde_json::from_str(line.trim()).ok()?;
        (!p.kind.is_empty()).then_some(p)
    }

    /// The packet as one line, newline included.
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.body.get(key).and_then(Value::as_str)
    }

    pub fn bool(&self, key: &str) -> bool {
        self.body.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        let v = self.body.get(key)?;
        v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    }

    pub fn float(&self, key: &str) -> Option<f64> {
        let v = self.body.get(key)?;
        v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    }

    pub fn payload_port(&self) -> Option<u16> {
        self.payload_transfer_info.as_ref()?.get("port")?.as_u64().and_then(|p| u16::try_from(p).ok())
    }
}

/// What a peer told us about itself.
#[derive(Debug, Clone, Default)]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
    pub device_type: String,
    pub protocol: i64,
    pub tcp_port: Option<u16>,
    pub incoming: Vec<String>,
}

impl PeerInfo {
    pub fn from_identity(p: &Packet) -> Option<PeerInfo> {
        if p.kind != IDENTITY {
            return None;
        }
        let list = |k: &str| -> Vec<String> {
            p.body.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
        };
        Some(PeerInfo {
            id: p.str("deviceId")?.to_string(),
            name: p.str("deviceName").unwrap_or("Unknown device").chars().take(64).collect(),
            device_type: p.str("deviceType").unwrap_or("phone").to_string(),
            protocol: p.int("protocolVersion").unwrap_or(0),
            tcp_port: p.int("tcpPort").and_then(|v| u16::try_from(v).ok()),
            incoming: list("incomingCapabilities"),
        })
    }
}

/// Our identity packet. `tcp_port` is only sent where the peer may connect back (UDP and the
/// plain-text step).
pub fn identity_packet(id: &str, name: &str, tcp_port: Option<u16>, incoming: &[&str], outgoing: &[&str]) -> Packet {
    let mut body = Map::new();
    body.insert("deviceId".into(), json!(id));
    body.insert("deviceName".into(), json!(name));
    body.insert("deviceType".into(), json!("desktop"));
    body.insert("protocolVersion".into(), json!(PROTOCOL_VERSION));
    body.insert("incomingCapabilities".into(), json!(incoming));
    body.insert("outgoingCapabilities".into(), json!(outgoing));
    if let Some(port) = tcp_port {
        body.insert("tcpPort".into(), json!(port));
    }
    Packet::new(IDENTITY, Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_identity_and_round_trips() {
        let line = r#"{"id":1700000000000,"type":"kdeconnect.identity","body":{"deviceId":"abc_def","deviceName":"Pixel","deviceType":"phone","protocolVersion":8,"tcpPort":1716,"incomingCapabilities":["kdeconnect.ping"],"outgoingCapabilities":["kdeconnect.battery"]}}"#;
        let p = Packet::parse(line).unwrap();
        let info = PeerInfo::from_identity(&p).unwrap();
        assert_eq!((info.id.as_str(), info.protocol, info.tcp_port), ("abc_def", 8, Some(1716)));
        assert_eq!(info.incoming, ["kdeconnect.ping"]);
        let again = Packet::parse(&p.to_line()).unwrap();
        assert_eq!(again, p);
        assert!(p.to_line().ends_with('\n'));
        assert!(!p.to_line().contains("payloadSize"));
    }

    #[test]
    fn string_ids_and_payload_info() {
        let p = Packet::parse(r#"{"id":"17","type":"kdeconnect.share.request","body":{"filename":"a.txt"},"payloadSize":5,"payloadTransferInfo":{"port":1739}}"#).unwrap();
        assert_eq!(p.payload_port(), Some(1739));
        assert_eq!(p.payload_size, Some(5));
        assert!(Packet::parse("not json").is_none());
    }
}
