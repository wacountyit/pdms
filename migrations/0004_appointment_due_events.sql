-- 0004_appointment_due_events.sql — Phase 3
--
-- New due events for appointments. Postgres does not allow a new enum value to
-- be used in the transaction that adds it, so these live in their own
-- migration ahead of 0005, which uses them.

ALTER TYPE due_event ADD VALUE 'term_end';
ALTER TYPE due_event ADD VALUE 'reappointment';
