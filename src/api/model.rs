//! Wire types for the Print Deploy API. Field names come from JSON struct tags in the official
//! client (`print-deploy-client` 2025-07-01).

use serde::{Deserialize, Serialize};
use url::Url;

/// Body of `POST /deploy/printers`, from which the server picks queues.
///
/// Layout recovered from the official client's Go type metadata:
/// `struct { Version string "json:\"version\""; *usercontextservice.ClientInfo }`, with the
/// embedded struct's `loggedInUser` and `machine` fields flattened to the top level. The *values*
/// of `os` and `loggedInUser.type` are not recoverable statically and are best guesses.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// Client version string.
    pub version: String,
    /// The user the queues are for.
    pub logged_in_user: LoggedInUser,
    /// The machine asking.
    pub machine: Machine,
}

/// `usercontextservice.LoggedInUser`.
#[derive(Debug, Clone, Serialize)]
pub struct LoggedInUser {
    pub username: String,
    /// Account kind (a Go string type). Its values are unknown, so papermend leaves it empty.
    #[serde(rename = "type")]
    pub kind: String,
}

/// `usercontextservice.Machine`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    /// Not used by official client; always left empty and kept for structural reasons.
    pub hostname: String,
    pub os: Os,
    /// Non-loopback local addresses. Servers commonly map IP ranges to queues.
    pub ip_addresses: Vec<String>,
    /// Not used by official client; always left empty and kept for structural reasons.
    pub active_directory_domain_name: String,
}

/// `usercontextservice.OS`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Os {
    /// OS family, matching Go's `runtime.GOOS`.
    pub name: String,
    /// Product name, e.g. `macOS`.
    pub os_name: String,
    /// Product version, e.g. `15.1`.
    pub version: String,
    /// Architecture, matching Go's `runtime.GOARCH`.
    pub arch: String,
}

/// Body of `POST /deploy/login`. Matches the official client's literal
/// `{"username":%q, "password":%q}`.
#[derive(Debug, Clone, Serialize)]
pub struct LoginRequest<'a> {
    pub username: &'a str,
    pub password: &'a str,
}

/// Response of `POST /deploy/login`. Observed from a live server as
/// `{"Token": "<JWT>", "NormalizedUsername": "...", "Message": ""}`; the other spellings are kept
/// in case other server versions differ.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginResponse {
    #[serde(
        rename = "Token",
        alias = "token",
        alias = "sessionToken",
        alias = "access_token",
        alias = "accessToken"
    )]
    pub token: String,
}

/// Response of `POST /deploy/printers`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PrintersResponse {
    pub printers: Vec<PrinterDef>,
}

/// One queue the server wants on this machine.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PrinterDef {
    /// Display name, e.g. `Find-Me-Printing`.
    pub name: String,
    /// Optional longer description.
    pub description: String,
    /// Self-service queues the user opts into, rather than ones always deployed.
    pub optional: bool,
    /// How to reach the queue.
    pub connection: Connection,
}

/// Network location of a queue.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Connection {
    /// One of `raw`, `lpr`, `pcraw`, `local`, `samba`, `ipp` (from `pc-printer-updater -r`).
    pub protocol: String,
    /// Print server or device host.
    pub host: String,
    /// Port, if non-default.
    pub port: Option<u16>,
    /// Queue name on the host.
    pub queue: String,
    /// Full connection string. For IPP this is the printer URL; for Mobility Print it is the
    /// Mobility Print server, e.g. `https://icl-print-mobile.ic.ac.uk:9164`.
    pub name: Option<String>,
    /// Queue kind, e.g. `mobilityprint` (observed). Interpret with [`PrinterDef::kind`].
    #[serde(rename = "type")]
    pub kind: String,
}

/// Queue kinds supported by papermend. Match by substring since we do not have original spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    /// Shared queue on a print server, which tracks jobs itself.
    Server,
    /// Straight to a device, relying on PaperCut's local job tracking.
    Direct,
    /// PaperCut Mobility Print queue. Jobs go to the Mobility Print server, which hands them to
    /// PaperCut, tracked server-side.
    Mobility,
    /// Anything else.
    Unknown,
}

impl PrinterDef {
    /// A Mobility Print queue called `name` on `server`, as `sync --printer` describes it: what
    /// `/deploy/printers` would return, with only the fields papermend reads.
    pub fn mobility(name: &str, server: &Url) -> Self {
        Self {
            name: name.into(),
            connection: Connection {
                kind: "mobilityprint".into(),
                name: Some(server.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Interprets [`Connection::kind`]. Matched by substring because only `mobilityprint` has been
    /// seen live.
    pub fn kind(&self) -> QueueKind {
        let kind = self.connection.kind.to_ascii_lowercase();
        if kind.contains("mobility") {
            QueueKind::Mobility
        } else if kind.contains("direct") {
            QueueKind::Direct
        } else if kind.contains("server") {
            QueueKind::Server
        } else {
            QueueKind::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A live `/deploy/printers` entry, trimmed, then the other kinds, a bare entry, and unknown
    /// fields.
    #[test]
    fn parses_printers() {
        let body = json!({"unknown": true, "printers": [
            {"name": "Canon-Colour", "zoneName": "Canon", "connection":
                {"name": "https://icl-print-mobile.ic.ac.uk:9164", "type": "mobilityprint"}},
            {"name": "S", "connection": {"type": "SERVER"}},
            {"name": "D", "connection": {"type": "DIRECT_PRINT"}},
            {"name": "Bare"}
        ]});
        let printers = serde_json::from_value::<PrintersResponse>(body).unwrap().printers;
        let kinds: Vec<_> = printers.iter().map(PrinterDef::kind).collect();
        use QueueKind::*;
        assert_eq!(kinds, [Mobility, Server, Direct, Unknown]);
        assert_eq!(
            printers[0].connection.name.as_deref(),
            Some("https://icl-print-mobile.ic.ac.uk:9164")
        );
    }

    #[test]
    fn client_info_matches_official_layout() {
        let info = ClientInfo {
            version: "v".into(),
            logged_in_user: LoggedInUser { username: "u".into(), kind: String::new() },
            machine: Machine {
                hostname: String::new(),
                os: Os {
                    name: "n".into(),
                    os_name: "o".into(),
                    version: "1".into(),
                    arch: "a".into(),
                },
                ip_addresses: vec!["10.0.0.1".into()],
                active_directory_domain_name: String::new(),
            },
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            json!({
                "version": "v",
                "loggedInUser": {"username": "u", "type": ""},
                "machine": {
                    "hostname": "",
                    "os": {"name": "n", "osName": "o", "version": "1", "arch": "a"},
                    "ipAddresses": ["10.0.0.1"],
                    "activeDirectoryDomainName": ""
                }
            })
        );
    }

    #[test]
    fn login_token_aliases() {
        for key in ["Token", "token", "sessionToken", "access_token"] {
            let response: LoginResponse = serde_json::from_value(json!({ key: "a" })).unwrap();
            assert_eq!(response.token, "a", "{key}");
        }
    }
}
