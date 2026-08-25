#![allow(
    dead_code,
    reason = "the socket API consumes the operation registry in the next stacked change"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::task_workspace::{TaskOperationControl, TaskOperationProgress, TaskPortBroker};
use super::{ProjectError, ProjectTaskService};

const MAX_RETAINED_OPERATIONS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectTaskOperationKind {
    Provision,
    Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectTaskOperationStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl ProjectTaskOperationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTaskOperationError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTaskOperationSnapshot {
    pub operation_id: String,
    pub project_id: String,
    pub project_root: PathBuf,
    pub task_id: String,
    pub kind: ProjectTaskOperationKind,
    pub status: ProjectTaskOperationStatus,
    pub cancellation_requested: bool,
    pub progress: Option<TaskOperationProgress>,
    pub error: Option<ProjectTaskOperationError>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TaskOperationKey {
    project_root: PathBuf,
    task_id: String,
}

#[derive(Debug)]
struct OperationRecord {
    sequence: u64,
    snapshot: ProjectTaskOperationSnapshot,
}

#[derive(Debug)]
struct OperationEntry {
    record: Mutex<OperationRecord>,
    cancelled: AtomicBool,
}

#[derive(Debug, Default)]
struct RegistryState {
    operations: BTreeMap<String, Arc<OperationEntry>>,
    active_tasks: BTreeMap<TaskOperationKey, String>,
}

struct RegistryInner {
    state: Mutex<RegistryState>,
    ports: Arc<TaskPortBroker>,
    observer: Option<Arc<OperationObserver>>,
    next_sequence: AtomicU64,
    shutting_down: AtomicBool,
}

type OperationObserver = dyn Fn(ProjectTaskOperationSnapshot) + Send + Sync + 'static;

impl std::fmt::Debug for RegistryInner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistryInner")
            .field("state", &self.state)
            .field("ports", &self.ports)
            .field("observer_configured", &self.observer.is_some())
            .field("next_sequence", &self.next_sequence)
            .field("shutting_down", &self.shutting_down)
            .finish()
    }
}

/// Server-owned, bounded registry for long-running project task mutations.
/// Workers update only this shared runtime state and durable task storage; they
/// never dispatch filesystem or process work through the app/render loop.
#[derive(Debug, Clone)]
pub struct ProjectTaskOperationRegistry {
    inner: Arc<RegistryInner>,
}

impl Default for ProjectTaskOperationRegistry {
    fn default() -> Self {
        Self::new(Arc::new(TaskPortBroker::default()))
    }
}

impl ProjectTaskOperationRegistry {
    pub fn new(ports: Arc<TaskPortBroker>) -> Self {
        Self::new_inner(ports, None)
    }

    pub(crate) fn with_observer(observer: Arc<OperationObserver>) -> Self {
        Self::new_inner(Arc::new(TaskPortBroker::default()), Some(observer))
    }

    fn new_inner(ports: Arc<TaskPortBroker>, observer: Option<Arc<OperationObserver>>) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: Mutex::new(RegistryState::default()),
                ports,
                observer,
                next_sequence: AtomicU64::new(1),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    pub fn start_provision(
        &self,
        path: &Path,
        task_id: &str,
    ) -> Result<ProjectTaskOperationSnapshot, ProjectError> {
        self.start_service_operation(path, task_id, ProjectTaskOperationKind::Provision)
    }

    pub fn start_cleanup(
        &self,
        path: &Path,
        task_id: &str,
    ) -> Result<ProjectTaskOperationSnapshot, ProjectError> {
        self.start_service_operation(path, task_id, ProjectTaskOperationKind::Cleanup)
    }

    pub fn get(&self, operation_id: &str) -> Result<ProjectTaskOperationSnapshot, ProjectError> {
        validate_operation_id(operation_id)?;
        let state = self.lock_state()?;
        let entry = state.operations.get(operation_id).ok_or_else(|| {
            ProjectError::new(
                "project_task_operation_not_found",
                "project task operation was not found in this server process",
            )
        })?;
        snapshot(entry)
    }

    pub fn cancel(&self, operation_id: &str) -> Result<ProjectTaskOperationSnapshot, ProjectError> {
        validate_operation_id(operation_id)?;
        let entry = {
            let state = self.lock_state()?;
            Arc::clone(state.operations.get(operation_id).ok_or_else(|| {
                ProjectError::new(
                    "project_task_operation_not_found",
                    "project task operation was not found in this server process",
                )
            })?)
        };
        let (snapshot, changed) = {
            let mut record = lock_record(&entry)?;
            let changed =
                !record.snapshot.status.is_terminal() && !record.snapshot.cancellation_requested;
            if !record.snapshot.status.is_terminal() {
                entry.cancelled.store(true, Ordering::Release);
                record.snapshot.cancellation_requested = true;
            }
            (record.snapshot.clone(), changed)
        };
        if changed {
            notify_observer(&self.inner, snapshot.clone());
        }
        Ok(snapshot)
    }

    pub fn cancel_all(&self) {
        self.inner.shutting_down.store(true, Ordering::Release);
        let Ok(state) = self.inner.state.lock() else {
            return;
        };
        let mut changed = Vec::new();
        for entry in state.operations.values() {
            let Ok(mut record) = entry.record.lock() else {
                continue;
            };
            if !record.snapshot.status.is_terminal() && !record.snapshot.cancellation_requested {
                entry.cancelled.store(true, Ordering::Release);
                record.snapshot.cancellation_requested = true;
                changed.push(record.snapshot.clone());
            }
        }
        drop(state);
        for snapshot in changed {
            notify_observer(&self.inner, snapshot);
        }
    }

    fn start_service_operation(
        &self,
        path: &Path,
        task_id: &str,
        kind: ProjectTaskOperationKind,
    ) -> Result<ProjectTaskOperationSnapshot, ProjectError> {
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return Err(shutting_down());
        }
        let service =
            ProjectTaskService::open_with_port_broker(path, Arc::clone(&self.inner.ports))?;
        let task_id = service.load_task(task_id)?.id;
        let project_id = service.project_id().to_string();
        let project_root = service.project_root().to_path_buf();
        let worker_task_id = task_id.clone();
        self.start_operation(project_id, project_root, task_id, kind, move |control| {
            match kind {
                ProjectTaskOperationKind::Provision => {
                    service.provision_with_control(&worker_task_id, control)?;
                }
                ProjectTaskOperationKind::Cleanup => {
                    service.cleanup_with_control(&worker_task_id, control)?;
                }
            }
            Ok(())
        })
    }

    fn start_operation(
        &self,
        project_id: String,
        project_root: PathBuf,
        task_id: String,
        kind: ProjectTaskOperationKind,
        work: impl FnOnce(&dyn TaskOperationControl) -> Result<(), ProjectError> + Send + 'static,
    ) -> Result<ProjectTaskOperationSnapshot, ProjectError> {
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return Err(shutting_down());
        }
        let sequence = self.inner.next_sequence.fetch_add(1, Ordering::Relaxed);
        let operation_id = format!("task-op-{}-{sequence}", std::process::id());
        let key = TaskOperationKey {
            project_root: project_root.clone(),
            task_id: task_id.clone(),
        };
        let entry = Arc::new(OperationEntry {
            record: Mutex::new(OperationRecord {
                sequence,
                snapshot: ProjectTaskOperationSnapshot {
                    operation_id: operation_id.clone(),
                    project_id,
                    project_root,
                    task_id,
                    kind,
                    status: ProjectTaskOperationStatus::Queued,
                    cancellation_requested: false,
                    progress: None,
                    error: None,
                },
            }),
            cancelled: AtomicBool::new(false),
        });

        {
            let mut state = self.lock_state()?;
            if state.active_tasks.contains_key(&key) {
                return Err(ProjectError::new(
                    "project_task_operation_active",
                    "this project task already has an active lifecycle operation",
                ));
            }
            prune_terminal_operations(&mut state);
            if state.operations.len() >= MAX_RETAINED_OPERATIONS {
                return Err(ProjectError::new(
                    "project_task_operation_capacity",
                    "too many project task operations are still active",
                ));
            }
            state.active_tasks.insert(key.clone(), operation_id.clone());
            state
                .operations
                .insert(operation_id.clone(), Arc::clone(&entry));
        }

        let initial = snapshot(&entry)?;
        let inner = Arc::clone(&self.inner);
        let worker_entry = Arc::clone(&entry);
        let worker_operation_id = operation_id.clone();
        let worker_key = key.clone();
        let spawn_result = std::thread::Builder::new()
            .name(worker_name(kind).to_string())
            .spawn(move || {
                mark_running(&inner, &worker_entry);
                let control = RegistryOperationControl {
                    entry: Arc::clone(&worker_entry),
                    inner: Arc::clone(&inner),
                };
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&control)))
                        .unwrap_or_else(|_| {
                            Err(ProjectError::new(
                                "project_task_operation_panicked",
                                "project task worker stopped unexpectedly",
                            ))
                        });
                finish_operation(
                    &inner,
                    &worker_operation_id,
                    &worker_key,
                    &worker_entry,
                    result,
                );
            });
        if let Err(error) = spawn_result {
            let mut state = self.lock_state()?;
            state.active_tasks.remove(&key);
            state.operations.remove(&operation_id);
            return Err(ProjectError::new(
                "project_task_operation_spawn_failed",
                format!("failed to start project task worker: {error}"),
            ));
        }
        Ok(initial)
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, RegistryState>, ProjectError> {
        self.inner.state.lock().map_err(|_| registry_unavailable())
    }
}

struct RegistryOperationControl {
    entry: Arc<OperationEntry>,
    inner: Arc<RegistryInner>,
}

impl TaskOperationControl for RegistryOperationControl {
    fn is_cancelled(&self) -> bool {
        self.entry.cancelled.load(Ordering::Acquire)
            || self.inner.shutting_down.load(Ordering::Acquire)
    }

    fn report_progress(&self, progress: &TaskOperationProgress) {
        let snapshot = if let Ok(mut record) = self.entry.record.lock() {
            if progress.task_id != record.snapshot.task_id
                || progress.completed_steps > progress.total_steps
                || record.snapshot.progress.as_ref().is_some_and(|previous| {
                    previous.total_steps != progress.total_steps
                        || previous.completed_steps > progress.completed_steps
                })
            {
                return;
            }
            record.snapshot.progress = Some(progress.clone());
            Some(record.snapshot.clone())
        } else {
            None
        };
        if let Some(snapshot) = snapshot {
            notify_observer(&self.inner, snapshot);
        }
    }
}

fn mark_running(inner: &RegistryInner, entry: &OperationEntry) {
    let snapshot = if let Ok(mut record) = entry.record.lock() {
        record.snapshot.status = ProjectTaskOperationStatus::Running;
        Some(record.snapshot.clone())
    } else {
        None
    };
    if let Some(snapshot) = snapshot {
        notify_observer(inner, snapshot);
    }
}

fn finish_operation(
    inner: &RegistryInner,
    operation_id: &str,
    key: &TaskOperationKey,
    entry: &OperationEntry,
    result: Result<(), ProjectError>,
) {
    let snapshot = {
        let Ok(mut state) = inner.state.lock() else {
            return;
        };
        let Ok(mut record) = entry.record.lock() else {
            return;
        };
        match result {
            Ok(()) => record.snapshot.status = ProjectTaskOperationStatus::Succeeded,
            Err(error) if error.code == "project_task_operation_cancelled" => {
                record.snapshot.status = ProjectTaskOperationStatus::Cancelled;
                record.snapshot.cancellation_requested = true;
            }
            Err(error) => {
                record.snapshot.status = ProjectTaskOperationStatus::Failed;
                record.snapshot.error = Some(ProjectTaskOperationError {
                    code: error.code,
                    message: error.message,
                });
            }
        }
        if state.active_tasks.get(key).map(String::as_str) == Some(operation_id) {
            state.active_tasks.remove(key);
        }
        record.snapshot.clone()
    };
    notify_observer(inner, snapshot);
}

fn notify_observer(inner: &RegistryInner, snapshot: ProjectTaskOperationSnapshot) {
    let Some(observer) = &inner.observer else {
        return;
    };
    let observer = Arc::clone(observer);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(snapshot)));
}

fn prune_terminal_operations(state: &mut RegistryState) {
    while state.operations.len() >= MAX_RETAINED_OPERATIONS {
        let oldest_terminal = state
            .operations
            .iter()
            .filter_map(|(operation_id, entry)| {
                let record = entry.record.lock().ok()?;
                record
                    .snapshot
                    .status
                    .is_terminal()
                    .then_some((record.sequence, operation_id.clone()))
            })
            .min_by_key(|(sequence, _)| *sequence)
            .map(|(_, operation_id)| operation_id);
        let Some(operation_id) = oldest_terminal else {
            return;
        };
        state.operations.remove(&operation_id);
    }
}

fn snapshot(entry: &OperationEntry) -> Result<ProjectTaskOperationSnapshot, ProjectError> {
    Ok(lock_record(entry)?.snapshot.clone())
}

fn lock_record(entry: &OperationEntry) -> Result<MutexGuard<'_, OperationRecord>, ProjectError> {
    entry.record.lock().map_err(|_| registry_unavailable())
}

fn validate_operation_id(operation_id: &str) -> Result<(), ProjectError> {
    if operation_id.is_empty()
        || operation_id.len() > 64
        || !operation_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(ProjectError::new(
            "invalid_project_task_operation_id",
            "project task operation id is not a safe identifier",
        ));
    }
    Ok(())
}

fn registry_unavailable() -> ProjectError {
    ProjectError::new(
        "project_task_operation_registry_unavailable",
        "project task operation state is unavailable",
    )
}

fn shutting_down() -> ProjectError {
    ProjectError::new(
        "project_task_operations_shutting_down",
        "project task operations are shutting down",
    )
}

fn worker_name(kind: ProjectTaskOperationKind) -> &'static str {
    match kind {
        ProjectTaskOperationKind::Provision => "project-task-provision",
        ProjectTaskOperationKind::Cleanup => "project-task-cleanup",
    }
}

#[cfg(test)]
mod tests;
