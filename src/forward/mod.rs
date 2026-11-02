//! The optional launchd agent that keeps print jobs working away from the server's network.
//!
//! `sync --forward` points every Mobility Print queue at `127.0.0.1:`[`PORT`] and installs a
//! LaunchAgent holding that socket. launchd starts `papermend forward` only when CUPS connects —
//! there is no resident process — and each run dials the real server, through an SSH proxy when
//! the direct dial fails, then exits with the connection. The plist is the one piece of state
//! papermend keeps; `remove`, or `sync` without `--forward`, deletes it.

use crate::error::{Error, Result};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Duration;
use url::Url;

/// Local port the agent listens on: the Mobility Print port, for recognisability.
pub const PORT: u16 = 9164;

/// launchd label and plist file stem.
const LABEL: &str = "papermend";

/// What the installed agent forwards, read back from the plist.
#[derive(Debug, PartialEq, Eq)]
pub struct Agent {
    /// Mobility Print `host:port` the agent dials.
    pub target: String,
    /// Comma-separated SSH proxies it falls back to.
    pub proxies: String,
}

/// Rewrites every per-user address to the agent's local port and returns the one `host:port`
/// they pointed at.
pub fn localise(addresses: &mut HashMap<String, String>) -> Result<String> {
    let mut targets = BTreeSet::new();
    for address in addresses.values_mut() {
        let mut url = Url::parse(address)?;
        let invalid =
            || Error::Invalid(format!("cannot forward address {}", crate::api::redact(address)));
        let host = url.host_str().ok_or_else(invalid)?.to_owned();
        targets.insert(format!("{host}:{}", url.port().unwrap_or(631)));
        url.set_host(Some("127.0.0.1")).map_err(|_| invalid())?;
        url.set_port(Some(PORT)).map_err(|()| invalid())?;
        *address = url.into();
    }
    let mut targets = targets.into_iter();
    match (targets.next(), targets.next()) {
        (Some(target), None) => Ok(target),
        (None, _) => Err(Error::Invalid("--forward found no Mobility Print queues.".into())),
        // ponytail: one agent, one socket. A second Mobility server needs a port per server.
        _ => Err(Error::Invalid("--forward supports a single Mobility Print server.".into())),
    }
}

/// Writes the plist and (re)loads it into launchd.
pub async fn install(target: &str, proxies: &[String], probe: Duration) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(Error::Invalid("--forward needs launchd and is macOS-only for now.".into()));
    }
    let exe = std::env::current_exe()?;
    let path = plist_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let rendered = plist(&exe.to_string_lossy(), target, &proxies.join(","), probe.as_secs());
    std::fs::write(&path, rendered)?;
    let domain = format!("gui/{}", uid().await?);
    // launchd refuses a second bootstrap, so boot out any previous agent first.
    launchctl(&["bootout", &format!("{domain}/{LABEL}")]).await.ok();
    launchctl(&["bootstrap", &domain, &path.to_string_lossy()]).await
}

/// Unloads and deletes the agent. `Ok(true)` when one was installed.
pub async fn uninstall() -> Result<bool> {
    let path = plist_path()?;
    if !path.exists() {
        return Ok(false);
    }
    launchctl(&["bootout", &format!("gui/{}/{LABEL}", uid().await?)]).await.ok();
    std::fs::remove_file(path)?;
    Ok(true)
}

/// The installed agent, if any, read back from the plist.
pub fn installed() -> Result<Option<Agent>> {
    match std::fs::read_to_string(plist_path()?) {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Where the LaunchAgent plist lives.
fn plist_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| Error::Invalid("HOME is not set.".into()))?;
    Ok(PathBuf::from(home).join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

/// Renders the LaunchAgent plist: an inetd-style socket on [`PORT`] that runs `exe forward` per
/// connection, with the connection as fd 0. Values are paths, hostnames, and numbers, so no XML
/// escaping is needed.
fn plist(exe: &str, target: &str, proxies: &str, probe: u64) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>forward</string>
        <string>--target</string><string>{target}</string>
        <string>--proxy</string><string>{proxies}</string>
        <string>--probe-timeout</string><string>{probe}</string>
    </array>
    <key>inetdCompatibility</key><dict><key>Wait</key><false/></dict>
    <key>Sockets</key>
    <dict>
        <key>Listener</key>
        <dict>
            <key>SockNodeName</key><string>127.0.0.1</string>
            <key>SockServiceName</key><string>{PORT}</string>
        </dict>
    </dict>
</dict>
</plist>
"#
    )
}

/// Reads the agent back out of a rendered plist.
fn parse(text: &str) -> Option<Agent> {
    let strings: Vec<&str> =
        text.split("<string>").skip(1).filter_map(|rest| rest.split("</string>").next()).collect();
    let after = |flag: &str| {
        strings
            .iter()
            .position(|s| *s == flag)
            .and_then(|i| strings.get(i + 1))
            .map(|s| (*s).to_owned())
    };
    Some(Agent { target: after("--target")?, proxies: after("--proxy")? })
}

/// This user's numeric ID, for the launchd GUI domain.
async fn uid() -> Result<String> {
    let output = tokio::process::Command::new("id").arg("-u").output().await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Runs `launchctl`, failing with its stderr.
async fn launchctl(args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new("launchctl").args(args).output().await?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "launchctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_roundtrips_and_binds_the_local_port() {
        let text = plist("/opt/bin/papermend", "m.example:9164", "shell1,shell5", 2);
        assert!(text.contains(&format!("<string>{PORT}</string>")), "{text}");
        assert_eq!(
            parse(&text),
            Some(Agent { target: "m.example:9164".into(), proxies: "shell1,shell5".into() })
        );
    }

    #[test]
    fn localise_rewrites_addresses_and_requires_one_server() {
        let mut addresses = HashMap::from([(
            "Q".to_owned(),
            "ipps://m.example:9164/printers/Q/users/u/0a1b".to_owned(),
        )]);
        assert_eq!(localise(&mut addresses).unwrap(), "m.example:9164");
        assert_eq!(addresses["Q"], format!("ipps://127.0.0.1:{PORT}/printers/Q/users/u/0a1b"));

        assert!(localise(&mut HashMap::new()).is_err());
        let mut two = HashMap::from([
            ("A".to_owned(), "ipps://a.example:9164/p".to_owned()),
            ("B".to_owned(), "ipps://b.example:9164/p".to_owned()),
        ]);
        assert!(localise(&mut two).is_err());
    }
}
