// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Documents: types, custom fields, records, versions, tags, sharing, search.

pub mod access;
pub mod fields;
pub mod files;
pub mod retention;
pub mod search;
mod service;
pub mod status;
pub mod tags;
pub mod types;

pub use service::*;
pub use status::DocumentStatus;
