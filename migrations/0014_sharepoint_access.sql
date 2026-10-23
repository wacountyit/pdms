-- SharePoint folder access, changes made outside PDMS, and the status panel
-- (spec §7.4, §7.6; plan P4).

-- Read access PDMS granted, one person at a time, on Departments/<department>/.
CREATE TABLE sharepoint_folder_access (
    department_id uuid NOT NULL REFERENCES departments (id),
    user_id       uuid NOT NULL REFERENCES users (id),
    permission_id text NOT NULL,                 -- Graph permission id, for removal
    granted_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (department_id, user_id)
);

-- Permissions on a department folder that PDMS did not grant: reported, never changed.
CREATE TABLE sharepoint_foreign_grants (
    department_id uuid NOT NULL REFERENCES departments (id),
    permission_id text NOT NULL,
    granted_to    text NOT NULL,
    roles         text NOT NULL,
    seen_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (department_id, permission_id)
);

-- Who changed a tracked file outside PDMS, and when it was noticed.
ALTER TABLE sharepoint_files
    ADD COLUMN changed_by text,
    ADD COLUMN changed_at timestamptz;

-- Files in the library that no document version points at.
CREATE TABLE sharepoint_unexpected_items (
    item_id      text PRIMARY KEY,
    name         text NOT NULL,
    path         text NOT NULL,
    created_by   text NOT NULL,
    by_pdms      boolean NOT NULL,               -- left behind by a PDMS save that did not finish
    first_seen   timestamptz NOT NULL DEFAULT now(),
    last_seen    timestamptz NOT NULL DEFAULT now(),
    dismissed_by uuid REFERENCES users (id),
    dismissed_at timestamptz
);

GRANT SELECT, INSERT, DELETE ON sharepoint_folder_access, sharepoint_foreign_grants TO pdms_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON sharepoint_unexpected_items TO pdms_app;
GRANT SELECT, INSERT, UPDATE ON sharepoint_files TO pdms_app;
