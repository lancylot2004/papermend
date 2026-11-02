//! SSH SOCKS tunnels for when the Print Deploy server is not directly reachable.
//!
//! papermend never handles an SSH credential: the tunnel is the user's own `ssh`, which brings
//! their config, keys, and agent. `BatchMode=yes` makes ssh refuse interactive password prompts,
//! so only key or agent authentication can succeed.

use crate::error::{Error, Result};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};

/// How long ssh gets to bring the SOCKS port up before the attempt is abandoned.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// A running `ssh -D` SOCKS tunnel. The ssh process is killed when this is dropped.
#[derive(Debug)]
pub struct Tunnel {
    /// Kept only for its kill-on-drop behaviour.
    _child: Child,
    /// Local SOCKS5 port.
    pub port: u16,
    /// The ssh destination the tunnel runs through, e.g. `shell1` or `user@host`.
    pub via: String,
}

impl Tunnel {
    /// Starts a SOCKS tunnel through `destination`: a `~/.ssh/config` alias or `user@host`, as
    /// `ssh` itself accepts. Returns once the local port accepts connections.
    pub async fn open(destination: &str) -> Result<Self> {
        Self::start(destination, None).await
    }

    /// Starts a tunnel that carries raw TCP from the local port straight to `target`
    /// (`host:port`) instead of speaking SOCKS.
    pub async fn to(destination: &str, target: &str) -> Result<Self> {
        Self::start(destination, Some(target)).await
    }

    async fn start(destination: &str, target: Option<&str>) -> Result<Self> {
        let port = free_port().await?;
        let forward = match target {
            Some(target) => format!("-L127.0.0.1:{port}:{target}"),
            None => format!("-D127.0.0.1:{port}"),
        };
        let mut child = Command::new("ssh")
            .args([&forward, "-N"])
            .args(["-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes"])
            .arg(destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if child.try_wait()?.is_some() {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    pipe.read_to_string(&mut stderr).await.ok();
                }
                return Err(fail(destination, stderr.trim()));
            }
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                return Ok(Self { _child: child, port, via: destination.to_owned() });
            }
            if Instant::now() > deadline {
                return Err(fail(destination, "the SOCKS port never came up"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// The error for one failed tunnel attempt.
fn fail(destination: &str, reason: &str) -> Error {
    Error::Invalid(format!("ssh to {destination} failed: {reason}"))
}

/// A currently free local port. The port could be taken between this check and ssh binding it,
/// but a collision just fails that tunnel attempt, which the caller already handles.
async fn free_port() -> Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0)).await?.local_addr()?.port())
}

/// `list` starting at a position picked from the clock, wrapping around, so repeated runs
/// spread load over the proxies without a random-number dependency.
pub fn shuffled(list: &[String]) -> impl Iterator<Item = &String> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    let start = if list.is_empty() { 0 } else { nanos as usize % list.len() };
    list[start..].iter().chain(list[..start].iter())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffled_keeps_every_proxy() {
        let list: Vec<String> = ["a", "b", "c"].map(String::from).into();
        let mut rotated: Vec<&String> = shuffled(&list).collect();
        rotated.sort();
        assert_eq!(rotated, list.iter().collect::<Vec<_>>());
        assert_eq!(shuffled(&[]).count(), 0);
    }
}
