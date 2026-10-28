//! Custom papermend errors.

use derive_more::{Display, Error, From};

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Every failure papermend can report to the user.
#[derive(Debug, Display, Error, From)]
pub enum Error {
    /// Transport-level failure talking to the Print Deploy server.
    #[display("Request to the Print Deploy server failed: {_0}")]
    Http(reqwest::Error),

    /// The server answered with a non-success status.
    #[display("{url} returned {status}: {body}")]
    #[from(ignore)]
    Status {
        status: reqwest::StatusCode,
        #[error(not(source))]
        url: String,
        #[error(not(source))]
        body: String,
    },

    /// The server answered with a body we could not decode.
    #[display("Could not decode response from {url} ({source}). Body: {body}")]
    #[from(ignore)]
    Decode {
        #[error(not(source))]
        url: String,
        source: serde_json::Error,
        #[error(not(source))]
        body: String,
    },

    /// A malformed server URL.
    #[display("Invalid server URL: {_0}")]
    URL(url::ParseError),

    /// Local I/O failure, for example spawning a CUPS tool.
    #[display("I/O error: {_0}")]
    IO(std::io::Error),

    /// A CUPS command exited unsuccessfully.
    #[display("`{command}` failed: {output}")]
    #[from(ignore)]
    CUPS {
        #[error(not(source))]
        command: String,
        #[error(not(source))]
        output: String,
    },

    /// Invalid input or an unexpected server response, with the message to show.
    #[display("{_0}")]
    #[from(ignore)]
    Invalid(#[error(not(source))] String),
}

impl Error {
    /// Renders this error with its full cause chain.
    pub fn report(&self) -> String {
        let mut message = self.to_string();
        let mut cause = std::error::Error::source(self);
        while let Some(error) = cause {
            let text = error.to_string();
            if !message.contains(&text) {
                message.push_str(": ");
                message.push_str(&text);
            }
            cause = error.source();
        }
        message
    }
}
