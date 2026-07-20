use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod gltf_fixtures;

const US_REGRESSION_FOLDER: &str = "SunshineUSExport";
const EXPECTED_US_STAGE_COUNT: usize = 108;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("regression failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "could not resolve the workspace root".to_string())?;

    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("regression") => run_regression(repo_root, RegressionOptions::parse(arguments)?),
        Some("gltf-fixtures") => gltf_fixtures::run(repo_root, &arguments[1..]),
        Some(command) => Err(usage(&format!("unknown command '{command}'"))),
        None => Err(usage("missing command")),
    }
}

fn run_regression(repo_root: &Path, options: RegressionOptions) -> Result<(), String> {
    gltf_fixtures::check(repo_root)?;
    run_cargo(repo_root, &["fmt", "--all", "--", "--check"], None)?;
    run_cargo(
        repo_root,
        &[
            "clippy",
            "--locked",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        None,
    )?;
    run_cargo(repo_root, &["test", "--locked", "--workspace"], None)?;

    if !options.code_only {
        let base_root = select_retail_root(options.base_root)?;
        validate_retail_root(&base_root)?;
        println!("\n==> Source-free retail census: {}", base_root.display());
        run_cargo(
            repo_root,
            &[
                "test",
                "--locked",
                "-p",
                "sms-scene",
                "stage_archive::tests::source_free_rebuilds_every_retail_stage_archive",
                "--",
                "--ignored",
                "--exact",
                "--nocapture",
            ],
            Some((&base_root, "SMS_BASE_ROOT")),
        )?;
    }

    run_cargo(
        repo_root,
        &["build", "--locked", "--release", "-p", "graffito-editor"],
        None,
    )?;
    println!("\nAll requested regression gates passed.");
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RegressionOptions {
    code_only: bool,
    base_root: Option<PathBuf>,
}

impl RegressionOptions {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut arguments = arguments.into_iter();
        let command = arguments.next().ok_or_else(|| usage("missing command"))?;
        if command != OsStr::new("regression") {
            return Err(usage("only the regression command is supported"));
        }

        let mut options = Self::default();
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--code-only") => options.code_only = true,
                Some("--base-root") => {
                    let path = arguments
                        .next()
                        .ok_or_else(|| usage("--base-root requires a path"))?;
                    options.base_root = Some(PathBuf::from(path));
                }
                Some("--help" | "-h") => return Err(usage("")),
                Some(other) => return Err(usage(&format!("unknown argument '{other}'"))),
                None => return Err(usage("arguments must be valid Unicode")),
            }
        }
        if options.code_only && options.base_root.is_some() {
            return Err(usage("--code-only cannot be combined with --base-root"));
        }
        Ok(options)
    }
}

fn usage(error: &str) -> String {
    let prefix = if error.is_empty() {
        String::new()
    } else {
        format!("{error}\n\n")
    };
    format!(
        "{prefix}usage:\n  cargo regression [--code-only | --base-root <EXTRACTED_US_ROOT>]\n  cargo gltf-fixtures [--check]"
    )
}

fn select_retail_root(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path);
    }

    if let Some(user_profile) = env::var_os("USERPROFILE") {
        let preferred = PathBuf::from(user_profile)
            .join("Downloads")
            .join(US_REGRESSION_FOLDER);
        if preferred.is_dir() {
            return Ok(preferred);
        }
    }

    if let Some(path) = env::var_os("SMS_BASE_ROOT") {
        return Ok(PathBuf::from(path));
    }

    Err(format!(
        "no retail baseline found; expected %USERPROFILE%\\Downloads\\{US_REGRESSION_FOLDER}, \
         SMS_BASE_ROOT, or an explicit --base-root"
    ))
}

fn validate_retail_root(base_root: &Path) -> Result<(), String> {
    let scene_root = base_root.join("files").join("data").join("scene");
    if !scene_root.is_dir() {
        return Err(format!(
            "retail baseline has no files/data/scene directory: {}",
            base_root.display()
        ));
    }
    let stage_count = fs::read_dir(&scene_root)
        .map_err(|error| format!("could not read {}: {error}", scene_root.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("szs"))
        })
        .count();
    if stage_count != EXPECTED_US_STAGE_COUNT {
        return Err(format!(
            "expected {EXPECTED_US_STAGE_COUNT} US .szs stages in {}, found {stage_count}",
            scene_root.display()
        ));
    }
    if !scene_root.join("test11.szs").is_file() {
        return Err(format!(
            "US retail baseline is missing {}",
            scene_root.join("test11.szs").display()
        ));
    }
    Ok(())
}

fn run_cargo(
    repo_root: &Path,
    arguments: &[&str],
    environment: Option<(&Path, &str)>,
) -> Result<(), String> {
    println!("\n==> cargo {}", arguments.join(" "));
    let mut command = Command::new("cargo");
    command.current_dir(repo_root).args(arguments);
    if let Some((value, name)) = environment {
        command.env(name, value);
    }
    let status = command
        .status()
        .map_err(|error| format!("could not run cargo: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "cargo {} exited with {status}",
            arguments.join(" ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_full_regression() {
        assert_eq!(
            RegressionOptions::parse([OsString::from("regression")]).unwrap(),
            RegressionOptions::default()
        );
    }

    #[test]
    fn parses_explicit_root_and_code_only_modes() {
        assert_eq!(
            RegressionOptions::parse([
                OsString::from("regression"),
                OsString::from("--base-root"),
                OsString::from("C:/retail"),
            ])
            .unwrap(),
            RegressionOptions {
                code_only: false,
                base_root: Some(PathBuf::from("C:/retail")),
            }
        );
        assert_eq!(
            RegressionOptions::parse(
                [OsString::from("regression"), OsString::from("--code-only"),]
            )
            .unwrap(),
            RegressionOptions {
                code_only: true,
                base_root: None,
            }
        );
    }

    #[test]
    fn rejects_conflicting_modes() {
        let error = RegressionOptions::parse([
            OsString::from("regression"),
            OsString::from("--code-only"),
            OsString::from("--base-root"),
            OsString::from("C:/retail"),
        ])
        .unwrap_err();
        assert!(error.contains("cannot be combined"));
    }
}
