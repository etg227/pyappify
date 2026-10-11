//! Thin Tauri adapter. Each operation belongs to one application in hub-core.
use hub_core::{ApplicationView, Catalog, Hub, UpdatePreview};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

pub fn initialize(app: &AppHandle) -> anyhow::Result<()> {
    let root = app.path().app_local_data_dir()?.join("tool-hub");
    let catalog = Catalog::from_json(include_str!("../../catalog/catalog.json"))?;
    app.manage(Arc::new(Hub::new(&root, catalog)?));
    Ok(())
}

async fn execute<T: Send + 'static>(
    operation: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn hub_list(state: State<'_, Arc<Hub>>) -> Result<Vec<ApplicationView>, String> {
    let hub = state.inner().clone();
    execute(move || hub.list()).await
}

#[tauri::command]
pub async fn hub_check_update(
    app_id: String,
    state: State<'_, Arc<Hub>>,
) -> Result<UpdatePreview, String> {
    let hub = state.inner().clone();
    execute(move || hub.check_update(&app_id)).await
}

#[tauri::command]
pub async fn hub_install(
    app_id: String,
    target_commit: String,
    python_executable: String,
    state: State<'_, Arc<Hub>>,
) -> Result<(), String> {
    let hub = state.inner().clone();
    execute(move || {
        hub.install(&app_id, &target_commit, &PathBuf::from(python_executable))
            .map(|_| ())
    })
    .await
}

#[tauri::command]
pub async fn hub_start(app_id: String, state: State<'_, Arc<Hub>>) -> Result<(), String> {
    let hub = state.inner().clone();
    execute(move || hub.start(&app_id)).await
}

#[tauri::command]
pub async fn hub_show(app_id: String, state: State<'_, Arc<Hub>>) -> Result<(), String> {
    let hub = state.inner().clone();
    execute(move || hub.show(&app_id)).await
}

#[tauri::command]
pub async fn hub_stop(app_id: String, state: State<'_, Arc<Hub>>) -> Result<(), String> {
    let hub = state.inner().clone();
    execute(move || hub.stop(&app_id)).await
}

#[tauri::command]
pub async fn hub_restore(app_id: String, state: State<'_, Arc<Hub>>) -> Result<(), String> {
    let hub = state.inner().clone();
    execute(move || hub.rollback(&app_id)).await
}

#[tauri::command]
pub async fn hub_read_log(app_id: String, state: State<'_, Arc<Hub>>) -> Result<String, String> {
    let hub = state.inner().clone();
    execute(move || hub.read_log(&app_id)).await
}

#[tauri::command]
pub async fn hub_shutdown(app: AppHandle, state: State<'_, Arc<Hub>>) -> Result<(), String> {
    let hub = state.inner().clone();
    execute(move || hub.shutdown()).await?;
    app.exit(0);
    Ok(())
}
