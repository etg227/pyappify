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

/// Exit status a tool uses when it cannot start for an environmental reason,
/// such as another copy already holding its single-instance lock. It is not a
/// defect of the deployment, so it never triggers an automatic restore.
pub(crate) const EXIT_TEMPORARILY_UNAVAILABLE: i32 = 75;

#[derive(Clone, Deserialize, Serialize)]
struct Session {
    session: String,
    pid: u32,
    #[serde(default)]
    protocol_pid: Option<u32>,
    // Creation times identify the processes beyond their PIDs, which the OS
    // reuses. They let a restarted manager discard sessions of dead processes.
    #[serde(default)]
    started: Option<u64>,
    #[serde(default)]
    protocol_started: Option<u64>,
}

enum Liveness {
    Alive(u64),
    Gone,
    Unknown,
}

#[cfg(windows)]
fn liveness(pid: u32) -> Liveness {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE,
    };
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        );
        if handle.is_null() {
            return if GetLastError() == ERROR_INVALID_PARAMETER {
                Liveness::Gone
            } else {
                Liveness::Unknown
            };
        }
        // An exited process object survives while any handle to it is open.
        let exited = WaitForSingleObject(handle, 0) == WAIT_OBJECT_0;
        let mut created: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let timed = GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        if exited {
            Liveness::Gone
        } else if timed {
            Liveness::Alive(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
        } else {
            Liveness::Unknown
        }
    }
}

#[cfg(target_os = "linux")]
fn liveness(pid: u32) -> Liveness {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Liveness::Gone,
        Err(_) => return Liveness::Unknown,
    };
    // The command name may contain spaces and parentheses; fields follow the last ')'.
    let fields: Vec<&str> = match stat.rfind(')') {
        Some(end) => stat[end + 1..].split_whitespace().collect(),
        None => return Liveness::Unknown,
    };
    match (fields.first(), fields.get(19).and_then(|v| v.parse().ok())) {
        (Some(&"Z") | Some(&"X"), _) => Liveness::Gone,
        (Some(_), Some(started)) => Liveness::Alive(started),
        _ => Liveness::Unknown,
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn liveness(_pid: u32) -> Liveness {
    Liveness::Unknown
}

fn start_time(pid: u32) -> Option<u64> {
    match liveness(pid) {
        Liveness::Alive(started) => Some(started),
        _ => None,
    }
}

/// True only when every recorded process provably no longer exists. A PID that
/// is alive with an unknown or matching creation time keeps the session active.
fn session_is_dead(session: &Session) -> bool {
    let recorded = [
        Some((session.pid, session.started)),
        session
            .protocol_pid
            .map(|pid| (pid, session.protocol_started)),
    ];
    recorded
        .into_iter()
        .flatten()
        .all(|(pid, started)| match liveness(pid) {
            Liveness::Gone => true,
            Liveness::Alive(actual) => started.is_some_and(|expected| expected != actual),
            Liveness::Unknown => false,
        })
}

fn discard_session(app: &Application, session: &Session) -> Result<()> {
    let directory = session_path(app, session);
    for name in ["ready.json", "command.json"] {
        remove_if_exists(&directory.join(name))?;
    }
    let _ = fs::remove_dir(&directory);
    remove_if_exists(&active_path(app))
}

fn remove_if_exists(path: &std::path::Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
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

/// Reaps the owned child if it has exited, returning its exit code.
fn reap_owned(app: &Application) -> Result<Option<Option<i32>>> {
    let mut process = lock(&app.process)?;
    let Some(running) = process.as_mut() else {
        return Ok(None);
    };
    let Some(status) = running.child.try_wait()? else {
        return Ok(None);
    };
    remove_if_exists(&active_path(app))?;
    *process = None;
    Ok(Some(status.code()))
}

pub(crate) fn is_running(app: &Application) -> Result<bool> {
    if reap_owned(app)?.is_some() {
        return Ok(false);
    }
    if lock(&app.process)?.is_some() {
        return Ok(true);
    }
    // A manager crash can leave an application alive. Fail closed unless the
    // recorded processes provably no longer exist (e.g. after a reboot).
    match read_session(app)? {
        Some(session) if session_is_dead(&session) => {
            discard_session(app, &session)?;
            Ok(false)
        }
        Some(_) => Ok(true),
        None => Ok(false),
    }
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
    let current = state.current.clone().context("Install this tool first")?;
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
        // The owned handle keeps this PID from being reused while it is read.
        started: start_time(child.id()),
        protocol_started: None,
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
        if let Some(code) = reap_owned(app)? {
            if code == Some(EXIT_TEMPORARILY_UNAVAILABLE) {
                bail!("The tool reported it cannot start right now (for example another copy is already running). No deployment was changed.");
            }
            if deployment::should_restore_previous(&state) {
                deployment::rollback(app)?;
                bail!("The new version exited before becoming ready; inspect runtime/hub-process.log. The previous working version was restored.");
            }
            bail!("Application exited before becoming ready (exit code {code:?}); inspect runtime/hub-process.log. No deployment was changed.");
        }
        if let Some(record) = read_ready(app, &session) {
            if record.pid == session.pid || is_owned_descendant(session.pid, record.pid) {
                session.protocol_pid = Some(record.pid);
                session.protocol_started = if record.pid == session.pid {
                    session.started
                } else {
                    start_time(record.pid)
                };
                storage::atomic_json(&active_path(app), &session)?;
                if let Some(owned) = lock(&app.process)?.as_mut() {
                    owned.session = session.clone();
                }
                // The tool is already running; failing to persist this only
                // keeps the deployment ineligible as an automatic restore target.
                let _ = deployment::mark_verified(app, &current.id);
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
