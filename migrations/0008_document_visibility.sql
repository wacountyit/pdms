-- D-085: one visibility level replaces is_confidential and inherit_acl.
--   department  : department and organization-wide grants apply (the default)
--   county_wide : as department, and every active account may also view
--   restricted  : only document-scope grants and the owner rule apply
CREATE TYPE document_visibility AS ENUM ('department', 'county_wide', 'restricted');

ALTER TABLE documents ADD COLUMN visibility document_visibility NOT NULL DEFAULT 'department';

-- Both old flags had the same effect in the engine (department and global grants
-- stopped applying), which is exactly 'restricted'.
UPDATE documents SET visibility = 'restricted' WHERE is_confidential OR NOT inherit_acl;

ALTER TABLE documents DROP COLUMN inherit_acl, DROP COLUMN is_confidential;

COMMENT ON COLUMN documents.visibility IS
  'department | county_wide (all active accounts may view) | restricted (document grants and owner only); D-085';
