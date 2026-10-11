use crate::{deployment, Application};
use anyhow::{bail, Context, Result};
use git2::{Oid, Repository, Sort};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpdatePreview {
    pub installed_commit: Option<String>,
    pub target_commit: String,
    pub available: bool,
    pub commits: Vec<String>,
    pub commit_count: usize,
    pub python_version: String,
    pub requires_tk: bool,
}

pub(crate) fn open(app: &Application) -> Result<Repository> {
    Repository::open(app.directory.join("repository")).context("Open cached repository")
}

pub(crate) fn check_update(app: &Application) -> Result<UpdatePreview> {
    let repository_path = app.directory.join("repository");
    std::fs::create_dir_all(&app.directory)?;
    let repo = if repository_path.exists() {
        Repository::open(&repository_path)?
    } else {
        let temporary = app
            .directory
            .join(format!(".repository-{}", crate::storage::unique_id()));
        match Repository::clone(&app.tool.repository, &temporary) {
            Ok(repo) => {
                drop(repo);
                std::fs::rename(&temporary, &repository_path)?;
                Repository::open(&repository_path)?
            }
            Err(error) => {
                let _ = std::fs::remove_dir_all(&temporary);
                return Err(error).context("Download tool source");
            }
        }
    };
    git2::Reference::is_valid_name(&format!("refs/heads/{}", app.tool.branch))
        .then_some(())
        .context("Invalid branch name")?;
    let mut remote = repo.find_remote("origin")?;
    if remote.url()? != app.tool.repository {
        bail!("Cached repository origin does not match the catalog");
    }
    remote.fetch(
        &[&format!(
            "+refs/heads/{}:refs/remotes/origin/{}",
            app.tool.branch, app.tool.branch
        )],
        None,
        None,
    )?;
    let target = repo
        .revparse_single(&format!("refs/remotes/origin/{}", app.tool.branch))?
        .peel_to_commit()?;
    let tree = target.tree()?;
    let entry = tree
        .get_path(std::path::Path::new("hub-tool.json"))
        .context("Selected commit has no tool-hub integration manifest")?;
    let blob = repo.find_blob(entry.id())?;
    let manifest: deployment::ToolManifest = serde_json::from_slice(blob.content())?;
    manifest.validate(&app.tool.id)?;
    let current = deployment::load_state(app)?.current.map(|d| d.commit);
    let mut commits = Vec::new();
    let mut count = 0;
    let mut available = current.as_deref() != Some(&target.id().to_string());
    if available {
        let mut walk = repo.revwalk()?;
        walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
        walk.push(target.id())?;
        if let Some(current) = &current {
            let current = Oid::from_str(current)?;
            if !repo.graph_descendant_of(target.id(), current)? {
                bail!("Branch history changed or target is older; use explicit restore instead of automatic update");
            }
            walk.hide(current)?;
            let before = repo.find_commit(current)?.tree()?;
            let after = target.tree()?;
            let diff = repo.diff_tree_to_tree(Some(&before), Some(&after), None)?;
            available = diff.deltas().any(|delta| {
                [delta.old_file().path(), delta.new_file().path()]
                    .into_iter()
                    .flatten()
                    .any(|path| {
                        let path = path.to_string_lossy();
                        !(path.starts_with("docs/")
                            || path.starts_with("tests/")
                            || path.starts_with(".github/")
                            || matches!(path.as_ref(), "README.md" | "README.txt" | "README.rst"))
                    })
            });
        }
        for oid in walk {
            let oid = oid?;
            count += 1;
            if commits.len() < 20 {
                let commit = repo.find_commit(oid)?;
                commits.push(format!(
                    "{} {}",
                    &commit.id().to_string()[..7],
                    commit.summary()?.unwrap_or("")
                ));
            }
        }
    }
    Ok(UpdatePreview {
        installed_commit: current,
        target_commit: target.id().to_string(),
        available,
        commits,
        commit_count: count,
        python_version: manifest.python,
        requires_tk: manifest.requires_tk,
    })
}
