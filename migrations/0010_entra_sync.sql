-- Entra ID user sync (spec 2026-10-14, §6; D-086): who manages each account,
-- the raw Department value, Department → PADM department mappings, and cursors
-- for change tracking.

CREATE TYPE user_source AS ENUM ('local', 'entra');

ALTER TABLE users
    ADD COLUMN source           user_source NOT NULL DEFAULT 'local',
    ADD COLUMN entra_department text,
    ADD COLUMN entra_synced_at  timestamptz;

-- Issuers compare trimmed and without a trailing slash (auth::oidc::normalize_issuer).
-- A row whose normalized (issuer, subject) pair another row already holds
-- is left as it is, so the unique key cannot abort the migration.
UPDATE users u
   SET oidc_issuer = rtrim(btrim(u.oidc_issuer), '/')
 WHERE u.oidc_issuer <> rtrim(btrim(u.oidc_issuer), '/')
   AND NOT EXISTS (
       SELECT 1 FROM users o
        WHERE o.id <> u.id
          AND o.oidc_subject = u.oidc_subject
          AND rtrim(btrim(o.oidc_issuer), '/') = rtrim(btrim(u.oidc_issuer), '/'));

CREATE TABLE department_entra_values (
    value_normalized text PRIMARY KEY CHECK (value_normalized <> ''),
    value_display    text NOT NULL CHECK (length(value_display) BETWEEN 1 AND 200),
    department_id    uuid NOT NULL REFERENCES departments (id),
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX department_entra_values_department_idx ON department_entra_values (department_id);

CREATE TABLE sync_cursors (
    name         text PRIMARY KEY,   -- 'entra.users'; P4 adds 'sharepoint.delta'
    delta_link   text,
    delta_select text,               -- the $select the delta link was started with
    last_run     timestamptz,
    last_error   text,
    last_summary jsonb
);

GRANT SELECT, INSERT, UPDATE, DELETE ON department_entra_values TO pdms_app;
GRANT SELECT, INSERT, UPDATE ON sync_cursors TO pdms_app;
