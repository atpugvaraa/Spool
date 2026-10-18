//! `spool-cli` — Spool's headless, agent-facing surface over the vendored figcraft engine.
//!
//! One invocation does one thing. **stdout is data, stderr is diagnostics**: in `--json` mode stdout
//! carries exactly one JSON value and nothing else, and failures print the engine's own error on
//! stderr and exit non-zero (`2` for bad arguments, `1` for a failed operation). There is no
//! window, no dialog and no prompt: a command that would ask the interface for something has its
//! request reported on stderr and otherwise ignored.
//!
//! One thing is deliberately out of reach: the engine's WebAssembly plugin sandbox. The engine
//! registers its commands, but `Agents.md` defers the subsystem, so `list-commands` does not list
//! them and `run`/`script` refuse them by name and by policy. See [`deferred`].
//!
//! What this is NOT: it does not open Spool projects. A `.spool` project is a *directory* of
//! authored HTML/CSS/SVG plus `lamine.yaml`; this crate may only depend on the engine crates, so it
//! reads and writes the engine's own `.figcraft` / `.json` documents and refuses `.spool` paths
//! with [`document::SPOOL_UNSUPPORTED`] rather than pretending otherwise.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod args;
mod deferred;
mod document;
mod export;
mod out;
mod script;

use std::io::{self, Write};
use std::process::ExitCode;

use args::{Args, Spec};
use figcraft_engine::Session;
use out::Out;
use serde_json::{json, Value};

pub const USAGE: &str = "\
spool-cli — Spool's document engine, without a window

USAGE:
  spool-cli list-commands [--filter <substr>] [--json]
  spool-cli run <command-id> [--params '<json>'] [--in <file>] [--out <file>] [--json]
  spool-cli script <file.json> [--in <file>] [--out <file>] [--json]
  spool-cli open <path> [--json]
  spool-cli save <path> --in <file> [--json]
  spool-cli inspect [--in <file>] [--depth N] [--all-pages] [--json]
  spool-cli export <file> --out <out> [--format png|jpg|svg|pdf|webp] [--scale N]
                            [--page NAME] [--node ID|NAME] [--json]
  spool-cli help | --help | -h | --version | -V

COMMANDS
  list-commands  Every engine command: id, label, whether it edits the document, its shortcut
                 and its parameter documentation. --filter matches id, label or params.
                 The WebAssembly plugin sandbox (plugin.*, plugin.directory.*, widget.*) is
                 deferred by policy: it is neither listed nor runnable here. See Agents.md.
  run             Run one command (the same ids `list-commands` prints) with JSON params. Prints
                 the command's JSON result. --in opens a document first, --out saves it after.
  script          Run a JSON array of {\"id\", \"params\"} steps as ONE transaction: a failing step
                 rolls every earlier step back and leaves no undo steps behind. A param of
                 {\"$step\": n} becomes the \"id\" field of an earlier step's result
                 (add \"field\": \"name\" for another field). Prints one result per step.
  open            Read a document and print its shape (pages, top-level frames, counts).
  save            Write a document read with --in to <path> (.figcraft, with its version history,
                 or .json).
  inspect         Dump the document as JSON (Figma-shaped: pages, layers, frames, text, paints,
                 auto layout). --depth limits the tree (default 3), --all-pages dives every page.
  export          Render to png, jpg, webp, svg or pdf. The format comes from --format or from
                 --out's extension; --scale multiplies the pixel size (vector formats ignore it).

OUTPUT
  stdout is data: in --json mode it is one JSON value and nothing else. Diagnostics (import notes,
  ignored interface requests) go to stderr. Exit code 2 = bad arguments, 1 = the operation failed,
  0 = success. There are no prompts: a command that needs a dialog is reported and ignored.

FILES
  .figcraft (the engine's native document) and .json. .svg and images are imported by `open`.

  .spool PROJECTS ARE NOT SUPPORTED. A Spool project is a directory (lamine.yaml plus authored
  HTML/CSS/SVG), not a document file, and spool-cli cannot depend on the crates that load one.
  This tool reads and writes the engine's own documents; opening a .spool project is not
  implemented.

TEXT
  Laid out with the fonts built into the engine only (the system font index is a crate spool-cli
  may not depend on), so an export is the same on every machine.";

/// Everything that can go wrong, and the exit code it maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// Bad arguments: the usage is printed too, exit code 2.
    Usage(String),
    /// The operation failed: exit code 1.
    Failed(String),
}

impl CliError {
    pub fn code(&self) -> u8 {
        match self {
            CliError::Usage(_) => 2,
            CliError::Failed(_) => 1,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Usage(m) | CliError::Failed(m) => f.write_str(m),
        }
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        CliError::Failed(message)
    }
}

pub type CliResult<T> = Result<T, CliError>;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let code = run(&argv, &mut stdout, &mut stderr);
    let _ = stdout.flush();
    let _ = stderr.flush();
    ExitCode::from(code)
}

/// Run one invocation. Returns the process exit code: 0, 1 or 2.
pub fn run(argv: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
    match dispatch(argv, stdout, stderr) {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(stderr, "error: {e}");
            if let CliError::Usage(_) = e {
                let _ = writeln!(stderr, "\n{USAGE}");
            }
            e.code()
        }
    }
}

fn dispatch(argv: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> CliResult<()> {
    // `--json` is global and may appear anywhere, before or after the subcommand.
    let (json, argv) = split_json(argv);
    let Some((verb, rest)) = argv.split_first() else {
        return Err(CliError::Usage("missing subcommand".into()));
    };
    match verb.as_str() {
        "help" | "--help" | "-h" => {
            let _ = writeln!(stdout, "{USAGE}");
            return Ok(());
        }
        "--version" | "-V" | "version" => {
            let _ = writeln!(stdout, "spool-cli {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    let mut out = Out::new(stdout, stderr, json);
    match verb.as_str() {
        "list-commands" => {
            let a = parse(rest, SPEC_LIST)?;
            list_commands(&mut out, &a)
        }
        "run" => {
            let a = parse(rest, SPEC_RUN)?;
            run_command(&mut out, &a)
        }
        "script" => {
            let a = parse(rest, SPEC_SCRIPT)?;
            run_script(&mut out, &a)
        }
        "open" => {
            let a = parse(rest, SPEC_OPEN)?;
            open(&mut out, &a)
        }
        "save" => {
            let a = parse(rest, SPEC_SAVE)?;
            save(&mut out, &a)
        }
        "inspect" => {
            let a = parse(rest, SPEC_INSPECT)?;
            inspect(&mut out, &a)
        }
        "export" => {
            let a = parse(rest, SPEC_EXPORT)?;
            export_page(&mut out, &a)
        }
        other => Err(CliError::Usage(format!("unknown subcommand `{other}`"))),
    }
}

fn parse(argv: &[String], spec: Spec) -> CliResult<Args> {
    Args::parse(argv, &spec).map_err(CliError::Usage)
}

/// Take the global `--json` flag out of `argv`, wherever it is.
fn split_json(argv: &[String]) -> (bool, Vec<String>) {
    let mut json = false;
    let mut rest = Vec::with_capacity(argv.len());
    for arg in argv {
        if arg == "--json" {
            json = true;
        } else {
            rest.push(arg.clone());
        }
    }
    (json, rest)
}

const SPEC_LIST: Spec = Spec {
    values: &["filter"],
    flags: &[],
    aliases: &[],
};

const SPEC_RUN: Spec = Spec {
    values: &["params", "in", "out"],
    flags: &[],
    aliases: &[("o", "out")],
};

const SPEC_SCRIPT: Spec = Spec {
    values: &["in", "out"],
    flags: &[],
    aliases: &[("o", "out")],
};

const SPEC_OPEN: Spec = Spec {
    values: &[],
    flags: &[],
    aliases: &[],
};

const SPEC_SAVE: Spec = Spec {
    values: &["in"],
    flags: &[],
    aliases: &[],
};

const SPEC_INSPECT: Spec = Spec {
    values: &["in", "depth"],
    flags: &["all-pages"],
    aliases: &[],
};

const SPEC_EXPORT: Spec = Spec {
    values: &["out", "format", "scale", "page", "node"],
    flags: &[],
    aliases: &[("o", "out")],
};

/// Every registered engine command, as agents see it.
fn list_commands(out: &mut Out, a: &Args) -> CliResult<()> {
    a.none_positional("list-commands")
        .map_err(CliError::Usage)?;
    let filter = a.value("filter").map(str::to_lowercase);
    let matches = |haystack: &str| {
        filter
            .as_ref()
            .is_none_or(|f| haystack.to_lowercase().contains(f))
    };
    // Deferred commands are filtered out before the filter matches, so `--filter plugin` cannot
    // even mention one (see `deferred`: the WebAssembly plugin sandbox is out of scope).
    let list: Vec<&figcraft_engine::CommandSpec> = figcraft_engine::all_commands()
        .iter()
        .filter(|c| !deferred::is_deferred(c.id))
        .filter(|c| matches(c.id) || matches(c.label) || matches(c.params))
        .collect();
    if out.json() {
        let items: Vec<Value> = list
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "label": c.label,
                    "params": c.params,
                    "menu": c.menu(),
                    "shortcut": c.shortcut(),
                    "editsDocument": c.history,
                })
            })
            .collect();
        out.data(&Value::Array(items));
    } else {
        out.text(&format!("{} command(s)", list.len()));
        for c in list {
            let shortcut = c.shortcut().map(|s| format!("  ({s})")).unwrap_or_default();
            out.text(&format!("{:<34} {}{}", c.id, c.label, shortcut));
            out.text(&format!(
                "    edits document: {}",
                if c.history { "yes" } else { "no" }
            ));
            if let Some(menu) = c.menu() {
                out.text(&format!("    menu: {menu}"));
            }
            out.text(&format!("    params: {}", c.params));
        }
    }
    out.check()
}

/// One command, in a session.
fn run_command(out: &mut Out, a: &Args) -> CliResult<()> {
    let id = a
        .one("run <command-id> [--params '<json>']")
        .map_err(CliError::Usage)?;
    let params = params_of(a.value("params"))?;
    // Refuse a bad command before a document is opened, so the error names the real problem.
    // A deferred one is refused first: it *is* registered, so "unknown command" would be false,
    // and running it would be the thing Spool's scope policy forbids.
    if deferred::is_deferred(id) {
        return Err(CliError::Failed(deferred::refusal(id)));
    }
    if figcraft_engine::find_command(id).is_none() {
        return Err(CliError::Failed(format!(
            "unknown command `{id}` (see `spool-cli list-commands --filter {id}`)"
        )));
    }
    let (mut session, notes) = document::session(a.value("in"))?;
    report_notes(out, &notes);
    let result = session
        .execute(id, &params)
        .map_err(|e| CliError::Failed(format!("`{id}` failed: {e}")))?;
    drain_ui_requests(out, &mut session);
    out.data(&result);
    if let Some(path) = a.value("out") {
        let saved = document::save_session(&session, path)?;
        report_saved(out, &saved);
    }
    out.check()
}

/// A whole task: a JSON array of steps, run as one transaction.
fn run_script(out: &mut Out, a: &Args) -> CliResult<()> {
    let file = a.one("script <file.json>").map_err(CliError::Usage)?;
    let text = std::fs::read_to_string(file)
        .map_err(|e| CliError::Failed(format!("can't read {file}: {e}")))?;
    let steps =
        script::parse_script(&text).map_err(|e| CliError::Failed(format!("{file}: {e}")))?;
    // The whole script is refused before anything opens, so no step can reach a deferred command.
    if let Some((step, id)) = steps
        .iter()
        .enumerate()
        .find(|(_, s)| deferred::is_deferred(&s.id))
        .map(|(i, s)| (i, s.id.as_str()))
    {
        return Err(CliError::Failed(deferred::script_refusal(step, id)));
    }
    let (mut session, notes) = document::session(a.value("in"))?;
    report_notes(out, &notes);
    let run = script::execute_all(&mut session, &steps);
    if out.json() {
        out.data(&Value::Array(run.steps.clone()));
    } else {
        for record in &run.steps {
            let step = record["step"].as_u64().unwrap_or_default();
            let command = record["command"].as_str().unwrap_or_default();
            if record["ok"] == Value::Bool(true) {
                let result = serde_json::to_string(&record["result"]).unwrap_or_default();
                out.text(&format!("step {step}  {command}  ok  {result}"));
            } else {
                let error = record["error"].as_str().unwrap_or_default();
                out.text(&format!("step {step}  {command}  FAILED  {error}"));
            }
        }
    }
    drain_ui_requests(out, &mut session);
    out.check()?;
    if let Some(e) = &run.error {
        let step = run
            .steps
            .iter()
            .rev()
            .find(|r| r["ok"] == Value::Bool(false))
            .and_then(|r| r["step"].as_u64())
            .unwrap_or_default();
        return Err(CliError::Failed(format!(
            "script step {step} failed: {e} (the whole script was rolled back — the document is as it was before it)"
        )));
    }
    if let Some(path) = a.value("out") {
        let saved = document::save_session(&session, path)?;
        report_saved(out, &saved);
    }
    Ok(())
}

/// Read a document and report its shape.
fn open(out: &mut Out, a: &Args) -> CliResult<()> {
    let path = a.one("open <path>").map_err(CliError::Usage)?;
    let (doc, notes) = document::load(path)?;
    let summary = document::summary(&doc, &notes, path);
    report_notes(out, &notes);
    if out.json() {
        out.data(&summary);
    } else {
        out.text(&format!(
            "{path} — {} ({} nodes, {} styles, {} variables, {} images)",
            doc.name,
            doc.node_count(),
            doc.styles.len(),
            doc.variables.variables.len(),
            doc.images.len()
        ));
        for page in summary["pages"].as_array().into_iter().flatten() {
            out.text(&format!(
                "page {} {:?}: {} nodes, {} top-level",
                page["id"].as_u64().unwrap_or_default(),
                page["name"].as_str().unwrap_or_default(),
                page["nodes"].as_u64().unwrap_or_default(),
                page["topLevel"].as_u64().unwrap_or_default()
            ));
            for frame in page["frames"].as_array().into_iter().flatten() {
                out.text(&format!(
                    "  frame {} {:?} {}×{}",
                    frame["id"].as_u64().unwrap_or_default(),
                    frame["name"].as_str().unwrap_or_default(),
                    frame["width"].as_f64().unwrap_or_default(),
                    frame["height"].as_f64().unwrap_or_default()
                ));
            }
        }
        out.text("(inspect it with `spool-cli inspect`; it is NOT a .spool project)");
    }
    out.check()
}

/// Copy a document read with `--in` to `path`.
fn save(out: &mut Out, a: &Args) -> CliResult<()> {
    let path = a.one("save <path> --in <file>").map_err(CliError::Usage)?;
    // Refuse a `.spool` target before reading the input, so the error is about the project format.
    document::guard_project(std::path::Path::new(path), "can't save")?;
    let input = a.value("in").ok_or_else(|| {
        CliError::Usage(
            "save needs `--in <file>`: spool-cli keeps no document between invocations. \
             Use `script --out` to save what a batch made, or `save --in` to copy an existing file."
                .into(),
        )
    })?;
    let (doc, notes) = document::load(input)?;
    report_notes(out, &notes);
    let saved = document::save_document(&doc, path)?;
    report_saved(out, &saved);
    out.check()
}

/// The document as JSON, shaped the way a designer reads it.
fn inspect(out: &mut Out, a: &Args) -> CliResult<()> {
    let (session, notes) = document::session(a.value("in"))?;
    report_notes(out, &notes);
    let depth = depth_of(a.value("depth"))?;
    let value = figcraft_engine::inspect::document(&session, depth, a.flag("all-pages"));
    // JSON in both modes: the dump IS the output, so `--json` changes nothing here.
    out.data(&value);
    out.check()
}

/// A page, frame or layer written to a file.
fn export_page(out: &mut Out, a: &Args) -> CliResult<()> {
    let input = a
        .one("export <file> --out <out>")
        .map_err(CliError::Usage)?;
    let out_path = a
        .value("out")
        .ok_or_else(|| CliError::Usage("export needs `--out <path>`".into()))?;
    let format = match a.value("format") {
        Some(f) => export::format(f)?,
        None => export::format(&document::extension(out_path))?,
    };
    let scale = export::scale(a.value("scale"))?;
    let (doc, notes) = document::load(input)?;
    report_notes(out, &notes);
    let page = document::page_named(&doc, a.value("page"))?;
    let ids: Vec<_> = match a.value("node") {
        Some(q) => vec![document::find_node(&doc, page, q)?],
        None => doc
            .node(page)
            .map(|p| p.children.clone())
            .unwrap_or_default(),
    };
    let exported = export::encode(&doc, &ids, format, scale)?;
    document::write_atomic(std::path::Path::new(out_path), &exported.bytes)?;
    if out.json() {
        out.data(&exported.report(out_path, format, scale));
    } else {
        out.text(&exported.describe(out_path, format));
    }
    out.check()
}

/// `--params` is a JSON object; absent means no parameters.
fn params_of(text: Option<&str>) -> CliResult<Value> {
    let Some(text) = text else {
        return Ok(json!({}));
    };
    let value: Value = serde_json::from_str(text)
        .map_err(|e| CliError::Usage(format!("--params is not valid JSON: {e}")))?;
    if !value.is_object() {
        return Err(CliError::Usage("--params must be a JSON object".into()));
    }
    Ok(value)
}

/// `--depth N`, clamped: deeper than a document's own nesting buys nothing.
fn depth_of(text: Option<&str>) -> CliResult<usize> {
    let Some(text) = text else { return Ok(3) };
    let n: usize = text
        .parse()
        .map_err(|_| CliError::Usage(format!("--depth needs a whole number, got `{text}`")))?;
    Ok(n.min(64))
}

/// The loader's notes (repairs, import warnings) are diagnostics, never data.
fn report_notes(out: &mut Out, notes: &[String]) {
    for note in notes {
        out.note(note);
    }
}

/// A saved file is reported on stderr in `--json` mode, so stdout stays one JSON value.
fn report_saved(out: &mut Out, saved: &Value) {
    out.note(&format!(
        "wrote {} ({} bytes)",
        saved["path"].as_str().unwrap_or_default(),
        saved["bytes"].as_u64().unwrap_or_default()
    ));
}

/// Commands only *ask* the interface for dialogs; headless, the requests are reported and dropped.
/// Nothing here blocks or prompts.
fn drain_ui_requests(out: &mut Out, session: &mut Session) {
    for request in std::mem::take(&mut session.ui_requests) {
        out.note(&format!(
            "a command asked the interface for `{}`: spool-cli has no window or dialog, so the request was ignored",
            request.kind
        ));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// One invocation: the exit code, stdout and stderr.
    struct Invocation {
        code: u8,
        stdout: String,
        stderr: String,
    }

    fn cli(args: &[&str]) -> Invocation {
        let argv: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&argv, &mut out, &mut err);
        Invocation {
            code,
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
        }
    }

    #[test]
    fn exit_codes_are_two_for_usage_one_for_failure_zero_for_success() {
        assert_eq!(CliError::Usage("x".into()).code(), 2);
        assert_eq!(CliError::Failed("x".into()).code(), 1);
        assert_eq!(CliError::from("boom".to_string()).code(), 1);
    }

    #[test]
    fn no_subcommand_and_an_unknown_subcommand_are_usage_errors() {
        let r = cli(&[]);
        assert_eq!(r.code, 2);
        assert_eq!(r.stdout, "");
        assert!(r.stderr.contains("missing subcommand"));

        let r = cli(&["frobnicate"]);
        assert_eq!(r.code, 2);
        assert_eq!(r.stdout, "");
        assert!(r.stderr.contains("unknown subcommand `frobnicate`"));
        // A usage error prints the usage, on stderr.
        assert!(r.stderr.contains("USAGE:"));
        assert!(r.stderr.contains(".spool PROJECTS ARE NOT SUPPORTED"));
    }

    #[test]
    fn help_and_version_go_to_stdout_and_succeed() {
        let r = cli(&["--help"]);
        assert_eq!(r.code, 0);
        assert!(r.stdout.contains("spool-cli list-commands"));
        assert_eq!(r.stderr, "");
        for verb in ["help", "-h"] {
            assert_eq!(cli(&[verb]).code, 0);
        }
        let r = cli(&["--version"]);
        assert_eq!(r.code, 0);
        assert!(r.stdout.starts_with("spool-cli "));
        assert_eq!(r.stderr, "");
    }

    #[test]
    fn bad_arguments_print_to_stderr_and_leave_stdout_empty() {
        let r = cli(&["run"]);
        assert_eq!(r.code, 2);
        assert_eq!(r.stdout, "");

        let r = cli(&["run", "node.create", "--params", "not json"]);
        assert_eq!(r.code, 2);
        assert_eq!(r.stdout, "");
        assert!(r.stderr.contains("--params is not valid JSON"));

        let r = cli(&["run", "node.create", "--params", "[1]"]);
        assert_eq!(r.code, 2);
        assert!(r.stderr.contains("--params must be a JSON object"));

        let r = cli(&["run", "node.create", "--nope", "1"]);
        assert_eq!(r.code, 2);
        assert!(r.stderr.contains("unknown option `--nope`"));

        let r = cli(&["list-commands", "extra"]);
        assert_eq!(r.code, 2);
        assert!(r.stderr.contains("takes no positional arguments"));
    }

    #[test]
    fn an_unknown_command_is_an_operation_failure_not_a_usage_error() {
        let r = cli(&["run", "node.nonesuch"]);
        assert_eq!(r.code, 1);
        assert_eq!(r.stdout, "");
        assert!(r.stderr.contains("unknown command `node.nonesuch`"));
        assert!(
            r.stderr.contains("list-commands"),
            "the error says where to look"
        );
    }

    // ---- The deferred plugin sandbox: unreachable through the CLI (see `deferred`) ----

    #[test]
    fn no_listed_command_is_a_deferred_plugin_command() {
        // Every filter that could surface a plugin id, not just "plugin": a sandbox command whose
        // label or parameter documentation mentions a plugin must not appear either.
        for filter in [
            "plugin",
            "plugins",
            "widget",
            "wasm",
            "sandbox",
            "runLast",
            "manage",
            "import",
            "directory",
        ] {
            let r = cli(&["list-commands", "--filter", filter, "--json"]);
            assert_eq!(r.code, 0, "stderr: {}", r.stderr);
            let items: Vec<Value> = serde_json::from_str(&r.stdout).unwrap();
            for item in &items {
                let id = item["id"].as_str().unwrap();
                assert!(
                    !deferred::is_deferred(id),
                    "`{id}` matched --filter {filter}"
                );
            }
        }
    }

    #[test]
    fn a_filter_naming_the_plugin_sandbox_lists_nothing() {
        let r = cli(&["list-commands", "--filter", "plugin", "--json"]);
        assert_eq!(r.code, 0);
        let items: Vec<Value> = serde_json::from_str(&r.stdout).unwrap();
        for item in &items {
            // `node.set`'s parameter documentation mentions `pluginData`, a document property
            // rather than the sandbox: it is a real command and stays listed.
            let id = item["id"].as_str().unwrap();
            assert!(
                !id.starts_with("plugin") && !id.starts_with("widget"),
                "`{id}` is a sandbox command"
            );
        }
        // Human mode agrees with JSON mode.
        let r = cli(&["list-commands", "--filter", "plugin"]);
        assert_eq!(r.code, 0);
        assert!(!r.stdout.contains("plugin.run"));
        assert!(!r.stdout.contains("plugin.directory"));
        assert!(!r.stdout.contains("widget."));
    }

    #[test]
    fn the_whole_registry_is_filtered_the_same_way() {
        // The unfiltered listing is the whole visible surface: same count as the registry minus
        // the deferred ids, and not one deferred id in it.
        let r = cli(&["list-commands", "--json"]);
        assert_eq!(r.code, 0);
        let items: Vec<Value> = serde_json::from_str(&r.stdout).unwrap();
        let all = figcraft_engine::all_commands().len();
        assert_eq!(items.len(), all - deferred::deferred().len());
        for item in &items {
            assert!(!deferred::is_deferred(item["id"].as_str().unwrap()));
        }
    }

    #[test]
    fn run_refuses_every_deferred_command_with_the_policy_on_stderr() {
        for id in deferred::deferred() {
            let r = cli(&["run", id, "--json"]);
            assert_ne!(r.code, 0, "`{id}` must not succeed");
            assert_eq!(r.code, 1, "`{id}` is a refusal, not a usage error");
            assert_eq!(r.stdout, "", "stdout is data: a refusal produces none");
            assert!(r.stderr.contains("deferred"), "`{id}`: {}", r.stderr);
            assert!(r.stderr.contains("Agents.md"), "`{id}`: {}", r.stderr);
            assert!(r.stderr.contains(&format!("`{id}`")), "`{id}` is named");
            assert!(
                !r.stderr.contains(&format!("unknown command `{id}`")),
                "`{id}` is registered: calling it unknown would be false"
            );
        }
    }

    #[test]
    fn a_deferred_command_is_refused_before_a_document_is_read() {
        // `--in` points at a file that does not exist: the error must still be the policy, so the
        // refusal cannot be bypassed by supplying a document (or by the order of the checks).
        let r = cli(&[
            "run",
            "plugin.run",
            "--in",
            "/nonexistent/nope.figcraft",
            "--json",
        ]);
        assert_eq!(r.code, 1);
        assert_eq!(r.stdout, "");
        assert!(r.stderr.contains("deferred"));
        assert!(!r.stderr.contains("can't open"));
    }

    #[test]
    fn script_refuses_a_deferred_step_and_runs_nothing() {
        let dir = scratch("deferred-script");
        let path = dir.join("steps.json");
        std::fs::write(
            &path,
            r#"[{"id": "node.create", "params": {"type": "frame", "name": "Made", "width": 8, "height": 8}},
                {"id": "plugin.import", "params": {"path": "p.json"}}]"#,
        )
        .unwrap();
        let doc = dir.join("never.figcraft");
        let r = cli(&[
            "script",
            path.to_str().unwrap(),
            "--out",
            doc.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 1);
        assert_eq!(r.stdout, "", "the script is refused whole: no step results");
        assert!(r.stderr.contains("script step 1"), "stderr: {}", r.stderr);
        assert!(r.stderr.contains("`plugin.import`"));
        assert!(r.stderr.contains("Agents.md"));
        assert!(!doc.exists(), "nothing ran, so nothing was written");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collaboration_is_untouched_by_the_filter() {
        // Listed...
        for id in ["collab.host", "collab.join", "collab.peers"] {
            let r = cli(&["list-commands", "--filter", id, "--json"]);
            assert_eq!(r.code, 0);
            let items: Vec<Value> = serde_json::from_str(&r.stdout).unwrap();
            assert!(
                items.iter().any(|i| i["id"] == id),
                "`{id}` is assigned by Agents.md and must stay listed"
            );
        }
        // ...and runnable: `collab.peers` answers headlessly, so a real result comes back on
        // stdout with no policy refusal anywhere.
        let r = cli(&["run", "collab.peers", "--json"]);
        assert_eq!(r.code, 0, "stderr: {}", r.stderr);
        assert!(!r.stderr.contains("deferred"), "stderr: {}", r.stderr);
        let value: Value = serde_json::from_str(&r.stdout).unwrap();
        assert_eq!(value["peers"], json!([]));
        // A collaboration command that cannot run here fails with the engine's own reason, not
        // with the CLI's policy.
        let r = cli(&["run", "collab.host", "--json"]);
        assert_eq!(r.code, 1);
        assert!(!r.stderr.contains("deferred"), "stderr: {}", r.stderr);
        assert!(r.stderr.contains("collab.host"), "the engine names it");
    }

    #[test]
    fn a_missing_file_is_a_failure_and_stdout_stays_empty() {
        let r = cli(&["open", "/nonexistent/nope.figcraft"]);
        assert_eq!(r.code, 1);
        assert_eq!(r.stdout, "");
        assert!(r.stderr.starts_with("error: can't open"));

        let r = cli(&["script", "/nonexistent/nope.json"]);
        assert_eq!(r.code, 1);
        assert_eq!(r.stdout, "");
        assert!(r.stderr.contains("can't read"));
    }

    #[test]
    fn a_spool_project_is_refused_with_the_reason_on_stderr() {
        let r = cli(&["open", "/tmp/MyProject.spool"]);
        assert_eq!(r.code, 1);
        assert_eq!(r.stdout, "", "stdout is data: a refusal produces none");
        assert!(r.stderr.contains("is not supported"));
        assert!(r.stderr.contains("NOT implemented"));
        // The same for a save target.
        let r = cli(&["save", "/tmp/x.spool", "--in", "/tmp/y.figcraft"]);
        assert_eq!(r.code, 1);
        assert!(r.stderr.contains("is not supported"));
    }

    #[test]
    fn json_mode_puts_json_on_stdout_and_nothing_on_stderr() {
        let r = cli(&["list-commands", "--filter", "align.", "--json"]);
        assert_eq!(r.code, 0);
        assert_eq!(r.stderr, "", "a clean run says nothing on stderr");
        let value: Value = serde_json::from_str(&r.stdout).unwrap();
        let items = value.as_array().unwrap();
        assert!(!items.is_empty());
        for item in items {
            let id = item["id"].as_str().unwrap();
            assert!(id.contains("align."), "{id} should have matched the filter");
            assert!(item["label"].is_string());
            assert!(item["editsDocument"].is_boolean());
            assert!(item["params"].is_string());
        }
    }

    #[test]
    fn human_mode_prints_a_summary_not_json() {
        let r = cli(&["list-commands", "--filter", "align.left"]);
        assert_eq!(r.code, 0);
        assert_eq!(r.stderr, "");
        assert!(r.stdout.contains("command(s)"));
        assert!(r.stdout.contains("align.left"));
        assert!(r.stdout.contains("edits document:"));
    }

    #[test]
    fn json_is_a_flag_that_may_appear_anywhere() {
        let a = cli(&["--json", "list-commands", "--filter", "align.left"]);
        let b = cli(&["list-commands", "--filter", "align.left", "--json"]);
        assert_eq!(a.stdout, b.stdout);
        assert!(serde_json::from_str::<Value>(&a.stdout).is_ok());
    }

    #[test]
    fn an_empty_filter_result_is_still_a_success_with_an_empty_array() {
        let r = cli(&[
            "list-commands",
            "--filter",
            "definitely-not-a-command",
            "--json",
        ]);
        assert_eq!(r.code, 0);
        assert_eq!(r.stdout.trim(), "[]");
    }

    #[test]
    fn params_and_depth_parsing_is_reported_as_usage() {
        assert_eq!(params_of(None).unwrap(), json!({}));
        assert_eq!(params_of(Some("{}")).unwrap(), json!({}));
        assert_eq!(params_of(Some(r#"{"a":1}"#)).unwrap(), json!({"a": 1}));
        assert_eq!(depth_of(None).unwrap(), 3);
        assert_eq!(depth_of(Some("0")).unwrap(), 0);
        assert_eq!(depth_of(Some("1000")).unwrap(), 64);
        assert!(depth_of(Some("-1")).is_err());
        assert!(depth_of(Some("deep")).is_err());
    }

    #[test]
    fn a_malformed_script_is_a_failure_that_names_the_file() {
        let dir = std::env::temp_dir().join(format!(
            "spool-cli-script-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.json");
        std::fs::write(&path, "[{\"id\": 3}]").unwrap();
        let r = cli(&["script", path.to_str().unwrap()]);
        assert_eq!(r.code, 1);
        assert_eq!(r.stdout, "");
        assert!(
            r.stderr.contains("script"),
            "the file is named: {}",
            r.stderr
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn export_needs_an_output_and_a_known_format() {
        let r = cli(&["export", "in.figcraft"]);
        assert_eq!(r.code, 2);
        assert!(r.stderr.contains("export needs `--out <path>`"));
        let r = cli(&["export", "/nonexistent/a.figcraft", "--out", "b.tiff"]);
        assert_eq!(r.code, 2);
        assert!(r.stderr.contains("unknown export format `tiff`"));
    }

    #[test]
    fn save_without_an_input_explains_the_stateless_shape() {
        let r = cli(&["save", "/tmp/out.figcraft"]);
        assert_eq!(r.code, 2);
        assert_eq!(r.stdout, "");
        assert!(r.stderr.contains("save needs `--in <file>`"));
    }

    /// A fresh directory for one test, removed by it.
    fn scratch(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "spool-cli-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_command_that_wants_a_dialog_is_reported_and_ignored_never_blocks() {
        // `layers.collapseAll` asks the interface to expand the panel; headless, the request is
        // reported on stderr, the command still succeeds, and stdout stays one JSON value.
        let r = cli(&["run", "layers.collapseAll", "--json"]);
        assert_eq!(r.code, 0, "stderr: {}", r.stderr);
        let value: Value = serde_json::from_str(&r.stdout).unwrap();
        assert_eq!(value["ok"], true);
        assert!(
            r.stderr
                .contains("asked the interface for `layers.collapseAll`"),
            "stderr: {}",
            r.stderr
        );
        assert!(r.stderr.contains("ignored"));
    }

    #[test]
    fn the_whole_loop_builds_opens_inspects_and_exports_a_document() {
        let dir = scratch("loop");
        let doc_path = dir.join("made.figcraft");
        let script_path = dir.join("steps.json");
        std::fs::write(
            &script_path,
            r#"[
              {"id": "node.create", "params": {"type": "frame", "name": "Hero", "width": 64, "height": 48}},
              {"id": "shape.rectangle", "params": {"x": 8, "y": 8, "width": 16, "height": 16, "parent": {"$step": 0}}}
            ]"#,
        )
        .unwrap();
        let (script, doc) = (script_path.to_str().unwrap(), doc_path.to_str().unwrap());

        // script: two steps, one transaction, the document saved.
        let r = cli(&["script", script, "--out", doc, "--json"]);
        assert_eq!(r.code, 0, "stderr: {}", r.stderr);
        // stdout is the step results; the file it wrote is a diagnostic, so it goes to stderr.
        assert!(r.stderr.starts_with("note: wrote "), "stderr: {}", r.stderr);
        let steps: Vec<Value> = serde_json::from_str(&r.stdout).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["command"], "node.create");
        assert_eq!(steps[0]["ok"], true);
        assert!(steps[0]["result"]["id"].is_number());
        assert_eq!(steps[1]["ok"], true);
        assert!(doc_path.is_file());

        // open: the shape of what was made.
        let r = cli(&["open", doc, "--json"]);
        assert_eq!(r.code, 0, "stderr: {}", r.stderr);
        let info: Value = serde_json::from_str(&r.stdout).unwrap();
        assert_eq!(info["pages"][0]["frames"][0]["name"], "Hero");
        assert_eq!(info["spoolProjectSupported"], false);

        // inspect: the tree.
        let r = cli(&["inspect", "--in", doc, "--depth", "4"]);
        assert_eq!(r.code, 0, "stderr: {}", r.stderr);
        let dump: Value = serde_json::from_str(&r.stdout).unwrap();
        let page = &dump["pages"][0];
        assert_eq!(page["children"][0]["name"], "Hero");
        assert_eq!(page["children"][0]["children"][0]["type"], "RECTANGLE");

        // export: png (scaled), svg and pdf from the same file.
        let png = dir.join("hero.png");
        let r = cli(&[
            "export",
            doc,
            "--out",
            png.to_str().unwrap(),
            "--scale",
            "0.5",
            "--json",
        ]);
        assert_eq!(r.code, 0, "stderr: {}", r.stderr);
        let report: Value = serde_json::from_str(&r.stdout).unwrap();
        assert_eq!(report["format"], "png");
        assert_eq!(report["width"], 32);
        assert_eq!(report["height"], 24);
        let bytes = std::fs::read(&png).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");

        let svg = dir.join("hero.svg");
        assert_eq!(cli(&["export", doc, "-o", svg.to_str().unwrap()]).code, 0);
        assert!(std::fs::read_to_string(&svg).unwrap().contains("<svg"));

        let pdf = dir.join("hero.pdf");
        assert_eq!(cli(&["export", doc, "-o", pdf.to_str().unwrap()]).code, 0);
        assert_eq!(&std::fs::read(&pdf).unwrap()[..4], b"%PDF");

        // save: a copy of the document, in both native and JSON form.
        let copy = dir.join("copy.figcraft");
        let r = cli(&["save", copy.to_str().unwrap(), "--in", doc, "--json"]);
        assert_eq!(r.code, 0, "stderr: {}", r.stderr);
        assert_eq!(
            r.stdout, "",
            "a save reports on stderr so stdout stays one value"
        );
        assert!(r.stderr.contains("wrote"));
        assert_eq!(cli(&["open", copy.to_str().unwrap(), "--json"]).code, 0);
        let as_json = dir.join("copy.json");
        assert_eq!(
            cli(&["save", as_json.to_str().unwrap(), "--in", doc]).code,
            0
        );
        assert!(serde_json::from_str::<Value>(&std::fs::read_to_string(&as_json).unwrap()).is_ok());

        // run: one command against the saved copy.
        let r = cli(&[
            "run",
            "node.create",
            "--params",
            r#"{"type": "ellipse", "name": "Dot", "width": 10, "height": 10}"#,
            "--in",
            copy.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "stderr: {}", r.stderr);
        let created: Value = serde_json::from_str(&r.stdout).unwrap();
        assert!(created["id"].is_number());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_script_exits_one_and_saves_nothing() {
        let dir = scratch("failing");
        let script = dir.join("steps.json");
        let doc = dir.join("never.figcraft");
        std::fs::write(
            &script,
            r#"[{"id": "node.create", "params": {"type": "frame", "name": "Doomed", "width": 40, "height": 40}},
                {"id": "node.set", "params": {"ids": [999], "props": {"name": "x"}}}]"#,
        )
        .unwrap();
        let r = cli(&[
            "script",
            script.to_str().unwrap(),
            "--out",
            doc.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 1);
        // The step results are still data, and they say which step failed.
        let steps: Vec<Value> = serde_json::from_str(&r.stdout).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1]["ok"], false);
        // The failure explains itself on stderr, and nothing was written.
        assert!(
            r.stderr.contains("script step 1 failed"),
            "stderr: {}",
            r.stderr
        );
        assert!(r.stderr.contains("rolled back"));
        assert!(
            !doc.exists(),
            "a failed script must not leave a file behind"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
