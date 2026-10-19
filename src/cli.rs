//! Public command-line interface. Capture commands return the underlying build's
//! exit code; reporting and archive operations never execute compilation commands.
use crate::{
    analysis,
    capture::{self, ProfileRequest, RecordRequest},
    launcher::{self, LauncherRequest},
    report,
    store::Store,
};
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Parse and execute one CLI request, preserving the exit status of failed builds.
pub fn run() -> Result<i32> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "__launch") {
        let start = if args.get(2).is_some_and(|arg| arg == "--") {
            3
        } else {
            2
        };
        return capture::launch(args[start..].to_vec());
    }
    let cli = Cli::parse();
    if let Commands::Launch {
        build_dir,
        detail,
        arguments,
    } = &cli.command
    {
        return launcher::launch(&LauncherRequest {
            build_dir: build_dir.clone(),
            full_detail: matches!(detail, Detail::Full),
            arguments: arguments.clone(),
        });
    }
    let store = Store::open(&cli.store)?;
    match cli.command {
        Commands::Launch { .. } => unreachable!("launcher handled without opening the database"),
        Commands::Collect { build_dir, label } => {
            if let Some(run) = launcher::collect(&store, &build_dir, label)? {
                println!(
                    "Collected {}: {} compiler invocations, {:.3}s observed compiler span. Open with: buildscope report {}",
                    run.id,
                    run.actions.len(),
                    run.elapsed_ms / 1000.0,
                    run.id
                );
            } else {
                println!(
                    "No completed, uncollected compiler invocations. No-op builds create no launcher data."
                );
            }
            Ok(0)
        }
        Commands::Record(args) => {
            anyhow::ensure!(
                !matches!(args.detail, Detail::Full) && !args.preprocess,
                "--detail full and --preprocess require the profile command"
            );
            let (_, code) = capture::record(&store, &args.request())?;
            Ok(code)
        }
        Commands::Profile {
            build,
            profile_dir,
            unit,
            preset,
            cmake_args,
            compiler,
        } => {
            let (_, code) = capture::profile(
                &store,
                &ProfileRequest {
                    original: build.request(),
                    profile_dir,
                    unit,
                    preset,
                    cmake_args,
                    compiler,
                },
            )?;
            Ok(code)
        }
        Commands::Report {
            run,
            port,
            no_open,
            tour,
            path_maps,
        } => {
            let mappings = path_maps
                .into_iter()
                .map(|mapping| {
                    mapping
                        .split_once('=')
                        .map(|(a, b)| (a.to_owned(), b.to_owned()))
                        .context("--path-map requires OLD=NEW")
                })
                .collect::<Result<Vec<_>>>()?;
            report::serve(
                store,
                (!tour).then_some(run.as_str()),
                port,
                !no_open,
                mappings,
            )?;
            Ok(0)
        }
        Commands::Inspect { run, json } => {
            let run = store.load(&run)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&run)?);
            } else {
                println!(
                    "{}  {}  {}  {:.3}s {}",
                    run.id,
                    run.status,
                    run.kind,
                    run.elapsed_ms / 1000.0,
                    if run.kind == "launcher" {
                        "observed compiler span"
                    } else {
                        "elapsed"
                    }
                );
                println!(
                    "{} actions; {} observed / {} inventory units; {} headers; {} compiler profiles",
                    run.actions.len(),
                    run.summary.observed_units,
                    run.project.units.len(),
                    run.headers.len(),
                    run.profiles.len()
                );
                println!("Git: {}", run.git);
                println!("\nCompilation units:");
                for unit in &run.project.units {
                    println!("  {}  {} → {}", unit.id, unit.source_key, unit.output_key);
                }
                println!("\nFindings:");
                for finding in &run.findings {
                    println!("  {}\n    {}", finding.title, finding.explanation);
                }
                for note in &run.notes {
                    println!("  Note: {note}");
                }
            }
            Ok(0)
        }
        Commands::List { json } => {
            let runs = store.list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&runs)?);
            } else {
                for run in runs {
                    println!(
                        "{}  {:<11} {:<7} {:>8.3}s  {}  {}",
                        run["id"].as_str().unwrap_or(""),
                        run["status"].as_str().unwrap_or(""),
                        run["kind"].as_str().unwrap_or(""),
                        run["elapsed_ms"].as_f64().unwrap_or_default() / 1000.0,
                        run["label"].as_str().unwrap_or(""),
                        serde_json::from_value::<crate::git::GitSnapshot>(run["git"].clone())
                            .unwrap_or_default()
                    );
                }
            }
            Ok(0)
        }
        Commands::Compare {
            before,
            after,
            json,
        } => {
            let comparison = analysis::compare(&store.load(&before)?, &store.load(&after)?);
            if json {
                println!("{}", serde_json::to_string_pretty(&comparison)?);
            } else {
                println!(
                    "Observed duration: {:.3}s → {:.3}s ({:+.3}s)",
                    comparison["before_elapsed_ms"].as_f64().unwrap() / 1000.0,
                    comparison["after_elapsed_ms"].as_f64().unwrap() / 1000.0,
                    comparison["delta_elapsed_ms"].as_f64().unwrap() / 1000.0
                );
                for (label, key) in [("Before Git", "before_git"), ("After Git", "after_git")] {
                    let git: crate::git::GitSnapshot =
                        serde_json::from_value(comparison[key].clone())?;
                    println!("{label}: {git}");
                }
                for difference in comparison["differences"].as_array().unwrap() {
                    println!("Difference: {}", difference.as_str().unwrap());
                }
                for limitation in comparison["limitations"].as_array().unwrap() {
                    println!("Note: {}", limitation.as_str().unwrap());
                }
            }
            Ok(0)
        }
        Commands::Export {
            run,
            output,
            include_source,
        } => {
            store.export(&run, &output, include_source)?;
            println!("Exported {}", output.display());
            Ok(0)
        }
        Commands::CompareSeries {
            before_label,
            after_label,
            json,
        } => {
            let result = analysis::compare_series(
                &store.with_label(&before_label)?,
                &store.with_label(&after_label)?,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "Successful samples: {} before, {} after",
                    result["before"]["count"], result["after"]["count"]
                );
                println!(
                    "Median observed duration: {:.3}s → {:.3}s ({:+.3}s)",
                    result["before"]["median_ms"].as_f64().unwrap() / 1000.0,
                    result["after"]["median_ms"].as_f64().unwrap() / 1000.0,
                    result["delta_median_ms"].as_f64().unwrap() / 1000.0
                );
                for (label, group) in [("Before Git", "before"), ("After Git", "after")] {
                    let git: crate::git::GitSnapshot =
                        serde_json::from_value(result[group]["git"].clone())?;
                    println!("{label}: {git}");
                }
                println!("Recorded conditions match: {}", result["conditions_match"]);
            }
            Ok(0)
        }
        Commands::Import { archive } => {
            println!("Imported {}", store.import(&archive)?);
            Ok(0)
        }
        Commands::Recover { run } => {
            println!("Recovered {}", capture::recover(&store, &run)?.id);
            Ok(0)
        }
    }
}

#[derive(Parser)]
#[command(
    name = "buildscope",
    version,
    about = "Explain C++ build time using Ninja, Clang, and GCC evidence"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        default_value = ".buildscope",
        env = "BUILDSCOPE_STORE"
    )]
    store: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// CMake compiler launcher: record one invocation into the build's local spool.
    Launch {
        /// Root Ninja build directory; defaults to searching working-directory ancestors.
        #[arg(long)]
        build_dir: Option<PathBuf>,
        /// Full Clang detail is expensive; standard retains the native trace cutoff.
        #[arg(long, value_enum, default_value = "standard")]
        detail: Detail,
        /// Compiler executable and arguments; separate with -- to preserve compiler flags.
        #[arg(last = true, required = true, num_args = 1..)]
        arguments: Vec<String>,
    },
    /// Assemble pending launcher invocations into a report without executing a build.
    Collect {
        #[arg(long)]
        build_dir: PathBuf,
        #[arg(long)]
        label: Option<String>,
    },
    /// Record an ordinary CMake/Ninja build, retaining its cache behavior.
    Record(BuildArgs),
    /// Profile a dedicated clone of an existing build configuration.
    Profile {
        #[command(flatten)]
        build: BuildArgs,
        /// Separate Buildscope-owned output tree; defaults to a stable store directory.
        #[arg(long)]
        profile_dir: Option<PathBuf>,
        /// Investigate exactly one unit ID/prefix or source key from the inventory.
        #[arg(long)]
        unit: Option<String>,
        /// Source CMake preset used when configuring the profiling tree.
        #[arg(long)]
        preset: Option<String>,
        /// Extra CMake argument, for example --cmake-arg=-DOPTION=ON.
        #[arg(long = "cmake-arg", allow_hyphen_values = true)]
        cmake_args: Vec<String>,
        /// Explicit alternative C++ compiler; creates a separately labelled configuration.
        #[arg(long)]
        compiler: Option<String>,
    },
    /// Serve the interactive report on loopback.
    Report {
        #[arg(default_value = "latest")]
        run: String,
        /// Port zero chooses an available port.
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long)]
        no_open: bool,
        /// Open the guided UI tour with example data; no recorded build is required.
        #[arg(long, conflicts_with = "run")]
        tour: bool,
        /// Remap source roots when opening a capture from another machine.
        #[arg(long = "path-map")]
        path_maps: Vec<String>,
    },
    /// Print a capture, compilation-unit IDs, and findings.
    Inspect {
        #[arg(default_value = "latest")]
        run: String,
        #[arg(long)]
        json: bool,
    },
    /// List saved captures.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Compare compatible entities and disclose differing capture conditions.
    Compare {
        before: String,
        after: String,
        #[arg(long)]
        json: bool,
    },
    /// Compare repeated successful captures grouped by exact experiment labels.
    CompareSeries {
        before_label: String,
        after_label: String,
        #[arg(long)]
        json: bool,
    },
    /// Export a portable archive with raw evidence.
    Export {
        #[arg(default_value = "latest")]
        run: String,
        #[arg(long)]
        output: PathBuf,
        /// Include only source files whose current content still matches this capture.
        #[arg(long)]
        include_source: bool,
    },
    /// Import a capture archive, rebuilding its searchable event index.
    Import { archive: PathBuf },
    /// Finalize retained artifacts from an interrupted recorder without running a build.
    Recover {
        #[arg(default_value = "latest")]
        run: String,
    },
}

#[derive(Args)]
struct BuildArgs {
    #[arg(long)]
    build_dir: PathBuf,
    #[arg(long = "target")]
    targets: Vec<String>,
    #[arg(long)]
    label: Option<String>,
    #[arg(short = 'j', long)]
    jobs: Option<std::num::NonZeroUsize>,
    #[arg(long, value_enum, default_value = "standard")]
    detail: Detail,
    /// Add an isolated post-build preprocessing-size measurement.
    #[arg(long)]
    preprocess: bool,
}

impl BuildArgs {
    fn request(&self) -> RecordRequest {
        RecordRequest {
            build_dir: self.build_dir.clone(),
            targets: self.targets.clone(),
            label: self.label.clone(),
            jobs: self.jobs.map(|j| j.get()),
            full_detail: matches!(self.detail, Detail::Full),
            preprocess: self.preprocess,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Detail {
    Standard,
    Full,
}
