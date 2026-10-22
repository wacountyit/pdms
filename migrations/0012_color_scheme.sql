-- UI redesign R2, part 9: each person's color scheme (R2 spec §7). Dark is
-- the default; `system` follows the device's setting.
ALTER TABLE users
    ADD COLUMN color_scheme text NOT NULL DEFAULT 'dark'
        CHECK (color_scheme IN ('dark', 'light', 'system'));
