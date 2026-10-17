//! A hand-rolled argument parser.
//!
//! The crate may only depend on the engine crates and serde/serde_json, so there is no clap. This
//! is deliberately small and strict: a subcommand declares which of its option names take a value
//! and which are bare flags, and anything else is an error rather than a silently ignored
//! argument. `--name value`, `--name=value` and a single-dash alias (`-o`) all work; `--` ends
//! option parsing.

/// What a subcommand accepts.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    /// Option names that take a value (`--filter create`).
    pub values: &'static [&'static str],
    /// Option names that are bare flags (`--json`).
    pub flags: &'static [&'static str],
    /// Other spellings of a name, without the dash: `(alias, canonical)`, so `-o` is `("o", "out")`.
    pub aliases: &'static [(&'static str, &'static str)],
}

impl Spec {
    /// The canonical spelling of `name`, or `name` itself.
    fn canonical<'a>(&self, name: &'a str) -> &'a str {
        self.aliases
            .iter()
            .find(|(from, _)| *from == name)
            .map(|(_, to)| *to)
            .unwrap_or(name)
    }
}

/// A parsed command line: positionals, `--key value` options and bare flags.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Args {
    pub positional: Vec<String>,
    pub options: Vec<(String, String)>,
    pub flags: Vec<String>,
}

impl Args {
    /// Parse `argv` (everything after the subcommand) against `spec`.
    pub fn parse(argv: &[String], spec: &Spec) -> Result<Self, String> {
        let mut args = Args::default();
        let mut i = 0;
        while i < argv.len() {
            let arg = &argv[i];
            i += 1;
            if arg == "--" {
                args.positional.extend_from_slice(&argv[i..]);
                break;
            }
            let Some(body) = token(arg) else {
                args.positional.push(arg.clone());
                continue;
            };
            let (name, inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };
            if name.is_empty() {
                return Err(format!("`{arg}` is not an option"));
            }
            let name = spec.canonical(name);
            if spec.values.contains(&name) {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        let Some(v) = argv.get(i).filter(|v| !v.starts_with("--")) else {
                            return Err(format!("`--{name}` needs a value"));
                        };
                        i += 1;
                        v.clone()
                    }
                };
                args.options.push((name.to_string(), value));
            } else if spec.flags.contains(&name) {
                if inline.is_some() {
                    return Err(format!("`--{name}` takes no value"));
                }
                if !args.flags.iter().any(|f| f == name) {
                    args.flags.push(name.to_string());
                }
            } else {
                return Err(format!("unknown option `{arg}`"));
            }
        }
        Ok(args)
    }

    /// The last value given for `name`, or `None`.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the bare flag `name` was given.
    pub fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    /// Exactly one positional argument.
    pub fn one(&self, usage: &str) -> Result<&str, String> {
        match self.positional.as_slice() {
            [only] => Ok(only.as_str()),
            [] => Err(format!("expected {usage}")),
            _ => Err(format!(
                "expected {usage}, but got {} positional arguments",
                self.positional.len()
            )),
        }
    }

    /// No positional arguments.
    pub fn none_positional(&self, what: &str) -> Result<(), String> {
        match self.positional.first() {
            None => Ok(()),
            Some(extra) => Err(format!(
                "{what} takes no positional arguments, but got `{extra}`"
            )),
        }
    }
}

/// The name part of `arg` if it looks like an option (`--name`, `-o`), else `None`. A lone `-` and
/// anything not starting with a dash are positionals.
fn token(arg: &str) -> Option<&str> {
    if let Some(body) = arg.strip_prefix("--") {
        return (!body.is_empty()).then_some(body);
    }
    arg.strip_prefix('-')
        .filter(|body| body.len() == 1 && !body.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    const SPEC: Spec = Spec {
        values: &["filter", "out"],
        flags: &["json", "all-pages"],
        aliases: &[("o", "out")],
    };

    /// A spec that accepts no options at all.
    const NOTHING: Spec = Spec {
        values: &[],
        flags: &[],
        aliases: &[],
    };

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parses_positionals_options_and_flags() {
        let a = Args::parse(
            &argv(&[
                "export",
                "in.figcraft",
                "--out",
                "out.png",
                "--filter=align.",
                "--json",
            ]),
            &SPEC,
        )
        .unwrap();
        assert_eq!(
            a.positional,
            ["export".to_string(), "in.figcraft".to_string()]
        );
        assert_eq!(a.value("out"), Some("out.png"));
        assert_eq!(a.value("filter"), Some("align."), "the --name=value form");
        assert!(a.flag("json"));
        assert!(!a.flag("all-pages"));
        assert_eq!(
            a.one("x").unwrap_err(),
            "expected x, but got 2 positional arguments"
        );
    }

    #[test]
    fn single_dash_alias_resolves_to_the_canonical_name() {
        let a = Args::parse(&argv(&["-o", "out.png"]), &SPEC).unwrap();
        assert_eq!(a.value("out"), Some("out.png"));
    }

    #[test]
    fn last_value_wins_and_repeating_a_flag_is_not_an_error() {
        let a = Args::parse(
            &argv(&["--out", "a.png", "--out", "b.png", "--json", "--json"]),
            &SPEC,
        )
        .unwrap();
        assert_eq!(a.value("out"), Some("b.png"));
        assert_eq!(a.flags, ["json".to_string()]);
    }

    #[test]
    fn rejects_unknown_options_missing_values_and_flag_values() {
        assert_eq!(
            Args::parse(&argv(&["--nope"]), &SPEC).unwrap_err(),
            "unknown option `--nope`"
        );
        assert_eq!(
            Args::parse(&argv(&["--out"]), &SPEC).unwrap_err(),
            "`--out` needs a value"
        );
        // A following option is not a value.
        assert_eq!(
            Args::parse(&argv(&["--out", "--json"]), &SPEC).unwrap_err(),
            "`--out` needs a value"
        );
        assert_eq!(
            Args::parse(&argv(&["--json=1"]), &SPEC).unwrap_err(),
            "`--json` takes no value"
        );
        // The error quotes what was typed, not a rebuilt `--name`.
        assert_eq!(
            Args::parse(&argv(&["---x"]), &SPEC).unwrap_err(),
            "unknown option `---x`"
        );
    }

    #[test]
    fn negative_numbers_and_flags_survive_as_values() {
        let a = Args::parse(&argv(&["--out", "-1"]), &SPEC).unwrap();
        assert_eq!(a.value("out"), Some("-1"));
    }

    #[test]
    fn double_dash_ends_options() {
        let a = Args::parse(&argv(&["--", "--json", "--out", "x"]), &SPEC).unwrap();
        assert_eq!(
            a.positional,
            ["--json".to_string(), "--out".to_string(), "x".to_string()]
        );
        assert!(!a.flag("json"));
        assert_eq!(a.value("out"), None);
    }

    #[test]
    fn lone_dash_and_empty_options_are_positional() {
        let a = Args::parse(&argv(&["-", "-", "--"]), &NOTHING).unwrap();
        assert_eq!(a.positional, ["-".to_string(), "-".to_string()]);
    }

    #[test]
    fn one_and_none_positional() {
        let a = Args::parse(&argv(&["only"]), &SPEC).unwrap();
        assert_eq!(a.one("run <command-id>").unwrap(), "only");
        let empty = Args::parse(&[], &SPEC).unwrap();
        assert_eq!(
            empty.one("open <path>").unwrap_err(),
            "expected open <path>"
        );
        assert!(empty.none_positional("list-commands").is_ok());
        assert_eq!(
            a.none_positional("list-commands").unwrap_err(),
            "list-commands takes no positional arguments, but got `only`"
        );
    }

    #[test]
    fn a_spec_with_nothing_still_takes_positional_arguments() {
        let a = Args::parse(&argv(&["a", "b"]), &NOTHING).unwrap();
        assert_eq!(a.positional, ["a".to_string(), "b".to_string()]);
        assert_eq!(
            Args::parse(&argv(&["--x"]), &NOTHING).unwrap_err(),
            "unknown option `--x`"
        );
    }
}
