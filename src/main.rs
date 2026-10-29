mod api;
mod auth;
mod cli;
mod config;
mod output;

use anyhow::{anyhow, bail, Context as _, Result};
use api::{encode, items};
use clap::{CommandFactory, Parser};
use cli::*;
use config::{Config, Context, CredentialStore, Credentials};
use output::{Column, Format};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

/// An error that maps to a specific process exit code.
#[derive(Debug)]
struct Exit {
    code: i32,
    message: String,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Exit {}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        if let Some(exit) = err.downcast_ref::<Exit>() {
            if !exit.message.is_empty() {
                eprintln!("error: {}", exit.message);
            }
            std::process::exit(exit.code);
        }
        eprintln!("error: {err}");
        for cause in err.chain().skip(1) {
            eprintln!("  caused by: {cause}");
        }
        std::process::exit(1);
    }
}

const PROJECT_COLUMNS: &[Column] = &[
    ("ID", "id"),
    ("NAME", "name"),
    ("DESCRIPTION", "description"),
    ("UPDATED", "updated|created"),
];
const JOB_COLUMNS: &[Column] = &[
    ("ID", "id"),
    ("NAME", "name"),
    ("TYPE", "options.type|type"),
    ("PROJECT", "project_id"),
    ("SCHEDULE", "schedule.cron|schedule"),
    ("UPDATED", "updated|created"),
];
const RUN_COLUMNS: &[Column] = &[
    ("ID", "id"),
    ("STATUS", "status"),
    ("JOB", "job_name|job_id|workflow_id"),
    ("INITIATOR", "initiator"),
    ("CREATED", "created"),
    ("UPDATED", "updated"),
];
const WORKFLOW_COLUMNS: &[Column] = &[
    ("ID", "id"),
    ("NAME", "name"),
    ("PROJECT", "project_id"),
    ("SCHEDULE", "schedule.cron|schedule"),
    ("UPDATED", "updated|created"),
];
const INFRASTRUCTURE_COLUMNS: &[Column] = &[
    ("ID", "id"),
    ("NAME", "name"),
    ("TYPE", "type"),
    ("LOCATION", "location"),
];
const INSTANCE_TYPE_COLUMNS: &[Column] = &[
    ("ID", "id"),
    ("CPUS", "cpus"),
    ("MEMORY", "memory"),
    ("GPUS", "gpus"),
    ("GPU MEMORY", "gpu_memory"),
    ("FAMILY", "family"),
];
const IDENTITY_COLUMNS: &[Column] = &[
    ("ID", "id"),
    ("NAME", "name"),
    ("TYPE", "type"),
    ("DESCRIPTION", "description"),
];
const SECRET_COLUMNS: &[Column] = &[
    ("ID", "id"),
    ("NAME", "name"),
    ("DESCRIPTION", "description"),
    ("UPDATED", "updated|created"),
];
const LIBRARY_COLUMNS: &[Column] = &[
    ("KEY", "key|id"),
    ("NAME", "name|filename|original_filename"),
    ("SIZE", "size|length"),
    ("UPDATED", "updated|created|last_modified"),
];

fn run(cli: Cli) -> Result<()> {
    let g = cli.globals;
    match cli.command {
        Command::Version => {
            println!("graalctl {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Completion { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "graalctl",
                &mut std::io::stdout(),
            );
            Ok(())
        }
        Command::Config(cmd) => config_command(&g, cmd),
        Command::Login(args) => login(&g, args),
        Command::Logout => logout(&g),
        Command::Whoami => whoami(&g),
        Command::Api(args) => raw_api(&g, args),
        command => {
            let session = connect(&g)?;
            resources(&g, &session, command)
        }
    }
}

// ---------------------------------------------------------------------------
// Contexts and sessions
// ---------------------------------------------------------------------------

struct Session {
    client: api::Client,
    ctx: Context,
    token: String,
}

fn tls(ctx: &Context) -> api::Tls {
    api::Tls {
        ca_file: ctx.ca_file.clone(),
        insecure: ctx.insecure,
    }
}

/// The context selected by `--context` (or the current one), with the global
/// overrides applied. Works without any configuration file when the API URL
/// and tenant come from flags or environment variables.
fn effective_context(g: &Globals) -> Result<(Option<String>, Context)> {
    let cfg = Config::load()?;
    let (name, mut ctx) = match cfg.resolve(g.context.as_deref())? {
        Some((n, c)) => (Some(n), c),
        None => (None, Context::default()),
    };
    if let Some(u) = &g.api_url {
        ctx.api_url = u.clone();
    }
    if let Some(t) = &g.tenant {
        ctx.tenant = t.clone();
    }
    if let Some(a) = &g.auth_url {
        ctx.auth_url = Some(a.clone());
    }
    if ctx.api_url.is_empty() {
        bail!("no API URL configured; create a context with `graalctl config set-context` or set GRAAL_API_URL");
    }
    if ctx.tenant.is_empty() {
        bail!("no tenant configured; create a context with `graalctl config set-context` or set GRAAL_TENANT");
    }
    Ok((name, ctx))
}

fn oidc<'a>(agent: &'a ureq::Agent, ctx: &Context, verbose: bool) -> Result<auth::Oidc<'a>> {
    let auth_url = ctx
        .auth_url
        .clone()
        .ok_or_else(|| anyhow!("no auth URL configured; set it with `graalctl config set-context --auth-url` or GRAAL_KEYCLOAK_URL"))?;
    let realm = match &ctx.realm {
        Some(r) => r.clone(),
        None => auth::discover_realm(
            &api::Client::new(agent.clone(), &ctx.api_url, &ctx.tenant, None, verbose),
            &ctx.tenant,
        ),
    };
    Ok(auth::Oidc {
        agent,
        auth_url,
        realm,
    })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Builds an authenticated API client.
///
/// Token sources, by priority: `GRAAL_TOKEN`; `GRAAL_CLIENT_ID` +
/// `GRAAL_CLIENT_SECRET` (application credentials, nothing stored);
/// credentials stored by `graalctl login` for the context.
fn connect(g: &Globals) -> Result<Session> {
    let (name, ctx) = effective_context(g)?;
    let agent = api::agent(&tls(&ctx), Duration::from_secs(g.http_timeout))?;
    let token = if let Some(token) = non_empty_env("GRAAL_TOKEN") {
        token
    } else if let (Some(id), Some(secret)) = (
        non_empty_env("GRAAL_CLIENT_ID"),
        non_empty_env("GRAAL_CLIENT_SECRET"),
    ) {
        oidc(&agent, &ctx, g.verbose)?
            .client_credentials(&id, &secret)?
            .access_token
    } else {
        let name =
            name.ok_or_else(|| anyhow!("not logged in; run `graalctl login` or set GRAAL_TOKEN"))?;
        let mut store = CredentialStore::load()?;
        let creds = store
            .contexts
            .get_mut(&name)
            .ok_or_else(|| anyhow!("not logged in to context '{name}'; run `graalctl login`"))?;
        let provider = if ctx.auth_url.is_some() {
            Some(oidc(&agent, &ctx, g.verbose)?)
        } else {
            None
        };
        let (token, changed) = auth::ensure_token(provider.as_ref(), creds)?;
        if changed {
            store.save()?;
        }
        token
    };
    let client = api::Client::new(
        agent,
        &ctx.api_url,
        &ctx.tenant,
        Some(token.clone()),
        g.verbose,
    );
    Ok(Session { client, ctx, token })
}

fn config_command(g: &Globals, cmd: ConfigCommand) -> Result<()> {
    let mut cfg = Config::load()?;
    match cmd {
        ConfigCommand::SetContext(a) => {
            let existing = cfg.contexts.get(&a.name).cloned();
            let mut ctx = existing.clone().unwrap_or_default();
            if let Some(v) = &g.api_url {
                ctx.api_url = v.trim_end_matches('/').to_string();
            }
            if let Some(v) = &g.auth_url {
                ctx.auth_url = Some(v.trim_end_matches('/').to_string());
            }
            if let Some(v) = &g.tenant {
                ctx.tenant = v.clone();
            }
            if a.realm.is_some() {
                ctx.realm = a.realm;
            }
            if a.client_id.is_some() {
                ctx.client_id = a.client_id;
            }
            if a.infrastructure.is_some() {
                ctx.infrastructure = a.infrastructure;
            }
            if a.ca_file.is_some() {
                ctx.ca_file = a.ca_file;
            }
            if let Some(insecure) = a.insecure {
                ctx.insecure = insecure;
            }
            if ctx.api_url.is_empty() || ctx.tenant.is_empty() {
                bail!("a new context needs --api-url and --tenant");
            }
            if !ctx.api_url.ends_with("/api/v1") {
                eprintln!("warning: the API URL usually ends with /api/v1");
            }
            if ctx.insecure {
                eprintln!(
                    "warning: TLS certificate verification is disabled for context '{}'",
                    a.name
                );
            }
            cfg.contexts.insert(a.name.clone(), ctx);
            if a.use_it || cfg.current_context.is_none() {
                cfg.current_context = Some(a.name.clone());
            }
            cfg.save()?;
            eprintln!(
                "Context '{}' {}.",
                a.name,
                if existing.is_some() {
                    "updated"
                } else {
                    "created"
                }
            );
        }
        ConfigCommand::UseContext { name } => {
            if !cfg.contexts.contains_key(&name) {
                bail!("context '{name}' does not exist");
            }
            cfg.current_context = Some(name.clone());
            cfg.save()?;
            eprintln!("Switched to context '{name}'.");
        }
        ConfigCommand::GetContexts => {
            let rows: Vec<Value> = cfg
                .contexts
                .iter()
                .map(|(n, c)| {
                    json!({
                        "current": if cfg.current_context.as_deref() == Some(n.as_str()) { "*" } else { "" },
                        "name": n, "api_url": c.api_url, "tenant": c.tenant, "auth_url": c.auth_url,
                    })
                })
                .collect();
            output::list(
                &rows,
                &[
                    ("NAME", "name"),
                    ("CURRENT", "current"),
                    ("TENANT", "tenant"),
                    ("API URL", "api_url"),
                    ("AUTH URL", "auth_url"),
                ],
                g.output,
            );
        }
        ConfigCommand::CurrentContext => match cfg.current_context {
            Some(c) => println!("{c}"),
            None => bail!("no current context"),
        },
        ConfigCommand::DeleteContext { name } => {
            if cfg.contexts.remove(&name).is_none() {
                bail!("context '{name}' does not exist");
            }
            if cfg.current_context.as_deref() == Some(name.as_str()) {
                cfg.current_context = None;
            }
            cfg.save()?;
            let mut store = CredentialStore::load()?;
            if store.contexts.remove(&name).is_some() {
                store.save()?;
            }
            eprintln!("Context '{name}' deleted.");
        }
        ConfigCommand::View => {
            eprintln!("# {}", Config::path()?.display());
            print!("{}", toml::to_string_pretty(&cfg)?);
        }
    }
    Ok(())
}

fn read_stdin_line(what: &str) -> Result<String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .with_context(|| format!("cannot read the {what} from stdin"))?;
    let line = line.trim().to_string();
    if line.is_empty() {
        bail!("empty {what} on stdin");
    }
    Ok(line)
}

fn login(g: &Globals, a: LoginArgs) -> Result<()> {
    let (name, mut ctx) = effective_context(g)?;
    let name = name.ok_or_else(|| {
        anyhow!("credentials are stored per context; create one first with `graalctl config set-context NAME --api-url URL --auth-url URL --tenant TENANT`")
    })?;
    let agent = api::agent(&tls(&ctx), Duration::from_secs(g.http_timeout))?;
    let mut creds = Credentials::default();

    if a.token_stdin {
        creds.access_token = Some(read_stdin_line("token")?);
    } else {
        let provider = oidc(&agent, &ctx, g.verbose)?;
        let secret = if a.client_secret_stdin {
            Some(read_stdin_line("client secret")?)
        } else {
            non_empty_env("GRAAL_CLIENT_SECRET")
        };
        let response = if let Some(secret) = secret {
            let id = a
                .client_id
                .clone()
                .ok_or_else(|| anyhow!("an application login needs --client-id"))?;
            let r = provider.client_credentials(&id, &secret)?;
            creds.client_id = Some(id);
            creds.client_secret = Some(secret);
            r
        } else {
            let client_id = a
                .client_id
                .clone()
                .or_else(|| ctx.client_id.clone())
                .unwrap_or_else(|| "graal-ui".into());
            creds.client_id = Some(client_id.clone());
            if a.device {
                provider.device(&client_id, |uri, code, complete| {
                    eprintln!(
                        "To log in, open {} and enter the code {code}",
                        complete.unwrap_or(uri)
                    );
                    eprintln!("Waiting for confirmation...");
                })?
            } else {
                let username = match a.username {
                    Some(u) => u,
                    None => {
                        eprint!("Username: ");
                        std::io::stderr().flush().ok();
                        read_stdin_line("username")?
                    }
                };
                let password = if a.password_stdin {
                    read_stdin_line("password")?
                } else {
                    rpassword::prompt_password("Password: ")?
                };
                provider.password(&client_id, &username, &password)?
            }
        };
        response.store_into(&mut creds);
        if ctx.realm.is_none() {
            // Remember the discovered realm to skip the lookup next time.
            let mut cfg = Config::load()?;
            if let Some(stored) = cfg.contexts.get_mut(&name) {
                stored.realm = Some(provider.realm.clone());
                cfg.save()?;
            }
            ctx.realm = Some(provider.realm);
        }
    }

    let mut store = CredentialStore::load()?;
    store.contexts.insert(name.clone(), creds.clone());
    store.save()?;
    let who = creds
        .access_token
        .as_deref()
        .and_then(auth::jwt_claims)
        .map(|c| principal(&c))
        .unwrap_or_else(|| "unknown".into());
    eprintln!("Logged in to '{name}' (tenant {}) as {who}.", ctx.tenant);
    Ok(())
}

fn principal(claims: &Value) -> String {
    [
        "preferred_username",
        "email",
        "clientId",
        "client_id",
        "azp",
        "sub",
    ]
    .iter()
    .find_map(|k| claims.get(*k).and_then(Value::as_str))
    .unwrap_or("unknown")
    .to_string()
}

fn logout(g: &Globals) -> Result<()> {
    let (name, ctx) = effective_context(g)?;
    let name = name.ok_or_else(|| anyhow!("no context selected"))?;
    let mut store = CredentialStore::load()?;
    match store.contexts.remove(&name) {
        Some(creds) => {
            // Best effort: end the session server-side too.
            if let (Some(refresh), Some(_)) = (&creds.refresh_token, &ctx.auth_url) {
                if let Ok(agent) = api::agent(&tls(&ctx), Duration::from_secs(10)) {
                    if let Ok(provider) = oidc(&agent, &ctx, g.verbose) {
                        let url = provider.token_endpoint().replace("/token", "/logout");
                        let mut form = format!(
                            "client_id={}&refresh_token={}",
                            encode(creds.client_id.as_deref().unwrap_or("graal-ui")),
                            encode(refresh)
                        );
                        if let Some(secret) = &creds.client_secret {
                            form.push_str(&format!("&client_secret={}", encode(secret)));
                        }
                        let _ = agent
                            .post(&url)
                            .header("Content-Type", "application/x-www-form-urlencoded")
                            .send(form);
                    }
                }
            }
            store.save()?;
            eprintln!("Logged out of '{name}'.");
        }
        None => eprintln!("Not logged in to '{name}'."),
    }
    Ok(())
}

fn whoami(g: &Globals) -> Result<()> {
    let session = connect(g)?;
    let claims = auth::jwt_claims(&session.token).unwrap_or(Value::Null);
    let expires = claims.get("exp").and_then(Value::as_u64).map(|exp| {
        let left = exp.saturating_sub(auth::now());
        if left == 0 {
            "expired".to_string()
        } else {
            format!("in {}m{:02}s", left / 60, left % 60)
        }
    });
    let info = json!({
        "user": principal(&claims),
        "email": claims.get("email"),
        "tenant": session.ctx.tenant,
        "api_url": session.ctx.api_url,
        "issuer": claims.get("iss"),
        "expires": expires,
    });
    match g.output {
        Format::Table => {
            for key in ["user", "email", "tenant", "api_url", "issuer", "expires"] {
                println!("{:<9} {}", format!("{key}:"), output::cell(info.get(key)));
            }
        }
        f => output::single(&info, f),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

fn paging(p: &Paging) -> Vec<(&'static str, String)> {
    vec![("page", p.page.to_string()), ("size", p.size.to_string())]
}

fn read_json_file(path: &Path) -> Result<Value> {
    let text = if path == Path::new("-") {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?
    };
    serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", path.display()))
}

/// Builds a JSON Patch document from `--file` or `--set /path=value` pairs.
fn patch_document(a: &PatchArgs) -> Result<Value> {
    let mut ops = match &a.file {
        Some(f) => match read_json_file(f)? {
            Value::Array(ops) => ops,
            _ => bail!("a JSON Patch document must be an array of operations"),
        },
        None => vec![],
    };
    for (path, value) in &a.set {
        let path = if path.starts_with('/') {
            path.clone()
        } else {
            format!("/{path}")
        };
        let value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.clone()));
        ops.push(json!({"op": "replace", "path": path, "value": value}));
    }
    Ok(Value::Array(ops))
}

fn create_in_project(
    s: &Session,
    file: &Path,
    project: Option<String>,
    collection: &str,
    kind: &str,
) -> Result<Value> {
    let mut body = read_json_file(file)?;
    let project = project
        .or_else(|| {
            body.get("project_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| anyhow!("no project: pass --project or set project_id in the file"))?;
    if let Some(obj) = body.as_object_mut() {
        obj.insert("project_id".into(), json!(project));
    }
    s.client.post(
        &format!("/projects/{}/{collection}", encode(&project)),
        kind,
        &body,
    )
}

fn confirm_delete(yes: bool, what: &str) -> Result<()> {
    if !yes {
        bail!("refusing to delete {what} without --yes");
    }
    Ok(())
}

fn pick_infrastructure(s: &Session, explicit: Option<String>) -> Result<Option<String>> {
    if let Some(i) = explicit.or_else(|| s.ctx.infrastructure.clone()) {
        return Ok(Some(i));
    }
    let all = items(&s.client.get("/infrastructures", "infrastructure", &[])?);
    let ids: Vec<String> = all
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    match ids.len() {
        0 => Ok(None),
        1 => Ok(ids.into_iter().next()),
        _ => bail!(
            "several infrastructures are available ({}); choose one with --infrastructure or `graalctl config set-context --infrastructure`",
            ids.join(", ")
        ),
    }
}

fn run_body(s: &Session, o: &RunOptions, default_name: String) -> Result<Value> {
    let mut body = json!({ "name": o.name.clone().unwrap_or(default_name) });
    if let Some(infra) = pick_infrastructure(s, o.infrastructure.clone())? {
        body["infrastructure_id"] = json!(infra);
    }
    Ok(body)
}

fn resources(g: &Globals, s: &Session, command: Command) -> Result<()> {
    let out = g.output;
    let c = &s.client;
    match command {
        Command::Projects(cmd) => match cmd {
            ProjectsCommand::List { paging: p } => output::list(
                &items(&c.get("/projects", "project", &paging(&p))?),
                PROJECT_COLUMNS,
                out,
            ),
            ProjectsCommand::Get { id } => output::single(
                &c.get(&format!("/projects/{}", encode(&id)), "project", &[])?,
                out,
            ),
            ProjectsCommand::Create {
                name,
                description,
                labels,
            } => {
                let labels: serde_json::Map<String, Value> =
                    labels.into_iter().map(|(k, v)| (k, json!(v))).collect();
                let body = json!({ "name": name, "description": description, "labels": labels });
                output::single(&c.post("/projects", "project", &body)?, out)
            }
        },
        Command::Jobs(cmd) => match cmd {
            JobsCommand::List { project, paging: p } => {
                let path = match project {
                    Some(p) => format!("/projects/{}/jobs", encode(&p)),
                    None => "/jobs".into(),
                };
                output::list(&items(&c.get(&path, "job", &paging(&p))?), JOB_COLUMNS, out)
            }
            JobsCommand::Get { id } => {
                output::single(&c.get(&format!("/jobs/{}", encode(&id)), "job", &[])?, out)
            }
            JobsCommand::Create { file, project } => {
                output::single(&create_in_project(s, &file, project, "jobs", "job")?, out)
            }
            JobsCommand::Patch(a) => output::single(
                &c.patch(
                    &format!("/jobs/{}", encode(&a.id)),
                    "job",
                    &patch_document(&a)?,
                )?,
                out,
            ),
            JobsCommand::Schedule { id, cron, clear } => {
                let ops = if clear {
                    json!([{ "op": "remove", "path": "/schedule" }])
                } else {
                    json!([{ "op": "replace", "path": "/schedule", "value": cron }])
                };
                let job = c.patch(&format!("/jobs/{}", encode(&id)), "job", &ops)?;
                match out {
                    Format::Table => eprintln!(
                        "Schedule of job {id} {}.",
                        if clear { "cleared" } else { "updated" }
                    ),
                    f => output::single(&job, f),
                }
            }
            JobsCommand::Run {
                id,
                options,
                follow,
            } => {
                let body = run_body(s, &options, format!("graalctl run of {id}"))?;
                let job_path = format!("/jobs/{}", encode(&id));
                let run = c.post(&format!("{job_path}/runs"), "jobrun", &body)?;
                let run_id = run.get("id").and_then(Value::as_str).map(str::to_string);
                if !(options.wait || follow) {
                    output::single(&run, out);
                    return Ok(());
                }
                let run_id = run_id.ok_or_else(|| anyhow!("the API did not return the run id"))?;
                eprintln!("Started run {run_id}.");
                let run_path = format!("{job_path}/runs/{}", encode(&run_id));
                let logs = follow.then(|| format!("{run_path}/logs"));
                let last = wait_run(
                    c,
                    &run_path,
                    "jobrun",
                    logs.as_deref(),
                    options.interval,
                    options.timeout,
                )?;
                finish(&last, out)?
            }
            JobsCommand::Delete { id, yes } => {
                confirm_delete(yes, &format!("job {id}"))?;
                c.delete(&format!("/jobs/{}", encode(&id)))?;
                eprintln!("Job {id} deleted.");
            }
        },
        Command::Workflows(cmd) => match cmd {
            WorkflowsCommand::List { project, paging: p } => {
                let path = match project {
                    Some(p) => format!("/projects/{}/workflows", encode(&p)),
                    None => "/workflows".into(),
                };
                output::list(
                    &items(&c.get(&path, "workflow", &paging(&p))?),
                    WORKFLOW_COLUMNS,
                    out,
                )
            }
            WorkflowsCommand::Get { id } => output::single(
                &c.get(&format!("/workflows/{}", encode(&id)), "workflow", &[])?,
                out,
            ),
            WorkflowsCommand::Create { file, project } => output::single(
                &create_in_project(s, &file, project, "workflows", "workflow")?,
                out,
            ),
            WorkflowsCommand::Patch(a) => output::single(
                &c.patch(
                    &format!("/workflows/{}", encode(&a.id)),
                    "workflow",
                    &patch_document(&a)?,
                )?,
                out,
            ),
            WorkflowsCommand::Run { id, options } => {
                let body = run_body(s, &options, format!("graalctl run of {id}"))?;
                let wf_path = format!("/workflows/{}", encode(&id));
                let run = c.post(&format!("{wf_path}/runs"), "workflowrun", &body)?;
                if !options.wait {
                    output::single(&run, out);
                    return Ok(());
                }
                let run_id = run
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("the API did not return the run id"))?;
                eprintln!("Started workflow run {run_id}.");
                let run_path = format!("{wf_path}/runs/{}", encode(run_id));
                let last = wait_run(
                    c,
                    &run_path,
                    "workflowrun",
                    None,
                    options.interval,
                    options.timeout,
                )?;
                finish(&last, out)?
            }
            WorkflowsCommand::Delete { id, yes } => {
                confirm_delete(yes, &format!("workflow {id}"))?;
                c.delete(&format!("/workflows/{}", encode(&id)))?;
                eprintln!("Workflow {id} deleted.");
            }
        },
        Command::Runs(cmd) => match cmd {
            RunsCommand::List {
                job,
                workflow,
                project,
                limit,
            } => {
                let (path, kind) = match (&job, &workflow, &project) {
                    (Some(j), _, _) => (format!("/jobs/{}/runs", encode(j)), "jobrun"),
                    (_, Some(w), _) => (format!("/workflows/{}/runs", encode(w)), "workflowrun"),
                    (_, _, Some(p)) => (format!("/projects/{}/runs", encode(p)), "jobrun-with-job"),
                    _ => ("/runs".into(), "jobrun-with-job"),
                };
                let query = [
                    ("number", limit.to_string()),
                    ("page", "0".into()),
                    ("size", limit.to_string()),
                ];
                let mut rows: Vec<Value> = items(&c.get(&path, kind, &query)?)
                    .into_iter()
                    .map(flatten_run)
                    .collect();
                rows.truncate(limit as usize);
                output::list(&rows, RUN_COLUMNS, out)
            }
            RunsCommand::Get { run, job, workflow } => {
                let (path, kind) = run_path(&run, job.as_deref(), workflow.as_deref());
                output::single(&c.get(&path, kind, &[])?, out)
            }
            RunsCommand::Logs { run, job, follow } => {
                let path = format!("/jobs/{}/runs/{}", encode(&job), encode(&run));
                if follow {
                    let last =
                        wait_run(c, &path, "jobrun", Some(&format!("{path}/logs")), 3, None)?;
                    finish(&last, Format::Id)?
                } else {
                    drain_logs(c, &format!("{path}/logs"), &mut LogCursor::default())?;
                }
            }
            RunsCommand::Wait {
                run,
                job,
                workflow,
                timeout,
                interval,
            } => {
                let (path, kind) = run_path(&run, job.as_deref(), workflow.as_deref());
                let last = wait_run(c, &path, kind, None, interval, timeout)?;
                finish(&last, out)?
            }
        },
        Command::Infrastructures(ListOnly::List { paging: p }) => output::list(
            &items(&c.get("/infrastructures", "infrastructure", &paging(&p))?),
            INFRASTRUCTURE_COLUMNS,
            out,
        ),
        Command::InstanceTypes(ListOnly::List { paging: p }) => output::list(
            &items(&c.get("/instance-types", "instance-type", &paging(&p))?),
            INSTANCE_TYPE_COLUMNS,
            out,
        ),
        Command::Identities(ListOnly::List { paging: p }) => output::list(
            &items(&c.get("/identities", "identity", &paging(&p))?),
            IDENTITY_COLUMNS,
            out,
        ),
        Command::Secrets(ListOnly::List { paging: p }) => {
            // Never print secret values, even in JSON output.
            let rows: Vec<Value> = items(&c.get("/secrets", "secret", &paging(&p))?)
                .into_iter()
                .map(|mut v| {
                    if let Some(o) = v.as_object_mut() {
                        o.remove("data");
                    }
                    v
                })
                .collect();
            output::list(&rows, SECRET_COLUMNS, out)
        }
        Command::Libraries(cmd) => match cmd {
            LibrariesCommand::List { paging: p } => output::list(
                &items(&c.get("/libraries", "blobmetadata", &paging(&p))?),
                LIBRARY_COLUMNS,
                out,
            ),
            LibrariesCommand::Upload { file } => {
                // The API answers with the list of uploaded files.
                let meta = items(&c.upload("/libraries", "blobmetadata", &file)?)
                    .into_iter()
                    .next()
                    .unwrap_or(Value::Null);
                match out {
                    Format::Table => println!("{}", output::cell(output::lookup(&meta, "key|id"))),
                    f => output::single(&meta, f),
                }
            }
        },
        _ => unreachable!("handled in run()"),
    }
    Ok(())
}

fn run_path(run: &str, job: Option<&str>, workflow: Option<&str>) -> (String, &'static str) {
    match (job, workflow) {
        (Some(j), _) => (
            format!("/jobs/{}/runs/{}", encode(j), encode(run)),
            "jobrun",
        ),
        (_, Some(w)) => (
            format!("/workflows/{}/runs/{}", encode(w), encode(run)),
            "workflowrun",
        ),
        _ => unreachable!("clap requires --job or --workflow"),
    }
}

/// `/runs` returns `{ run, job }` pairs; flatten them into one row.
fn flatten_run(v: Value) -> Value {
    match (v.get("run"), v.get("job")) {
        (Some(Value::Object(run)), job) => {
            let mut row = run.clone();
            if let Some(name) = job.and_then(|j| j.get("name")) {
                row.insert("job_name".into(), name.clone());
            }
            Value::Object(row)
        }
        _ => v,
    }
}

// ---------------------------------------------------------------------------
// Waiting for runs and streaming logs
// ---------------------------------------------------------------------------

/// Statuses after which a run will not change any more. The API reports
/// `complete`, `failed` or `killed`; the console vocabulary is accepted too.
fn outcome(status: &str) -> Option<bool> {
    match status {
        "complete" | "completed" | "succeeded" | "success" | "skipped" => Some(true),
        "failed" | "killed" | "cancelled" | "canceled" | "error" => Some(false),
        _ => None,
    }
}

fn status_of(run: &Value) -> String {
    run.get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_ascii_lowercase()
}

fn wait_run(
    c: &api::Client,
    path: &str,
    kind: &str,
    logs: Option<&str>,
    interval: u64,
    timeout: Option<u64>,
) -> Result<Value> {
    let started = Instant::now();
    let mut cursor = LogCursor::default();
    let mut last = String::new();
    loop {
        if let Some(l) = logs {
            drain_logs(c, l, &mut cursor)?;
        }
        let run = c.get(path, kind, &[])?;
        let status = status_of(&run);
        if status != last {
            eprintln!("status: {status}");
            last = status.clone();
        }
        if outcome(&status).is_some() {
            if let Some(l) = logs {
                drain_logs(c, l, &mut cursor)?;
            }
            return Ok(run);
        }
        if timeout.is_some_and(|t| started.elapsed() >= Duration::from_secs(t)) {
            return Err(Exit {
                code: 124,
                message: format!("timed out while the run was still {status}"),
            }
            .into());
        }
        std::thread::sleep(Duration::from_secs(interval.max(1)));
    }
}

/// Prints the final state of a run and fails when it did not succeed.
fn finish(run: &Value, out: Format) -> Result<()> {
    let status = status_of(run);
    if out == Format::Json {
        output::single(run, out);
    }
    match outcome(&status) {
        Some(true) => {
            eprintln!("Run finished: {status}.");
            Ok(())
        }
        _ => Err(Exit {
            code: 1,
            message: format!("run finished with status {status}"),
        }
        .into()),
    }
}

/// Position in the logs of a run. The API paginates logs with a cursor
/// returned in the `X-Cursor` header; when no cursor is returned, entries
/// already printed are skipped by count.
#[derive(Default)]
struct LogCursor {
    cursor: Option<String>,
    printed: usize,
}

fn drain_logs(c: &api::Client, path: &str, pos: &mut LogCursor) -> Result<()> {
    let mut out = std::io::stdout().lock();
    for _ in 0..1000 {
        let query: Vec<(&str, String)> = pos
            .cursor
            .iter()
            .map(|cur| ("cursor", cur.clone()))
            .collect();
        let resp = c.send("GET", path, &query, &api::accept("log"), None)?;
        let body = resp.json()?;
        let next = resp.header("X-Cursor").map(str::to_string).or_else(|| {
            ["cursor", "next_cursor", "next"]
                .iter()
                .find_map(|k| body.get(*k).and_then(Value::as_str).map(str::to_string))
        });
        let entries = items(&body);
        let skip = if pos.cursor.is_none() && next.is_none() {
            pos.printed
        } else {
            0
        };
        let new = entries.len().saturating_sub(skip);
        for entry in entries.iter().skip(skip) {
            let line = match entry {
                Value::String(s) => s.clone(),
                other => other
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| other.to_string()),
            };
            writeln!(out, "{}", line.trim_end_matches('\n'))?;
        }
        out.flush()?;
        match next {
            Some(n) if new > 0 && pos.cursor.as_deref() != Some(n.as_str()) => pos.cursor = Some(n),
            Some(n) => {
                pos.cursor = Some(n);
                return Ok(());
            }
            None => {
                pos.printed = skip + new;
                return Ok(());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Raw API access
// ---------------------------------------------------------------------------

fn raw_api(g: &Globals, a: ApiArgs) -> Result<()> {
    let s = connect(g)?;
    let body = match &a.data {
        None => None,
        Some(d) if d == "@-" => {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf)?;
            Some(buf)
        }
        Some(d) if d.starts_with('@') => {
            Some(std::fs::read(&d[1..]).with_context(|| format!("cannot read {}", &d[1..]))?)
        }
        Some(d) => Some(d.clone().into_bytes()),
    };
    let method = a
        .method
        .clone()
        .unwrap_or_else(|| {
            if body.is_some() {
                "POST".into()
            } else {
                "GET".into()
            }
        })
        .to_ascii_uppercase();
    let content_type = match (&method[..], &a.kind) {
        ("PATCH", _) => api::JSON_PATCH.to_string(),
        (_, Some(k)) => format!("{};charset=UTF-8", api::media_type(k)),
        _ => "application/json".to_string(),
    };
    let accept = a
        .kind
        .as_deref()
        .map(api::accept)
        .unwrap_or_else(|| "application/json, */*;q=0.5".into());
    let query: Vec<(&str, String)> = a
        .query
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    let resp = s.client.send(
        &method,
        &a.path,
        &query,
        &accept,
        body.map(|b| (content_type.as_str(), b)),
    )?;
    if g.verbose {
        eprintln!("< status {}", resp.status);
    }
    match serde_json::from_slice::<Value>(&resp.body) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
        Err(_) => std::io::stdout().write_all(&resp.body)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn outcomes() {
        assert_eq!(outcome("complete"), Some(true));
        assert_eq!(outcome("killed"), Some(false));
        assert_eq!(outcome("running"), None);
        assert_eq!(outcome("retrying"), None);
    }

    #[test]
    fn patch_from_set_pairs() {
        let a = PatchArgs {
            id: "j".into(),
            file: None,
            set: vec![
                ("timeout_seconds".into(), "600".into()),
                ("/description".into(), "nightly load".into()),
            ],
        };
        assert_eq!(
            patch_document(&a).unwrap(),
            json!([
                {"op": "replace", "path": "/timeout_seconds", "value": 600},
                {"op": "replace", "path": "/description", "value": "nightly load"}
            ])
        );
    }

    #[test]
    fn flattens_run_with_job() {
        let row =
            flatten_run(json!({"run": {"id": "r1", "status": "running"}, "job": {"name": "load"}}));
        assert_eq!(
            row,
            json!({"id": "r1", "status": "running", "job_name": "load"})
        );
    }
}
