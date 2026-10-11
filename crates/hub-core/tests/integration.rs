use git2::{Repository, Signature};
use hub_core::{Catalog, Hub, InstallState, Tool};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn python() -> PathBuf {
    let executable = std::env::var("TEST_PYTHON")
        .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.into());
    let output = Command::new(executable)
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

fn python_version() -> String {
    let output = Command::new(python())
        .args(["-c", "import sys; print('%d.%d' % sys.version_info[:2])"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn commit(repo: &Repository, message: &str) -> String {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Hub tests", "test@example.invalid").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )
    .unwrap()
    .to_string()
}

fn repository(root: &Path, id: &str) -> (Tool, Repository) {
    let path = root.join(id);
    fs::create_dir_all(&path).unwrap();
    let repo = Repository::init(&path).unwrap();
    fs::write(
        path.join("hub-tool.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema": 1, "id": id, "python": python_version(), "script": "main.py",
            "requires_tk": false, "lifecycle": "file-v1",
        }))
        .unwrap(),
    )
    .unwrap();
    // This is an actual managed Python process, not an in-memory mock.
    fs::write(path.join("main.py"), r#"
import argparse, json, os, time
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('--data-dir')
p.add_argument('--hub-session')
args = p.parse_args()
data = Path(args.data_dir)
directory = data / 'runtime' / 'hub' / args.hub_session
directory.mkdir(parents=True, exist_ok=True)
ready = directory / 'ready.json'
command = directory / 'command.json'
temp = directory / 'ready.tmp'
temp.write_text(json.dumps({'schema': 1, 'session': args.hub_session, 'pid': os.getpid(), 'state': 'ready'}))
temp.replace(ready)
try:
    while True:
        if command.exists():
            request = json.loads(command.read_text())
            command.unlink()
            if request.get('session') == args.hub_session:
                if request.get('action') == 'stop':
                    (data / 'cleaned-up').write_text('graceful')
                    break
                if request.get('action') == 'show':
                    (data / 'shown').write_text('shown')
        time.sleep(0.03)
finally:
    ready.unlink(missing_ok=True)
"#).unwrap();
    commit(&repo, "Initial executable tool");
    let branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    let tool = Tool {
        id: id.into(),
        title: id.into(),
        description: "Test tool".into(),
        repository: path.to_str().unwrap().into(),
        branch,
        platforms: vec![std::env::consts::OS.into()],
        channel: "preview".into(),
    };
    (tool, repo)
}

fn state(root: &Path, id: &str) -> InstallState {
    serde_json::from_slice(&fs::read(root.join("apps").join(id).join("current.json")).unwrap())
        .unwrap()
}

#[test]
fn catalog_rejects_duplicate_and_traversal_ids() {
    let temp = TempDir::new().unwrap();
    let (tool, _) = repository(temp.path(), "one");
    assert!(Catalog {
        schema: 1,
        tools: vec![tool.clone(), tool.clone()]
    }
    .validate()
    .is_err());
    for id in ["../escape", "", "/absolute", "one\\two"] {
        let mut bad = tool.clone();
        bad.id = id.into();
        assert!(Catalog {
            schema: 1,
            tools: vec![bad]
        }
        .validate()
        .is_err());
    }
}

#[test]
fn installs_the_prompted_commit_even_when_branch_moves_then_rolls_back_both_source_and_environment()
{
    let temp = TempDir::new().unwrap();
    let (tool, repo) = repository(temp.path(), "one");
    let root = temp.path().join("hub");
    let hub = Hub::new(
        &root,
        Catalog {
            schema: 1,
            tools: vec![tool],
        },
    )
    .unwrap();
    let initial = hub.check_update("one").unwrap();
    let source = repo.workdir().unwrap();
    fs::write(source.join("new-feature.txt"), "new content").unwrap();
    let moved = commit(&repo, "Another commit after the prompt");
    let first = hub
        .install("one", &initial.target_commit, &python())
        .unwrap();
    assert_eq!(first.commit, initial.target_commit);
    let first_path = root.join("apps/one/deployments").join(&first.id);
    assert!(!first_path.join("source/new-feature.txt").exists());
    let latest = hub.check_update("one").unwrap();
    assert_eq!(latest.target_commit, moved);
    let data = root.join("apps/one/data/private");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("secret.json"), "keep me").unwrap();
    let second = hub
        .install("one", &latest.target_commit, &python())
        .unwrap();
    assert_ne!(second.id, first.id);
    hub.rollback("one").unwrap();
    assert_eq!(state(&root, "one").current.unwrap().id, first.id);
    assert!(first_path.join(".venv").is_dir());
    assert_eq!(
        fs::read_to_string(data.join("secret.json")).unwrap(),
        "keep me"
    );
}

#[test]
fn two_projects_run_independently_and_stop_uses_graceful_cleanup() {
    let temp = TempDir::new().unwrap();
    let (one, _) = repository(temp.path(), "one");
    let (two, _) = repository(temp.path(), "two");
    let root = temp.path().join("hub");
    let hub = Hub::new(
        &root,
        Catalog {
            schema: 1,
            tools: vec![one, two],
        },
    )
    .unwrap();
    for id in ["one", "two"] {
        let target = hub.check_update(id).unwrap();
        hub.install(id, &target.target_commit, &python()).unwrap();
        hub.start(id).unwrap();
    }
    assert!(hub.list().unwrap().iter().all(|app| app.running));
    hub.stop("one").unwrap();
    let apps = hub.list().unwrap();
    assert!(!apps[0].running);
    assert!(apps[1].running);
    assert_eq!(
        fs::read_to_string(root.join("apps/one/data/cleaned-up")).unwrap(),
        "graceful"
    );
    hub.show("two").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !root.join("apps/two/data/shown").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(root.join("apps/two/data/shown").exists());
    hub.shutdown().unwrap();
    assert!(hub.list().unwrap().iter().all(|app| !app.running));
}

#[test]
fn failed_environment_preparation_keeps_the_current_deployment_and_data() {
    let temp = TempDir::new().unwrap();
    let (tool, repo) = repository(temp.path(), "one");
    let root = temp.path().join("hub");
    let hub = Hub::new(
        &root,
        Catalog {
            schema: 1,
            tools: vec![tool],
        },
    )
    .unwrap();
    let initial = hub.check_update("one").unwrap();
    let first = hub
        .install("one", &initial.target_commit, &python())
        .unwrap();
    let manifest_path = repo.workdir().unwrap().join("hub-tool.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["python"] = "0.0".into();
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    commit(&repo, "Incompatible runtime requirement");
    let update = hub.check_update("one").unwrap();
    assert!(hub
        .install("one", &update.target_commit, &python())
        .is_err());
    assert_eq!(state(&root, "one").current.unwrap().id, first.id);
    assert_eq!(
        fs::read_dir(root.join("apps/one/deployments"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn documentation_changes_do_not_prompt_updates_and_running_tools_cannot_be_replaced() {
    let temp = TempDir::new().unwrap();
    let (tool, repo) = repository(temp.path(), "one");
    let root = temp.path().join("hub");
    let hub = Hub::new(
        &root,
        Catalog {
            schema: 1,
            tools: vec![tool],
        },
    )
    .unwrap();
    let initial = hub.check_update("one").unwrap();
    hub.install("one", &initial.target_commit, &python())
        .unwrap();
    fs::write(repo.workdir().unwrap().join("README.md"), "Docs only").unwrap();
    commit(&repo, "Improve documentation");
    assert!(!hub.check_update("one").unwrap().available);
    fs::write(
        repo.workdir().unwrap().join("feature.txt"),
        "Runtime change",
    )
    .unwrap();
    commit(&repo, "Change runtime");
    let update = hub.check_update("one").unwrap();
    assert!(update.available);
    hub.start("one").unwrap();
    let result = hub.install("one", &update.target_commit, &python());
    hub.stop("one").unwrap();
    assert!(result.unwrap_err().to_string().contains("Stop this tool"));
}

#[test]
fn failed_candidate_start_restores_the_previous_deployment() {
    let temp = TempDir::new().unwrap();
    let (tool, repo) = repository(temp.path(), "one");
    let root = temp.path().join("hub");
    let hub = Hub::new(
        &root,
        Catalog {
            schema: 1,
            tools: vec![tool],
        },
    )
    .unwrap();
    let initial = hub.check_update("one").unwrap();
    let first = hub
        .install("one", &initial.target_commit, &python())
        .unwrap();
    fs::write(
        repo.workdir().unwrap().join("main.py"),
        "raise RuntimeError('failed candidate')\n",
    )
    .unwrap();
    commit(&repo, "Broken candidate");
    let update = hub.check_update("one").unwrap();
    hub.install("one", &update.target_commit, &python())
        .unwrap();
    assert!(hub.start("one").is_err());
    assert_eq!(state(&root, "one").current.unwrap().id, first.id);
    assert!(hub.read_log("one").unwrap().contains("failed candidate"));
}

#[test]
fn changed_branch_history_requires_explicit_recovery() {
    let temp = TempDir::new().unwrap();
    let (tool, repo) = repository(temp.path(), "one");
    let root = temp.path().join("hub");
    let hub = Hub::new(
        &root,
        Catalog {
            schema: 1,
            tools: vec![tool],
        },
    )
    .unwrap();
    let initial = hub.check_update("one").unwrap();
    hub.install("one", &initial.target_commit, &python())
        .unwrap();
    let tree = repo
        .find_commit(git2::Oid::from_str(&initial.target_commit).unwrap())
        .unwrap()
        .tree()
        .unwrap();
    let signature = Signature::now("Hub tests", "test@example.invalid").unwrap();
    let unrelated = repo
        .commit(None, &signature, &signature, "Rewritten root", &tree, &[])
        .unwrap();
    repo.reference(
        repo.head().unwrap().name().unwrap(),
        unrelated,
        true,
        "Rewrite branch",
    )
    .unwrap();
    assert!(hub
        .check_update("one")
        .unwrap_err()
        .to_string()
        .contains("Branch history changed"));
}
