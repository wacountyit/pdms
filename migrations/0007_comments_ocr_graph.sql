-- 0007_comments_ocr_graph.sql — Phase 5
--
-- Threaded document comments with preserved revisions, text extracted from
-- files (searchable) with date suggestions awaiting confirmation, and the
-- links that keep Outlook calendar events (Microsoft Graph) in step with
-- document and appointment dates.

-- ---------------------------------------------------------------------------
-- Comments
-- ---------------------------------------------------------------------------

CREATE TABLE comments (
    id          uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES documents (id),
    parent_id   uuid REFERENCES comments (id),     -- a reply; threads are one level deep
    author_id   uuid NOT NULL REFERENCES users (id),
    body        text NOT NULL CHECK (length(body) BETWEEN 1 AND 10000),  -- current text ('' never)
    mentions    uuid[] NOT NULL DEFAULT '{}',
    created_at  timestamptz NOT NULL DEFAULT now(),
    edited_at   timestamptz,
    deleted_at  timestamptz,                         -- the text stays in comment_revisions
    deleted_by  uuid REFERENCES users (id)
);
CREATE INDEX comments_document_idx ON comments (document_id, created_at);

-- Every version of every comment, original first. Never updated or deleted.
CREATE TABLE comment_revisions (
    id          uuid PRIMARY KEY,
    comment_id  uuid NOT NULL REFERENCES comments (id),
    revision_no integer NOT NULL CHECK (revision_no > 0),
    body        text NOT NULL,
    edited_by   uuid REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (comment_id, revision_no)
);

CREATE FUNCTION comment_revisions_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'comment revisions are immutable';
END $$;
CREATE TRIGGER comment_revisions_no_change BEFORE UPDATE OR DELETE ON comment_revisions
    FOR EACH ROW EXECUTE FUNCTION comment_revisions_immutable();
CREATE TRIGGER comment_revisions_no_truncate BEFORE TRUNCATE ON comment_revisions
    FOR EACH STATEMENT EXECUTE FUNCTION comment_revisions_immutable();

-- ---------------------------------------------------------------------------
-- Extracted text and date suggestions
-- ---------------------------------------------------------------------------

CREATE TYPE extraction_status AS ENUM ('done', 'empty', 'unsupported', 'failed');

-- Text extracted from one file version (embedded text, or OCR).
CREATE TABLE document_texts (
    document_id uuid NOT NULL REFERENCES documents (id),
    version_no  integer NOT NULL,
    blob_sha256 bytea NOT NULL,
    status      extraction_status NOT NULL,
    method      text NOT NULL,                       -- pdf_text, docx, plain, ocr, none
    text        text NOT NULL DEFAULT '',
    char_count  integer NOT NULL DEFAULT 0,
    error       text,
    extracted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (document_id, version_no)
);

CREATE TYPE suggestion_status AS ENUM ('pending', 'accepted', 'dismissed');

-- A date (or interval) found in a file, waiting for a person to confirm it.
-- Never applied automatically.
CREATE TABLE date_suggestions (
    id              uuid PRIMARY KEY,
    document_id     uuid NOT NULL REFERENCES documents (id),
    version_no      integer NOT NULL,
    field           text NOT NULL CHECK (field IN ('execution_date', 'effective_date', 'renewal_date',
                                                   'expiration_date', 'next_review_date', 'review_interval_months')),
    suggested_date  date,
    suggested_months integer,
    snippet         text NOT NULL,
    confidence      smallint NOT NULL CHECK (confidence BETWEEN 0 AND 100),
    status          suggestion_status NOT NULL DEFAULT 'pending',
    decided_by      uuid REFERENCES users (id),
    decided_at      timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(suggested_date, suggested_months) = 1),
    UNIQUE NULLS NOT DISTINCT (document_id, version_no, field, suggested_date, suggested_months)
);
CREATE INDEX date_suggestions_document_idx ON date_suggestions (document_id, status);

-- Full-text search now also covers the text of each document's latest file.
CREATE OR REPLACE FUNCTION refresh_document_search(doc uuid) RETURNS void
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
             WHERE v.document_id = d.id AND v.file_changed), '')), 'D') ||
        setweight(to_tsvector('english', coalesce((
            SELECT left(x.text, 400000) FROM document_texts x
             WHERE x.document_id = d.id AND x.status = 'done'
             ORDER BY x.version_no DESC LIMIT 1), '')), 'D')
     WHERE d.id = doc
$$;

-- ---------------------------------------------------------------------------
-- Outlook calendar (Microsoft Graph)
-- ---------------------------------------------------------------------------

-- One Outlook event per date (document review/renewal/expiration, appointment
-- term end/reappointment decision), so changes update and removals delete it.
CREATE TABLE graph_calendar_links (
    source_key   text PRIMARY KEY,                   -- e.g. doc:<id>:renewal, appt:<id>:term_end
    mailbox      text NOT NULL,
    event_id     text NOT NULL,                      -- Graph's event ID
    event_date   date NOT NULL,
    content_hash text NOT NULL,                      -- of subject + body + date, to detect changes
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    last_error   text
);

-- ---------------------------------------------------------------------------
-- Privileges
-- ---------------------------------------------------------------------------

GRANT SELECT, INSERT, UPDATE ON comments TO pdms_app;
GRANT SELECT, INSERT ON comment_revisions TO pdms_app;
GRANT SELECT, INSERT, UPDATE ON document_texts, date_suggestions TO pdms_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON graph_calendar_links TO pdms_app;
