-- Database bootstrap, run once by a superuser before `pdms migrate`.
-- install.sh runs this with generated passwords:
--   psql -v owner_password=... -v app_password=... -f 00_roles.sql
--
-- pdms_owner owns the schema and runs migrations. pdms_app is what the server
-- and worker connect as; it owns nothing, so it cannot ALTER tables, disable
-- triggers, or grant itself privileges.

CREATE ROLE pdms_owner LOGIN PASSWORD :'owner_password';
CREATE ROLE pdms_app   LOGIN PASSWORD :'app_password' NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT;

CREATE DATABASE pdms OWNER pdms_owner ENCODING 'UTF8' TEMPLATE template0;
REVOKE ALL ON DATABASE pdms FROM PUBLIC;
GRANT CONNECT ON DATABASE pdms TO pdms_app;

\connect pdms
ALTER SCHEMA public OWNER TO pdms_owner;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
