-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Copyright (C) 2026 PDMS contributors
--
-- Prints the ids.json that shoot.py reads: the ids of a few records from the
-- demo data (`pdms seed-demo`). Run it against the database the local
-- instance uses, e.g.
--   docker exec -i pdms-dev-db psql -U postgres -d pdms -tA < tools/ui-screenshots/ids.sql > tools/ui-screenshots/ids.json
-- If a value comes out null, the demo data changed: pick another record of
-- the same kind (any document, user, position, ...) and adjust the query.
SELECT json_build_object(
  'user',     (SELECT id FROM users WHERE email = 'alice@example.org'),
  'dept',     (SELECT id FROM departments WHERE name = 'Operations'),
  'group',    (SELECT id FROM groups WHERE name = 'Contracts Review Committee'),
  'doctype',  (SELECT id FROM document_types WHERE name = 'Policy'),
  'field',    (SELECT id FROM custom_field_definitions WHERE key = 'contract_amount' ORDER BY created_at LIMIT 1),
  'position', (SELECT id FROM positions WHERE name = 'Seat 1' AND body_name = 'Advisory Committee'),
  'doc',      (SELECT id FROM documents WHERE title = 'Asphalt Supply Contract FY2026'),
  -- The demo's agreement with history and missed review cycles (shown overdue).
  'doc2',     (SELECT id FROM documents WHERE title LIKE '%Facilities Maintenance with Example Partner' ORDER BY created_at LIMIT 1)
);
