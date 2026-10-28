//! A minimal PaperCut Print Deploy client for macOS and Linux.
//!
//! papermend asks a Print Deploy server which print queues the current user should have, and drives
//! CUPS to match. There is no UI, self-updater, telemetry, job-tracking, or background daemon.
//!
//! This means that direct-print queues, which rely on PaperCut's job tracking for accounting, are
//! refused rather than installed untracked.
//!
//! The Print Deploy HTTP API is private. Paths were recovered from the official client; request
//! and response shapes are inferred and marked as such in [`api::model`].

#[cfg(not(unix))]
compile_error!("papermend drives CUPS, so it currently supports macOS and Linux only.");

pub mod error;
pub mod style;
