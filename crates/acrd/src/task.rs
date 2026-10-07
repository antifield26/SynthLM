//! acrd background-task state machine plus append-only journal (WAL) with
//! startup replay (TSK-501; ARCHITECTURE §8, DEC-009/027).
//!
//! [`crate::task::Task`] moves `Pending → Running → Done | Failed`, with
//! `Failed → Pending` allowed as an explicit retry. Every other transition is
//! rejected and leaves the state untouched. Each creation and each transition
//! is journaled to `journal.jsonl` (one JSON object per line); the file is
//! logically append-only (earlier lines are never mutated) and physically
//! committed through same-directory tmp-file + rename so readers never see a
//! torn tail.
//!
//! [`crate::task::TaskLog::open`] replays the journal at startup: `Running`
//! tasks are reset to `Pending` (re-run), `Done`/`Failed` are kept, corrupt
//! lines are skipped and counted (never fatal). Dropping a
//! [`crate::task::TaskLog`] without further calls is crash-safe by
//! construction (there is no close/flush protocol), so the kill -9 drill is
//! `drop(handle)` + reopen. Compaction (snapshot + truncate) and fsync of the
//! parent directory are deferred as documented follow-ups, not silent gaps.

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Journal file name inside the daemon state directory.
pub const JOURNAL_FILE_NAME: &str = "journal.jsonl";

/// Background-task lifecycle state (ARCHITECTURE §8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    /// Accepted, not yet running.
    Pending,
    /// Currently executing; reset to [`crate::task::TaskState::Pending`] by
    /// replay after a crash.
    Running,
    /// Terminal success; kept across restarts.
    Done,
    /// Terminal failure; kept across restarts, retryable via an explicit
    /// transition back to [`crate::task::TaskState::Pending`].
    Failed,
}

impl TaskState {
    /// Stable lowercase wire/file string for this state.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
        }
    }

    /// Parse a state string; returns `None` for anything else (callers count
    /// such journal lines as corrupt).
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(TaskState::Pending),
            "running" => Some(TaskState::Running),
            "done" => Some(TaskState::Done),
            "failed" => Some(TaskState::Failed),
            _ => None,
        }
    }
}

/// Whether `from → to` is a legal transition.
///
/// Legal: `Pending → Running`, `Running → Done`, `Running → Failed`,
/// `Failed → Pending` (explicit retry). Everything else — including
/// self-transitions and any exit from `Done` — is illegal.
fn is_legal(from: TaskState, to: TaskState) -> bool {
    matches!(
        (from, to),
        (TaskState::Pending, TaskState::Running)
            | (TaskState::Running, TaskState::Done)
            | (TaskState::Running, TaskState::Failed)
            | (TaskState::Failed, TaskState::Pending)
    )
}

/// One background task: stable id, task kind label, and lifecycle state.
///
/// The id/kind labels are internal scheduler strings (never keys, PCM,
/// prompts, or absolute paths).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// Stable task id (unique within one [`crate::task::TaskLog`]).
    pub id: String,
    /// Task kind label (e.g. `"render"`, `"plan"`, `"serve"`).
    pub kind: String,
    /// Current lifecycle state.
    pub state: TaskState,
}

impl Task {
    /// Create a task in [`crate::task::TaskState::Pending`] state.
    pub fn new(id: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            state: TaskState::Pending,
        }
    }

    /// Attempt `self.state → next`.
    ///
    /// On success the state is updated; on failure
    /// [`crate::task::TaskError::IllegalTransition`] is returned and the
    /// state is left untouched.
    pub fn transition(&mut self, next: TaskState) -> Result<(), TaskError> {
        if is_legal(self.state, next) {
            self.state = next;
            Ok(())
        } else {
            Err(TaskError::IllegalTransition {
                from: self.state.as_str(),
                to: next.as_str(),
            })
        }
    }
}

/// Task-store failure. All variants are secret-free by construction: they
/// carry task ids and operation names only, never file content or absolute
/// paths (AGENTS.md §8).
#[derive(Debug)]
pub enum TaskError {
    /// Rejected state transition; the task state is unchanged.
    IllegalTransition {
        /// State the task was in.
        from: &'static str,
        /// State that was requested.
        to: &'static str,
    },
    /// No task with this id exists.
    UnknownTask(String),
    /// A task with this id already exists.
    DuplicateTask(String),
    /// Filesystem failure named by operation only (no paths).
    Io(String),
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskError::IllegalTransition { from, to } => {
                write!(f, "illegal task transition {from} -> {to}")
            }
            TaskError::UnknownTask(id) => write!(f, "unknown task {id:?}"),
            TaskError::DuplicateTask(id) => write!(f, "duplicate task {id:?}"),
            TaskError::Io(op) => write!(f, "task store i/o: {op}"),
        }
    }
}

impl std::error::Error for TaskError {}

/// One journal line: the full task snapshot after a creation/transition.
#[derive(Clone, Debug)]
struct JournalLine {
    /// Task id.
    id: String,
    /// Task kind label.
    kind: String,
    /// State after the recorded change.
    state: TaskState,
    /// Monotonic line number (informational; gaps from corrupt lines are
    /// tolerated on replay).
    seq: u64,
}

/// Outcome of [`crate::task::replay_journal`]: folded tasks plus diagnostics.
#[derive(Debug)]
pub struct Replay {
    /// Latest snapshot per task id (`Running` already normalized to
    /// [`crate::task::TaskState::Pending`]).
    pub tasks: HashMap<String, Task>,
    /// Full append-only line history (normalized the same way), kept so the
    /// next persist stays append-only.
    history: Vec<JournalLine>,
    /// Next sequence number to assign.
    pub next_seq: u64,
    /// Non-empty lines that failed to parse (skipped, never fatal).
    pub skipped_corrupt: u64,
}

/// Fold the journal file at `path` into task snapshots.
///
/// A missing file replays as empty (first boot). `Running` entries are
/// normalized to [`crate::task::TaskState::Pending`] (re-run after a crash);
/// `Done`/`Failed` are kept. Corrupt lines are skipped and counted.
pub fn replay_journal(path: &Path) -> Result<Replay, TaskError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err(TaskError::Io("journal unreadable".to_owned())),
    };
    let mut tasks: HashMap<String, Task> = HashMap::new();
    let mut history: Vec<JournalLine> = Vec::new();
    let mut skipped_corrupt: u64 = 0;
    let mut max_seq: u64 = 0;
    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        match parse_line(raw) {
            Some(mut entry) => {
                max_seq = max_seq.max(entry.seq);
                if entry.state == TaskState::Running {
                    entry.state = TaskState::Pending;
                }
                let mut task = Task::new(entry.id.clone(), entry.kind.clone());
                task.state = entry.state;
                tasks.insert(entry.id.clone(), task);
                history.push(entry);
            }
            None => {
                skipped_corrupt = skipped_corrupt.saturating_add(1);
            }
        }
    }
    Ok(Replay {
        tasks,
        history,
        next_seq: max_seq.saturating_add(1),
        skipped_corrupt,
    })
}

/// Parse one journal line; `None` means corrupt (caller counts it).
fn parse_line(raw: &str) -> Option<JournalLine> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let id = value.get("id")?.as_str()?.to_owned();
    let kind = value.get("kind")?.as_str()?.to_owned();
    let state = TaskState::parse(value.get("state")?.as_str()?)?;
    let seq = value
        .get("seq")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(JournalLine {
        id,
        kind,
        state,
        seq,
    })
}

/// Append-only task store with crash-safe journal commits.
#[derive(Debug)]
pub struct TaskLog {
    /// Journal file path (`<state-dir>/journal.jsonl`).
    path: PathBuf,
    /// Latest snapshot per task id.
    tasks: HashMap<String, Task>,
    /// Append-only line history (mirrors the file content).
    history: Vec<JournalLine>,
    /// Next sequence number to assign.
    next_seq: u64,
    /// Corrupt lines skipped by the startup replay.
    skipped_corrupt: u64,
}

impl TaskLog {
    /// Open (creating) the state directory and replay the journal.
    pub fn open(dir: &Path) -> Result<Self, TaskError> {
        fs::create_dir_all(dir).map_err(|_| TaskError::Io("state dir not creatable".to_owned()))?;
        let path = dir.join(JOURNAL_FILE_NAME);
        let replay = replay_journal(&path)?;
        Ok(Self {
            path,
            tasks: replay.tasks,
            history: replay.history,
            next_seq: replay.next_seq,
            skipped_corrupt: replay.skipped_corrupt,
        })
    }

    /// Look up a task by id.
    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.get(id)
    }

    /// Number of tracked tasks.
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Corrupt journal lines skipped by the startup replay.
    pub fn skipped_corrupt(&self) -> u64 {
        self.skipped_corrupt
    }

    /// Create a task (journals a `Pending` line so a crash before the first
    /// transition still replays the task).
    pub fn create_task(
        &mut self,
        id: impl Into<String>,
        kind: impl Into<String>,
    ) -> Result<(), TaskError> {
        let id = id.into();
        let kind = kind.into();
        if self.tasks.contains_key(&id) {
            return Err(TaskError::DuplicateTask(id));
        }
        self.history.push(JournalLine {
            id: id.clone(),
            kind: kind.clone(),
            state: TaskState::Pending,
            seq: self.next_seq,
        });
        self.next_seq = self.next_seq.saturating_add(1);
        if let Err(err) = self.persist() {
            self.history.pop();
            self.next_seq = self.next_seq.saturating_sub(1);
            return Err(err);
        }
        self.tasks.insert(id.clone(), Task::new(id, kind));
        Ok(())
    }

    /// Transition a task, journaling first: on persist failure the in-memory
    /// state is untouched; on success after a crash the replay recovers at
    /// least the journaled state (the file leads memory, never lags it).
    pub fn transition(&mut self, id: &str, next: TaskState) -> Result<(), TaskError> {
        let (current, kind) = match self.tasks.get(id) {
            Some(task) => (task.state, task.kind.clone()),
            None => return Err(TaskError::UnknownTask(id.to_owned())),
        };
        if !is_legal(current, next) {
            return Err(TaskError::IllegalTransition {
                from: current.as_str(),
                to: next.as_str(),
            });
        }
        self.history.push(JournalLine {
            id: id.to_owned(),
            kind,
            state: next,
            seq: self.next_seq,
        });
        self.next_seq = self.next_seq.saturating_add(1);
        if let Err(err) = self.persist() {
            self.history.pop();
            self.next_seq = self.next_seq.saturating_sub(1);
            return Err(err);
        }
        // Apply through the same validator: pre-commit validation above
        // guarantees success, so a failure here (impossible single-threaded)
        // is reported without inventing state.
        match self.tasks.get_mut(id) {
            Some(task) => task
                .transition(next)
                .map_err(|_| TaskError::UnknownTask(id.to_owned())),
            None => Err(TaskError::UnknownTask(id.to_owned())),
        }
    }

    /// Rewrite the whole journal through a same-directory tmp file + rename.
    /// Logically append-only ([`crate::task::TaskLog::history`] only grows);
    /// physically atomic so readers never see a torn tail.
    fn persist(&self) -> Result<(), TaskError> {
        let mut text = String::with_capacity(self.history.len().saturating_mul(64));
        for line in &self.history {
            let value = serde_json::json!({
                "id": line.id,
                "kind": line.kind,
                "state": line.state.as_str(),
                "seq": line.seq,
            });
            let rendered = serde_json::to_string(&value)
                .map_err(|_| TaskError::Io("journal encode failed".to_owned()))?;
            text.push_str(&rendered);
            text.push('\n');
        }
        let tmp = self.path.with_extension("tmp");
        {
            let mut file = fs::File::create(&tmp)
                .map_err(|_| TaskError::Io("journal tmp not creatable".to_owned()))?;
            file.write_all(text.as_bytes())
                .map_err(|_| TaskError::Io("journal tmp not writable".to_owned()))?;
            file.sync_all()
                .map_err(|_| TaskError::Io("journal tmp not durable".to_owned()))?;
        }
        fs::rename(&tmp, &self.path)
            .map_err(|_| TaskError::Io("journal not committable".to_owned()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    /// Unique scratch dir per test (temp root + pid + tag; removed after).
    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("synthlm-acrd-task-{}-{tag}", std::process::id()))
    }

    fn fresh_dir(tag: &str) -> PathBuf {
        let dir = scratch_dir(tag);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn task_state_strings_roundtrip() {
        for state in [
            TaskState::Pending,
            TaskState::Running,
            TaskState::Done,
            TaskState::Failed,
        ] {
            let text = state.as_str();
            assert!(!text.is_empty());
            assert_eq!(TaskState::parse(text), Some(state));
        }
        assert_eq!(TaskState::parse("bogus"), None);
        assert_eq!(TaskState::parse(""), None);
    }

    #[test]
    fn legal_transitions_walk_full_lifecycles() {
        let mut task = Task::new("t1", "render");
        assert_eq!(task.state, TaskState::Pending);
        task.transition(TaskState::Running)
            .expect("pending->running");
        assert_eq!(task.state, TaskState::Running);
        task.transition(TaskState::Done).expect("running->done");
        assert_eq!(task.state, TaskState::Done);

        let mut retry = Task::new("t2", "plan");
        retry
            .transition(TaskState::Running)
            .expect("pending->running");
        retry
            .transition(TaskState::Failed)
            .expect("running->failed");
        assert_eq!(retry.state, TaskState::Failed);
        retry
            .transition(TaskState::Pending)
            .expect("failed->pending retry");
        assert_eq!(retry.state, TaskState::Pending);
    }

    #[test]
    fn illegal_transitions_are_rejected_and_state_is_unchanged() {
        let illegal: &[(TaskState, TaskState)] = &[
            (TaskState::Pending, TaskState::Pending),
            (TaskState::Pending, TaskState::Done),
            (TaskState::Pending, TaskState::Failed),
            (TaskState::Running, TaskState::Pending),
            (TaskState::Running, TaskState::Running),
            (TaskState::Done, TaskState::Pending),
            (TaskState::Done, TaskState::Running),
            (TaskState::Done, TaskState::Done),
            (TaskState::Done, TaskState::Failed),
            (TaskState::Failed, TaskState::Running),
            (TaskState::Failed, TaskState::Done),
            (TaskState::Failed, TaskState::Failed),
        ];
        for (from, to) in illegal {
            let mut task = Task {
                id: "t".to_owned(),
                kind: "k".to_owned(),
                state: *from,
            };
            let err = task.transition(*to).expect_err("must reject");
            assert!(
                matches!(err, TaskError::IllegalTransition { .. }),
                "unexpected error {err:?}"
            );
            assert_eq!(task.state, *from, "rejected transition must not move state");
        }
    }

    #[test]
    fn journal_persists_and_replays_with_running_reset() {
        let dir = fresh_dir("replay");
        {
            let mut log = TaskLog::open(&dir).expect("open");
            log.create_task("t1", "render").expect("create t1");
            log.transition("t1", TaskState::Running)
                .expect("t1 running");
            log.create_task("t2", "plan").expect("create t2");
            log.transition("t2", TaskState::Running)
                .expect("t2 running");
            log.transition("t2", TaskState::Done).expect("t2 done");
            log.create_task("t3", "eval").expect("create t3");
            log.transition("t3", TaskState::Running)
                .expect("t3 running");
            log.transition("t3", TaskState::Failed).expect("t3 failed");
            assert_eq!(log.task_count(), 3);
        }
        let log = TaskLog::open(&dir).expect("reopen");
        assert_eq!(log.skipped_corrupt(), 0);
        assert_eq!(
            log.get("t1").expect("t1 replays").state,
            TaskState::Pending,
            "running must reset to pending"
        );
        assert_eq!(
            log.get("t2").expect("t2 replays").state,
            TaskState::Done,
            "done must be kept"
        );
        assert_eq!(
            log.get("t3").expect("t3 replays").state,
            TaskState::Failed,
            "failed must be kept"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_lines_are_skipped_and_counted_without_losing_tasks() {
        let dir = fresh_dir("corrupt");
        {
            let mut log = TaskLog::open(&dir).expect("open");
            log.create_task("good", "render").expect("create");
            log.transition("good", TaskState::Running).expect("running");
        }
        {
            let journal = dir.join(JOURNAL_FILE_NAME);
            let mut file = OpenOptions::new()
                .append(true)
                .open(&journal)
                .expect("append journal");
            use std::io::Write as _;
            file.write_all(b"this is not json\n")
                .expect("write garbage");
            file.write_all(b"{\"id\": 42}\n").expect("write bad schema");
            file.write_all(b"{\"id\":\"x\",\"kind\":\"y\",\"state\":\"bogus\",\"seq\":99}\n")
                .expect("write bad state");
            file.sync_all().expect("sync");
        }
        let log = TaskLog::open(&dir).expect("reopen must survive corrupt lines");
        assert_eq!(log.skipped_corrupt(), 3);
        assert_eq!(
            log.get("good").expect("good task survives").state,
            TaskState::Pending
        );
        // The store stays writable after a corrupt replay.
        let mut log = log;
        log.transition("good", TaskState::Running)
            .expect("still writable");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_simulation_dropping_the_handle_loses_no_task() {
        // Kill -9 model: no close/flush protocol exists, so dropping the
        // handle mid-flight is the crash. Reopen must recover everything.
        let dir = fresh_dir("crash");
        {
            let mut log = TaskLog::open(&dir).expect("open");
            log.create_task("t1", "render").expect("create t1");
            log.transition("t1", TaskState::Running)
                .expect("t1 running");
            log.create_task("t2", "plan").expect("create t2");
            log.transition("t2", TaskState::Running)
                .expect("t2 running");
            log.transition("t2", TaskState::Done).expect("t2 done");
            drop(log); // simulated crash: no cleanup, no commit ceremony
        }
        let log = TaskLog::open(&dir).expect("reopen after crash");
        assert_eq!(log.task_count(), 2, "no task may be lost");
        assert_eq!(log.get("t1").expect("t1").state, TaskState::Pending);
        assert_eq!(log.get("t2").expect("t2").state, TaskState::Done);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_and_unknown_tasks_are_rejected() {
        let dir = fresh_dir("dup");
        let mut log = TaskLog::open(&dir).expect("open");
        log.create_task("t1", "render").expect("create");
        let err = log.create_task("t1", "render").expect_err("duplicate");
        assert!(matches!(err, TaskError::DuplicateTask(_)), "{err:?}");
        let err = log
            .transition("missing", TaskState::Running)
            .expect_err("unknown");
        assert!(matches!(err, TaskError::UnknownTask(_)), "{err:?}");
        assert!(log.get("missing").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_leaves_no_tmp_file_behind() {
        let dir = fresh_dir("tmp");
        let mut log = TaskLog::open(&dir).expect("open");
        log.create_task("t1", "render").expect("create");
        log.transition("t1", TaskState::Running).expect("run");
        log.transition("t1", TaskState::Done).expect("done");
        drop(log);
        let mut entries: Vec<String> = fs::read_dir(&dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        entries.sort();
        assert_eq!(entries, vec![JOURNAL_FILE_NAME.to_owned()]);
        let _ = fs::remove_dir_all(&dir);
    }
}
