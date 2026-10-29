-- Entra ID sync limited to one group (GS): which departed accounts the sync
-- may bring back.

ALTER TABLE users
    ADD COLUMN departed_from_group boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN users.departed_from_group IS
    'set when the Entra sync departs someone for leaving the PDMS group (GS); the sync reactivates only these. Any other status change clears it.';
