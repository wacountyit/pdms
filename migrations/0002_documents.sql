-- 0002_documents.sql — Phase 1
--
-- Document types, versioned custom fields, documents, immutable content and
-- metadata versions, content-addressed blobs, tags, retention policies, and
-- full-text search. Also links document-scoped grants to documents.

CREATE TYPE document_status AS ENUM (
    'draft', 'in_review', 'active', 'renewal_due', 'overdue', 'expired', 'superseded', 'archived');
CREATE TYPE field_type AS ENUM (
    'text', 'long_text', 'number', 'date', 'boolean', 'single_select', 'multi_select',
    'user', 'department', 'url');
CREATE TYPE retention_basis AS ENUM ('created_at', 'effective_date', 'expiration_date', 'archived_at');
CREATE TYPE version_kind AS ENUM (
    'initial', 'revision', 'amendment', 'signed_copy', 'metadata', 'restore');

-- ---------------------------------------------------------------------------
-- Retention and document types
-- ---------------------------------------------------------------------------

CREATE TABLE retention_policies (
    id             uuid PRIMARY KEY,
    name           text NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 200),
    description    text NOT NULL DEFAULT '',
    -- NULL = keep permanently. Otherwise deletion is blocked until
    -- basis date + retain_years has passed.
    retain_years   integer CHECK (retain_years IS NULL OR retain_years BETWEEN 0 AND 200),
    basis          retention_basis NOT NULL DEFAULT 'created_at',
    legal_citation text NOT NULL DEFAULT '',   -- e.g. a records retention schedule item number
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE document_types (
    id                     uuid PRIMARY KEY,
    key                    text NOT NULL UNIQUE CHECK (key ~ '^[a-z][a-z0-9_]{1,63}$'),
    name                   text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    description            text NOT NULL DEFAULT '',
    -- Prefix of the stable external ID, e.g. PAG-2026-000123.
    id_prefix              text NOT NULL CHECK (id_prefix ~ '^[A-Z]{2,8}$'),
    -- Default review cycle (Phase 2 scheduling); NULL = no recurring review.
    review_interval_months integer CHECK (review_interval_months IS NULL OR review_interval_months > 0),
    requires_legal_review  boolean NOT NULL DEFAULT false,   -- used by Phase 3 workflow
    requires_signatures    boolean NOT NULL DEFAULT false,   -- used by Phase 3 workflow
    retention_policy_id    uuid REFERENCES retention_policies (id),
    is_active              boolean NOT NULL DEFAULT true,
    sort_order             integer NOT NULL DEFAULT 0,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Custom fields: identity + immutable versions of the definition
-- ---------------------------------------------------------------------------
-- Values live in documents.custom_fields keyed by the immutable `key`.
-- Renaming, re-labelling, or changing options adds a version; the type never
-- changes, and values are never rewritten, so old records stay valid.

CREATE TABLE custom_field_definitions (
    id               uuid PRIMARY KEY,
    key              text NOT NULL UNIQUE CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
    field_type       field_type NOT NULL,
    document_type_id uuid REFERENCES document_types (id),   -- NULL = all types
    current_version  integer NOT NULL DEFAULT 1 CHECK (current_version > 0),
    is_deprecated    boolean NOT NULL DEFAULT false,
    sort_order       integer NOT NULL DEFAULT 0,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE custom_field_versions (
    field_id      uuid NOT NULL REFERENCES custom_field_definitions (id),
    version       integer NOT NULL CHECK (version > 0),
    label         text NOT NULL CHECK (length(label) BETWEEN 1 AND 200),
    help_text     text NOT NULL DEFAULT '',
    required      boolean NOT NULL DEFAULT false,
    -- [{"value": "...", "label": "..."}] for select types
    options       jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(options) = 'array'),
    -- {"min": "0", "max": "100", "max_length": 200, "pattern": "^...$"}
    validation    jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(validation) = 'object'),
    default_value jsonb,
    created_by    uuid REFERENCES users (id),
    created_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (field_id, version)
);

-- ---------------------------------------------------------------------------
-- Documents
-- ---------------------------------------------------------------------------

CREATE SEQUENCE document_external_seq;

CREATE TABLE documents (
    id                     uuid PRIMARY KEY,
    external_id            text NOT NULL UNIQUE,      -- stable ID for people and integrations
    document_type_id       uuid NOT NULL REFERENCES document_types (id),
    department_id          uuid NOT NULL REFERENCES departments (id),
    owner_id               uuid NOT NULL REFERENCES users (id),   -- primary record owner
    title                  text NOT NULL CHECK (length(title) BETWEEN 1 AND 500),
    description            text NOT NULL DEFAULT '',
    status                 document_status NOT NULL DEFAULT 'draft',
    execution_date         date,
    effective_date         date,
    last_review_date       date,
    next_review_date       date,
    renewal_date           date,
    expiration_date        date,                      -- NULL = no expiration; stays active
    review_interval_months integer CHECK (review_interval_months IS NULL OR review_interval_months > 0),
    custom_fields          jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom_fields) = 'object'),
    inherit_acl            boolean NOT NULL DEFAULT true,
    is_confidential        boolean NOT NULL DEFAULT false,
    legal_hold             boolean NOT NULL DEFAULT false,
    retention_policy_id    uuid REFERENCES retention_policies (id),   -- overrides the type's
    current_version        integer NOT NULL DEFAULT 0,
    archived_at            timestamptz,
    deleted_at             timestamptz,                -- soft delete; row kept as a tombstone
    deleted_by             uuid REFERENCES users (id),
    row_version            integer NOT NULL DEFAULT 1, -- optimistic concurrency for edits
    search_extra           text NOT NULL DEFAULT '',   -- searchable custom-field text (maintained by the app)
    search                 tsvector,
    created_by             uuid REFERENCES users (id),         -- NULL = system
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX documents_department_idx ON documents (department_id) WHERE deleted_at IS NULL;
CREATE INDEX documents_owner_idx      ON documents (owner_id) WHERE deleted_at IS NULL;
CREATE INDEX documents_type_idx       ON documents (document_type_id) WHERE deleted_at IS NULL;
CREATE INDEX documents_status_idx     ON documents (status) WHERE deleted_at IS NULL;
CREATE INDEX documents_next_review_idx ON documents (next_review_date) WHERE deleted_at IS NULL;
CREATE INDEX documents_renewal_idx    ON documents (renewal_date) WHERE deleted_at IS NULL;
CREATE INDEX documents_expiration_idx ON documents (expiration_date) WHERE deleted_at IS NULL;
CREATE INDEX documents_search_idx     ON documents USING gin (search);
CREATE INDEX documents_custom_idx     ON documents USING gin (custom_fields jsonb_path_ops);

ALTER TABLE acl_entries
    ADD CONSTRAINT acl_entries_document_fk FOREIGN KEY (scope_document_id) REFERENCES documents (id);

-- ---------------------------------------------------------------------------
-- Content: blobs and versions (both immutable)
-- ---------------------------------------------------------------------------

CREATE TABLE blobs (
    sha256          bytea PRIMARY KEY CHECK (octet_length(sha256) = 32),
    size_bytes      bigint NOT NULL CHECK (size_bytes >= 0),
    media_type      text NOT NULL,                     -- sniffed from content, not trusted from the client
    storage_backend text NOT NULL,                     -- 'local' or 's3'
    created_at      timestamptz NOT NULL DEFAULT now()
);

-- Every change to a document appends a version: a full metadata snapshot plus
-- a pointer to the file current at that point. Rows are never updated.
CREATE TABLE document_versions (
    id             uuid PRIMARY KEY,
    document_id    uuid NOT NULL REFERENCES documents (id),
    version_no     integer NOT NULL CHECK (version_no > 0),
    kind           version_kind NOT NULL,
    file_changed   boolean NOT NULL,                   -- true when this version introduced a file
    blob_sha256    bytea REFERENCES blobs (sha256),    -- the file current at this version, if any
    file_name      text,
    metadata       jsonb NOT NULL,                     -- snapshot: standard fields, custom fields, tags
    field_versions jsonb NOT NULL DEFAULT '{}',        -- {field key: definition version} at snapshot time
    note           text NOT NULL DEFAULT '',           -- e.g. "Amendment 1"
    milestone_date date,                               -- e.g. the date an amendment was executed
    created_by     uuid REFERENCES users (id),             -- NULL = system
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (document_id, version_no),
    CHECK (NOT file_changed OR blob_sha256 IS NOT NULL)
);
CREATE INDEX document_versions_blob_idx ON document_versions (blob_sha256);

CREATE FUNCTION reject_modification() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is immutable: % rejected', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'insufficient_privilege';
END
$$;
CREATE TRIGGER document_versions_immutable BEFORE UPDATE OR DELETE ON document_versions
    FOR EACH ROW EXECUTE FUNCTION reject_modification();
CREATE TRIGGER blobs_immutable BEFORE UPDATE OR DELETE ON blobs
    FOR EACH ROW EXECUTE FUNCTION reject_modification();
CREATE TRIGGER custom_field_versions_immutable BEFORE UPDATE OR DELETE ON custom_field_versions
    FOR EACH ROW EXECUTE FUNCTION reject_modification();

-- ---------------------------------------------------------------------------
-- Tags
-- ---------------------------------------------------------------------------

CREATE TABLE tags (
    id          uuid PRIMARY KEY,
    name        text NOT NULL CHECK (length(name) BETWEEN 1 AND 60),
    slug        text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,59}$'),
    color       text NOT NULL DEFAULT '#57606a' CHECK (color ~ '^#[0-9a-f]{6}$'),
    description text NOT NULL DEFAULT '',
    is_curated  boolean NOT NULL DEFAULT false,        -- curated tags are managed by administrators
    created_by  uuid REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE document_tags (
    document_id uuid NOT NULL REFERENCES documents (id),
    tag_id      uuid NOT NULL REFERENCES tags (id),
    added_by    uuid REFERENCES users (id),
    added_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (document_id, tag_id)
);
CREATE INDEX document_tags_tag_idx ON document_tags (tag_id);

-- ---------------------------------------------------------------------------
-- Full-text search
-- ---------------------------------------------------------------------------

CREATE FUNCTION refresh_document_search(doc uuid) RETURNS void
LANGUAGE sql AS $$
    UPDATE documents d SET search =
        setweight(to_tsvector('english', d.title), 'A') ||
        setweight(to_tsvector('simple', d.external_id), 'A') ||
        setweight(to_tsvector('english', d.description), 'B') ||
        setweight(to_tsvector('english', coalesce((
            SELECT string_agg(t.name, ' ') FROM document_tags dt JOIN tags t ON t.id = dt.tag_id
             WHERE dt.document_id = d.id), '')), 'B') ||
        setweight(to_tsvector('english', d.search_extra), 'C') ||
        setweight(to_tsvector('simple', coalesce((
            SELECT string_agg(DISTINCT v.file_name, ' ') FROM document_versions v
             WHERE v.document_id = d.id AND v.file_changed), '')), 'D')
     WHERE d.id = doc
$$;

-- ---------------------------------------------------------------------------
-- Seed data
-- ---------------------------------------------------------------------------

INSERT INTO retention_policies (id, name, description, retain_years, basis) VALUES
    (gen_random_uuid(), 'Permanent', 'Never delete.', NULL, 'created_at'),
    (gen_random_uuid(), 'Contract: expiration + 5 years',
        'Keep until five years after the agreement expires. Confirm against your records retention schedule.',
        5, 'expiration_date');

INSERT INTO document_types (id, key, name, description, id_prefix, review_interval_months,
                            requires_legal_review, requires_signatures, retention_policy_id, sort_order)
SELECT gen_random_uuid(), t.key, t.name, t.description, t.prefix, t.months, t.legal, t.sign,
       (SELECT id FROM retention_policies WHERE name = t.retention), t.sort
  FROM (VALUES
    ('policy', 'Policy', 'Adopted policies and procedures.', 'POL', 12, false, false, 'Permanent', 10),
    ('contract', 'Contract', 'Contracts and service agreements.', 'CON', NULL::integer, true, true,
        'Contract: expiration + 5 years', 20),
    ('partnership_agreement', 'Partnership Agreement', 'Agreements with partner organizations.',
        'PAG', 60, true, true, 'Contract: expiration + 5 years', 30),
    ('other', 'Other', 'Any other document with review, renewal, or expiration dates.', 'DOC',
        NULL::integer, false, false, NULL, 90)
  ) AS t (key, name, description, prefix, months, legal, sign, retention, sort);


-- ---------------------------------------------------------------------------
-- Privileges
-- ---------------------------------------------------------------------------

GRANT SELECT, INSERT, UPDATE ON retention_policies, document_types, custom_field_definitions,
                                documents, tags TO pdms_app;
GRANT SELECT, INSERT         ON custom_field_versions, blobs, document_versions TO pdms_app;
GRANT SELECT, INSERT, DELETE ON document_tags TO pdms_app;
GRANT USAGE ON SEQUENCE document_external_seq TO pdms_app;
REVOKE ALL ON FUNCTION reject_modification() FROM PUBLIC;
REVOKE ALL ON FUNCTION refresh_document_search(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION refresh_document_search(uuid) TO pdms_app;
