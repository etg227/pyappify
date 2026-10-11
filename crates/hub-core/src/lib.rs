//! Multi-application engine, independent of the desktop UI and legacy singleton.
mod catalog;
mod deployment;
mod processes;
mod repository;
mod storage;

pub use catalog::{Catalog, Tool};
pub use deployment::{Deployment, InstallState, ToolManifest};
pub use repository::UpdatePreview;

use anyhow::{anyhow, bail, Result};
use processes::RunningProcess;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| anyhow!("Application state lock was poisoned"))
}

pub(crate) struct Application {
    tool: Tool,
    directory: PathBuf,
    operation: Mutex<()>,
    pending: Mutex<Option<UpdatePreview>>,
    process: Mutex<Option<RunningProcess>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApplicationView {
    pub tool: Tool,
    pub supported: bool,
    pub installed_commit: Option<String>,
    pub previous_commit: Option<String>,
    pub running: bool,
    pub busy: bool,
    pub update: Option<UpdatePreview>,
    pub python_executable: Option<PathBuf>,
}

pub struct Hub {
    applications: BTreeMap<String, Arc<Application>>,
}

impl Hub {
    pub fn new(root: &Path, catalog: Catalog) -> Result<Self> {
        catalog.validate()?;
        std::fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        let applications = catalog
            .tools
            .into_iter()
            .map(|tool| {
                let id = tool.id.clone();
                let application = Arc::new(Application {
                    directory: root.join("apps").join(&id),
                    tool,
                    operation: Mutex::new(()),
                    pending: Mutex::new(None),
                    process: Mutex::new(None),
                });
                (id, application)
            })
            .collect();
        Ok(Self { applications })
    }

    fn application(&self, id: &str) -> Result<Arc<Application>> {
        self.applications
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("Unknown tool: {id}"))
    }

    pub fn list(&self) -> Result<Vec<ApplicationView>> {
        self.applications
            .values()
            .map(|app| {
                let state = deployment::load_state(app)?;
                Ok(ApplicationView {
                    tool: app.tool.clone(),
                    supported: app.tool.platforms.iter().any(|p| p == std::env::consts::OS),
                    installed_commit: state.current.map(|d| d.commit),
                    previous_commit: state.previous.map(|d| d.commit),
                    running: processes::is_running(app)?,
                    busy: app.operation.try_lock().is_err(),
                    update: lock(&app.pending)?.clone(),
                    python_executable: state.python_executable,
                })
            })
            .collect()
    }

    pub fn check_update(&self, id: &str) -> Result<UpdatePreview> {
        let app = self.application(id)?;
        let _operation = app
            .operation
            .try_lock()
            .map_err(|_| anyhow!("This tool is busy"))?;
        let preview = repository::check_update(&app)?;
        *lock(&app.pending)? = Some(preview.clone());
        Ok(preview)
    }

    pub fn install(&self, id: &str, target: &str, python: &Path) -> Result<Deployment> {
        let app = self.application(id)?;
        let _operation = app
            .operation
            .try_lock()
            .map_err(|_| anyhow!("This tool is busy"))?;
        if processes::is_running(&app)? {
            bail!("Stop this tool before installing an update");
        }
        let pending = lock(&app.pending)?
            .clone()
            .ok_or_else(|| anyhow!("Check for updates first"))?;
        if pending.target_commit != target {
            bail!("Update target changed; check again before installing");
        }
        let deployment = deployment::install(&app, &pending, python)?;
        *lock(&app.pending)? = None;
        Ok(deployment)
    }

    pub fn start(&self, id: &str) -> Result<()> {
        let app = self.application(id)?;
        let _operation = app
            .operation
            .try_lock()
            .map_err(|_| anyhow!("This tool is busy"))?;
        processes::start(&app)
    }

    pub fn show(&self, id: &str) -> Result<()> {
        let app = self.application(id)?;
        let _operation = app
            .operation
            .try_lock()
            .map_err(|_| anyhow!("This tool is busy"))?;
        processes::send_command(&app, "show")
    }

    pub fn stop(&self, id: &str) -> Result<()> {
        let app = self.application(id)?;
        let _operation = app
            .operation
            .try_lock()
            .map_err(|_| anyhow!("This tool is busy"))?;
        processes::stop(&app)
    }

    pub fn rollback(&self, id: &str) -> Result<()> {
        let app = self.application(id)?;
        let _operation = app
            .operation
            .try_lock()
            .map_err(|_| anyhow!("This tool is busy"))?;
        if processes::is_running(&app)? {
            bail!("Stop this tool before restoring a deployment");
        }
        deployment::rollback(&app)
    }

    pub fn read_log(&self, id: &str) -> Result<String> {
        use std::io::{Read, Seek, SeekFrom};
        let app = self.application(id)?;
        let path = app.directory.join("data/runtime/hub-process.log");
        if !path.exists() {
            return Ok(String::new());
        }
        let mut file = std::fs::File::open(path)?;
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(64 * 1024)))?;
        let mut bytes = Vec::new();
        file.take(64 * 1024).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Do not exit the manager when a child has not confirmed graceful shutdown.
    pub fn shutdown(&self) -> Result<()> {
        let mut errors = Vec::new();
        for id in self.applications.keys() {
            if let Err(error) = self.stop(id) {
                errors.push(format!("{id}: {error:#}"));
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("\n"));
        }
        Ok(())
    }
}
