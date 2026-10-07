use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Run end-to-end flows against real systems, and record demos from them.
#[derive(Debug, Parser)]
#[command(name = "lest", version, about, long_about = None, propagate_version = true)]
pub struct Cli {
    /// Project directory (default: the nearest ancestor with a lest.yaml).
    #[arg(short = 'p', long, global = true, value_name = "DIR")]
    pub project: Option<PathBuf>,
    /// Disable color.
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a flow.
    Run(RunArgs),
    /// List the project's flows.
    #[command(visible_alias = "ls")]
    List(ListArgs),
    /// Show one flow.
    Get(GetArgs),
    /// Check flow files for errors.
    Validate(ValidateArgs),
    /// Inspect past runs.
    Runs {
        #[command(subcommand)]
        command: RunsCommand,
    },
    /// Run the cleanups a past run recorded but did not run.
    Cleanup(CleanupArgs),
    /// Create a lest.yaml and an example flow.
    Init(InitArgs),
    /// Check that tools, browsers and settings are ready.
    Doctor,
    /// Store and remove secrets in the OS keyring.
    Secrets {
        #[command(subcommand)]
        command: SecretsCommand,
    },
    /// Print a JSON Schema.
    Schema {
        #[arg(value_enum, default_value_t = SchemaKind::Flow)]
        kind: SchemaKind,
    },
    /// Open the UI.
    Ui(UiArgs),
    /// Remove stored run data.
    Data {
        #[command(subcommand)]
        command: DataCommand,
    },
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Flow id, unique id prefix, or path to a .lest.yaml file.
    pub flow: String,
    /// Environment to run against.
    #[arg(short = 'e', long = "env", value_name = "NAME")]
    pub environment: Option<String>,
    /// Input value, repeatable.
    #[arg(short = 'i', long = "input", value_name = "NAME=VALUE")]
    pub inputs: Vec<String>,
    /// Re-run from this top-level step, reusing earlier results from the
    /// latest run (or --resume-run).
    #[arg(long, value_name = "STEP")]
    pub from: Option<String>,
    /// The run to take earlier results from when using --from.
    #[arg(long, value_name = "RUN", requires = "from")]
    pub resume_run: Option<String>,
    /// Write the JSON report to this path.
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: Option<PathBuf>,
    /// Write a JUnit XML report to this path.
    #[arg(long, value_name = "PATH")]
    pub junit: Option<PathBuf>,
    /// Show the browser window for browser steps.
    #[arg(long)]
    pub headed: bool,
    /// Never start an interactive sign-in for a tool.
    #[arg(long)]
    pub non_interactive: bool,
    /// Print only the final result line.
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Debug, Args)]
pub struct UiArgs {
    /// Port to listen on (default: any free port).
    #[arg(long, default_value_t = 0)]
    pub port: u16,
    /// Print the address instead of opening a browser.
    #[arg(long)]
    pub no_browser: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Only flows with this tag.
    #[arg(short = 't', long)]
    pub tag: Option<String>,
    /// Case-insensitive substring of the name or id.
    #[arg(long)]
    pub name: Option<String>,
    /// Print only ids, one per line.
    #[arg(long)]
    pub id: bool,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    pub flow: String,
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// Files to check (default: every flow in the project).
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum RunsCommand {
    /// List runs, newest first.
    #[command(visible_alias = "ls")]
    List {
        /// Only runs of this flow.
        #[arg(long)]
        flow: Option<String>,
        #[arg(short = 'm', long, default_value_t = 50)]
        max: usize,
        /// Print only run ids.
        #[arg(long)]
        id: bool,
    },
    /// Show one run.
    Get {
        /// Run id or unique prefix.
        run: String,
        /// Write the JSON report to this path.
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
pub struct CleanupArgs {
    /// Run id or unique prefix.
    pub run: String,
    /// Print the commands without running them.
    #[arg(long)]
    pub dry_run: bool,
    /// Run without asking.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Project name.
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    /// Fail instead of asking for missing values.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Debug, Subcommand)]
pub enum SecretsCommand {
    /// Store a secret, read from stdin or an environment variable.
    Create {
        name: String,
        /// Read the value from stdin.
        #[arg(long, conflicts_with = "from_env")]
        stdin: bool,
        /// Read the value from this environment variable.
        #[arg(long, value_name = "VAR")]
        from_env: Option<String>,
        /// Replace an existing value.
        #[arg(long)]
        overwrite: bool,
    },
    /// Show which secrets the project's flows need and whether each resolves.
    #[command(visible_alias = "ls")]
    List,
    /// Remove a secret from the keyring.
    #[command(visible_alias = "rm")]
    Delete {
        name: String,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum DataCommand {
    /// Delete all but the newest runs of each flow.
    Prune {
        /// Runs to keep per flow.
        #[arg(long, default_value_t = 20)]
        keep: usize,
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete every stored run.
    Purge {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SchemaKind {
    Flow,
    Project,
    Report,
}
