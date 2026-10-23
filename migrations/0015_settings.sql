-- Settings in Administration (S1, plan 2026-10-23-admin-settings).
-- Non-secret settings use the existing app_settings table (key, JSON value).

-- Secret settings (sign-in client secret, SMTP password, the Microsoft Graph
-- certificate's private key), encrypted with a key derived from the master
-- key. `ciphertext` is AES-256-GCM `nonce || ciphertext`, bound to `key`.
CREATE TABLE app_secrets (
    key        text PRIMARY KEY CHECK (key ~ '^[a-z][a-z0-9_.-]{1,127}$'),
    ciphertext bytea NOT NULL,
    updated_by uuid REFERENCES users (id),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Certificates for the "PDMS Service" app registration, made in PDMS. The
-- private key of each is in app_secrets as graph.certificate.<id>.
CREATE TABLE graph_certificates (
    id                uuid PRIMARY KEY,
    thumbprint_sha256 text NOT NULL,
    thumbprint_sha1   text NOT NULL,
    not_after         timestamptz NOT NULL,
    state             text NOT NULL CHECK (state IN ('pending', 'active', 'retired')),
    created_by        uuid REFERENCES users (id),
    created_at        timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX graph_certificates_one_active ON graph_certificates (state) WHERE state = 'active';

-- Bumped by every settings change, so each process can tell its settings are
-- out of date (with NOTIFY pdms_settings, and a periodic check).
CREATE TABLE settings_version (
    id      boolean PRIMARY KEY DEFAULT true CHECK (id),
    version bigint NOT NULL
);
INSERT INTO settings_version (id, version) VALUES (true, 0);

GRANT SELECT, INSERT, UPDATE, DELETE ON app_secrets TO pdms_app;
GRANT SELECT, INSERT, UPDATE ON graph_certificates TO pdms_app;
GRANT SELECT, UPDATE ON settings_version TO pdms_app;
