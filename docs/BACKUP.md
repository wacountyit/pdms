# Backup and restore

PDMS is the system of record for your documents and their audit trail, so it
must be backed up like one. This page says what to protect, how to take a
consistent backup (including with an enterprise backup system), and how to restore and prove the
restore is complete with `verify-integrity`.

## What to protect

| What | Where (Docker install) | Why it matters |
|---|---|---|
| **PostgreSQL database** `pdms` | `pgdata` volume | Every record, version, permission, notification, workflow, appointment, and the hash-chained audit log |
| **File store** | `pdmsdata` volume, `blobs/` (or your S3 bucket) | Every uploaded file and every records request package, stored by SHA-256 |
| **Secrets** | `secrets/` | `pdms_secret_key` encrypts TOTP enrollments and webhook signing secrets, and signs sessions. **Without it**, break-glass accounts must re-enroll and webhook secrets must be rotated. Also the database passwords and the SSO/SMTP secrets |
| **Configuration** | `config/pdms.toml` (and `.env` if you use one) | Organization name, time zone, sign-in, storage, email, integrations |

Not needed: the `spool/` directory (uploads in progress), the `clamdb`
volume (virus definitions, downloaded again), container images (rebuilt), and
the job queue's finished rows (they are inside the database anyway).

**Consistency rule: database first, then files.** PDMS never modifies or
deletes a stored file; files are added before the database rows that point at
them are committed. So a database dump followed by a file copy always contains
every file the dump refers to. (A copy taken in the other order can miss files
uploaded in between, and `verify-integrity` would report them as missing.)

Store backups encrypted and off the server. `config-secrets.tar` contains
passwords and the master key: protect it like a password vault.

## Taking a backup

From the install directory:

```sh
deploy/backup/pdms-backup.sh                  # writes backups/<UTC timestamp>/
deploy/backup/pdms-backup.sh /srv/pdms-backup # or a directory your backup system protects
```

The system keeps running. The script writes:

| File | Contents |
|---|---|
| `pdms.dump` | `pg_dump -Fc` of the `pdms` database (consistent snapshot) |
| `roles.sql` | `pg_dumpall --roles-only` (the `pdms_owner` / `pdms_app` roles; for reference) |
| `pdmsdata.tar` | The file store, copied after the dump (local storage backend only) |
| `config-secrets.tar` | `config/` (including a generated `Caddyfile`), `secrets/`, `.env`, and `docker-compose.override.yml` if present |
| `INFO` | Schema version, last audit sequence number, file count and bytes |
| `SHA256SUMS` | Checksums of all of the above: `sha256sum -c SHA256SUMS` |

Run it daily at least. Keep enough copies for your retention policy; audit
entries and documents are never purged by the application, so every backup is
a complete history up to its time.

### With an enterprise backup system

Two approaches; the first is simplest and always consistent.

1. **File-level backup of a dump directory (recommended).** Register a
   *pre-script* on the backup job for the PDMS server that runs
   `cd /opt/pdms && deploy/backup/pdms-backup.sh /srv/pdms-backup/current`
   (clear or rotate the directory first), and protect `/srv/pdms-backup`.
   Exclude the Docker volume directories (`/var/lib/docker/volumes/pdms_*`)
   from that job so crash-consistent copies of the live database files are not
   mistaken for backups.
2. **VM snapshot.** A crash-consistent VM snapshot is acceptable for PostgreSQL
   (it recovers like after a power loss) *only if* the database and the file
   store are on the same snapshot. Still run the pre-script so you also have a
   portable dump; restore from the dump unless you are restoring the whole VM.

### With S3-compatible storage

If `[storage] backend = "s3"`, the script backs up the database and
configuration only. Protect the bucket with **versioning** (so nothing can be
overwritten or deleted irrecoverably) and replication or your object-store
backup. Take the bucket snapshot *after* the database dump, as above.

### Other options

- `pg_basebackup` with WAL archiving gives point-in-time recovery; restore the
  base backup, replay WAL to the moment you need, then restore a file-store
  copy taken *after* that moment.
- Forward the audit log to your SIEM as it is written (`[audit_forward]`,
  file or syslog). It is not a backup of PDMS, but it is an independent copy of
  every audit entry with its hash, which makes tampering evident even if the
  server and its backups were compromised.

## Restoring

The restore script restores everything from one backup directory and finishes
by running `verify-integrity`:

```sh
deploy/backup/pdms-restore.sh /srv/pdms-backup/2026-10-13T020000Z
```

It:

1. checks `SHA256SUMS`;
2. asks you to type `RESTORE` (add `--yes` for scripted drills);
3. restores `config/` and `secrets/` if they are missing (a rebuilt server);
4. stops the app and worker, recreates the `pdms` database, and `pg_restore`s
   the dump (as the Postgres superuser, keeping owners and grants);
5. replaces the file store with the backup's copy;
6. starts PDMS (the `migrate` service applies any newer migrations if you
   restored into a newer version) and runs `verify-integrity`.

**Rebuilding a server from nothing:** install Docker, copy the PDMS release
(the same version or newer) to the install directory, copy the backup
directory over, then run `deploy/backup/pdms-restore.sh <dir> --yes`. You do
not need to run `install.sh`: the restored `config/` and `secrets/` are used,
and a fresh `pgdata` volume is initialized with the same database passwords.
Keep deployment settings such as `PDMS_HTTP_BIND` in a `.env` file next to
`docker-compose.yml` (Docker Compose reads it automatically, and the backup
includes it) rather than in your shell, so a rebuilt server listens where the
reverse proxy expects.

**Restoring only the database** (for example to undo a mistaken bulk change)
is not recommended: the audit log would then lack the entries recorded after
the backup. Prefer restoring into a separate install to look up what you need.

## Verifying a restore: `verify-integrity`

```sh
docker compose run --rm app verify-integrity            # full check
docker compose run --rm app verify-integrity --skip-files   # presence and size only (fast)
docker compose run --rm app verify-integrity --json
```

It checks that:

- every stored file the database records is present, has the recorded size,
  and still hashes to its recorded SHA-256 (every byte is re-read);
- every records request package is present and intact;
- every document version points at a recorded file;
- the audit hash chain is unbroken from the first entry to the last.

It prints a summary, lists each problem, records the run in the audit log
(`integrity.verified`), and exits with code **2** if anything is wrong, so a
scheduled job or a restore drill fails loudly. Compare its `Audit entries`
and file counts with the backup's `INFO` file: after a restore they match
(plus the entries written by the restore check itself).

## Restore drill (acceptance test 8)

Run a drill at least twice a year, and after any change to storage or backup
configuration. On a test server or a separate Docker project:

```sh
deploy/backup/pdms-backup.sh /tmp/drill          # 1. back up
docker compose down -v                           # 2. destroy the database and files
rm -rf config secrets                            #    ...and the configuration
deploy/backup/pdms-restore.sh /tmp/drill --yes   # 3. restore
```

The drill passes when the restore ends with
`Integrity verified: no problems found.` and you can sign in and open a
document and its file. This exact drill was run against the Docker install
during Phase 4 (see `docs/ARCHITECTURE.md`, section 17).
