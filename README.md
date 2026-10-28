# graalctl

`graalctl` is the command-line interface for the [graal Data Platform](https://graal.systems/en/docs).
It drives a graal installation through its REST API (`/api/v1`): projects, jobs, runs, logs,
workflows, schedules, libraries and more — from a terminal or a CI pipeline.

- **Fast and small**: a single static binary of about 3 MB, written in Rust, that starts in a few
  milliseconds (the previous Java-based binary weighed 63 MB).
- **Linux, macOS and Windows**, x86_64 and ARM64.
- **Built for automation**: `--wait` makes the exit code reflect the outcome of a run, `-o json`
  and `-o id` give machine-readable output, credentials can come from environment variables.
- **Works with self-hosted installations**: any OpenID Connect (Keycloak) server, custom CA
  bundles, HTTP(S) proxies from the environment.

## Install

### Linux and macOS

```sh
curl -fsSL https://raw.githubusercontent.com/graalsystems/graalctl/main/install.sh | sh
```

The script downloads the latest release for your platform, checks its SHA-256 checksum and
installs it in `/usr/local/bin` (or `~/.local/bin` when `/usr/local/bin` is not writable).
Set `GRAALCTL_VERSION=v1.0.0` to pin a version or `GRAALCTL_INSTALL_DIR` to choose the target
directory.

### Windows

Download `graalctl-windows-x86_64.exe` from the
[releases page](https://github.com/graalsystems/graalctl/releases), rename it to `graalctl.exe`
and put it in a directory listed in your `PATH`.

### From source

```sh
cargo install --git https://github.com/graalsystems/graalctl
```

## Get started

1. Create a context. It tells `graalctl` which installation and tenant to talk to:

   ```sh
   graalctl config set-context prod \
     --api-url https://graal.example.com/api/v1 \
     --auth-url https://sso.example.com \
     --tenant acme
   ```

   `--auth-url` is the base URL of the installation's OpenID Connect server (Keycloak), without
   `/realms/...`. The realm is discovered from the tenant; set it with `--realm` if needed.

2. Log in:

   ```sh
   graalctl login                 # username and password (local accounts)
   graalctl login --device        # SSO: open a URL in any browser and enter a code
   echo "$SECRET" | graalctl login --client-id my-app --client-secret-stdin   # application
   ```

   The device flow needs an OIDC client with the *OAuth 2.0 Device Authorization Grant* enabled
   in the tenant's realm; pass it with `--client-id` or `config set-context --client-id`.
   For scripts and integrations, prefer application credentials over personal ones.

3. Use it:

   ```sh
   graalctl projects list
   graalctl jobs list --project 7f1c...
   graalctl jobs run 5ea3... --follow          # start a run and stream its logs
   graalctl runs list --job 5ea3...
   graalctl jobs schedule 5ea3... "0 6 * * *"  # every day at 06:00
   graalctl workflows run 91be... --wait
   ```

## Command reference

| Command | Description |
|---|---|
| `login`, `logout`, `whoami` | Manage the session of the current context |
| `config set-context`, `use-context`, `get-contexts`, `current-context`, `delete-context`, `view` | Manage contexts |
| `projects list`, `get`, `create` | Projects |
| `jobs list`, `get`, `create`, `patch`, `schedule`, `run`, `delete` | Jobs |
| `runs list`, `get`, `logs`, `wait` | Runs and their logs |
| `workflows list`, `get`, `create`, `patch`, `run`, `delete` | Workflows |
| `libraries list`, `upload` | Files delivered to runs (code, wheels, jars) |
| `infrastructures list`, `instance-types list`, `identities list`, `secrets list` | Reference data (secret values are never shown) |
| `api` | Raw request to any REST endpoint |
| `completion` | Shell completion scripts (bash, zsh, fish, PowerShell, elvish) |
| `version` | Print the version |

Run `graalctl <command> --help` for all options.

### Creating and editing jobs

Jobs and workflows are created from their JSON definition, as shown in the console's
"Copy as…" menu or in the [API documentation](https://graal.systems/en/docs/api):

```sh
KEY=$(graalctl libraries upload first_job.py)      # code reaches runs through libraries
cat > job.json <<JSON
{
  "name": "first-job",
  "identity_id": "$IDENTITY_ID",
  "timeout_seconds": 900,
  "max_retries": 0,
  "libraries": [{ "type": "file", "key": "$KEY" }],
  "options": {
    "type": "python",
    "file": "first_job.py",
    "instance_type": "Standard_Development_D1_v1"
  }
}
JSON
graalctl jobs create --project 7f1c... -f job.json
```

The API has no full `PUT`: edits are JSON Patch documents (RFC 6902).

```sh
graalctl jobs patch 5ea3... --set timeout_seconds=3600 --set max_retries=2
graalctl jobs patch 5ea3... -f patch.json
graalctl jobs schedule 5ea3... --clear
```

### Raw API access

`graalctl api` sends any request with the right authentication and tenant headers:

```sh
graalctl api /instance-types
graalctl api /jobs/5ea3.../runs -q page=0 -q size=10
graalctl api -X PATCH /jobs/5ea3... -d '[{"op":"replace","path":"/description","value":"nightly"}]'
graalctl api /projects -t project -d @project.json      # POST with the project media type
```

### Output formats

`-o table` (default) prints aligned columns, `-o json` prints the API objects and `-o id` prints
one identifier per line:

```sh
for job in $(graalctl jobs list -p 7f1c... -o id); do graalctl jobs get "$job" -o json; done
```

## Using graalctl in CI

Nothing needs to be stored on the runner: give the API URL, tenant and application credentials
through environment variables.

```yaml
# GitHub Actions
- name: Run the nightly load
  env:
    GRAAL_API_URL: https://graal.example.com/api/v1
    GRAAL_KEYCLOAK_URL: https://sso.example.com
    GRAAL_TENANT: acme
    GRAAL_CLIENT_ID: ${{ secrets.GRAAL_CLIENT_ID }}
    GRAAL_CLIENT_SECRET: ${{ secrets.GRAAL_CLIENT_SECRET }}
  run: |
    curl -fsSL https://raw.githubusercontent.com/graalsystems/graalctl/main/install.sh | sh
    graalctl jobs run "$JOB_ID" --follow --timeout 3600
```

`--wait` / `--follow` exit with `0` when the run succeeds, `1` when it fails or is cancelled and
`124` on `--timeout`.

## Environment variables

| Variable | Purpose |
|---|---|
| `GRAAL_CONTEXT` | Context to use |
| `GRAAL_API_URL` | API URL (ends with `/api/v1`) |
| `GRAAL_TENANT` | Tenant |
| `GRAAL_KEYCLOAK_URL` | OpenID Connect base URL |
| `GRAAL_TOKEN` | Bearer token to use as is |
| `GRAAL_CLIENT_ID`, `GRAAL_CLIENT_SECRET` | Application credentials (client credentials grant) |
| `GRAAL_INFRASTRUCTURE` | Default infrastructure for runs |
| `GRAALCTL_CONFIG_DIR` | Configuration directory |
| `HTTPS_PROXY`, `NO_PROXY` | Proxy settings |

## Configuration files

Contexts are stored in `config.toml` and tokens in `credentials.json` (readable by the owner
only) in the user configuration directory:

- Linux: `~/.config/graalctl/`
- macOS: `~/Library/Application Support/graalctl/`
- Windows: `%APPDATA%\graalctl\`

For installations using an internal PKI, `config set-context --ca-file ca.pem` replaces the
system trust store with the given certificates.

## Errors

API errors are printed with their stable `code`, for example
`HTTP 501 feature_disabled: ...` when a feature (such as a job type) is not enabled on the
installation. Use `-v` to print every HTTP request.

## Development

```sh
cargo test      # unit tests and end-to-end tests against a mock API
cargo clippy --all-targets
cargo build --release
```

Releases are built by GitHub Actions when a `v*` tag is pushed.

## License

Apache License 2.0.
