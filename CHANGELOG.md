# Changelog

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
