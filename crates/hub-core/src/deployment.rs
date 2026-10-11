use crate::{repository, storage, Application};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolManifest {
    pub schema: u32,
    pub id: String,
    pub python: String,
    pub script: String,
    pub requires_tk: bool,
    pub lifecycle: String,
    #[serde(default)]
    pub requirements: Option<String>,
}

impl ToolManifest {
    pub(crate) fn validate(&self, id: &str) -> Result<()> {
        if self.schema != 1 || self.id != id || self.lifecycle != "file-v1" {
            bail!("Tool manifest is incompatible with this manager");
        }
        let parts: Vec<_> = self.python.split('.').collect();
        if parts.len() != 2 || parts.iter().any(|p| p.parse::<u8>().is_err()) {
            bail!("Python version must be major.minor");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Deployment {
    pub id: String,
    pub commit: String,
    pub manifest: ToolManifest,
    /// Set once this deployment has reached readiness. Only a verified deployment
    /// is a safe automatic restore target.
    #[serde(default)]
    pub verified: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct InstallState {
    pub current: Option<Deployment>,
    pub previous: Option<Deployment>,
    #[serde(default)]
    pub python_executable: Option<PathBuf>,
}

pub(crate) fn load_state(app: &Application) -> Result<InstallState> {
    let path = app.directory.join("current.json");
    if !path.exists() {
        return Ok(InstallState::default());
    }
    let state: InstallState = storage::read_json(&path)?;
    for deployment in [&state.current, &state.previous].into_iter().flatten() {
        validate_deployment(deployment, &app.tool.id)?;
    }
    Ok(state)
}

fn validate_deployment(deployment: &Deployment, id: &str) -> Result<()> {
    if !crate::catalog::valid_id(&deployment.id) || deployment.commit.len() != 40 {
        bail!("Invalid deployment record");
    }
    git2::Oid::from_str(&deployment.commit)?;
    deployment.manifest.validate(id)
}

pub(crate) fn path(app: &Application, deployment: &Deployment) -> PathBuf {
    app.directory.join("deployments").join(&deployment.id)
}

pub(crate) fn python(deployment_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        deployment_dir.join(".venv/Scripts/python.exe")
    } else {
        deployment_dir.join(".venv/bin/python")
    }
}

pub(crate) fn source_file(source: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("Tool file must be a relative path inside its source directory");
    }
    let file = source
        .join(relative)
        .canonicalize()
        .context("Tool file not found")?;
    if !file.starts_with(source.canonicalize()?) || !file.is_file() {
        bail!("Tool file escapes the source directory");
    }
    Ok(file)
}

pub(crate) fn command(executable: &Path) -> Command {
    let mut cmd = Command::new(executable);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    for name in ["PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV", "FLY_DATA_DIR"] {
        cmd.env_remove(name);
    }
    cmd
}

fn checked(cmd: &mut Command, description: &str) -> Result<()> {
    let output = cmd.output().with_context(|| description.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{description}: {}",
            stderr.chars().take(4000).collect::<String>()
        );
    }
    Ok(())
}

pub(crate) fn install(
    app: &Application,
    target: &repository::UpdatePreview,
    base_python: &Path,
) -> Result<Deployment> {
    if !app.tool.platforms.iter().any(|p| p == std::env::consts::OS) {
        bail!("This tool does not support the current operating system");
    }
    let repo = repository::open(app)?;
    let oid = git2::Oid::from_str(&target.target_commit)?;
    let commit = repo
        .find_commit(oid)
        .context("The checked commit is not available locally; check again")?;
    let base_python = base_python
        .canonicalize()
        .context("Choose an existing Python executable")?;
    if !base_python.is_file() {
        bail!("Python executable is not a file");
    }
    let id = format!("{}-{}", &oid.to_string()[..12], storage::unique_id());
    let deployment_dir = app.directory.join("deployments").join(&id);
    let source = deployment_dir.join("source");
    fs::create_dir_all(&source)?;
    // Source and venv are prepared at their final paths. No active files are overwritten.
    let result = (|| -> Result<Deployment> {
        repo.checkout_tree(
            commit.as_object(),
            Some(
                git2::build::CheckoutBuilder::new()
                    .force()
                    .target_dir(&source),
            ),
        )?;
        let manifest: ToolManifest = storage::read_json(&source_file(&source, "hub-tool.json")?)?;
        manifest.validate(&app.tool.id)?;
        source_file(&source, &manifest.script)?;
        let probe = format!(
            "import sys; assert '%d.%d' % sys.version_info[:2] == {:?}, 'Wrong Python version'{}",
            manifest.python,
            if manifest.requires_tk {
                "; import tkinter; tkinter.Tcl().eval('info patchlevel')"
            } else {
                ""
            }
        );
        checked(
            command(&base_python).args(["-I", "-c", &probe]),
            "Validate Python / Tcl-Tk runtime",
        )?;
        let mut create_env = command(&base_python);
        create_env.args(["-I", "-m", "venv"]);
        if manifest.requirements.is_none() {
            create_env.arg("--without-pip");
        }
        create_env.arg(deployment_dir.join(".venv"));
        checked(&mut create_env, "Create isolated Python environment")?;
        let interpreter = python(&deployment_dir);
        if let Some(requirements) = &manifest.requirements {
            let requirements = source_file(&source, requirements)?;
            checked(
                command(&interpreter)
                    .args(["-m", "pip", "install", "--require-hashes", "-r"])
                    .arg(requirements)
                    .current_dir(&source),
                "Install locked dependencies",
            )?;
        }
        checked(
            command(&interpreter).args(["-I", "-c", &probe]),
            "Verify deployment environment",
        )?;
        let deployment = Deployment {
            id: id.clone(),
            commit: oid.to_string(),
            manifest,
            verified: false,
        };
        storage::atomic_json(&deployment_dir.join("deployment.json"), &deployment)?;
        let previous_state = load_state(app)?;
        storage::atomic_json(
            &app.directory.join("current.json"),
            &InstallState {
                current: Some(deployment.clone()),
                previous: previous_state.current,
                python_executable: Some(base_python),
            },
        )?;
        Ok(deployment)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&deployment_dir);
    }
    result
}

/// Automatic restore is limited to an unverified candidate replacing a deployment
/// that has already run. Anything else would swap versions on every failed start.
pub(crate) fn should_restore_previous(state: &InstallState) -> bool {
    matches!(
        (&state.current, &state.previous),
        (Some(current), Some(previous)) if !current.verified && previous.verified
    )
}

pub(crate) fn mark_verified(app: &Application, deployment_id: &str) -> Result<()> {
    let mut state = load_state(app)?;
    match state.current.as_mut() {
        Some(current) if current.id == deployment_id && !current.verified => {
            current.verified = true;
            storage::atomic_json(&app.directory.join("current.json"), &state)
        }
        _ => Ok(()),
    }
}

pub(crate) fn rollback(app: &Application) -> Result<()> {
    let state = load_state(app)?;
    let previous = state
        .previous
        .context("No previous deployment to restore")?;
    let directory = path(app, &previous);
    let recorded: Deployment = storage::read_json(&directory.join("deployment.json"))?;
    if recorded.id != previous.id
        || recorded.commit != previous.commit
        || !python(&directory).is_file()
    {
        bail!("Previous deployment is incomplete; no state was changed");
    }
    source_file(&directory.join("source"), &previous.manifest.script)?;
    storage::atomic_json(
        &app.directory.join("current.json"),
        &InstallState {
            current: Some(previous),
            previous: state.current,
            python_executable: state.python_executable,
        },
    )
}
