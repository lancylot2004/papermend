//! Thin wrapper over the CUPS command-line tools.
//!
//! CUPS itself is the only state store: queues papermend owns are recognised by [`PREFIX`]. Admin
//! users on macOS, and members of the `lpadmin` group on most Linux distributions, may run
//! `lpadmin` without `sudo`. Text output is parsed under the C locale, as the official client does.

use crate::api::redact;
use crate::error::{Error, Result};
use tokio::process::Command;

/// Name prefix marking queues papermend manages. The print dialog shows the description instead.
pub const PREFIX: &str = "papermend_";

/// `lpadmin` model arguments for a generic PostScript driver, used when no other model applies:
/// macOS's built-in PPD, or the sample generic PPD CUPS ships elsewhere.
#[cfg(target_os = "macos")]
const GENERIC_MODEL: [&str; 2] = [
    "-P",
    "/System/Library/Frameworks/ApplicationServices.framework/Versions/A/Frameworks/\
     PrintCore.framework/Versions/A/Resources/Generic.ppd",
];
#[cfg(not(target_os = "macos"))]
const GENERIC_MODEL: [&str; 2] = COLOUR_MODEL;

/// `lpadmin` model arguments for a colour PostScript driver: the sample generic PPD, which CUPS
/// ships on every platform and which declares a colour device. macOS's own `Generic.ppd` is
/// greyscale-only, so colour jobs would be printed, and charged, as mono.
const COLOUR_MODEL: [&str; 2] = ["-m", "drv:///sample.drv/generic.ppd"];

/// A queue as papermend wants it to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queue {
    /// CUPS queue name, including [`PREFIX`].
    pub name: String,
    /// Device URI, e.g. `smb://server/queue`.
    pub uri: String,
    /// Human-readable name shown in print dialogs.
    pub description: String,
    /// Whether the destination needs ready-rendered PostScript. Mobility Print hands documents
    /// to the server-side print queue untouched, and those queues expect PostScript: PDF jobs are
    /// accepted and then silently dropped.
    pub postscript: bool,
}

impl std::fmt::Display for Queue {
    /// `Display Name [device URI]`, with any per-user token hidden.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]", self.description, redact(&self.uri))
    }
}

/// A managed queue as it currently exists in CUPS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// CUPS queue name, including [`PREFIX`].
    pub name: String,
    /// Device URI.
    pub uri: String,
}

/// Converts a server-side queue name into a valid, prefixed CUPS queue name.
pub fn queue_name(name: &str) -> String {
    let sanitised: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("{PREFIX}{sanitised}")
}

/// Lists the queues papermend manages, with their device URIs.
pub async fn managed() -> Result<Vec<Installed>> {
    match run(Command::new("lpstat").arg("-v")).await {
        Ok(stdout) => Ok(parse_lpstat(&stdout)),
        // `lpstat -v` exits non-zero when no queues exist at all.
        Err(Error::CUPS { output, .. }) if output.contains("No destinations added") => Ok(vec![]),
        Err(e) => Err(e),
    }
}

/// Creates `queue`, or updates it in place if it already exists.
pub async fn add(queue: &Queue) -> Result<()> {
    run(&mut add_command(queue)).await.map(drop)
}

/// Deletes the queue called `name`.
pub async fn remove(name: &str) -> Result<()> {
    run(Command::new("lpadmin").args(["-x", name])).await.map(drop)
}

/// The `lpadmin` command that creates or updates `queue`. The model is the generic PostScript
/// driver for [`Queue::postscript`] queues and non-IPP URIs, and IPP Everywhere otherwise.
fn add_command(queue: &Queue) -> Command {
    let mut command = Command::new("lpadmin");
    command.args(["-p", &queue.name, "-E", "-v", &queue.uri, "-D", &queue.description]);
    command.args(["-o", "printer-is-shared=false"]);
    let ipp = queue.uri.starts_with("ipp://") || queue.uri.starts_with("ipps://");
    // ponytail: colour is guessed from the queue name, e.g. `Canon-Colour`. Read the server's
    // driver or the printer's IPP attributes if names stop saying it.
    let name = queue.name.to_ascii_lowercase();
    if ipp && !queue.postscript {
        command.args(["-m", "everywhere"]);
    } else if name.contains("colour") || name.contains("color") {
        command.args(COLOUR_MODEL);
    } else {
        command.args(GENERIC_MODEL);
    }
    command
}

/// Parses `lpstat -v` output, `device for NAME: URI` per line, keeping only managed queues.
fn parse_lpstat(stdout: &str) -> Vec<Installed> {
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix("device for "))
        .filter_map(|rest| rest.split_once(": "))
        .filter(|(name, _)| name.starts_with(PREFIX))
        .map(|(name, uri)| Installed { name: name.into(), uri: uri.trim().into() })
        .collect()
}

/// Runs a CUPS tool under the C locale and returns its stdout, or fails with its stderr. The
/// error is redacted, since a device URI can carry a per-user token.
async fn run(command: &mut Command) -> Result<String> {
    let output = command.env("LC_ALL", "C").env("LANG", "C").output().await?;
    if !output.status.success() {
        let std = command.as_std();
        let words: Vec<_> = std::iter::once(std.get_program())
            .chain(std.get_args())
            .map(|w| w.to_string_lossy())
            .collect();
        let stderr = redact(String::from_utf8_lossy(&output.stderr).trim());
        return Err(Error::CUPS { command: redact(&words.join(" ")), output: stderr });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("Find-Me Printing", "Find-Me_Printing")]
    #[case("Library/Colour", "Library_Colour")]
    #[case("Café", "Caf_")]
    fn names_are_sanitised(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(queue_name(input), format!("{PREFIX}{expected}"));
    }

    #[test]
    fn lpstat_keeps_managed_queues() {
        let stdout = format!(
            "device for {PREFIX}A: smb://ps/A\ndevice for Office: ipp://10.0.0.2/ipp/print\n\
             garbage line\n"
        );
        assert_eq!(
            parse_lpstat(&stdout),
            vec![Installed { name: format!("{PREFIX}A"), uri: "smb://ps/A".into() }]
        );
    }

    #[rstest]
    #[case("q", "ipps://ps/printers/A", false, &["-m", "everywhere"])]
    #[case("q", "ipps://m/printers/A/users/u/0123", true, &GENERIC_MODEL)]
    #[case("Canon-Colour", "ipps://m/printers/A/users/u/0123", true, &COLOUR_MODEL)]
    #[case("q", "smb://ps/A", false, &GENERIC_MODEL)]
    fn model_is_chosen(
        #[case] name: &str,
        #[case] uri: &str,
        #[case] postscript: bool,
        #[case] tail: &[&str],
    ) {
        let queue =
            Queue { name: name.into(), uri: uri.into(), description: "A".into(), postscript };
        let command = add_command(&queue);
        let args: Vec<_> = command.as_std().get_args().collect();
        assert_eq!(args[..5], ["-p", name, "-E", "-v", uri]);
        assert!(
            args.ends_with(tail.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>().as_slice())
        );
    }
}
