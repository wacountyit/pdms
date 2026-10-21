// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Microsoft Graph as PADM itself: app-only tokens from a certificate, and a
//! small client for JSON, delta queries, uploads, and downloads.

pub mod cert;
mod error;

pub use error::GraphError;
