// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Reading multipart upload forms.
//!
//! For no-JavaScript file forms the CSRF middleware defers the token check to
//! here: the token must be the first field, and it is verified before any
//! other field (and before any file data) is read.

use std::collections::BTreeMap;

use axum::extract::Multipart;
use axum::extract::multipart::MultipartError;
use pdms_core::CoreError;
use pdms_core::documents::files::Upload;
use pdms_storage::spool::{SpoolError, SpoolWriter, sanitize_file_name};

use crate::error::{AppError, AppResult};
use crate::middleware::{CSRF_FIELD, DeferredCsrf};
use crate::state::AppState;

const MAX_TEXT_FIELD: usize = 64 * 1024;
const MAX_FIELDS: usize = 500;

/// Text fields (repeatable) and at most one file named `file`.
#[derive(Default)]
pub struct UploadForm {
    pub fields: BTreeMap<String, Vec<String>>,
    pub file: Option<Upload>,
}

impl UploadForm {
    pub fn get(&self, name: &str) -> &str {
        self.fields
            .get(name)
            .and_then(|v| v.first())
            .map_or("", String::as_str)
    }
}

fn bad(e: MultipartError) -> AppError {
    AppError::BadRequest(format!("could not read the form: {}", e.body_text()))
}

pub async fn read(
    state: &AppState,
    deferred: Option<DeferredCsrf>,
    mut mp: Multipart,
) -> AppResult<UploadForm> {
    let mut form = UploadForm::default();
    let mut count = 0usize;
    let mut csrf_checked = deferred.is_none();
    while let Some(mut field) = mp.next_field().await.map_err(bad)? {
        count += 1;
        if count > MAX_FIELDS {
            return Err(AppError::BadRequest("too many form fields".into()));
        }
        let name = field.name().unwrap_or("").to_owned();
        if let Some(DeferredCsrf(session)) = deferred.as_ref().filter(|_| !csrf_checked) {
            if name != CSRF_FIELD {
                return Err(AppError::Csrf);
            }
            let token = field.text().await.map_err(bad)?;
            if !state.core.keys.verify_csrf(session, &token) {
                return Err(AppError::Csrf);
            }
            csrf_checked = true;
            continue;
        }
        if name == "file" {
            let file_name = field.file_name().unwrap_or("").to_owned();
            if form.file.is_some() {
                return Err(AppError::BadRequest("only one file per upload".into()));
            }
            let limit = state.core.config.storage.max_upload_bytes;
            let mut w = SpoolWriter::create(&state.core.config.storage.spool_path, limit)
                .await
                .map_err(|e| AppError::Core(CoreError::internal(e)))?;
            let mut any = false;
            while let Some(chunk) = field.chunk().await.map_err(bad)? {
                any = true;
                w.write(&chunk).await.map_err(|e| match e {
                    SpoolError::TooLarge { limit } => {
                        AppError::Core(CoreError::Validation(format!(
                            "The file is larger than the {} MB limit.",
                            limit / (1024 * 1024)
                        )))
                    }
                    other => AppError::Core(CoreError::internal(other)),
                })?;
            }
            // An empty file input submits a part with no filename and no bytes.
            if any || !file_name.is_empty() {
                let spooled = w
                    .finish()
                    .await
                    .map_err(|e| AppError::Core(CoreError::internal(e)))?;
                if spooled.size > 0 {
                    form.file = Some(Upload {
                        file: spooled,
                        file_name: sanitize_file_name(&file_name),
                    });
                }
            }
            continue;
        }
        let mut text = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(bad)? {
            text.extend_from_slice(&chunk);
            if text.len() > MAX_TEXT_FIELD {
                return Err(AppError::BadRequest(format!(
                    "the {name} field is too long"
                )));
            }
        }
        let text = String::from_utf8(text)
            .map_err(|_| AppError::BadRequest("form text must be UTF-8".into()))?;
        form.fields.entry(name).or_default().push(text);
    }
    if !csrf_checked {
        return Err(AppError::Csrf);
    }
    Ok(form)
}
