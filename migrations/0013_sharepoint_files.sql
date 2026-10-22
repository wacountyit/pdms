-- SharePoint document storage (spec §5–§7, plan P3). Files live in one
-- SharePoint library; these tables remember where, by Graph item ID, so
-- renames and moves never depend on paths.

-- One folder per department inside each area folder.
CREATE TABLE sharepoint_department_folders (
    area          text NOT NULL CHECK (area IN ('departments', 'county_wide', 'restricted')),
    department_id uuid NOT NULL REFERENCES departments (id),
    item_id       text NOT NULL UNIQUE,
    name          text NOT NULL,
    PRIMARY KEY (area, department_id)
);

-- One folder per document; it moves between department folders.
CREATE TABLE sharepoint_document_folders (
    document_id    uuid PRIMARY KEY REFERENCES documents (id),
    item_id        text NOT NULL UNIQUE,
    parent_item_id text NOT NULL,
    name           text NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

-- One file per version that introduced a file. Never deleted.
CREATE TABLE sharepoint_files (
    document_version_id uuid PRIMARY KEY REFERENCES document_versions (id),
    document_id         uuid NOT NULL REFERENCES documents (id),
    drive_id            text NOT NULL,
    item_id             text NOT NULL UNIQUE,
    list_item_id        text NOT NULL,
    name                text NOT NULL,
    quick_xor_hash      text NOT NULL,          -- recorded at upload, compared by change detection (P4)
    size                bigint NOT NULL CHECK (size >= 0),
    last_known_path     text NOT NULL,
    fields_hash         text,                   -- SHA-256 of the column values last written
    state               text NOT NULL DEFAULT 'ok'
                        CHECK (state IN ('ok', 'changed_outside', 'missing')),
    uploaded_at         timestamptz NOT NULL DEFAULT now(),
    checked_at          timestamptz
);
CREATE INDEX sharepoint_files_document_idx ON sharepoint_files (document_id);

GRANT SELECT, INSERT, UPDATE ON sharepoint_department_folders, sharepoint_document_folders,
                                sharepoint_files TO pdms_app;
