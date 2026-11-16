//! `sync`: run the aspect over the workspace and write kotlin-lsp's `workspace.json`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use bkl_core::aspect::read_targets;
use bkl_core::aspect_template::{JARS_GROUP, JSON_GROUP, RuleRepos, render_aspect};
use bkl_core::bazel_mod::{
    LocalModule, ModGraph, local_modules, normalize, parse_mod_graph, parse_repo_mapping,
    parse_show_repo, rule_repos,
};
use bkl_core::bep::aspect_json_files;
use bkl_core::cache::{
    CacheLayout, cache_root, digest_hex, lock_exclusive, write_if_changed, write_owner,
};
use bkl_core::model::{ModelContext, RealFs, build_model, jar_targets, render};
use bkl_core::root::{BazelRoot, find_bazel_root};
use bkl_core::scope::{
    NARROW_THRESHOLD, Scope, ScopeConfig, load_scope_config, resolve_directories, target_patterns,
};
use bkl_core::version::MIN_BAZEL;
use serde::{Deserialize, Serialize};

use crate::bazel::{Bazel, BazelRun, BuildOutcome, Cancel, classify_build, find_bazel};

const CONFIG_FILE: &str = ".bazel-kotlin.toml";

#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    pub start_dir: PathBuf,
    pub out_dir: Option<PathBuf>,
    pub bazel: Option<PathBuf>,
    pub scope_dirs: Vec<String>,
    pub targets: Vec<String>,
    pub shared_output_base: bool,
    /// Overrides the platform cache root.
    pub cache_root: Option<PathBuf>,
    /// Copy Bazel's stderr to ours.
    pub echo_bazel: bool,
    /// Lets another thread stop the sync; Bazel then runs in its own process group.
    pub cancel: Option<Cancel>,
    pub progress: Option<Progress>,
}

/// Told the name of each phase a sync enters.
#[derive(Clone)]
pub struct Progress(Arc<dyn Fn(&str) + Send + Sync>);

impl Progress {
    pub fn new(report: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Progress(Arc::new(report))
    }

    pub fn report(&self, phase: &str) {
        (self.0)(phase)
    }
}

impl fmt::Debug for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Progress")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncSummary {
    /// `ok`, `partial` or `failed`.
    pub status: String,
    pub changed: bool,
    pub modules: usize,
    pub libraries: usize,
    /// What became source modules: `whole repository`, `folder <dir>`, `flags` or `.bazel-kotlin.toml`.
    pub scope: String,
    pub warnings: Vec<String>,
    pub timings_ms: BTreeMap<String, u64>,
    pub error: Option<String>,
    /// Why a `partial` sync is partial: the first Bazel build that failed.
    pub partial_reason: Option<SyncError>,
    pub log: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct SyncOutcome {
    /// Logs, build events and the summary.
    pub out_dir: PathBuf,
    /// Holds `workspace.json` alone: the folder kotlin-lsp's `projects` option points at.
    pub model_dir: PathBuf,
    pub summary: SyncSummary,
    /// Folders of the `local_path_override` modules.
    pub local_module_dirs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncError {
    pub message: String,
    pub command: Option<String>,
    pub stderr_tail: Vec<String>,
    pub log: Option<PathBuf>,
}

impl SyncError {
    fn new(message: impl Into<String>) -> Self {
        SyncError {
            message: message.into(),
            ..Default::default()
        }
    }

    fn bazel(message: impl Into<String>, run: &BazelRun, log: &Path) -> Self {
        SyncError {
            message: message.into(),
            command: Some(run.command_line.clone()),
            stderr_tail: run.stderr_tail.clone(),
            log: Some(log.to_path_buf()),
        }
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(command) = &self.command {
            write!(f, "\nReproduce: {command}")?;
        }
        for line in &self.stderr_tail {
            write!(f, "\n  {line}")?;
        }
        if let Some(log) = &self.log {
            write!(f, "\nLog: {}", log.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for SyncError {}

impl From<anyhow::Error> for SyncError {
    fn from(e: anyhow::Error) -> Self {
        SyncError::new(format!("{e:#}"))
    }
}

struct Timer(BTreeMap<String, u64>, Instant);

impl Timer {
    fn lap(&mut self, phase: &str) {
        self.0
            .insert(phase.into(), self.1.elapsed().as_millis() as u64);
        self.1 = Instant::now();
    }
}

/// Where a sync of `opts` reads and writes, found without running Bazel.
#[derive(Debug, Clone)]
pub struct SyncPaths {
    pub start_dir: PathBuf,
    pub root: PathBuf,
    pub layout: CacheLayout,
    pub out_dir: PathBuf,
    pub model_dir: PathBuf,
    pub log: PathBuf,
}

#[derive(Debug)]
pub enum Located {
    Bzlmod(SyncPaths),
    /// Not inside a Bazel workspace, or a WORKSPACE-only one.
    NotBzlmod(SyncError),
}

/// Finds the workspace root and the cache layout of a sync, without running Bazel or creating folders.
pub fn locate(opts: &SyncOptions) -> Result<Located, SyncError> {
    let start_dir = std::path::absolute(&opts.start_dir)
        .map(|p| normalize(&p))
        .map_err(|e| SyncError::new(format!("{}: {e}", opts.start_dir.display())))?;
    let root = match find_bazel_root(&start_dir) {
        None => {
            return Ok(Located::NotBzlmod(SyncError::new(format!(
                "{} is not inside a Bazel workspace",
                start_dir.display()
            ))));
        }
        Some(BazelRoot::WorkspaceOnly(root)) => {
            return Ok(Located::NotBzlmod(SyncError::new(format!(
                "{}: WORKSPACE-only repositories are not supported (Bazel 8+ with MODULE.bazel required)",
                root.display()
            ))));
        }
        Some(BazelRoot::Bzlmod(root)) => root,
    };
    let realpath = root.canonicalize().unwrap_or_else(|_| root.clone());
    let cache = opts
        .cache_root
        .clone()
        .or_else(|| cache_root(&|k| std::env::var(k).ok(), cfg!(target_os = "macos")))
        .ok_or_else(|| {
            SyncError::new("cannot find a cache folder: set HOME to an absolute path")
        })?;
    let layout = CacheLayout::for_workspace(&cache, &realpath);
    let out_dir = opts.out_dir.clone().unwrap_or_else(|| layout.dir.clone());
    Ok(Located::Bzlmod(SyncPaths {
        start_dir,
        root,
        layout,
        model_dir: model_dir(&out_dir),
        log: out_dir.join("sync.log"),
        out_dir,
    }))
}

pub fn run_sync(opts: &SyncOptions) -> Result<SyncOutcome, SyncError> {
    let SyncPaths {
        start_dir,
        root,
        layout,
        out_dir,
        model_dir,
        log,
    } = match locate(opts)? {
        Located::Bzlmod(paths) => paths,
        Located::NotBzlmod(why) => return Err(why),
    };
    std::fs::create_dir_all(&out_dir)
        .and_then(|_| std::fs::create_dir_all(&layout.dir))
        .map_err(|e| SyncError::new(format!("{}: {e}", out_dir.display())))?;
    let _lock = lock_exclusive(&layout.dir)
        .map_err(|e| SyncError::new(format!("locking {}: {e}", layout.dir.display())))?;
    std::fs::write(&log, "").map_err(|e| SyncError::new(format!("{}: {e}", log.display())))?;
    write_owner(&layout.dir, &root, now_secs())
        .map_err(|e| SyncError::new(format!("{}: {e}", layout.dir.display())))?;

    let mut timer = Timer(BTreeMap::new(), Instant::now());
    let mut result = sync_in(opts, &start_dir, &root, &layout, &out_dir, &log, &mut timer);
    if result.is_err() && opts.cancel.as_ref().is_some_and(Cancel::is_cancelled) {
        result = Err(SyncError::new("sync cancelled"));
    }
    let summary = match &result {
        Ok((summary, _)) => summary.clone(),
        Err(e) => SyncSummary {
            status: "failed".into(),
            error: Some(e.to_string()),
            log: log.clone(),
            timings_ms: timer.0.clone(),
            ..Default::default()
        },
    };
    let _ = write_if_changed(
        &out_dir.join("sync-summary.json"),
        &serde_json::to_vec_pretty(&summary).unwrap_or_default(),
    );
    result.map(|(summary, local_module_dirs)| SyncOutcome {
        model_dir,
        out_dir,
        summary,
        local_module_dirs,
    })
}

fn sync_in(
    opts: &SyncOptions,
    start_dir: &Path,
    root: &Path,
    layout: &CacheLayout,
    out_dir: &Path,
    log: &Path,
    timer: &mut Timer,
) -> Result<(SyncSummary, Vec<PathBuf>), SyncError> {
    let phase = |name: &str| {
        if let Some(progress) = &opts.progress {
            progress.report(name);
        }
    };
    phase("querying Bazel");
    let mut warnings = Vec::new();
    if opts.cancel.as_ref().is_some_and(Cancel::is_cancelled) {
        return Err(SyncError::new("sync cancelled"));
    }
    let (binary, version) = find_bazel(opts.bazel.as_deref(), root, std::env::var_os("PATH"))?;
    let bazel = Bazel {
        binary,
        workspace: root.to_path_buf(),
        output_base: (!opts.shared_output_base).then(|| layout.output_base()),
        aspect_dir: layout.aspect_dir(),
        log: log.to_path_buf(),
        echo: opts.echo_bazel,
        cancel: opts.cancel.clone(),
    };
    match version {
        Some(v) if v < MIN_BAZEL => {
            return Err(SyncError::new(format!(
                "Bazel {v} is too old; bazel-kotlin-lsp needs {MIN_BAZEL} or later"
            )));
        }
        Some(_) => {}
        None => warnings.push("unknown Bazel version (development build?)".to_string()),
    }
    if !bazel.aspect_dir.join("aspect.bzl").is_file() {
        let placeholder = RuleRepos {
            rules_java: "rules_java+".into(),
            rules_kotlin: "rules_kotlin+".into(),
        };
        write_aspect(&bazel.aspect_dir, &placeholder)?;
    }

    let info_run = checked(
        &bazel,
        "info",
        &["output_base".into(), "execution_root".into()],
        log,
    )?;
    let info: BTreeMap<&str, &str> = info_run
        .stdout
        .lines()
        .filter_map(|l| l.split_once(": "))
        .collect();
    let (Some(output_base), Some(execution_root)) =
        (info.get("output_base"), info.get("execution_root"))
    else {
        return Err(SyncError::bazel(
            "`bazel info` did not report output_base and execution_root",
            &info_run,
            log,
        ));
    };
    let graph_run = checked(
        &bazel,
        "mod",
        &["graph".into(), "--output=json".into()],
        log,
    )?;
    let graph = parse_mod_graph(&graph_run.stdout).map_err(bazel_error(&graph_run, log))?;
    let modules = discover_local_modules(&bazel, &graph, root, log)?;
    let mapping_run = checked(
        &bazel,
        "mod",
        &["dump_repo_mapping".into(), String::new()],
        log,
    )?;
    let root_mapping = parse_repo_mapping(mapping_run.stdout.lines().next().unwrap_or("{}"))
        .map_err(bazel_error(&mapping_run, log))?;
    let repos = rule_repos(&root_mapping, &graph)?;
    write_aspect(&bazel.aspect_dir, &repos)?;

    let (scope, scope_description) = choose_scope(
        opts,
        &bazel,
        start_dir,
        layout,
        &modules,
        &root_mapping,
        log,
    )?;
    timer.lap("bazel_setup");

    let stdlib = format!("@@{}//kotlin/compiler:kotlin-stdlib", repos.rules_kotlin);
    let patterns = target_patterns(&scope, &modules, std::slice::from_ref(&stdlib));
    write_lines(&layout.patterns_file(), &patterns)?;
    phase("running the aspect");
    let bep = out_dir.join("bep.json");
    let _ = std::fs::remove_file(&bep);
    let build = bazel.run(
        "build",
        &Bazel::build_args(&layout.patterns_file(), JSON_GROUP, Some(&bep)),
    )?;
    if !matches!(build.exit_code, 0 | 1) {
        return Err(SyncError::bazel(
            format!("Bazel build failed (exit {})", build.exit_code),
            &build,
            log,
        ));
    }
    let bep_text = std::fs::read_to_string(&bep).unwrap_or_default();
    let files = aspect_json_files(&bep_text).map_err(|e| {
        SyncError::bazel(
            format!(
                "could not read Bazel's build event file {}: {e:#}",
                bep.display()
            ),
            &build,
            log,
        )
    })?;
    let missing: Vec<&PathBuf> = files.local.iter().filter(|p| !p.is_file()).collect();
    if !files.not_local.is_empty() || !missing.is_empty() {
        return Err(SyncError::bazel(
            "aspect output is not on local disk; a --remote_download_* setting in .bazelrc may be keeping it remote",
            &build,
            log,
        ));
    }
    let mut outcome = classify_build(build.exit_code, files.local.len());
    let mut partial_reason = (outcome == BuildOutcome::Partial).then(|| {
        SyncError::bazel(
            format!("some targets failed to build (exit {})", build.exit_code),
            &build,
            log,
        )
    });
    if outcome == BuildOutcome::Failed {
        let message = match (build.exit_code, files.local.len()) {
            (0, 0) => "Bazel produced no aspect output".to_string(),
            (code, _) => format!("Bazel build failed (exit {code})"),
        };
        return Err(SyncError::bazel(message, &build, log));
    }
    timer.lap("aspect_build");

    let targets = read_targets(&files.local)?;
    let local_module_dirs = modules.iter().map(|m| m.abs_path.clone()).collect();
    let ctx = ModelContext {
        workspace_root: root.to_path_buf(),
        execution_root: PathBuf::from(execution_root),
        output_base: PathBuf::from(output_base),
        root_module_name: if graph.root_name.is_empty() {
            "main".into()
        } else {
            graph.root_name.clone()
        },
        local_modules: modules,
        scope,
        kotlin_stdlib: Some(stdlib),
        java_home_fallback: std::env::var_os("JAVA_HOME").map(PathBuf::from),
    };
    let jars = jar_targets(&targets, &ctx);
    if !jars.is_empty() {
        phase("building library jars");
        write_lines(&layout.jar_patterns_file(), &jars)?;
        let run = bazel.run(
            "build",
            &Bazel::build_args(&layout.jar_patterns_file(), JARS_GROUP, None),
        )?;
        if run.exit_code != 0 {
            outcome = BuildOutcome::Partial;
            partial_reason.get_or_insert_with(|| {
                SyncError::bazel(
                    format!("building library jars failed (exit {})", run.exit_code),
                    &run,
                    log,
                )
            });
            warnings.push(format!(
                "building library jars failed (exit {}); see {}",
                run.exit_code,
                log.display()
            ));
        }
    }
    timer.lap("jar_build");

    phase("writing workspace.json");
    let model = build_model(&targets, &ctx, &RealFs);
    warnings.extend(model.warnings);
    timer.lap("model");
    let changed = write_if_changed(
        &model_dir(out_dir).join("workspace.json"),
        &render(&model.workspace),
    )
    .map_err(|e| SyncError::new(format!("writing workspace.json: {e}")))?;
    timer.lap("write");

    let summary = SyncSummary {
        status: if outcome == BuildOutcome::Full {
            "ok"
        } else {
            "partial"
        }
        .into(),
        changed,
        modules: model.workspace.modules.len(),
        libraries: model.workspace.libraries.len(),
        scope: scope_description,
        warnings,
        timings_ms: timer.0.clone(),
        error: None,
        partial_reason,
        log: log.to_path_buf(),
    };
    Ok((summary, local_module_dirs))
}

/// Turns a failure to read a Bazel command's output into an error that names the command and the log.
fn bazel_error<'a>(run: &'a BazelRun, log: &'a Path) -> impl Fn(anyhow::Error) -> SyncError + 'a {
    move |e| SyncError::bazel(format!("{e:#}"), run, log)
}

fn checked(
    bazel: &Bazel,
    command: &str,
    args: &[String],
    log: &Path,
) -> Result<BazelRun, SyncError> {
    let run = bazel.run(command, args)?;
    if run.exit_code != 0 {
        return Err(SyncError::bazel(
            format!("`bazel {command}` failed (exit {})", run.exit_code),
            &run,
            log,
        ));
    }
    Ok(run)
}

fn discover_local_modules(
    bazel: &Bazel,
    graph: &ModGraph,
    root: &Path,
    log: &Path,
) -> Result<Vec<LocalModule>, SyncError> {
    if graph.overridden.is_empty() {
        return Ok(vec![]);
    }
    let mut args = vec!["show_repo".to_string()];
    args.extend(graph.overridden.iter().map(|m| format!("{m}@_")));
    let shown = checked(bazel, "mod", &args, log)?;
    Ok(local_modules(&parse_show_repo(&shown.stdout), root))
}

/// The narrowing decision for one start folder, reused while the root `MODULE.bazel` is unchanged.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct ScopeDecision {
    start: String,
    module_bazel: String,
    jvm_targets: usize,
}

fn choose_scope(
    opts: &SyncOptions,
    bazel: &Bazel,
    start_dir: &Path,
    layout: &CacheLayout,
    modules: &[LocalModule],
    root_mapping: &BTreeMap<String, String>,
    log: &Path,
) -> Result<(Scope, String), SyncError> {
    let root = bazel.workspace.as_path();
    if !opts.scope_dirs.is_empty() || !opts.targets.is_empty() {
        let cfg = ScopeConfig {
            directories: opts.scope_dirs.clone(),
            targets: opts.targets.clone(),
        };
        let cfg = resolve_directories(&cfg, root)
            .map_err(|e| SyncError::new(format!("--scope-dir: {e:#}")))?;
        let scope = Scope::from_config(&cfg, root_mapping)
            .map_err(|e| SyncError::new(format!("--target: {e:#}")))?;
        return Ok((scope, "flags".into()));
    }
    let config_file = root.join(CONFIG_FILE);
    if let Some(cfg) = load_scope_config(&config_file)? {
        let scope = resolve_directories(&cfg, root)
            .and_then(|cfg| Scope::from_config(&cfg, root_mapping))
            .map_err(|e| SyncError::new(format!("{}: {e:#}", config_file.display())))?;
        return Ok((scope, CONFIG_FILE.into()));
    }
    let rel = start_dir.strip_prefix(root).unwrap_or(Path::new(""));
    if rel.as_os_str().is_empty() {
        return Ok((Scope::whole_repo(), "whole repository".into()));
    }
    let key = ScopeDecision {
        start: rel.to_string_lossy().into_owned(),
        module_bazel: digest_hex(&std::fs::read(root.join("MODULE.bazel")).unwrap_or_default()),
        jvm_targets: 0,
    };
    let saved = std::fs::read(layout.scope_decision_file())
        .ok()
        .and_then(|b| serde_json::from_slice::<ScopeDecision>(&b).ok())
        .filter(|d| d.start == key.start && d.module_bazel == key.module_bazel);
    let jvm_targets = match saved {
        Some(decision) => decision.jvm_targets,
        None => {
            let jvm_targets = count_jvm_targets(bazel, modules, log)?;
            let decision = ScopeDecision { jvm_targets, ..key };
            write_if_changed(
                &layout.scope_decision_file(),
                &serde_json::to_vec_pretty(&decision).unwrap_or_default(),
            )
            .map_err(|e| SyncError::new(format!("saving the scope decision: {e}")))?;
            jvm_targets
        }
    };
    Ok(if jvm_targets > NARROW_THRESHOLD {
        (Scope::folder(rel), format!("folder {}", rel.display()))
    } else {
        (Scope::whole_repo(), "whole repository".into())
    })
}

fn count_jvm_targets(
    bazel: &Bazel,
    modules: &[LocalModule],
    log: &Path,
) -> Result<usize, SyncError> {
    let patterns = target_patterns(&Scope::whole_repo(), modules, &[]);
    let (positive, negative): (Vec<&String>, Vec<&String>) =
        patterns.iter().partition(|p| !p.starts_with('-'));
    let mut universe = positive
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(" + ");
    for n in negative {
        universe.push_str(" - ");
        universe.push_str(&n[1..]);
    }
    let query = format!("kind(\"(kt_jvm|java)_(library|test|binary) rule\", {universe})");
    let run = bazel.run(
        "query",
        &["--keep_going".into(), "--output=label".into(), query],
    )?;
    if run.exit_code != 0 && run.exit_code != 3 {
        return Err(SyncError::bazel(
            "`bazel query` for the target count failed",
            &run,
            log,
        ));
    }
    Ok(run.stdout.lines().filter(|l| !l.trim().is_empty()).count())
}

fn model_dir(out_dir: &Path) -> PathBuf {
    out_dir.join("model")
}

fn write_aspect(dir: &Path, repos: &RuleRepos) -> Result<(), SyncError> {
    for (name, content) in render_aspect(repos) {
        write_if_changed(&dir.join(name), content.as_bytes())
            .map_err(|e| SyncError::new(format!("writing the aspect: {e}")))?;
    }
    Ok(())
}

fn write_lines(path: &Path, lines: &[String]) -> Result<(), SyncError> {
    write_if_changed(path, (lines.join("\n") + "\n").as_bytes())
        .map(|_| ())
        .map_err(|e| SyncError::new(format!("{}: {e}", path.display())))
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
