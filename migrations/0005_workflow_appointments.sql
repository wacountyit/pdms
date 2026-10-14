-- 0005_workflow_appointments.sql — Phase 3
--
-- Review/approval workflows (including the legal counsel step), signature
-- tracking, positions and appointments with requirement checklists, and the
-- generalization of notifications so appointments and "action needed"
-- notices use the same delivery pipeline as document reminders.

-- ---------------------------------------------------------------------------
-- Approval workflows
-- ---------------------------------------------------------------------------

CREATE TYPE workflow_status AS ENUM ('in_progress', 'approved', 'rejected', 'changes_requested', 'cancelled');
CREATE TYPE step_kind AS ENUM ('review', 'approval', 'legal_review');
CREATE TYPE step_status AS ENUM ('waiting', 'pending', 'approved', 'rejected', 'changes_requested', 'skipped');

-- One round of review for a document. A document has at most one workflow in
-- progress; earlier rounds are kept as history.
CREATE TABLE workflows (
    id              uuid PRIMARY KEY,
    document_id     uuid NOT NULL REFERENCES documents (id),
    version_no      integer NOT NULL,                   -- the version under review
    status          workflow_status NOT NULL DEFAULT 'in_progress',
    previous_status document_status NOT NULL,           -- restored if the round does not end in approval
    submitted_by    uuid REFERENCES users (id),
    submitted_at    timestamptz NOT NULL DEFAULT now(),
    note            text NOT NULL DEFAULT '',
    completed_at    timestamptz
);
CREATE UNIQUE INDEX workflows_one_active ON workflows (document_id) WHERE status = 'in_progress';
CREATE INDEX workflows_document_idx ON workflows (document_id, submitted_at DESC);

-- Steps run in stages: all steps with the lowest unfinished `stage` are
-- pending at once (parallel); later stages wait.
CREATE TABLE workflow_steps (
    id               uuid PRIMARY KEY,
    workflow_id      uuid NOT NULL REFERENCES workflows (id),
    stage            integer NOT NULL CHECK (stage > 0),
    kind             step_kind NOT NULL,
    assignee_user_id uuid REFERENCES users (id),
    -- A role: any holder org-wide or for the document's department may act.
    assignee_role_id uuid REFERENCES roles (id),
    status           step_status NOT NULL DEFAULT 'waiting',
    acted_by         uuid REFERENCES users (id),
    acted_at         timestamptz,
    comment          text NOT NULL DEFAULT '',
    CONSTRAINT workflow_steps_one_assignee CHECK (num_nonnulls(assignee_user_id, assignee_role_id) = 1)
);
CREATE INDEX workflow_steps_workflow_idx ON workflow_steps (workflow_id, stage);
CREATE INDEX workflow_steps_pending_user_idx ON workflow_steps (assignee_user_id) WHERE status = 'pending';
CREATE INDEX workflow_steps_pending_role_idx ON workflow_steps (assignee_role_id) WHERE status = 'pending';

-- Everything that happened in a workflow, for display (the audit log is the record).
CREATE TABLE workflow_events (
    id          uuid PRIMARY KEY,
    workflow_id uuid NOT NULL REFERENCES workflows (id),
    step_id     uuid REFERENCES workflow_steps (id),
    actor_id    uuid REFERENCES users (id),
    action      text NOT NULL,      -- submitted, approved, rejected, changes_requested, commented, cancelled, completed
    comment     text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX workflow_events_workflow_idx ON workflow_events (workflow_id, created_at);

-- ---------------------------------------------------------------------------
-- Signatures (status tracking, not e-signature)
-- ---------------------------------------------------------------------------

CREATE TYPE signature_status AS ENUM ('pending', 'requested', 'signed', 'declined');

CREATE TABLE signatories (
    id                uuid PRIMARY KEY,
    document_id       uuid NOT NULL REFERENCES documents (id),
    sort_order        integer NOT NULL DEFAULT 0,
    name              text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    title             text NOT NULL DEFAULT '',          -- e.g. "Board Chair", "Managing Director"
    organization      text NOT NULL DEFAULT '',          -- e.g. the other party to a partnership agreement
    email             text NOT NULL DEFAULT '',
    user_id           uuid REFERENCES users (id),        -- set when the signatory is a user here
    status            signature_status NOT NULL DEFAULT 'pending',
    requested_at      timestamptz,
    requested_by      uuid REFERENCES users (id),
    signed_on         date,
    recorded_by       uuid REFERENCES users (id),
    recorded_at       timestamptz,
    signed_version_no integer,                           -- version holding the signed copy
    provider          text NOT NULL DEFAULT 'manual',
    provider_ref      text,
    note              text NOT NULL DEFAULT '',
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX signatories_document_idx ON signatories (document_id, sort_order);

-- ---------------------------------------------------------------------------
-- Positions and appointments
-- ---------------------------------------------------------------------------

CREATE TYPE appointment_status AS ENUM ('active', 'ended');

-- A seat on a board, committee, or commission, or an appointed role.
CREATE TABLE positions (
    id                      uuid PRIMARY KEY,
    name                    text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),   -- e.g. "Seat 3"
    body_name               text NOT NULL CHECK (length(body_name) BETWEEN 1 AND 200), -- e.g. "Advisory Committee"
    department_id           uuid REFERENCES departments (id),
    appointing_authority    text NOT NULL DEFAULT '',   -- e.g. "Board of Directors"
    term_months             integer CHECK (term_months IS NULL OR term_months > 0),
    seats                   integer NOT NULL DEFAULT 1 CHECK (seats > 0),
    manager_user_id         uuid REFERENCES users (id), -- staff responsible for this position
    reappointment_lead_days integer NOT NULL DEFAULT 90 CHECK (reappointment_lead_days >= 0),
    description             text NOT NULL DEFAULT '',
    is_active               boolean NOT NULL DEFAULT true,
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now()
);

-- The checklist every appointment to a position must satisfy.
CREATE TABLE position_requirements (
    id          uuid PRIMARY KEY,
    position_id uuid NOT NULL REFERENCES positions (id),
    label       text NOT NULL CHECK (length(label) BETWEEN 1 AND 200),  -- e.g. "Acceptance letter on file"
    description text NOT NULL DEFAULT '',
    sort_order  integer NOT NULL DEFAULT 0,
    is_active   boolean NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX position_requirements_position_idx ON position_requirements (position_id, sort_order);

CREATE TABLE appointments (
    id                     uuid PRIMARY KEY,
    position_id            uuid NOT NULL REFERENCES positions (id),
    person_name            text NOT NULL CHECK (length(person_name) BETWEEN 1 AND 200),
    user_id                uuid REFERENCES users (id),
    contact_email          text NOT NULL DEFAULT '',
    appointed_by           text NOT NULL DEFAULT '',     -- authority that made this appointment
    appointment_reference  text NOT NULL DEFAULT '',     -- e.g. resolution or meeting reference
    start_date             date NOT NULL,
    end_date               date,                          -- term end; NULL = indefinite
    reappointment_due_date date,                          -- when a reappointment or replacement decision is due
    status                 appointment_status NOT NULL DEFAULT 'active',
    ended_on               date,
    end_reason             text NOT NULL DEFAULT '',      -- reappointed, replaced, resigned, removed, term ended
    predecessor_id         uuid REFERENCES appointments (id),
    notes                  text NOT NULL DEFAULT '',
    created_by             uuid REFERENCES users (id),
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    CHECK (end_date IS NULL OR end_date >= start_date)
);
CREATE INDEX appointments_position_idx ON appointments (position_id, start_date DESC);
CREATE INDEX appointments_active_idx ON appointments (end_date) WHERE status = 'active';

CREATE TABLE appointment_requirement_status (
    appointment_id uuid NOT NULL REFERENCES appointments (id),
    requirement_id uuid NOT NULL REFERENCES position_requirements (id),
    satisfied      boolean NOT NULL DEFAULT false,
    satisfied_by   uuid REFERENCES users (id),
    satisfied_at   timestamptz,
    note           text NOT NULL DEFAULT '',
    PRIMARY KEY (appointment_id, requirement_id)
);

-- ---------------------------------------------------------------------------
-- Notifications: appointments and "action needed" notices
-- ---------------------------------------------------------------------------

CREATE TYPE notification_kind AS ENUM ('reminder', 'action');

ALTER TABLE notification_rules
    ADD COLUMN position_id uuid REFERENCES positions (id),
    DROP CONSTRAINT notification_rules_one_scope,
    ADD CONSTRAINT notification_rules_one_scope CHECK (num_nonnulls(document_type_id, document_id, position_id) <= 1);

ALTER TABLE notifications
    ADD COLUMN kind notification_kind NOT NULL DEFAULT 'reminder',
    ADD COLUMN appointment_id uuid REFERENCES appointments (id),
    ADD COLUMN subject text,
    ADD COLUMN body text,
    ADD COLUMN link text,
    ADD COLUMN dedupe_key text UNIQUE,
    ALTER COLUMN document_id DROP NOT NULL,
    ALTER COLUMN rule_id DROP NOT NULL,
    ALTER COLUMN event DROP NOT NULL,
    ALTER COLUMN due_date DROP NOT NULL,
    ALTER COLUMN trigger DROP NOT NULL,
    ALTER COLUMN offset_days DROP NOT NULL,
    ADD COLUMN target_id uuid GENERATED ALWAYS AS (coalesce(document_id, appointment_id)) STORED,
    DROP CONSTRAINT notifications_key,
    ADD CONSTRAINT notifications_key UNIQUE (target_id, rule_id, event, due_date, trigger, offset_days),
    ADD CONSTRAINT notifications_shape CHECK (
           (kind = 'reminder' AND num_nonnulls(document_id, appointment_id) = 1
                AND num_nonnulls(rule_id, event, due_date, trigger, offset_days) = 5)
        OR (kind = 'action' AND subject IS NOT NULL AND link IS NOT NULL));
CREATE INDEX notifications_appointment_idx ON notifications (appointment_id) WHERE appointment_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Seed data
-- ---------------------------------------------------------------------------

INSERT INTO roles (id, key, name, description, is_system) VALUES
    (gen_random_uuid(), 'appointments_clerk', 'Appointments Clerk',
     'Manages positions, appointments, and requirement checklists.', true);

-- Legal counsel reviews documents from every department, so holders of the
-- role may view non-confidential documents org-wide (confidential documents
-- still need a document grant). The role has no holders until an
-- administrator assigns it.
INSERT INTO acl_entries (id, subject_role_id, scope, permission, reason)
SELECT gen_random_uuid(), id, 'global', 'view', 'seeded default for legal counsel review'
  FROM roles WHERE key = 'legal_counsel';

-- Department heads are the default approvers (see 0001): a head assigned for
-- a department may approve that department's non-confidential documents; an
-- org-wide holder may approve any.
INSERT INTO acl_entries (id, subject_role_id, scope, permission, reason)
SELECT gen_random_uuid(), id, 'global', 'approve', 'seeded default: department heads approve'
  FROM roles WHERE key = 'department_head';

-- Organization defaults for appointments: remind the responsible staff member
-- (the position's "owner") and the appointments clerks.
WITH r AS (
    INSERT INTO notification_rules (id, event, offsets_days, overdue_repeat_days, channels)
    VALUES (gen_random_uuid(), 'term_end',      '{90,30,7,0}', NULL, '{email,in_app}'),
           (gen_random_uuid(), 'reappointment', '{60,30,7,0}', 7,    '{email,in_app}')
    RETURNING id
)
INSERT INTO notification_rule_recipients (id, rule_id, kind, role_id)
SELECT gen_random_uuid(), r.id, k.kind, k.role_id
  FROM r
 CROSS JOIN (VALUES ('owner'::recipient_kind, NULL::uuid),
                    ('role'::recipient_kind, (SELECT id FROM roles WHERE key = 'appointments_clerk'))) AS k (kind, role_id);

-- ---------------------------------------------------------------------------
-- Privileges
-- ---------------------------------------------------------------------------

GRANT SELECT, INSERT, UPDATE ON workflows, workflow_steps, signatories, positions, position_requirements,
                                appointments, appointment_requirement_status TO pdms_app;
GRANT SELECT, INSERT ON workflow_events TO pdms_app;
-- Signers who were never asked may be removed (audited); everything else is kept.
GRANT DELETE ON signatories TO pdms_app;
