-- UI redesign R1: branding assets (the seal) and usernames for local accounts.
-- Name and accent live in app_settings (keys branding.org_name, branding.accent).

CREATE TABLE branding_assets (
    name         text PRIMARY KEY CHECK (name IN ('seal')),
    content_type text NOT NULL CHECK (content_type IN ('image/png', 'image/jpeg')),
    bytes        bytea NOT NULL CHECK (octet_length(bytes) <= 524288),
    sha256       text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    width        integer NOT NULL CHECK (width BETWEEN 16 AND 2048),
    height       integer NOT NULL CHECK (height BETWEEN 16 AND 2048),
    updated_by   uuid REFERENCES users (id),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE users ADD COLUMN username text
    CHECK (username ~ '^[A-Za-z0-9._-]{2,64}$');
CREATE UNIQUE INDEX users_username_key ON users (lower(username))
    WHERE username IS NOT NULL;

GRANT SELECT, INSERT, UPDATE, DELETE ON branding_assets TO pdms_app;
-- Clearing a branding value (or resetting) removes its settings row.
GRANT DELETE ON app_settings TO pdms_app;
