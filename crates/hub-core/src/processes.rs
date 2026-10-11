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
    #[serde(default)]
    protocol_pid: Option<u32>,
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

fn read_ready(app: &Application, session: &Session) -> Option<Ready> {
    let record: Ready = storage::read_json(&session_path(app, session).join("ready.json")).ok()?;
    (record.schema == 1 && record.session == session.session && record.state == "ready")
        .then_some(record)
}

fn ready(app: &Application, session: &Session) -> bool {
    read_ready(app, session)
        .is_some_and(|record| record.pid == session.pid || Some(record.pid) == session.protocol_pid)
}

#[cfg(windows)]
fn is_owned_descendant(parent: u32, candidate: u32) -> bool {
    use std::collections::HashMap;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    // Windows venv's redirector can launch the real interpreter as a child.
    // Accept its readiness only after verifying ancestry under the live handle
    // we spawned, never by accepting an arbitrary PID from a readiness file.
    let mut parents = HashMap::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    let mut current = candidate;
    for _ in 0..32 {
        match parents.get(&current).copied() {
            Some(ancestor) if ancestor == parent => return true,
            Some(ancestor) if ancestor != 0 && ancestor != current => current = ancestor,
            _ => return false,
        }
    }
    false
}

#[cfg(not(windows))]
fn is_owned_descendant(_parent: u32, _candidate: u32) -> bool {
    false
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
        if owned.session.session != session.session
            || owned.session.pid != session.pid
            || owned.session.protocol_pid != session.protocol_pid
        {
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
    let mut session = Session {
        session: session_id,
        pid: child.id(),
        protocol_pid: None,
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
        if let Some(record) = read_ready(app, &session) {
            if record.pid == session.pid || is_owned_descendant(session.pid, record.pid) {
                session.protocol_pid = Some(record.pid);
                storage::atomic_json(&active_path(app), &session)?;
                if let Some(owned) = lock(&app.process)?.as_mut() {
                    owned.session = session.clone();
                }
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            let reported = read_ready(app, &session).map(|r| r.pid);
            bail!("Application did not become ready in time (owned PID {}, reported PID {:?}). It remains tracked; inspect its log and close it normally before updating.", session.pid, reported);
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
