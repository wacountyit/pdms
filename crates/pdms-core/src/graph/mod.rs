// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Microsoft Graph as PDMS itself: app-only tokens from a certificate, and a
//! small client for JSON, delta queries, uploads, and downloads.

pub mod cert;
mod client;
mod error;

pub use client::{DeltaPage, GraphClient};
pub use error::GraphError;
