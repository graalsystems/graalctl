# Changelog

## 1.0.2

Fixes found by running graalctl against a real graal installation.

- `jobs schedule` sends the schedule object the API expects
  (`{"type": "cron", "cron_expression": "..."}`); 1.0.1 sent the bare cron string and the API
  answered 500. The `SCHEDULE` column of `jobs list` and `workflows list` shows the cron
  expression, or `once` for a job without schedule.
- `jobs patch --set` uses the JSON Patch `add` operation, which sets a field whether the API
  returned it or not; `replace` was refused on fields the API leaves out when empty.

## 1.0.1

- `jobs run`, `runs get`, `runs wait` and `runs logs --follow` use the
  `application/vnd.graal.systems.v1.jobrun+json` media type that the API routes require;
  1.0.0 sent `run+json` and starting a run was refused.

## 1.0.0

Complete rewrite in Rust of the former Java (GraalVM native image) CLI.

- Single static binary of about 3 MB (was 63 MB) that starts in milliseconds.
- Linux (x86_64, ARM64), macOS (Intel, Apple silicon) and Windows builds, with SHA-256
  checksums and an install script.
- Contexts for several installations and tenants (`graalctl config ...`).
- Logins: password, OAuth 2.0 device flow (SSO), application credentials, existing token;
  automatic token refresh; credentials stored with owner-only permissions.
- Commands for projects, jobs, runs, logs, workflows, schedules, libraries, infrastructures,
  instance types, identities and secrets (names only), plus raw API access (`graalctl api`).
- `--wait` and `--follow` for CI: the exit code reflects the outcome of the run.
- JSON Patch edits, vendor media types and `X-Tenant` header as documented in the REST API.
- Custom CA bundles and proxies from the environment.
