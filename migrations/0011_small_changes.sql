-- Sub-project 3, small changes (D-087, D-088), built with UI redesign R2.

-- D-088: the seeded "Partnership Agreement" type becomes Interlocal Agreement
-- (ILA). Documents keep the IDs they have; new ones are numbered ILA-....
-- A name or prefix an administrator already changed is left as it is.
UPDATE document_types SET key = 'interlocal_agreement'
 WHERE key = 'partnership_agreement';
UPDATE document_types
   SET name = 'Interlocal Agreement (ILA)',
       description = 'Agreements with other public agencies.'
 WHERE key = 'interlocal_agreement' AND name = 'Partnership Agreement';
UPDATE document_types SET id_prefix = 'ILA'
 WHERE key = 'interlocal_agreement' AND id_prefix = 'PAG'
   AND NOT EXISTS (SELECT 1 FROM document_types WHERE id_prefix = 'ILA');

-- D-087: the Outlook calendar sync is removed. Its queued runs and its links
-- to Outlook events go with it. Events it already created in Outlook stay
-- there until someone deletes them (see the administrator's guide).
DELETE FROM jobs WHERE kind = 'calendar.graph_sync';
DROP TABLE graph_calendar_links;
