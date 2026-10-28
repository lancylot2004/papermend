//! Async client for the private Print Deploy HTTP API.
//!
//! Endpoints were recovered from the official client: `/deploy/ping`, `/deploy/login`, and
//! `/deploy/printers`. OAuth (`/deploy/oauth/*`) and driver downloads are not
//! implemented.

pub mod mobility;
pub mod model;

use crate::error::{Error, Result};
use model::{ClientInfo, LoginRequest, LoginResponse, PrinterDef, PrintersResponse};
use reqwest::{RequestBuilder, header};
use serde::de::DeserializeOwned;
use std::time::Duration;
use url::Url;

/// An HTTP server, with the Print Deploy session token once logged in.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: Option<String>,
    verbose: bool,
}

impl Client {
    /// Creates a client for `base`, e.g. `https://host:9174`, with a per-request `timeout`. With
    /// `verbose`, every response is printed to stderr with secrets redacted. TLS certificates are
    /// always verified.
    pub fn new(base: Url, timeout: Duration, verbose: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("papermend/", env!("CARGO_PKG_VERSION")))
            .timeout(timeout)
            .build()?;
        Ok(Self { http, base, token: None, verbose })
    }

    /// Checks the server is reachable.
    pub async fn ping(&self) -> Result<()> {
        self.send_raw(self.http.get(self.url("/deploy/ping")?)).await.map(|_| ())
    }

    /// Exchanges a username and password for a session token, which later requests send.
    pub async fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let request = self.http.post(self.url("/deploy/login")?);
        let response: LoginResponse =
            self.send(request.json(&LoginRequest { username, password })).await?;
        self.token = Some(response.token);
        Ok(())
    }

    /// Asks which queues this client should have.
    pub async fn printers(&self, info: &ClientInfo) -> Result<Vec<PrinterDef>> {
        let request = self.http.post(self.url("/deploy/printers")?).json(info);
        let response: PrintersResponse = self.send(request).await?;
        Ok(response.printers)
    }

    /// Resolves an absolute API path against the base URL.
    fn url(&self, path: &str) -> Result<Url> {
        Ok(self.base.join(path)?)
    }

    /// Sends a request and decodes a JSON response.
    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let (url, body) = self.send_raw(request).await?;
        serde_json::from_str(&body).map_err(|source| Error::Decode {
            url,
            source,
            body: redact(&body),
        })
    }

    /// Sends a request, attaching the session token, and returns the URL and body on success.
    async fn send_raw(&self, request: RequestBuilder) -> Result<(String, String)> {
        let request = match &self.token {
            Some(token) => request.header(header::AUTHORIZATION, format!("Bearer {token}")),
            None => request,
        };
        let response = request.send().await?;
        let url = response.url().to_string();
        let status = response.status();
        let body = response.text().await?;
        if self.verbose {
            eprintln!("{status} {url}\n{}\n", redact(&body));
        }
        if !status.is_success() {
            return Err(Error::Status { status, url, body: redact(&body) });
        }
        Ok((url, body))
    }
}

/// Placeholder for removed secrets.
const REDACTED: &str = "████████";

/// Makes text safe to show by replacing every word shaped like a secret: a JWT (`eyJ...`, the
/// Print Deploy session token) or 64 hex digits (a Mobility Print per-user address token).
pub fn redact(text: &str) -> String {
    let is_delimiter = |c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    text.split_inclusive(is_delimiter)
        .map(|piece| {
            let word = piece.trim_end_matches(is_delimiter);
            let secret = (word.starts_with("eyJ") && word.matches('.').count() == 2)
                || (word.len() == 64 && word.bytes().all(|b| b.is_ascii_hexdigit()));
            if secret { format!("{REDACTED}{}", &piece[word.len()..]) } else { piece.to_owned() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(r#"{"Token":"eyJh.eyJp.sig","n":"eyJ"}"#, format!(r#"{{"Token":"{REDACTED}","n":"eyJ"}}"#))]
    #[case(
        &format!("ipps://m:9164/printers/A/users/u/{}", "0a".repeat(32)),
        format!("ipps://m:9164/printers/A/users/u/{REDACTED}")
    )]
    #[case("ipps://m:9164/printers/A/users/u/0a1b", "ipps://m:9164/printers/A/users/u/0a1b".into())]
    #[case("<html>not json</html>", "<html>not json</html>".into())]
    fn redacts_secrets(#[case] text: &str, #[case] expected: String) {
        assert_eq!(redact(text), expected);
    }
}
