#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![allow(dead_code)]

use pdms_core::config::Secret;
use pdms_core::{Config, Core, Ctx, directory};
use pdms_testkit::TestDb;
use uuid::Uuid;

pub const MASTER_KEY: &str = "dGVzdC1rZXktdGVzdC1rZXktdGVzdC1rZXktdGVzdC1rZXktdGVzdC1rZXk=";

pub async fn core() -> (Core, TestDb) {
    let db = pdms_testkit::test_db().await;
    let mut config = Config::default();
    config.secret_key = Secret::new(MASTER_KEY);
    config.auth.local.max_failed_attempts = 3;
    let dir = std::env::temp_dir().join(format!("pdms-test-{}", Uuid::new_v4()));
    config.storage.local_path = dir.join("blobs");
    config.storage.spool_path = dir.join("spool");
    let core = Core::new(db.app.clone(), config).unwrap();
    (core, db)
}

pub fn system() -> Ctx {
    Ctx::system("test")
}

/// Create a user in a new department; returns (user_id, department_id).
pub async fn user_in_new_department(core: &Core, code: &str, email: &str) -> (Uuid, Uuid) {
    let dept = directory::create_department(&core.db, &system(), code, code)
        .await
        .unwrap();
    let user = directory::create_user(&core.db, &system(), email, email, Some(dept))
        .await
        .unwrap();
    (user, dept)
}

pub async fn audit_actions(core: &Core) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT action FROM audit_log ORDER BY seq")
        .fetch_all(&core.db)
        .await
        .unwrap()
}

/// Act as a user (principal loaded fresh, as the web layer does per request).
pub async fn as_user(core: &Core, user: Uuid) -> Ctx {
    let p = pdms_core::auth::principal::load(&core.db, user)
        .await
        .unwrap();
    Ctx::user(
        p,
        "test user",
        pdms_audit::AuthMethod::Local,
        &pdms_core::ClientInfo::default(),
    )
}

/// Spool bytes as if they had been uploaded.
pub async fn upload(core: &Core, name: &str, bytes: &[u8]) -> pdms_core::documents::files::Upload {
    let mut w =
        pdms_storage::spool::SpoolWriter::create(&core.config().storage.spool_path, 1 << 30)
            .await
            .unwrap();
    w.write(bytes).await.unwrap();
    pdms_core::documents::files::Upload {
        file: w.finish().await.unwrap(),
        file_name: pdms_storage::spool::sanitize_file_name(name),
    }
}

pub fn pdf(text: &str) -> Vec<u8> {
    format!("%PDF-1.4\n% {text}\n%%EOF\n").into_bytes()
}
