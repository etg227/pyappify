use crate::{deployment, lock, storage, Application};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

pub(crate) struct RunningProcess {
    child: Child,
    session: Session,
}

#[derive(Clone, Deserialize, Serialize)]
struct Session {
    session: String,
    pid: u32,
}

#[derive(Deserialize)]
struct Ready {
    schema: u32,
    session: String,
    pid: u32,
    state: String,
}

fn active_path(app: &Application) -> PathBuf {
    app.directory.join("active-session.json")
}
fn session_path(app: &Application, session: &Session) -> PathBuf {
    app.directory
        .join("data/runtime/hub")
        .join(&session.session)
}

fn read_session(app: &Application) -> Result<Option<Session>> {
    if !active_path(app).exists() {
        return Ok(None);
    }
    let session: Session = storage::read_json(&active_path(app))?;
    if session.session.len() != 32
        || !session
            .session
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("Invalid active session record");
    }
    Ok(Some(session))
}

fn ready(app: &Application, session: &Session) -> bool {
    let record: Result<Ready> = storage::read_json(&session_path(app, session).join("ready.json"));
    record
        .map(|r| {
            r.schema == 1
                && r.session == session.session
                && r.pid == session.pid
                && r.state == "ready"
        })
        .unwrap_or(false)
}

pub(crate) fn is_running(app: &Application) -> Result<bool> {
    let mut process = lock(&app.process)?;
    if let Some(running) = process.as_mut() {
        if running.child.try_wait()?.is_some() {
            fs::remove_file(active_path(app)).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
            *process = None;
            return Ok(false);
        }
        return Ok(true);
    }
    // A manager crash can leave an application alive. Fail closed until its
    // lifecycle protocol acknowledges shutdown; never guess from a PID alone.
    Ok(read_session(app)?.is_some())
}

pub(crate) fn send_command(app: &Application, action: &str) -> Result<()> {
    let session = read_session(app)?.context("Tool is not running")?;
    if let Some(owned) = lock(&app.process)?.as_ref() {
        if owned.session.session != session.session || owned.session.pid != session.pid {
            bail!("Session record does not match the owned process");
        }
    }
    if !ready(app, &session) {
        bail!("Cannot confirm the application's session; open its log before retrying");
    }
    storage::atomic_json(
        &session_path(app, &session).join("command.json"),
        &serde_json::json!({
            "schema": 1, "session": session.session, "action": action,
        }),
    )
}

pub(crate) fn start(app: &Application) -> Result<()> {
    if is_running(app)? {
        return send_command(app, "show");
    }
    let state = deployment::load_state(app)?;
    let current = state.current.context("Install this tool first")?;
    let directory = deployment::path(app, &current);
    let source = directory.join("source");
    let script = deployment::source_file(&source, &current.manifest.script)?;
    let data = app.directory.join("data");
    fs::create_dir_all(data.join("runtime"))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("runtime/hub-process.log"))?;
    let session_id = storage::unique_id();
    let mut child = deployment::command(&deployment::python(&directory))
        .arg(script)
        .arg("--data-dir")
        .arg(&data)
        .arg("--hub-session")
        .arg(&session_id)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(&source)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    let session = Session {
        session: session_id,
        pid: child.id(),
    };
    if let Err(error) = storage::atomic_json(&active_path(app), &session) {
        // No readiness/manager record exists yet. Retain the owned handle if
        // killing fails so the caller cannot install over a potentially live app.
        if child.kill().is_ok() {
            let _ = child.wait();
        } else {
            *lock(&app.process)? = Some(RunningProcess { child, session });
        }
        return Err(error);
    }
    *lock(&app.process)? = Some(RunningProcess {
        child,
        session: session.clone(),
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if !is_running(app)? {
            if state.previous.is_some() {
                deployment::rollback(app)?;
            }
            bail!("Application exited before becoming ready; inspect runtime/hub-process.log. Previous deployment was retained.");
        }
        if ready(app, &session) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("Application did not become ready in time. It remains tracked; inspect its log and close it normally before updating.");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(crate) fn stop(app: &Application) -> Result<()> {
    if !is_running(app)? {
        return Ok(());
    }
    let session = read_session(app)?.context("Running application has no session record")?;
    send_command(app, "stop")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if !is_running(app)? {
            return Ok(());
        }
        // After restart the manager has no Child handle. Removal of a verified
        // readiness file is the application protocol's cleanup acknowledgment.
        if lock(&app.process)?.is_none() && !session_path(app, &session).join("ready.json").exists()
        {
            fs::remove_file(active_path(app))?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("Graceful stop timed out. No process was force-killed and no deployment was changed.");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
