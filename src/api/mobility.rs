//! PaperCut Mobility Print: per-user printer addresses.
//!
//! A Mobility Print server only routes a job to the right PaperCut user when it arrives at that
//! user's own printer address, `…/printers/<name>/users/<user>/<token>`. Jobs sent to the shared
//! `…/printers/<name>` address are accepted and then dropped. The official client looks the
//! address up with `GET /printer-url`, authenticated with the user's PaperCut credentials.

use super::Client;
use crate::error::{Error, Result};

impl Client {
    /// Looks up `username`'s per-user IPP address for `printer` on the Mobility Print server at
    /// this client's base URL, e.g. `https://host:9164`. Returns an `ipps://` URI for CUPS.
    ///
    /// The address works like a credential, so `--verbose` shows it redacted.
    pub async fn mobility_printer_url(
        &self,
        printer: &str,
        username: &str,
        password: &str,
    ) -> Result<String> {
        let mut url = self.url("/printer-url")?;
        let host = self.base.host_str().unwrap_or_default().to_owned();
        url.query_pairs_mut()
            .append_pair("printerName", printer)
            .append_pair("serverName", &host)
            .append_pair("scheme", "ipps");
        let (_, body) =
            self.send_raw(self.http.get(url).basic_auth(username, Some(password))).await?;
        ipps_uri(body.trim())
    }
}

/// Converts the `https://` address the server returns into the `ipps://` URI CUPS needs. Done on
/// the string because the `url` crate refuses to switch between special and non-special schemes.
fn ipps_uri(address: &str) -> Result<String> {
    match address.split_once("://") {
        Some(("https" | "ipps", rest)) if !rest.is_empty() => Ok(format!("ipps://{rest}")),
        _ => Err(Error::Invalid(format!(
            "Unexpected Mobility Print printer address {:?}.",
            super::redact(address)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("https://m:9164/printers/A/users/u/0a1b", Some("ipps://m:9164/printers/A/users/u/0a1b"))]
    #[case("ipps://m:9164/printers/A/users/u/0a1b", Some("ipps://m:9164/printers/A/users/u/0a1b"))]
    #[case("http://m:9163/printers/A", None)]
    #[case("{\"message\":\"Authentication required\"}", None)]
    #[case("https://", None)]
    fn converts_server_address(#[case] address: &str, #[case] expected: Option<&str>) {
        assert_eq!(ipps_uri(address).ok().as_deref(), expected);
    }
}
