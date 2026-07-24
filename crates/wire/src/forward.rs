//! Port-forwarding wire types shared by the control-server and the native viewer.
//! Control plane rides port-1 ([`ForwardsMsg`] server→viewer tag 5,
//! [`ForwardStatusMsg`] viewer→server tag 2); the data plane's per-connection header
//! is [`ForwardHeader`]. [`ForwardRuntime`] is the volatile `forwards` SSE payload entry.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One rule pushed to the viewer: the persisted [`crate::PortForward`] subset it needs
/// to run a listener, tagged with its owning clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardRule {
    pub host_id: String,
    pub id: String,
    pub remote_port: u16,
    pub local_port: u16,
}

/// Server→viewer (port-1 tag 5): the full desired rule set (union across all clones)
/// plus the control-server's data port, sent on connect and on every config change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardsMsg {
    pub forward_port: u16,
    pub rules: Vec<ForwardRule>,
}

/// Live state of a forward rule. `Offline` is server-derived (no viewer connected);
/// the viewer only ever reports `Listening`/`Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub enum ForwardState {
    Listening,
    Error,
    Offline,
}

/// Viewer→server (port-1 tag 2): a rule's listener bound, failed to bind, or died.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardStatusMsg {
    pub host_id: String,
    pub id: String,
    pub state: ForwardState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One rule's runtime status inside the `forwards` SSE event (payload:
/// `Record<hostId, ForwardRuntime[]>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../frontend/app/lib/wire/")]
pub struct ForwardRuntime {
    pub id: String,
    pub state: ForwardState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub active_conns: u32,
}

/// Data-plane header: the first length-prefixed JSON on a `:forward` connection,
/// followed by the raw byte stream. `id` is the rule id (so the server attributes
/// active-conn counts); `token` is reserved for auth (unenforced in v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardHeader {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub host_id: String,
    pub id: String,
    pub remote_port: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_msg_round_trips() {
        let m = ForwardsMsg {
            forward_port: 9005,
            rules: vec![ForwardRule {
                host_id: "pega-abc".into(),
                id: "f8080".into(),
                remote_port: 3000,
                local_port: 8080,
            }],
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"forwardPort\":9005"), "got {json}");
        assert!(json.contains("\"hostId\":\"pega-abc\""), "got {json}");
        assert_eq!(serde_json::from_str::<ForwardsMsg>(&json).unwrap(), m);
    }

    #[test]
    fn status_msg_state_is_lowercase() {
        let m = ForwardStatusMsg {
            host_id: "h".into(),
            id: "f8080".into(),
            state: ForwardState::Error,
            error: Some("addr in use".into()),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"state\":\"error\""), "got {json}");
        assert_eq!(serde_json::from_str::<ForwardStatusMsg>(&json).unwrap(), m);
    }

    #[test]
    fn header_round_trips_without_token() {
        let h = ForwardHeader { token: None, host_id: "h".into(), id: "f22".into(), remote_port: 22 };
        let json = serde_json::to_string(&h).unwrap();
        assert!(!json.contains("token"), "None token must be omitted: {json}");
        assert_eq!(serde_json::from_str::<ForwardHeader>(&json).unwrap(), h);
    }
}
