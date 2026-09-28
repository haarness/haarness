---
name: haarness-r2-storage
description: Provision and validate Haarness Cloud private R2 storage, environment-specific credentials, marketplace and profile archives, releases, snapshots, exports, and cleanup.
---

# Haarness R2 storage

Use the active Cloud checkout beside the native repository. Discover all current `ObjectStorage` consumers
before choosing coverage: marketplace artifacts, profile archives, release artifacts, local-provider
snapshots, and workspace exports currently use it. Live workspace files are on the workspace volume;
uploading a file in the explorer alone does not test R2. Fly native snapshots remain Fly resources.
Explore relevant undocumented scenarios too; this skill is a starting point, not a coverage ceiling.

## Provision and configure

Use separate private `haarness-cloud-dev` and `haarness-cloud-prod` buckets. For the requested US East
preference, select Eastern North America (`enam`) at creation and read back Settings → Location. Automatic
placement follows the caller and may choose Asia Pacific from Japan. A location hint is best effort, not an
Ashburn facility guarantee. Do not delete/recreate a bucket to change its location; Cloudflare remembers
placement for the original name. See [data location](https://developers.cloudflare.com/r2/reference/data-location/).

Keep public development URLs and custom public domains disabled. Use a separate **Object Read & Write**
credential restricted to each bucket, rather than the shared account administrator credential. Account
service tokens survive membership changes; distinguish them from personal tokens. Use the browser tool's
confirmation policy when creating persistent credentials. Capture one-time credentials directly into
private local configuration without printing them or saving screenshots containing them.

Set these only in a private environment file or deployment secret store:

- `HAARNESS_CLOUD_S3_ENDPOINT=https://<ACCOUNT_ID>.r2.cloudflarestorage.com`
- `HAARNESS_CLOUD_S3_PUBLIC_ENDPOINT` equal to that endpoint (presigning also uses S3, not an r2.dev domain)
- `HAARNESS_CLOUD_S3_BUCKET` for the selected environment
- `HAARNESS_CLOUD_S3_REGION=auto`, `HAARNESS_CLOUD_S3_PATH_STYLE=true`
- `HAARNESS_CLOUD_S3_ACCESS_KEY`, `HAARNESS_CLOUD_S3_SECRET_KEY`

Do not put production credentials in the development environment. Keep private files mode 0600 and out
of Git; do not print expanded Compose configuration. Do not enable wildcard CORS speculatively: current
web uploads go through Haarness's API. Add narrowly scoped CORS only if an actual browser consumer needs it.
Keep the default rule aborting abandoned multipart uploads after seven days.

## Real storage acceptance

Explicitly load only the intended S3 configuration into the test process. The tests below are ignored by
default and require real R2; absence of credentials must not silently select the in-memory fixture.
Use a disposable database through `HAARNESS_CLOUD_TEST_DATABASE_URL` for API tests and reuse the build cache.

```sh
cargo test -p cloud-integrations --test object_store_s3 live_r2_integrity_and_privacy -- --ignored --exact --nocapture
cargo test -p cloud-integrations --test local_restore live_r2_snapshot_restore_and_export -- --ignored --exact --nocapture
cargo test -p cloud-api --test marketplace_routes live_r2_marketplace_and_profiles -- --ignored --exact --nocapture
cargo test -p cloud-api --test releases live_r2_release_lifecycle -- --ignored --exact --nocapture
```

`tests/support/live_r2.rs` puts each run beneath its own `tests/<uuid>/` prefix, tracks attempted writes,
and deletes only those objects even after an assertion panic. Require cleanup output and zero exit status.
An externally killed process cannot run that cleanup: record and inspect only its exact prefix before
removing stranded test objects. Never empty a shared bucket or broadly delete all `tests/` objects.

Coverage includes content type and byte integrity, anonymous denial, tampered/expired signed URL denial,
6 MiB multipart round-trip, interrupted stream handling, idempotent deletion, corrupt restore preserving
live data, valid snapshot restore, export checksums and extracted content, marketplace signed downloads,
profile tenant isolation, release checksum rejection, immutable publication, rollback and withdrawn releases.
The API scenarios use an isolated database, so they do not publish releases to the production application.

After provisioning a scoped credential, set `HAARNESS_R2_FORBIDDEN_BUCKET` to the *other Haarness environment*
and run this dedicated check. It must receive an authorization failure for read/write there, while the
positive tests above must succeed in the intended bucket. Do not target another application's bucket.

```sh
cargo test -p cloud-integrations --test object_store_s3 live_r2_bucket_isolation -- --ignored --exact --nocapture
```

Repeat in both directions when both keys exist. A passing shared-admin functional test is not credential
isolation proof. Production testing should use only synthetic objects under a unique prefix and clean them
up immediately. Do not infer successful deployment from a local API test against the production bucket.

## Chrome upload acceptance

The optional `live_r2_browser_fixture` test in `marketplace_routes.rs` runs the real API on 127.0.0.1:19188
with an isolated database and namespaced real R2. Set `HAARNESS_R2_TEST_PASSWORD` to a synthetic password
(at least 15 characters) and `HAARNESS_R2_TEST_STOP_FILE` to an absent private path. It seeds the disposable
account `r2-browser@fixture.invalid`. Start the frontend with API origin 19188 and Vite port 19189.

```sh
cargo test -p cloud-api --test marketplace_routes live_r2_browser_fixture -- --ignored --exact --nocapture
HAARNESS_CLOUD_API_ORIGIN=http://127.0.0.1:19188 npm --prefix web run dev -- --port 19189
```

Sign in with the already created synthetic credential. In Marketplace → Publish a resource, choose plugin,
private visibility, and a ZIP containing `.claude-plugin/plugin.json` and a command Markdown file. Upload
through the browser file chooser and submit. Verify the visible submission status and actual stored bytes.
A worker is not started by this fixture: submission acceptance proves upload/queueing, not completed catalog
validation or installation. Use the marketplace lifecycle skill/tests for that additional surface.

Chrome's extension requires “Allow access to file URLs” for the chooser. If missing, follow the browser
troubleshooting guidance and report the UI-upload limitation; a programmatic upload is not browser proof.
Create the stop file to finish: the fixture downloads stored artifacts, checks SHA-256, removes its owned
objects, and drops the database schema. Verify zero exit and schema absence, then stop the owned frontend.
A 45-minute timeout prevents an abandoned fixture from running indefinitely.

## Regression checks and reporting

Run normal `marketplace_routes`, `releases`, `local_restore`, and storage/configuration tests, plus focused
Clippy and formatting for changed files. Use `--test-threads=4` for schema-heavy PostgreSQL suites on the
small local database; higher concurrency can exhaust `max_locks_per_transaction`.
Report bucket placement/privacy, key scopes, actual API/storage tests, Chrome proof, and cleanup separately.
List blocked or unverified checks explicitly; never turn fixture-only success into live cloud proof.
