-- 0006_integrations.sql — Phase 4
--
-- API tokens and the API request log, outbound webhooks (and integration
-- providers) with their delivery logs, the audit-event cursor that feeds them,
-- and records request packages.

-- ---------------------------------------------------------------------------
-- API tokens
-- ---------------------------------------------------------------------------

-- A token acts as its user, limited to its scopes. Only a SHA-256 of the
-- secret is stored; the secret is shown once.
CREATE TABLE api_tokens (
    id           uuid PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES users (id),
    name         text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    token_hash   bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    token_prefix text NOT NULL,                       -- first characters, for recognizing a token
    scopes       text[] NOT NULL CHECK (cardinality(scopes) > 0),
    created_by   uuid REFERENCES users (id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    last_used_at timestamptz,
    last_audited_at timestamptz,
    revoked_at   timestamptz,
    revoked_by   uuid REFERENCES users (id)
);
CREATE INDEX api_tokens_user_idx ON api_tokens (user_id);

-- Every API request (the audit log records token creation, revocation, and
-- use; this table records each call). Purged after a configurable period.
CREATE TABLE api_request_log (
    id          uuid PRIMARY KEY,
    token_id    uuid REFERENCES api_tokens (id),
    user_id     uuid REFERENCES users (id),
    method      text NOT NULL,
    path        text NOT NULL,
    status      integer NOT NULL,
    duration_ms integer NOT NULL,
    ip          inet,
    user_agent  text,
    request_id  text,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX api_request_log_created_idx ON api_request_log (created_at);
CREATE INDEX api_request_log_token_idx ON api_request_log (token_id, created_at);

-- ---------------------------------------------------------------------------
-- Outbound events: webhooks and integration providers
-- ---------------------------------------------------------------------------

-- Webhooks send events about what their service user may see.
CREATE TABLE webhooks (
    id           uuid PRIMARY KEY,
    name         text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    url          text NOT NULL CHECK (url ~ '^https?://'),
    secret_enc   bytea NOT NULL,                      -- HMAC signing secret, encrypted with the master key
    events       text[] NOT NULL CHECK (cardinality(events) > 0),   -- e.g. {document.*, workflow.approved}
    acts_as_user_id uuid NOT NULL REFERENCES users (id),
    is_active    boolean NOT NULL DEFAULT true,
    created_by   uuid REFERENCES users (id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TYPE outbound_status AS ENUM ('pending', 'delivered', 'failed', 'skipped');

-- One event for one target (a webhook or an integration provider).
CREATE TABLE webhook_deliveries (
    id           uuid PRIMARY KEY,
    webhook_id   uuid REFERENCES webhooks (id),
    provider     text,
    audit_seq    bigint NOT NULL,                     -- the event's audit entry
    event        text NOT NULL,
    payload      jsonb NOT NULL,
    status       outbound_status NOT NULL DEFAULT 'pending',
    attempts     integer NOT NULL DEFAULT 0,
    last_status_code integer,
    last_error   text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    delivered_at timestamptz,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT webhook_deliveries_target CHECK (num_nonnulls(webhook_id, provider) = 1)
);
CREATE UNIQUE INDEX webhook_deliveries_webhook_key ON webhook_deliveries (webhook_id, audit_seq) WHERE webhook_id IS NOT NULL;
CREATE UNIQUE INDEX webhook_deliveries_provider_key ON webhook_deliveries (provider, audit_seq) WHERE provider IS NOT NULL;
CREATE INDEX webhook_deliveries_created_idx ON webhook_deliveries (created_at DESC);

CREATE TABLE webhook_delivery_attempts (
    id          uuid PRIMARY KEY,
    delivery_id uuid NOT NULL REFERENCES webhook_deliveries (id),
    attempt_no  integer NOT NULL,
    started_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    status_code integer,
    error       text,
    worker      text NOT NULL
);
CREATE INDEX webhook_delivery_attempts_delivery_idx ON webhook_delivery_attempts (delivery_id, attempt_no);

-- How far each consumer of the audit stream has read (webhooks/providers,
-- the audit forwarder). Audit sequence numbers are committed in order.
CREATE TABLE outbound_cursors (
    name     text PRIMARY KEY,
    last_seq bigint NOT NULL DEFAULT 0
);

-- ---------------------------------------------------------------------------
-- Records request packages
-- ---------------------------------------------------------------------------

CREATE TABLE export_packages (
    id                uuid PRIMARY KEY,
    request_reference text NOT NULL CHECK (length(request_reference) BETWEEN 1 AND 100),
    requested_by      uuid NOT NULL REFERENCES users (id),
    created_at        timestamptz NOT NULL DEFAULT now(),
    document_ids      uuid[] NOT NULL,
    include_versions  text NOT NULL CHECK (include_versions IN ('current', 'all', 'none')),
    include_audit     boolean NOT NULL,
    fields            text[] NOT NULL,
    note              text NOT NULL DEFAULT '',
    file_count        integer NOT NULL,
    size_bytes        bigint NOT NULL,
    package_sha256    bytea NOT NULL CHECK (octet_length(package_sha256) = 32),
    manifest_sha256   bytea NOT NULL CHECK (octet_length(manifest_sha256) = 32),
    storage_backend   text NOT NULL
);
CREATE INDEX export_packages_requested_by_idx ON export_packages (requested_by, created_at DESC);

-- Records officers may export non-confidential documents org-wide (export
-- implies view). Confidential documents still need a document grant.
INSERT INTO acl_entries (id, subject_role_id, scope, permission, reason)
SELECT gen_random_uuid(), id, 'global', 'export', 'seeded default for records requests'
  FROM roles WHERE key = 'records_officer';

-- ---------------------------------------------------------------------------
-- Privileges
-- ---------------------------------------------------------------------------

GRANT SELECT, INSERT, UPDATE ON api_tokens, webhooks, webhook_deliveries, outbound_cursors TO pdms_app;
GRANT SELECT, INSERT, DELETE ON api_request_log TO pdms_app;
GRANT SELECT, INSERT, UPDATE ON webhook_delivery_attempts TO pdms_app;
GRANT SELECT, INSERT ON export_packages TO pdms_app;
