use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Result, anyhow, bail};
use log::{debug, trace};
use serde_json::Value;
use tokio::process::Command;

use crate::util::project::remove_filename_from_path;

pub struct EvalOpts {
    pub json: bool,
    pub impure: bool,
}

impl Default for EvalOpts {
    fn default() -> Self {
        Self {
            json: false,
            impure: true,
        }
    }
}

#[derive(Debug)]
pub enum EvalResult {
    Json(serde_json::Value),
    Raw(String),
}

#[derive(Debug, Clone)]
pub struct FixedOutputStoreEntry {
    pub path: PathBuf,
    pub hash: String,
}

pub async fn evaluate(code: &str, opts: EvalOpts) -> Result<EvalResult> {
    let mut args: Vec<&str> = vec![];
    args.append(&mut vec!["eval", "--show-trace"]);

    if opts.json {
        args.push("--json");
    }
    if opts.impure {
        args.push("--impure");
    }

    args.append(&mut vec!["--expr", &code]);

    debug!("Running nix {}", args.join(" "));
    let output = Command::new("nix").args(args).output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nix eval failed\n{stderr}")
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    if opts.json {
        Ok(EvalResult::Json(serde_json::from_str(stdout.trim())?))
    } else {
        Ok(EvalResult::Raw(stdout.trim().to_string()))
    }
}

pub async fn get_system() -> Result<String> {
    trace!("Getting system platform");
    match evaluate(
        "builtins.currentSystem",
        EvalOpts {
            json: true,
            impure: true,
        },
    )
    .await?
    {
        EvalResult::Json(value) => match &value {
            serde_json::Value::String(s) => {
                debug!("Got {s}");
                return Ok(value.as_str().unwrap().to_string());
            }
            _ => bail!("Got: '{value:?}', Expected String"),
        },
        EvalResult::Raw(v) => bail!("Somehow returned raw with value: '{v}'"),
    };
}

pub async fn get_path_hash<P>(path: P) -> Result<String>
where
    P: Into<PathBuf>,
{
    let path: PathBuf = path.into();
    trace!("Getting hash for {path:?}");

    let dir = remove_filename_from_path(path.clone());

    let output = Command::new("nix")
        .args(["hash", "path", dir.to_str().unwrap(), "--type", "sha256"])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nix-hash failed:\n{stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    Ok(stdout.trim().to_string())
}

pub async fn get_file_hash<P>(path: P) -> Result<String>
where
    P: Into<PathBuf>,
{
    let path: PathBuf = path.into();
    trace!("Getting hash for {path:?}");

    let output = Command::new("nix")
        .args(["hash", "file", path.to_str().unwrap(), "--type", "sha256"])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nix-hash failed:\n{stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    Ok(stdout.trim().to_string())
}

pub async fn get_store_hash<P>(path: P) -> Result<String>
where
    P: Into<PathBuf>,
{
    let path: PathBuf = path.into();
    trace!("Getting hash for {path:?}");

    let dir = remove_filename_from_path(path.clone());

    let output = Command::new("nix-store")
        .args(["--query", dir.to_str().unwrap(), "--hash"])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nix-hash failed:\n{stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    let hash = stdout.trim().split(":").last().unwrap().to_string();

    debug!("Got hash {hash:?} for path {path:?}");

    Ok(hash)
}

pub async fn add_to_store<P>(path: P) -> Result<FixedOutputStoreEntry>
where
    P: Into<PathBuf>,
{
    let path: PathBuf = path.into();
    trace!("Adding {path:?} to store");

    let output = Command::new("nix-store")
        .args([
            "--recursive",
            "--add-fixed",
            "sha256",
            path.to_str().unwrap(),
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nix-store add failed:\n{stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    let store_path = PathBuf::from(stdout.trim());
    let hash = get_store_hash(&store_path).await?;

    Ok(FixedOutputStoreEntry {
        path: store_path,
        hash,
    })
}

pub async fn realise<P>(path: P) -> Result<Vec<PathBuf>>
where
    P: Into<PathBuf> + std::fmt::Debug,
{
    let path: PathBuf = path.into();
    trace!("Realising {path:?}");
    let output = Command::new("nix-store")
        .args(["--realise", path.to_str().unwrap()])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nix-store realise failed:\n{stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    Ok(stdout
        .lines()
        .map(|i| PathBuf::from(i))
        .collect::<Vec<PathBuf>>())
}

pub struct BuildOpts<'a> {
    pub link: bool,
    pub report: bool,
    pub system: &'a str,
}

pub async fn build<P>(file: P, name: &str, opts: BuildOpts<'_>) -> Result<Vec<String>>
where
    P: AsRef<Path>,
{
    let mut args = vec!["build"];
    if !opts.link {
        args.push("--no-link");
    }
    if opts.report {
        args.push("--print-out-paths");
    }
    args.push("-f");
    args.push(file.as_ref().to_str().unwrap());
    if opts.system != "" {
        args.push("--system");
        args.push(opts.system);
    };
    args.push(&name);
    trace!("Running nix {}", args.join(" "));
    let cmd = Command::new("nix")
        .stdout(Stdio::piped())
        .args(args)
        .spawn()?;

    return Ok(
        String::from_utf8_lossy(&cmd.wait_with_output().await.unwrap().stdout)
            .lines()
            .map(|s| s.to_owned())
            .collect(),
    );
}

pub struct ShellOpts<'a> {
    pub system: &'a str,
}

pub fn shell<P>(file: P, name: &str, opts: ShellOpts<'_>)
where
    P: AsRef<Path>,
{
    let mut args = vec![file.as_ref().to_str().unwrap()];
    if opts.system != "" {
        args.push("--system");
        args.push(opts.system);
    }
    args.push("-A");
    args.push(name);

    debug!("Replacing process with nix-shell {name}");
    cargo_util::ProcessBuilder::new("nix-shell")
        .args(&args)
        .exec_replace()
        .unwrap();
    std::process::exit(0);
}

pub struct GetMainProgramOpts<'a> {
    pub system: &'a str,
}

pub async fn get_main_program(
    file: &str,
    entry: FixedOutputStoreEntry,
    name: &str,
    opts: GetMainProgramOpts<'_>,
) -> Result<String> {
    let file_str = entry.path.to_str().unwrap();

    let hash = entry.hash;

    let main = evaluate(
        &format!(
            "
			let
        source = builtins.path {{ path = \"{file_str}\"; sha256 = \"{hash}\"; }};
        project = import \"${{source}}/{file}\";
				system = \"{}\";
				name = \"{name}\";
			in
				project.packages.${{name}}.result.${{system}}.meta.mainProgram or name
			",
            if opts.system == "" {
                get_system().await?
            } else {
                opts.system.to_string()
            }
        ),
        EvalOpts {
            json: true,
            impure: true,
        },
    )
    .await?;

    match main {
        EvalResult::Json(Value::String(s)) => Ok(s),
        _ => bail!("Somehow got raw or wrong type"),
    }
}

pub async fn exists_in_project(
    file: &str,
    entry: FixedOutputStoreEntry,
    name: &str,
) -> Result<bool> {
    let file_str = entry.path.to_str().unwrap();

    let hash = entry.hash;

    let code = if name.contains('.') {
        let parts = name.split('.').collect::<Vec<&str>>();
        let last = parts.last().ok_or(anyhow!("How did we get here"))?;
        let init = &parts[0..parts.len() - 1].join(".");
        format!(
            "
            let
              source = builtins.path {{ path = \"{file_str}\"; sha256 = \"{hash}\"; }};
              project = import \"${{source}}/{file}\";
            in
              (project.{init} or {{}}) ? {last}
            "
        )
    } else {
        format!(
            "
		let
      source = builtins.path {{ path = \"{file_str}\"; sha256 = \"{hash}\"; }};
      project = import \"${{source}}/{file}\";
		in
			project ? {name}
		"
        )
    };

    let result = evaluate(
        &code,
        EvalOpts {
            json: true,
            impure: true,
        },
    )
    .await?;

    match result {
        EvalResult::Json(Value::Bool(b)) => Ok(b),
        _ => bail!("Got a non boolean result {result:?}"),
    }
}
