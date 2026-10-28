//! Command-line definition.

use crate::output::Format;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "graalctl",
    version,
    about = "Command-line interface for the graal Data Platform",
    long_about = "Command-line interface for the graal Data Platform.\n\n\
                  Manage projects, jobs, runs and workflows of a graal installation \
                  through its REST API (/api/v1).\n\n\
                  Get started:\n  \
                  graalctl config set-context prod --api-url https://graal.example.com/api/v1 \\\n      \
                  --auth-url https://sso.example.com --tenant acme\n  \
                  graalctl login\n  \
                  graalctl projects list",
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub globals: Globals,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Clone)]
#[command(next_help_heading = "Global options")]
pub struct Globals {
    /// Context to use instead of the current one.
    #[arg(long, global = true, env = "GRAAL_CONTEXT")]
    pub context: Option<String>,
    /// Override the API URL (must end with /api/v1).
    #[arg(long, global = true, env = "GRAAL_API_URL")]
    pub api_url: Option<String>,
    /// Override the tenant.
    #[arg(long, global = true, env = "GRAAL_TENANT")]
    pub tenant: Option<String>,
    /// Override the OpenID Connect (Keycloak) base URL.
    #[arg(long, global = true, env = "GRAAL_KEYCLOAK_URL")]
    pub auth_url: Option<String>,
    /// Output format.
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    pub output: Format,
    /// HTTP timeout in seconds.
    #[arg(long, global = true, default_value_t = 60)]
    pub http_timeout: u64,
    /// Print HTTP requests to stderr.
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Log in to the current context.
    Login(LoginArgs),
    /// Forget the credentials of the current context.
    Logout,
    /// Show who you are logged in as.
    Whoami,
    /// Manage contexts (installations and tenants).
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Manage projects.
    #[command(subcommand, visible_alias = "project")]
    Projects(ProjectsCommand),
    /// Manage jobs.
    #[command(subcommand, visible_alias = "job")]
    Jobs(JobsCommand),
    /// Inspect runs and their logs.
    #[command(subcommand, visible_alias = "run")]
    Runs(RunsCommand),
    /// Manage workflows.
    #[command(subcommand, visible_alias = "workflow")]
    Workflows(WorkflowsCommand),
    /// List infrastructures (where runs are placed).
    #[command(subcommand, visible_alias = "infrastructure")]
    Infrastructures(ListOnly),
    /// List instance types (CPU, memory and GPU templates for runs).
    #[command(subcommand, visible_alias = "instance-type")]
    InstanceTypes(ListOnly),
    /// List identities (accounts runs execute as).
    #[command(subcommand, visible_alias = "identity")]
    Identities(ListOnly),
    /// List project secrets (names only; values are never displayed).
    #[command(subcommand, visible_alias = "secret")]
    Secrets(ListOnly),
    /// Manage libraries (files delivered to runs).
    #[command(subcommand, visible_alias = "library")]
    Libraries(LibrariesCommand),
    /// Send a raw request to the REST API.
    Api(ApiArgs),
    /// Generate shell completion scripts.
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the version.
    Version,
}

#[derive(Args)]
pub struct LoginArgs {
    /// OIDC client id. For an application login, the graal Application id.
    #[arg(long, env = "GRAAL_CLIENT_ID")]
    pub client_id: Option<String>,
    /// Read the application secret from stdin (application login, for scripts and CI).
    #[arg(long)]
    pub client_secret_stdin: bool,
    /// Use the device authorization flow (works with SSO; open a URL in any browser).
    #[arg(long, conflicts_with_all = ["username", "password_stdin", "client_secret_stdin", "token_stdin"])]
    pub device: bool,
    /// Username for a password login.
    #[arg(long, short)]
    pub username: Option<String>,
    /// Read the password from stdin instead of prompting.
    #[arg(long)]
    pub password_stdin: bool,
    /// Read an existing access token from stdin and store it as is.
    #[arg(long, conflicts_with_all = ["username", "password_stdin", "client_secret_stdin"])]
    pub token_stdin: bool,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Create or update a context.
    SetContext(SetContextArgs),
    /// Switch the current context.
    UseContext { name: String },
    /// List contexts.
    GetContexts,
    /// Print the name of the current context.
    CurrentContext,
    /// Delete a context and its credentials.
    DeleteContext { name: String },
    /// Print the configuration file.
    View,
}

/// The API URL, auth URL and tenant are taken from the global
/// `--api-url`, `--auth-url` and `--tenant` options.
#[derive(Args)]
pub struct SetContextArgs {
    pub name: String,
    /// Authentication realm (discovered from the tenant when omitted).
    #[arg(long)]
    pub realm: Option<String>,
    /// OIDC client for interactive logins (default: graal-ui).
    #[arg(long)]
    pub client_id: Option<String>,
    /// Default infrastructure for runs.
    #[arg(long)]
    pub infrastructure: Option<String>,
    /// PEM file with the CA certificates to trust.
    #[arg(long)]
    pub ca_file: Option<PathBuf>,
    /// Disable TLS certificate verification (unsafe).
    #[arg(long)]
    pub insecure: Option<bool>,
    /// Also make it the current context.
    #[arg(long = "use")]
    pub use_it: bool,
}

#[derive(Args, Clone)]
pub struct Paging {
    /// Page number (from 0).
    #[arg(long, default_value_t = 0)]
    pub page: u32,
    /// Page size.
    #[arg(long, default_value_t = 100)]
    pub size: u32,
}

#[derive(Subcommand)]
pub enum ListOnly {
    /// List resources.
    #[command(visible_alias = "ls")]
    List {
        #[command(flatten)]
        paging: Paging,
    },
}

#[derive(Subcommand)]
pub enum ProjectsCommand {
    /// List projects.
    #[command(visible_alias = "ls")]
    List {
        #[command(flatten)]
        paging: Paging,
    },
    /// Show a project.
    Get { id: String },
    /// Create a project.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// Label as key=value (repeatable).
        #[arg(long = "label", value_parser = parse_key_value)]
        labels: Vec<(String, String)>,
    },
}

#[derive(Args)]
pub struct RunOptions {
    /// Infrastructure to run on (default: context setting, or the only one available).
    #[arg(long, env = "GRAAL_INFRASTRUCTURE")]
    pub infrastructure: Option<String>,
    /// Name of the run.
    #[arg(long)]
    pub name: Option<String>,
    /// Wait for the run to finish; the exit code reflects its outcome.
    #[arg(long, short)]
    pub wait: bool,
    /// Give up waiting after this many seconds (exit code 124).
    #[arg(long)]
    pub timeout: Option<u64>,
    /// Seconds between status checks.
    #[arg(long, default_value_t = 3)]
    pub interval: u64,
}

#[derive(Subcommand)]
pub enum JobsCommand {
    /// List jobs.
    #[command(visible_alias = "ls")]
    List {
        /// Only jobs of this project.
        #[arg(long, short)]
        project: Option<String>,
        #[command(flatten)]
        paging: Paging,
    },
    /// Show a job.
    Get { id: String },
    /// Create a job from a JSON definition (file path or - for stdin).
    Create {
        #[arg(long, short)]
        file: PathBuf,
        /// Project (default: project_id from the file).
        #[arg(long, short)]
        project: Option<String>,
    },
    /// Edit a job with JSON Patch.
    Patch(PatchArgs),
    /// Set or clear the cron schedule of a job.
    Schedule {
        id: String,
        /// Cron expression, e.g. "0 6 * * *".
        #[arg(required_unless_present = "clear")]
        cron: Option<String>,
        /// Remove the schedule.
        #[arg(long, conflicts_with = "cron")]
        clear: bool,
    },
    /// Start a run of a job.
    Run {
        id: String,
        #[command(flatten)]
        options: RunOptions,
        /// Stream the logs while waiting (implies --wait).
        #[arg(long, short)]
        follow: bool,
    },
    /// Delete a job.
    Delete {
        id: String,
        /// Confirm the deletion.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args)]
pub struct PatchArgs {
    pub id: String,
    /// JSON Patch document (file path or - for stdin).
    #[arg(long, short, required_unless_present = "set")]
    pub file: Option<PathBuf>,
    /// Replace a field: /path=value (value parsed as JSON when possible; repeatable).
    #[arg(long, value_parser = parse_key_value)]
    pub set: Vec<(String, String)>,
}

#[derive(Subcommand)]
pub enum RunsCommand {
    /// List runs (latest runs of the tenant by default).
    #[command(visible_alias = "ls")]
    List {
        #[arg(long, short, conflicts_with = "workflow")]
        job: Option<String>,
        #[arg(long, short)]
        workflow: Option<String>,
        /// Latest runs of a project.
        #[arg(long, short, conflicts_with_all = ["job", "workflow"])]
        project: Option<String>,
        /// Maximum number of runs.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Show a run.
    Get {
        run: String,
        #[arg(
            long,
            short,
            required_unless_present = "workflow",
            conflicts_with = "workflow"
        )]
        job: Option<String>,
        #[arg(long, short)]
        workflow: Option<String>,
    },
    /// Print the logs of a job run.
    Logs {
        run: String,
        #[arg(long, short)]
        job: String,
        /// Keep streaming until the run ends.
        #[arg(long, short)]
        follow: bool,
    },
    /// Wait for a run to finish; the exit code reflects its outcome.
    Wait {
        run: String,
        #[arg(
            long,
            short,
            required_unless_present = "workflow",
            conflicts_with = "workflow"
        )]
        job: Option<String>,
        #[arg(long, short)]
        workflow: Option<String>,
        /// Give up after this many seconds (exit code 124).
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long, default_value_t = 3)]
        interval: u64,
    },
}

#[derive(Subcommand)]
pub enum WorkflowsCommand {
    /// List workflows.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long, short)]
        project: Option<String>,
        #[command(flatten)]
        paging: Paging,
    },
    /// Show a workflow.
    Get { id: String },
    /// Create a workflow from a JSON definition (file path or - for stdin).
    Create {
        #[arg(long, short)]
        file: PathBuf,
        #[arg(long, short)]
        project: Option<String>,
    },
    /// Edit a workflow with JSON Patch.
    Patch(PatchArgs),
    /// Start a run of a workflow.
    Run {
        id: String,
        #[command(flatten)]
        options: RunOptions,
    },
    /// Delete a workflow.
    Delete {
        id: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum LibrariesCommand {
    /// List libraries.
    #[command(visible_alias = "ls")]
    List {
        #[command(flatten)]
        paging: Paging,
    },
    /// Upload a file (code, wheel, jar, archive) and print its key.
    Upload { file: PathBuf },
}

#[derive(Args)]
pub struct ApiArgs {
    /// Path relative to the API URL, e.g. /projects
    pub path: String,
    /// HTTP method (default: GET, or POST when --data is given).
    #[arg(long, short = 'X')]
    pub method: Option<String>,
    /// Request body: inline JSON, @file or @- for stdin.
    #[arg(long, short)]
    pub data: Option<String>,
    /// Resource type for the vendor media type (project, job, run, ...).
    #[arg(long = "type", short)]
    pub kind: Option<String>,
    /// Query parameter as key=value (repeatable).
    #[arg(long, short, value_parser = parse_key_value)]
    pub query: Vec<(String, String)>,
}

pub fn parse_key_value(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .filter(|(k, _)| !k.is_empty())
        .ok_or_else(|| format!("expected key=value, got '{s}'"))
}
