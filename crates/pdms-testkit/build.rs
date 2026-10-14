// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
// `sqlx::migrate!` embeds the migrations at compile time; rebuild when they change.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
