#!/bin/sh
set -eu
# Runs only when initializing an empty PostgreSQL volume.
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<'SQL'
\getenv app_password BIKESNEST_DB_PASSWORD
CREATE ROLE bikesnest_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD :'app_password';
ALTER DATABASE bikesnest OWNER TO bikesnest_app;
CREATE EXTENSION IF NOT EXISTS postgis;
SQL
