// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Tags: free-form (anyone who can edit a document, if enabled) and curated
//! (administrator-managed, with colors). Changes are audited.

use pdms_audit::{AuditEvent, Category};
use pdms_authz::Capability;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct TagRef {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub color: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Tag {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub color: String,
    pub description: String,
    pub is_curated: bool,
    pub document_count: i64,
}

pub const DEFAULT_COLOR: &str = "#57606a";

/// Lowercase, hyphen-separated identifier used for uniqueness and URLs.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(60).collect()
}

fn clean_name(name: &str) -> CoreResult<(String, String)> {
    let name: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect();
    let slug = slugify(&name);
    if name.is_empty() || slug.is_empty() {
        return Err(CoreError::validation(
            "tag names need at least one letter or digit",
        ));
    }
    Ok((name, slug))
}

fn clean_color(color: &str) -> CoreResult<String> {
    let c = color.trim().to_ascii_lowercase();
    let ok = c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit());
    if ok {
        Ok(c)
    } else {
        Err(CoreError::validation("color must look like #1a2b3c"))
    }
}

pub async fn list(db: &PgPool) -> CoreResult<Vec<Tag>> {
    Ok(sqlx::query_as!(
        Tag,
        r#"SELECT t.id, t.name, t.slug, t.color, t.description, t.is_curated,
                  (SELECT count(*) FROM document_tags dt JOIN documents d ON d.id = dt.document_id
                    WHERE dt.tag_id = t.id AND d.deleted_at IS NULL) AS "document_count!"
             FROM tags t ORDER BY t.is_curated DESC, t.name"#
    )
    .fetch_all(db)
    .await?)
}

/// Find a tag by name, creating a free-form one if allowed. Returns the tag
/// and the audit event for its creation, if it was created.
pub(crate) async fn find_or_create(
    conn: &mut PgConnection,
    ctx: &Ctx,
    name: &str,
    allow_create: bool,
) -> CoreResult<(TagRef, Option<AuditEvent>)> {
    let (name, slug) = clean_name(name)?;
    if let Some(t) = sqlx::query_as!(
        TagRef,
        "SELECT id, name, slug, color FROM tags WHERE slug = $1",
        slug
    )
    .fetch_optional(&mut *conn)
    .await?
    {
        return Ok((t, None));
    }
    if !allow_create {
        return Err(CoreError::validation(format!(
            "“{name}” is not an existing tag; ask an administrator to add it"
        )));
    }
    let id = Uuid::now_v7();
    let t = sqlx::query_as!(
        TagRef,
        "INSERT INTO tags (id, name, slug, color, created_by) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (slug) DO UPDATE SET slug = EXCLUDED.slug
         RETURNING id, name, slug, color",
        id,
        name,
        slug,
        DEFAULT_COLOR,
        ctx.user_id(),
    )
    .fetch_one(&mut *conn)
    .await?;
    let created = (t.id == id).then(|| {
        AuditEvent::new(Category::Metadata, "tag.created", ctx.actor.clone(), "tag")
            .object(id, Some(&t.name))
            .change("name", None::<String>, t.name.clone())
            .context("curated", false)
    });
    Ok((t, created))
}

pub async fn create_curated(
    db: &PgPool,
    ctx: &Ctx,
    name: &str,
    color: &str,
    description: &str,
) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageTags)?;
    let (name, slug) = clean_name(name)?;
    let color = clean_color(color)?;
    let description: String = description.trim().chars().take(500).collect();
    let id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO tags (id, name, slug, color, description, is_curated, created_by)
         VALUES ($1, $2, $3, $4, $5, true, $6)",
        id,
        name,
        slug,
        color,
        description,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "a tag with that name already exists"))?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(Category::Admin, "tag.created", ctx.actor.clone(), "tag")
                .object(id, Some(&name))
                .change("name", None::<String>, name.as_str())
                .change("color", None::<String>, color.as_str())
                .context("curated", true),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Rename, recolor, or (un)curate a tag. Renaming re-indexes tagged documents.
pub async fn update(
    db: &PgPool,
    ctx: &Ctx,
    id: Uuid,
    name: &str,
    color: &str,
    description: &str,
    is_curated: bool,
) -> CoreResult<()> {
    ctx.require(Capability::ManageTags)?;
    let (name, slug) = clean_name(name)?;
    let color = clean_color(color)?;
    let description: String = description.trim().chars().take(500).collect();
    let mut tx = db.begin().await?;
    let before = sqlx::query!(
        "SELECT name, color, description, is_curated FROM tags WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    sqlx::query!(
        "UPDATE tags SET name = $2, slug = $3, color = $4, description = $5, is_curated = $6 WHERE id = $1",
        id,
        name,
        slug,
        color,
        description,
        is_curated
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "a tag with that name already exists"))?;
    if before.name != name {
        sqlx::query(
            "SELECT refresh_document_search(document_id) FROM document_tags WHERE tag_id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    let e = AuditEvent::new(Category::Admin, "tag.updated", ctx.actor.clone(), "tag")
        .object(id, Some(&name))
        .change("name", before.name, name.as_str())
        .change("color", before.color, color.as_str())
        .change("description", before.description, description.as_str())
        .change("is_curated", before.is_curated, is_curated);
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("  Operations  "), "operations");
        assert_eq!(slugify("FY-2026 Budget!"), "fy-2026-budget");
        assert_eq!(slugify("Ünïcode"), "n-code");
        assert_eq!(slugify("***"), "");
    }
}
