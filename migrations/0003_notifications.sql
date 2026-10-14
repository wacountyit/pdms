-- 0003_notifications.sql — Phase 2
--
-- Review and renewal tasks, notification rules (org default, per document
-- type, per document), materialized notifications, per-recipient deliveries
-- with an attempt log, and revocable calendar (ICS) feeds.

CREATE TYPE due_event AS ENUM ('review', 'renewal', 'expiration');
CREATE TYPE task_status AS ENUM ('open', 'initiated', 'completed', 'approved', 'overdue', 'cancelled');
CREATE TYPE notification_trigger AS ENUM ('advance', 'due', 'overdue');
CREATE TYPE delivery_channel AS ENUM ('email', 'in_app');
CREATE TYPE delivery_status AS ENUM ('pending', 'sending', 'sent', 'failed', 'skipped');
CREATE TYPE recipient_kind AS ENUM (
    'owner', 'document_department', 'user', 'group', 'department', 'role');

-- ---------------------------------------------------------------------------
-- Review and renewal tasks
-- ---------------------------------------------------------------------------

-- One task per document, kind, and due date. Created by the planner when a
-- due date comes within the lead window (or has passed).
CREATE TABLE review_tasks (
    id             uuid PRIMARY KEY,
    document_id    uuid NOT NULL REFERENCES documents (id),
    kind           due_event NOT NULL CHECK (kind IN ('review', 'renewal')),
    due_date       date NOT NULL,
    status         task_status NOT NULL DEFAULT 'open',
    initiated_by   uuid REFERENCES users (id),
    initiated_at   timestamptz,
    completed_by   uuid REFERENCES users (id),
    completed_at   timestamptz,
    completed_on   date,                       -- the date the review/renewal actually happened
    approved_by    uuid REFERENCES users (id),
    approved_at    timestamptz,
    outcome_note   text NOT NULL DEFAULT '',
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (document_id, kind, due_date)
);
CREATE INDEX review_tasks_open_idx ON review_tasks (due_date)
    WHERE status IN ('open', 'initiated', 'overdue');

-- ---------------------------------------------------------------------------
-- Notification rules
-- ---------------------------------------------------------------------------
-- Precedence for a document and event: the document's own rules if it has any
-- for that event, otherwise its type's, otherwise the organization defaults.

CREATE TABLE notification_rules (
    id                  uuid PRIMARY KEY,
    document_type_id    uuid REFERENCES document_types (id),
    document_id         uuid REFERENCES documents (id),
    event               due_event NOT NULL,
    -- Days before the due date to notify; 0 = on the due date.
    offsets_days        integer[] NOT NULL CHECK (cardinality(offsets_days) BETWEEN 1 AND 20),
    -- Repeat every N days after the due date while unresolved; NULL = no repeats.
    overdue_repeat_days integer CHECK (overdue_repeat_days IS NULL OR overdue_repeat_days BETWEEN 1 AND 365),
    channels            delivery_channel[] NOT NULL CHECK (cardinality(channels) >= 1),
    is_active           boolean NOT NULL DEFAULT true,
    created_by          uuid REFERENCES users (id),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT notification_rules_one_scope CHECK (document_type_id IS NULL OR document_id IS NULL)
);
CREATE INDEX notification_rules_doc_idx ON notification_rules (document_id) WHERE document_id IS NOT NULL;
CREATE INDEX notification_rules_type_idx ON notification_rules (document_type_id) WHERE document_type_id IS NOT NULL;

CREATE TABLE notification_rule_recipients (
    id            uuid PRIMARY KEY,
    rule_id       uuid NOT NULL REFERENCES notification_rules (id) ON DELETE CASCADE,
    kind          recipient_kind NOT NULL,
    user_id       uuid REFERENCES users (id),
    group_id      uuid REFERENCES groups (id) ON DELETE CASCADE,
    department_id uuid REFERENCES departments (id),
    role_id       uuid REFERENCES roles (id),
    CONSTRAINT recipient_target CHECK (
           (kind IN ('owner', 'document_department') AND num_nonnulls(user_id, group_id, department_id, role_id) = 0)
        OR (kind = 'user'       AND user_id IS NOT NULL       AND num_nonnulls(group_id, department_id, role_id) = 0)
        OR (kind = 'group'      AND group_id IS NOT NULL      AND num_nonnulls(user_id, department_id, role_id) = 0)
        OR (kind = 'department' AND department_id IS NOT NULL AND num_nonnulls(user_id, group_id, role_id) = 0)
        -- role: holders org-wide or for the document's department
        OR (kind = 'role'       AND role_id IS NOT NULL       AND num_nonnulls(user_id, group_id, department_id) = 0))
);
CREATE INDEX notification_rule_recipients_rule_idx ON notification_rule_recipients (rule_id);

-- ---------------------------------------------------------------------------
-- Notifications and deliveries
-- ---------------------------------------------------------------------------

-- One row per (document, rule, event, due date, trigger, offset). The unique
-- key makes planning idempotent: re-running after a crash creates nothing new.
CREATE TABLE notifications (
    id            uuid PRIMARY KEY,
    document_id   uuid NOT NULL REFERENCES documents (id),
    rule_id       uuid NOT NULL REFERENCES notification_rules (id),
    event         due_event NOT NULL,
    due_date      date NOT NULL,
    trigger       notification_trigger NOT NULL,
    -- advance: days before due; due: 0; overdue: days after due.
    offset_days   integer NOT NULL,
    scheduled_for date NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT notifications_key UNIQUE (document_id, rule_id, event, due_date, trigger, offset_days)
);
CREATE INDEX notifications_document_idx ON notifications (document_id, due_date);

CREATE TABLE notification_deliveries (
    id              uuid PRIMARY KEY,
    notification_id uuid NOT NULL REFERENCES notifications (id),
    user_id         uuid NOT NULL REFERENCES users (id),
    channel         delivery_channel NOT NULL,
    status          delivery_status NOT NULL DEFAULT 'pending',
    attempts        integer NOT NULL DEFAULT 0,
    last_error      text,
    skip_reason     text,
    message_id      text,                 -- deterministic email Message-ID
    sent_at         timestamptz,
    read_at         timestamptz,          -- in-app: opened
    acknowledged_at timestamptz,          -- in-app: acknowledged
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT notification_deliveries_key UNIQUE (notification_id, user_id, channel)
);
CREATE INDEX notification_deliveries_inbox_idx ON notification_deliveries (user_id, created_at DESC)
    WHERE channel = 'in_app';
CREATE INDEX notification_deliveries_status_idx ON notification_deliveries (status) WHERE status IN ('pending', 'sending', 'failed');

-- Every send attempt, kept for troubleshooting.
CREATE TABLE notification_delivery_attempts (
    id          uuid PRIMARY KEY,
    delivery_id uuid NOT NULL REFERENCES notification_deliveries (id),
    attempt_no  integer NOT NULL,
    worker      text NOT NULL,
    started_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    outcome     text,                     -- sent | failed | skipped
    error       text
);
CREATE INDEX notification_delivery_attempts_idx ON notification_delivery_attempts (delivery_id, attempt_no);

-- ---------------------------------------------------------------------------
-- Calendar feeds
-- ---------------------------------------------------------------------------

-- A subscribable ICS URL. The feed is rendered with its owner's current
-- permissions at fetch time, so revoked access disappears from the feed too.
CREATE TABLE calendar_feeds (
    id              uuid PRIMARY KEY,
    user_id         uuid NOT NULL REFERENCES users (id),
    name            text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    department_id   uuid REFERENCES departments (id),   -- NULL = everything the user can see
    only_owned      boolean NOT NULL DEFAULT false,
    token_hash      bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    created_at      timestamptz NOT NULL DEFAULT now(),
    revoked_at      timestamptz,
    last_used_at    timestamptz,
    last_audited_at timestamptz
);
CREATE INDEX calendar_feeds_user_idx ON calendar_feeds (user_id);

-- ---------------------------------------------------------------------------
-- Seed: organization-wide default rules (see DECISIONS: reminder defaults)
-- ---------------------------------------------------------------------------

WITH r AS (
    INSERT INTO notification_rules (id, event, offsets_days, overdue_repeat_days, channels)
    VALUES (gen_random_uuid(), 'review',     '{90,30,7,0}', 7,    '{email,in_app}'),
           (gen_random_uuid(), 'renewal',    '{90,30,7,0}', 7,    '{email,in_app}'),
           (gen_random_uuid(), 'expiration', '{90,30,7,0}', NULL, '{email,in_app}')
    RETURNING id
)
INSERT INTO notification_rule_recipients (id, rule_id, kind, role_id)
SELECT gen_random_uuid(), r.id, k.kind, k.role_id
  FROM r
 CROSS JOIN (VALUES ('owner'::recipient_kind, NULL::uuid),
                    ('role'::recipient_kind, (SELECT id FROM roles WHERE key = 'department_head'))) AS k (kind, role_id);

-- ---------------------------------------------------------------------------
-- Privileges
-- ---------------------------------------------------------------------------

GRANT SELECT, INSERT, UPDATE ON review_tasks, notification_deliveries, calendar_feeds TO pdms_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON notification_rules, notification_rule_recipients TO pdms_app;
GRANT SELECT, INSERT ON notifications TO pdms_app;
GRANT SELECT, INSERT, UPDATE ON notification_delivery_attempts TO pdms_app;
