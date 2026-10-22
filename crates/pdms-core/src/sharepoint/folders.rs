// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Area, department, and document folders. They are created on demand and
//! tracked by Graph item ID, so renames and moves never depend on paths.

use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::layout::{self, Area};
use crate::Core;
use crate::error::{CoreError, CoreResult};
use crate::graph::{GraphClient, GraphError};

#[derive(Deserialize)]
pub(super) struct Item {
    pub id: String,
}

/// A top-level area folder, created if missing. Returns its ID and whether
/// PADM created it.
pub(super) async fn ensure_area_folder(
    g: &GraphClient,
    drive: &str,
    area: Area,
) -> CoreResult<(String, bool)> {
    let path = format!(
        "/drives/{drive}/root:/{}?$select=id",
        layout::encode_segment(area.folder_name())
    );
    match g.get::<Item>(&path).await {
        Ok(item) => return Ok((item.id, false)),
        Err(GraphError::NotFound) => {}
        Err(e) => return Err(e.into()),
    }
    let body = json!({"name": area.folder_name(), "folder": {}, "@microsoft.graph.conflictBehavior": "fail"});
    match g
        .post::<_, Item>(&format!("/drives/{drive}/root/children"), &body)
        .await
    {
        Ok(item) => Ok((item.id, true)),
        // Another process created it first.
        Err(GraphError::Conflict) => Ok((g.get::<Item>(&path).await?.id, false)),
        Err(e) => Err(e.into()),
    }
}

/// The folder for `department_id` inside `area`: created if missing, renamed
/// when the department's name changed. Another department may already have
/// the plain name; in that case the folder is suffixed with the department code.
pub async fn department_folder_id(
    core: &Core,
    area: Area,
    department_id: Uuid,
) -> CoreResult<String> {
    let sp = core
        .sharepoint
        .as_ref()
        .ok_or_else(|| CoreError::internal("SharePoint storage is not configured"))?;
    let r = sp.resolved().await?;
    let dept = sqlx::query!(
        "SELECT name, code FROM departments WHERE id = $1",
        department_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let want = layout::department_folder_name(&dept.name);
    let fallback = layout::department_folder_name_with_code(&dept.name, &dept.code);
    let row = sqlx::query!(
        "SELECT item_id, name FROM sharepoint_department_folders WHERE area = $1 AND department_id = $2",
        area.as_str(),
        department_id
    )
    .fetch_optional(&core.db)
    .await?;
    if let Some(row) = row {
        if row.name != want && row.name != fallback {
            let name = move_item(
                sp.graph(),
                &r.drive_id,
                &row.item_id,
                None,
                &want,
                &fallback,
            )
            .await?;
            sqlx::query!(
                "UPDATE sharepoint_department_folders SET name = $3 WHERE area = $1 AND department_id = $2",
                area.as_str(),
                department_id,
                name
            )
            .execute(&core.db)
            .await?;
        }
        return Ok(row.item_id);
    }
    let (item_id, name) = create_folder(
        &core.db,
        sp.graph(),
        &r.drive_id,
        r.area_id(area),
        &want,
        &fallback,
    )
    .await?;
    sqlx::query!(
        "INSERT INTO sharepoint_department_folders (area, department_id, item_id, name)
         VALUES ($1, $2, $3, $4) ON CONFLICT (area, department_id) DO NOTHING",
        area.as_str(),
        department_id,
        item_id,
        name
    )
    .execute(&core.db)
    .await?;
    // A concurrent caller may have won; use whatever was recorded.
    Ok(sqlx::query_scalar!(
        "SELECT item_id FROM sharepoint_department_folders WHERE area = $1 AND department_id = $2",
        area.as_str(),
        department_id
    )
    .fetch_one(&core.db)
    .await?)
}

/// Create folder `name` under `parent`. If the name is taken by a folder no
/// PADM record points at (left by an interrupted run), adopt it. If it
/// belongs to another record, try `fallback`.
pub(super) async fn create_folder(
    db: &PgPool,
    g: &GraphClient,
    drive: &str,
    parent: &str,
    name: &str,
    fallback: &str,
) -> CoreResult<(String, String)> {
    for candidate in [name, fallback] {
        let body =
            json!({"name": candidate, "folder": {}, "@microsoft.graph.conflictBehavior": "fail"});
        match g
            .post::<_, Item>(&format!("/drives/{drive}/items/{parent}/children"), &body)
            .await
        {
            Ok(item) => return Ok((item.id, candidate.to_owned())),
            Err(GraphError::Conflict) => {
                let existing: Item = g
                    .get(&format!(
                        "/drives/{drive}/items/{parent}:/{}?$select=id",
                        layout::encode_segment(candidate)
                    ))
                    .await?;
                if !is_tracked(db, &existing.id).await? {
                    return Ok((existing.id, candidate.to_owned()));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(CoreError::Conflict(format!(
        "SharePoint already has folders named {name:?} and {fallback:?} that belong to other records"
    )))
}

async fn is_tracked(db: &PgPool, item_id: &str) -> CoreResult<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM sharepoint_department_folders WHERE item_id = $1)
               OR EXISTS (SELECT 1 FROM sharepoint_document_folders WHERE item_id = $1) AS "tracked!""#,
        item_id
    )
    .fetch_one(db)
    .await?)
}

/// Rename an item, and with `parent` also move it. If the name is taken
/// there, try `fallback`. Returns the name it ended up with.
pub(super) async fn move_item(
    g: &GraphClient,
    drive: &str,
    item: &str,
    parent: Option<&str>,
    name: &str,
    fallback: &str,
) -> CoreResult<String> {
    for candidate in [name, fallback] {
        let mut body = json!({"name": candidate});
        if let (Some(p), Some(o)) = (parent, body.as_object_mut()) {
            o.insert("parentReference".into(), json!({"id": p}));
        }
        match g
            .patch::<_, serde_json::Value>(&format!("/drives/{drive}/items/{item}"), &body)
            .await
        {
            Ok(_) => return Ok(candidate.to_owned()),
            Err(GraphError::Conflict) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(CoreError::Conflict(format!(
        "SharePoint already has items named {name:?} and {fallback:?} in that folder"
    )))
}
