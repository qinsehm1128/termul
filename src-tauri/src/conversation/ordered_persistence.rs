//! Bounded, source-sequenced ACP persistence workers.
//!
//! Live relay sequence assignment remains in `web::sink`. This module accepts that exact source
//! sequence, resolves the opaque agent session through the canonical binding index before any
//! worker is allocated, and serializes durable appends through one retained worker per active
//! agent session. Source sequences are validation metadata only; the repository continues to own
//! its canonical per-Conversation sequence.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde_json::Value;

use crate::conversation::{
    ConversationId, ConversationPersistenceAdapter, ConversationPersistenceError,
};

/// Maximum accepted-but-not-yet-persisted records for one agent session.
pub const QUEUE_CAPACITY: usize = 256;
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
// A production flush is a durability barrier, not an interactive latency budget. Keep it bounded,
// but allow heavily loaded hosts and full-suite CI enough time to drain accepted records without
// reporting a false durability failure. Focused backpressure tests use shorter injected timeouts.
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

const SOURCE_SEQUENCE_INVALID: &str = "CONVERSATION_SOURCE_SEQUENCE_INVALID";
const WRITER_UNHEALTHY: &str = "CONVERSATION_PERSISTENCE_UNHEALTHY";
const WRITER_QUEUE_CLOSED: &str = "CONVERSATION_PERSISTENCE_QUEUE_CLOSED";
const WRITER_DRAIN_TIMEOUT: &str = "CONVERSATION_PERSISTENCE_DRAIN_TIMEOUT";
const WRITER_SHUT_DOWN: &str = "CONVERSATION_PERSISTENCE_SHUT_DOWN";

/// Secret-safe health snapshot for one ordered writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedPersistenceHealth {
    pub conversation_id: ConversationId,
    pub pending_count: usize,
    pub last_accepted_source_seq: u64,
    pub last_persisted_source_seq: u64,
    pub last_error_code: Option<&'static str>,
    pub running: bool,
}

struct HealthState {
    snapshot: OrderedPersistenceHealth,
}

struct WorkerShared {
    health: Mutex<HealthState>,
    capacity_available: Condvar,
}

impl WorkerShared {
    fn new(conversation_id: ConversationId) -> Self {
        Self {
            health: Mutex::new(HealthState {
                snapshot: OrderedPersistenceHealth {
                    conversation_id,
                    pending_count: 0,
                    last_accepted_source_seq: 0,
                    last_persisted_source_seq: 0,
                    last_error_code: None,
                    running: true,
                },
            }),
            capacity_available: Condvar::new(),
        }
    }

    fn snapshot(&self) -> OrderedPersistenceHealth {
        self.health.lock().snapshot.clone()
    }

    fn fail(&self, code: &'static str) {
        self.health.lock().snapshot.last_error_code = Some(code);
        self.capacity_available.notify_all();
    }

    fn finish_record(&self, persisted_source_seq: Option<u64>) {
        let mut health = self.health.lock();
        if let Some(source_seq) = persisted_source_seq {
            health.snapshot.last_persisted_source_seq = source_seq;
        }
        health.snapshot.pending_count = health.snapshot.pending_count.saturating_sub(1);
        drop(health);
        self.capacity_available.notify_all();
    }
}

type AppendFuture<'a> = Pin<
    Box<dyn Future<Output = std::result::Result<u64, ConversationPersistenceError>> + Send + 'a>,
>;

trait PersistenceTarget: Send + Sync + 'static {
    fn resolve(&self, agent_session_id: &str) -> Option<ConversationId>;

    fn append<'a>(
        &'a self,
        agent_session_id: &'a str,
        event_type: &'a str,
        payload: Value,
    ) -> AppendFuture<'a>;
}

struct AdapterTarget {
    adapter: Arc<ConversationPersistenceAdapter>,
}

impl PersistenceTarget for AdapterTarget {
    fn resolve(&self, agent_session_id: &str) -> Option<ConversationId> {
        self.adapter.conversation_id_for_session(agent_session_id)
    }

    fn append<'a>(
        &'a self,
        agent_session_id: &'a str,
        event_type: &'a str,
        payload: Value,
    ) -> AppendFuture<'a> {
        Box::pin(
            self.adapter
                .append_acp_event(agent_session_id, event_type, payload),
        )
    }
}

struct RecordCommand {
    agent_session_id: String,
    source_seq: u64,
    event_type: String,
    payload: Value,
}

struct BarrierAck {
    target_source_seq: u64,
    result: std::result::Result<(), &'static str>,
}

enum WorkerCommand {
    Record(RecordCommand),
    Barrier {
        target_source_seq: u64,
        reply: SyncSender<BarrierAck>,
    },
    Shutdown,
}

#[derive(Clone)]
struct WorkerControl {
    sender: SyncSender<WorkerCommand>,
    shared: Arc<WorkerShared>,
    submit_lock: Arc<Mutex<()>>,
}

struct WorkerEntry {
    control: WorkerControl,
    join_handle: Option<JoinHandle<()>>,
}

/// One bounded ordered persistence coordinator shared by every relay using an adapter.
///
/// `submit` is intentionally synchronous because [`crate::web::EventSink::emit`] is synchronous.
/// Once a session reaches [`QUEUE_CAPACITY`] accepted records, only producers for that session
/// block until its retained worker crosses a durability boundary.
pub struct OrderedConversationPersistence {
    target: Arc<dyn PersistenceTarget>,
    workers: Mutex<HashMap<String, WorkerEntry>>,
    shutting_down: AtomicBool,
    idle_timeout: Duration,
    drain_timeout: Duration,
}

impl OrderedConversationPersistence {
    #[must_use]
    pub fn new(adapter: Arc<ConversationPersistenceAdapter>) -> Self {
        Self::with_target(
            Arc::new(AdapterTarget { adapter }),
            DEFAULT_IDLE_TIMEOUT,
            DEFAULT_DRAIN_TIMEOUT,
        )
    }

    fn with_target(
        target: Arc<dyn PersistenceTarget>,
        idle_timeout: Duration,
        drain_timeout: Duration,
    ) -> Self {
        Self {
            target,
            workers: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
            idle_timeout,
            drain_timeout,
        }
    }

    /// Accept one relay event for ordered persistence.
    ///
    /// Binding resolution happens before worker creation. Zero, duplicate, or decreasing source
    /// sequences fail closed before append. Error details never contain the opaque session id or
    /// payload.
    pub fn submit(
        &self,
        agent_session_id: &str,
        source_seq: u64,
        event_type: &str,
        payload: Value,
    ) -> Result<(), ConversationPersistenceError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(persistence_error(
                WRITER_SHUT_DOWN,
                "ordered_submit",
                "ordered persistence is shutting down",
            ));
        }
        if source_seq == 0 {
            return Err(persistence_error(
                SOURCE_SEQUENCE_INVALID,
                "ordered_submit",
                "source sequence must be greater than zero",
            ));
        }
        let conversation_id = self.target.resolve(agent_session_id).ok_or_else(|| {
            persistence_error(
                "CONVERSATION_BINDING_NOT_FOUND",
                "ordered_submit",
                "opaque agent session id has no canonical Conversation binding",
            )
        })?;

        let mut command = Some(RecordCommand {
            agent_session_id: agent_session_id.to_string(),
            source_seq,
            event_type: event_type.to_string(),
            payload,
        });
        // An idle worker can retire in the narrow interval between lookup and send. Retry once
        // after joining/replacing that retained handle; a second disconnect is a stable failure.
        'submit_attempts: for attempt in 0..=1 {
            let control = self.ensure_worker(agent_session_id, conversation_id)?;
            let submit_guard = control.submit_lock.lock();
            let wait_started = Instant::now();
            {
                let mut health = control.shared.health.lock();
                if self.shutting_down.load(Ordering::Acquire) {
                    return Err(persistence_error(
                        WRITER_SHUT_DOWN,
                        "ordered_submit",
                        "ordered persistence is shutting down",
                    ));
                }
                if !health.snapshot.running {
                    drop(health);
                    drop(submit_guard);
                    self.restart_worker(agent_session_id, conversation_id)?;
                    continue 'submit_attempts;
                }
                if let Some(code) = health.snapshot.last_error_code {
                    let (reported_code, detail) = if code == SOURCE_SEQUENCE_INVALID {
                        (
                            SOURCE_SEQUENCE_INVALID,
                            "source sequence validation previously failed".to_string(),
                        )
                    } else {
                        (
                            WRITER_UNHEALTHY,
                            format!("ordered writer is unhealthy ({code})"),
                        )
                    };
                    return Err(persistence_error(reported_code, "ordered_submit", detail));
                }
                if source_seq <= health.snapshot.last_accepted_source_seq {
                    health.snapshot.last_error_code = Some(SOURCE_SEQUENCE_INVALID);
                    let accepted = health.snapshot.last_accepted_source_seq;
                    drop(health);
                    control.shared.capacity_available.notify_all();
                    log::error!(
                        "[conversation-persistence] source sequence violation code={} conversation_id={}",
                        SOURCE_SEQUENCE_INVALID,
                        conversation_id
                    );
                    return Err(persistence_error(
                        SOURCE_SEQUENCE_INVALID,
                        "ordered_submit",
                        format!(
                            "source sequence must be strictly increasing; last accepted frontier is {accepted}"
                        ),
                    ));
                }
                while health.snapshot.pending_count >= QUEUE_CAPACITY {
                    if self.shutting_down.load(Ordering::Acquire) {
                        return Err(persistence_error(
                            WRITER_SHUT_DOWN,
                            "ordered_submit",
                            "ordered persistence is shutting down",
                        ));
                    }
                    control.shared.capacity_available.wait(&mut health);
                    if let Some(code) = health.snapshot.last_error_code {
                        let reported_code = if code == SOURCE_SEQUENCE_INVALID {
                            SOURCE_SEQUENCE_INVALID
                        } else {
                            WRITER_UNHEALTHY
                        };
                        return Err(persistence_error(
                            reported_code,
                            "ordered_submit",
                            format!("ordered writer is unhealthy ({code})"),
                        ));
                    }
                    if !health.snapshot.running {
                        drop(health);
                        drop(submit_guard);
                        self.restart_worker(agent_session_id, conversation_id)?;
                        continue 'submit_attempts;
                    }
                }
                health.snapshot.pending_count += 1;
                health.snapshot.last_accepted_source_seq = source_seq;
            }
            let waited = wait_started.elapsed();
            if !waited.is_zero() && waited >= Duration::from_millis(1) {
                log::warn!(
                    "[conversation-persistence] bounded writer backpressure conversation_id={} duration_ms={}",
                    conversation_id,
                    waited.as_millis()
                );
            }

            let record = command.take().expect("record command is sent once");
            match control.sender.send(WorkerCommand::Record(record)) {
                Ok(()) => return Ok(()),
                Err(send_error) => {
                    let WorkerCommand::Record(record) = send_error.0 else {
                        unreachable!("submit only sends records")
                    };
                    command = Some(record);
                    let mut health = control.shared.health.lock();
                    health.snapshot.pending_count = health.snapshot.pending_count.saturating_sub(1);
                    health.snapshot.last_accepted_source_seq =
                        health.snapshot.last_persisted_source_seq;
                    health.snapshot.running = false;
                    drop(health);
                    control.shared.capacity_available.notify_all();
                    if attempt == 0 {
                        self.restart_worker(agent_session_id, conversation_id)?;
                        continue 'submit_attempts;
                    }
                    control.shared.fail(WRITER_QUEUE_CLOSED);
                    log::error!(
                        "[conversation-persistence] writer queue closed code={} conversation_id={}",
                        WRITER_QUEUE_CLOSED,
                        conversation_id
                    );
                    return Err(persistence_error(
                        WRITER_QUEUE_CLOSED,
                        "ordered_submit",
                        "ordered writer queue is closed",
                    ));
                }
            }
        }
        unreachable!("submit retry loop returns")
    }

    /// Health for a mapped agent session. No worker is created by this query.
    pub fn health(
        &self,
        agent_session_id: &str,
    ) -> Result<Option<OrderedPersistenceHealth>, ConversationPersistenceError> {
        let conversation_id = self.target.resolve(agent_session_id).ok_or_else(|| {
            persistence_error(
                "CONVERSATION_BINDING_NOT_FOUND",
                "ordered_health",
                "opaque agent session id has no canonical Conversation binding",
            )
        })?;
        Ok(self
            .workers
            .lock()
            .get(agent_session_id)
            .map(|entry| entry.control.shared.snapshot())
            .filter(|health| health.conversation_id == conversation_id))
    }

    /// Number of retained session entries. Finished idle workers keep their health and handle until
    /// the next submit/flush/shutdown reaps them, so diagnostics never lose an error frontier.
    #[must_use]
    pub fn retained_worker_count(&self) -> usize {
        self.workers.lock().len()
    }

    /// Number of workers currently running (idle-retired entries are excluded).
    #[must_use]
    pub fn active_worker_count(&self) -> usize {
        self.workers
            .lock()
            .values()
            .filter(|entry| entry.control.shared.snapshot().running)
            .count()
    }

    /// Enqueue a barrier behind all records accepted before this call and await every worker.
    pub async fn flush_all(&self) -> Result<(), ConversationPersistenceError> {
        let started = Instant::now();
        let controls = self.worker_controls();
        let mut barriers = Vec::with_capacity(controls.len());
        let mut first_error = None;
        for control in controls {
            let _submit_guard = control.submit_lock.lock();
            let health = control.shared.snapshot();
            if !health.running {
                if health.pending_count == 0
                    && health.last_persisted_source_seq == health.last_accepted_source_seq
                    && health.last_error_code.is_none()
                {
                    continue;
                }
                let error = health.last_error_code.map_or_else(
                    || {
                        persistence_error(
                            WRITER_QUEUE_CLOSED,
                            "ordered_flush_all",
                            "ordered writer stopped before reaching its accepted frontier",
                        )
                    },
                    |code| {
                        persistence_error(
                            WRITER_UNHEALTHY,
                            "ordered_flush_all",
                            format!("ordered writer is unhealthy ({code})"),
                        )
                    },
                );
                first_error.get_or_insert(error);
                continue;
            }
            let (reply, receiver) = mpsc::sync_channel(1);
            if let Err(error) = self.send_control_bounded(
                &control.sender,
                WorkerCommand::Barrier {
                    target_source_seq: health.last_accepted_source_seq,
                    reply,
                },
                health.conversation_id,
            ) {
                first_error.get_or_insert(error);
                continue;
            }
            barriers.push((
                health.conversation_id,
                health.last_accepted_source_seq,
                receiver,
            ));
        }

        let timeout = self.drain_timeout;
        let acknowledgements = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + timeout;
            barriers
                .into_iter()
                .map(|(conversation_id, target, receiver)| {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    receiver.recv_timeout(remaining).map_or_else(
                        |_| {
                            Err(persistence_error(
                                WRITER_DRAIN_TIMEOUT,
                                "ordered_flush_all",
                                "timed out waiting for ordered writer barrier",
                            ))
                        },
                        |acknowledgement| Ok((conversation_id, target, acknowledgement)),
                    )
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|_| {
            persistence_error(
                WRITER_QUEUE_CLOSED,
                "ordered_flush_all",
                "ordered writer barrier waiter failed",
            )
        })?;

        for acknowledgement in acknowledgements {
            let (conversation_id, target, acknowledgement) = match acknowledgement {
                Ok(acknowledgement) => acknowledgement,
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            };
            if let Err(code) = acknowledgement.result {
                first_error.get_or_insert_with(|| {
                    persistence_error(
                        WRITER_UNHEALTHY,
                        "ordered_flush_all",
                        format!("ordered writer barrier failed ({code})"),
                    )
                });
                continue;
            }
            if acknowledgement.target_source_seq != target {
                first_error.get_or_insert_with(|| {
                    persistence_error(
                        WRITER_QUEUE_CLOSED,
                        "ordered_flush_all",
                        "ordered writer acknowledged the wrong source frontier",
                    )
                });
                continue;
            }
            log::info!(
                "[conversation-persistence] writer flush conversation_id={} pending_count=0 last_accepted_source_seq={} last_persisted_source_seq={} duration_ms={}",
                conversation_id,
                target,
                target,
                started.elapsed().as_millis()
            );
        }
        self.reap_finished_workers();
        first_error.map_or(Ok(()), Err)
    }

    /// Stop accepting records, flush every accepted record, and retain any timed-out handle so a
    /// host can report an unhealthy shutdown rather than claiming a successful drain.
    pub async fn shutdown(&self) -> Result<(), ConversationPersistenceError> {
        self.shutting_down.store(true, Ordering::Release);
        for control in self.worker_controls() {
            control.shared.capacity_available.notify_all();
        }
        let flush_result = self.flush_all().await;
        let controls = self.worker_controls();
        for control in controls {
            let health = control.shared.snapshot();
            if health.running {
                let _submit_guard = control.submit_lock.lock();
                let _ = self.send_control_bounded(
                    &control.sender,
                    WorkerCommand::Shutdown,
                    health.conversation_id,
                );
            }
        }

        let deadline = tokio::time::Instant::now() + self.drain_timeout;
        loop {
            self.reap_finished_workers();
            if self.active_worker_count() == 0 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(persistence_error(
                    WRITER_DRAIN_TIMEOUT,
                    "ordered_shutdown",
                    "timed out joining ordered persistence workers",
                ));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        flush_result
    }

    fn worker_controls(&self) -> Vec<WorkerControl> {
        self.workers
            .lock()
            .values()
            .map(|entry| entry.control.clone())
            .collect()
    }

    fn ensure_worker(
        &self,
        agent_session_id: &str,
        conversation_id: ConversationId,
    ) -> Result<WorkerControl, ConversationPersistenceError> {
        let mut workers = self.workers.lock();
        if let Some(entry) = workers.get_mut(agent_session_id) {
            if entry.control.shared.snapshot().conversation_id != conversation_id {
                return Err(persistence_error(
                    "CONVERSATION_BINDING_CONFLICT",
                    "ordered_submit",
                    "agent session binding changed while an ordered writer was retained",
                ));
            }
            let running = entry.control.shared.snapshot().running;
            let finished = entry
                .join_handle
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished);
            if !running || finished || entry.join_handle.is_none() {
                if let Some(handle) = entry.join_handle.take() {
                    let _ = handle.join();
                }
                let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
                entry.control.sender = sender;
                entry.control.shared.health.lock().snapshot.running = true;
                entry.join_handle = Some(spawn_worker(
                    Arc::clone(&self.target),
                    Arc::clone(&entry.control.shared),
                    Arc::clone(&entry.control.submit_lock),
                    receiver,
                    self.idle_timeout,
                )?);
            }
            return Ok(entry.control.clone());
        }

        let shared = Arc::new(WorkerShared::new(conversation_id));
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let control = WorkerControl {
            sender,
            shared: Arc::clone(&shared),
            submit_lock: Arc::new(Mutex::new(())),
        };
        let handle = spawn_worker(
            Arc::clone(&self.target),
            shared,
            Arc::clone(&control.submit_lock),
            receiver,
            self.idle_timeout,
        )?;
        workers.insert(
            agent_session_id.to_string(),
            WorkerEntry {
                control: control.clone(),
                join_handle: Some(handle),
            },
        );
        Ok(control)
    }

    fn restart_worker(
        &self,
        agent_session_id: &str,
        conversation_id: ConversationId,
    ) -> Result<(), ConversationPersistenceError> {
        let mut workers = self.workers.lock();
        let Some(entry) = workers.get_mut(agent_session_id) else {
            return Ok(());
        };
        if let Some(handle) = entry.join_handle.take() {
            let _ = handle.join();
        }
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        entry.control.sender = sender;
        {
            let mut health = entry.control.shared.health.lock();
            health.snapshot.conversation_id = conversation_id;
            health.snapshot.running = true;
        }
        entry.join_handle = Some(spawn_worker(
            Arc::clone(&self.target),
            Arc::clone(&entry.control.shared),
            Arc::clone(&entry.control.submit_lock),
            receiver,
            self.idle_timeout,
        )?);
        Ok(())
    }

    fn send_control_bounded(
        &self,
        sender: &SyncSender<WorkerCommand>,
        mut command: WorkerCommand,
        conversation_id: ConversationId,
    ) -> Result<(), ConversationPersistenceError> {
        let deadline = Instant::now() + self.drain_timeout;
        loop {
            match sender.try_send(command) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                    command = returned;
                    std::thread::yield_now();
                }
                Err(TrySendError::Full(_)) => {
                    log::error!(
                        "[conversation-persistence] drain timeout code={} conversation_id={}",
                        WRITER_DRAIN_TIMEOUT,
                        conversation_id
                    );
                    return Err(persistence_error(
                        WRITER_DRAIN_TIMEOUT,
                        "ordered_control",
                        "timed out enqueueing ordered writer control message",
                    ));
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(persistence_error(
                        WRITER_QUEUE_CLOSED,
                        "ordered_control",
                        "ordered writer queue is closed",
                    ));
                }
            }
        }
    }

    fn reap_finished_workers(&self) {
        let mut workers = self.workers.lock();
        for entry in workers.values_mut() {
            if entry
                .join_handle
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
            {
                if let Some(handle) = entry.join_handle.take() {
                    let _ = handle.join();
                }
            }
        }
    }
}

impl Drop for OrderedConversationPersistence {
    fn drop(&mut self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) && self.active_worker_count() == 0 {
            return;
        }
        let deadline = Instant::now() + self.drain_timeout;
        let controls = self.worker_controls();
        let mut barriers = Vec::new();
        for control in &controls {
            control.shared.capacity_available.notify_all();
            let _submit_guard = control.submit_lock.lock();
            let health = control.shared.snapshot();
            if !health.running || health.last_error_code.is_some() {
                continue;
            }
            let (reply, receiver) = mpsc::sync_channel(1);
            match self.send_control_bounded(
                &control.sender,
                WorkerCommand::Barrier {
                    target_source_seq: health.last_accepted_source_seq,
                    reply,
                },
                health.conversation_id,
            ) {
                Ok(()) => barriers.push((health.conversation_id, receiver)),
                Err(error) => log::error!(
                    "[conversation-persistence] drop drain failed code={} conversation_id={}",
                    error.code,
                    health.conversation_id
                ),
            }
        }
        for (conversation_id, receiver) in barriers {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok(BarrierAck { result: Ok(()), .. }) => {}
                Ok(BarrierAck {
                    result: Err(code), ..
                }) => log::error!(
                    "[conversation-persistence] drop drain failed code={} conversation_id={}",
                    code,
                    conversation_id
                ),
                Err(_) => log::error!(
                    "[conversation-persistence] drop drain failed code={} conversation_id={}",
                    WRITER_DRAIN_TIMEOUT,
                    conversation_id
                ),
            }
        }
        for control in controls {
            let health = control.shared.snapshot();
            if health.running {
                let _ = self.send_control_bounded(
                    &control.sender,
                    WorkerCommand::Shutdown,
                    health.conversation_id,
                );
            }
        }
        while Instant::now() < deadline {
            self.reap_finished_workers();
            if self.active_worker_count() == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        for entry in self.workers.get_mut().values() {
            let health = entry.control.shared.snapshot();
            if health.running {
                log::error!(
                    "[conversation-persistence] drop join failed code={} conversation_id={}",
                    WRITER_DRAIN_TIMEOUT,
                    health.conversation_id
                );
            }
        }
    }
}

fn spawn_worker(
    target: Arc<dyn PersistenceTarget>,
    shared: Arc<WorkerShared>,
    submit_lock: Arc<Mutex<()>>,
    receiver: Receiver<WorkerCommand>,
    idle_timeout: Duration,
) -> Result<JoinHandle<()>, ConversationPersistenceError> {
    let conversation_id = shared.snapshot().conversation_id;
    std::thread::Builder::new()
        .name(format!("conversation-writer-{conversation_id}"))
        .spawn(move || run_worker(target, shared, submit_lock, receiver, idle_timeout))
        .map_err(|_| {
            persistence_error(
                WRITER_QUEUE_CLOSED,
                "ordered_worker_start",
                "failed to start ordered persistence worker",
            )
        })
}

fn run_worker(
    target: Arc<dyn PersistenceTarget>,
    shared: Arc<WorkerShared>,
    submit_lock: Arc<Mutex<()>>,
    receiver: Receiver<WorkerCommand>,
    idle_timeout: Duration,
) {
    let conversation_id = shared.snapshot().conversation_id;
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            shared.fail(WRITER_QUEUE_CLOSED);
            shared.health.lock().snapshot.running = false;
            return;
        }
    };
    log::info!(
        "[conversation-persistence] writer start conversation_id={} pending_count={} last_accepted_source_seq={} last_persisted_source_seq={}",
        conversation_id,
        shared.snapshot().pending_count,
        shared.snapshot().last_accepted_source_seq,
        shared.snapshot().last_persisted_source_seq
    );

    loop {
        let command = match receiver.recv_timeout(idle_timeout) {
            Ok(command) => command,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                // Retirement is serialized with both record and barrier submission. Re-check the
                // receiver after acquiring the gate so a command sent concurrently with the
                // timeout cannot be stranded behind a worker that has already decided to exit.
                let retirement_guard = submit_lock.lock();
                match receiver.try_recv() {
                    Ok(command) => {
                        drop(retirement_guard);
                        command
                    }
                    Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {
                        let mut health = shared.health.lock();
                        if health.snapshot.pending_count == 0 {
                            health.snapshot.running = false;
                            let last_accepted_source_seq = health.snapshot.last_accepted_source_seq;
                            let last_persisted_source_seq =
                                health.snapshot.last_persisted_source_seq;
                            drop(health);
                            drop(retirement_guard);
                            log::info!(
                                "[conversation-persistence] writer idle stop conversation_id={} pending_count=0 last_accepted_source_seq={} last_persisted_source_seq={}",
                                conversation_id,
                                last_accepted_source_seq,
                                last_persisted_source_seq
                            );
                            shared.capacity_available.notify_all();
                            return;
                        }
                        drop(health);
                        drop(retirement_guard);
                        continue;
                    }
                }
            }
        };

        match command {
            WorkerCommand::Record(record) => {
                let health = shared.snapshot();
                // A rejected duplicate/decreasing submission is not itself an accepted record:
                // drain records that were already accepted before the violation. Repository or
                // worker failures remain fatal and prevent later queued appends.
                if health
                    .last_error_code
                    .is_some_and(|code| code != SOURCE_SEQUENCE_INVALID)
                {
                    shared.finish_record(None);
                    continue;
                }
                if record.source_seq <= health.last_persisted_source_seq {
                    shared.fail(SOURCE_SEQUENCE_INVALID);
                    shared.finish_record(None);
                    log::error!(
                        "[conversation-persistence] source sequence violation code={} conversation_id={}",
                        SOURCE_SEQUENCE_INVALID,
                        conversation_id
                    );
                    continue;
                }
                match runtime.block_on(target.append(
                    &record.agent_session_id,
                    &record.event_type,
                    record.payload,
                )) {
                    Ok(_) => shared.finish_record(Some(record.source_seq)),
                    Err(error) => {
                        shared.fail(error.code);
                        shared.finish_record(None);
                        log::error!(
                            "[conversation-persistence] append failure code={} conversation_id={}",
                            error.code,
                            conversation_id
                        );
                    }
                }
            }
            WorkerCommand::Barrier {
                target_source_seq,
                reply,
            } => {
                let health = shared.snapshot();
                let result = health.last_error_code.map_or_else(
                    || {
                        if health.pending_count == 0
                            && health.last_persisted_source_seq >= target_source_seq
                        {
                            Ok(())
                        } else {
                            Err(WRITER_QUEUE_CLOSED)
                        }
                    },
                    Err,
                );
                let _ = reply.send(BarrierAck {
                    target_source_seq,
                    result,
                });
            }
            WorkerCommand::Shutdown => break,
        }
    }
    shared.health.lock().snapshot.running = false;
    shared.capacity_available.notify_all();
}

fn persistence_error(
    code: &'static str,
    operation: &'static str,
    detail: impl Into<String>,
) -> ConversationPersistenceError {
    ConversationPersistenceError {
        code,
        operation,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    const CONVERSATION_A: &str = "11111111-1111-4111-8111-111111111111";
    const CONVERSATION_B: &str = "22222222-2222-4222-8222-222222222222";

    type RecordedEvent = (u64, String, Value);
    type RecordedSessions = HashMap<String, Vec<RecordedEvent>>;

    struct FakeTarget {
        mappings: HashMap<String, ConversationId>,
        records: Mutex<RecordedSessions>,
        append_count: AtomicUsize,
        blocked: AtomicBool,
        release: (Mutex<bool>, Condvar),
        fail_after: Option<usize>,
    }

    impl FakeTarget {
        fn new() -> Self {
            Self {
                mappings: HashMap::from([
                    (
                        "opaque-a".to_string(),
                        ConversationId::parse(CONVERSATION_A).unwrap(),
                    ),
                    (
                        "opaque-b".to_string(),
                        ConversationId::parse(CONVERSATION_B).unwrap(),
                    ),
                ]),
                records: Mutex::new(HashMap::new()),
                append_count: AtomicUsize::new(0),
                blocked: AtomicBool::new(false),
                release: (Mutex::new(false), Condvar::new()),
                fail_after: None,
            }
        }

        fn blocked() -> Self {
            let target = Self::new();
            target.blocked.store(true, Ordering::Release);
            target
        }

        fn release(&self) {
            *self.release.0.lock() = true;
            self.release.1.notify_all();
        }
    }

    impl PersistenceTarget for FakeTarget {
        fn resolve(&self, agent_session_id: &str) -> Option<ConversationId> {
            self.mappings.get(agent_session_id).copied()
        }

        fn append<'a>(
            &'a self,
            agent_session_id: &'a str,
            event_type: &'a str,
            payload: Value,
        ) -> AppendFuture<'a> {
            Box::pin(async move {
                if self.blocked.load(Ordering::Acquire) {
                    let mut released = self.release.0.lock();
                    while !*released {
                        self.release.1.wait(&mut released);
                    }
                }
                let count = self.append_count.fetch_add(1, Ordering::AcqRel) + 1;
                if self.fail_after.is_some_and(|limit| count > limit) {
                    return Err(persistence_error(
                        "CONVERSATION_EVENT_APPEND_FAILED",
                        "fake_append",
                        "injected failure containing prompt=do-not-log token=do-not-log",
                    ));
                }
                let source_seq = payload["sourceSeq"].as_u64().unwrap();
                self.records
                    .lock()
                    .entry(agent_session_id.to_string())
                    .or_default()
                    .push((source_seq, event_type.to_string(), payload));
                Ok(count as u64)
            })
        }
    }

    fn ordered(target: Arc<FakeTarget>) -> OrderedConversationPersistence {
        OrderedConversationPersistence::with_target(
            target,
            Duration::from_millis(50),
            Duration::from_secs(3),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ten_thousand_events_remain_ordered_per_session_with_two_retained_workers() {
        let target = Arc::new(FakeTarget::new());
        let persistence = ordered(Arc::clone(&target));
        for source_seq in 1..=5_000 {
            for session in ["opaque-a", "opaque-b"] {
                let event_type = match source_seq % 5 {
                    0 => "message_chunk",
                    1 => "tool_call",
                    2 => "tool_call_update",
                    3 => "prompt_complete",
                    _ => "session_info_update",
                };
                persistence
                    .submit(
                        session,
                        source_seq,
                        event_type,
                        serde_json::json!({"sourceSeq": source_seq, "body": source_seq}),
                    )
                    .unwrap();
            }
        }
        persistence.flush_all().await.unwrap();
        assert_eq!(persistence.retained_worker_count(), 2);
        {
            let records = target.records.lock();
            for session in ["opaque-a", "opaque-b"] {
                let durable = &records[session];
                assert_eq!(durable.len(), 5_000);
                assert_eq!(
                    durable.iter().map(|record| record.0).collect::<Vec<_>>(),
                    (1..=5_000).collect::<Vec<_>>()
                );
            }
        }
        persistence.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_backpressure_caps_accepted_backlog_and_shutdown_drains() {
        let target = Arc::new(FakeTarget::blocked());
        let persistence = Arc::new(ordered(Arc::clone(&target)));
        for source_seq in 1..=QUEUE_CAPACITY as u64 {
            persistence
                .submit(
                    "opaque-a",
                    source_seq,
                    "message_chunk",
                    serde_json::json!({"sourceSeq": source_seq}),
                )
                .unwrap();
        }
        assert_eq!(
            persistence
                .health("opaque-a")
                .unwrap()
                .unwrap()
                .pending_count,
            QUEUE_CAPACITY
        );
        let submitted = Arc::new(AtomicBool::new(false));
        let producer_persistence = Arc::clone(&persistence);
        let producer_submitted = Arc::clone(&submitted);
        let producer = std::thread::spawn(move || {
            producer_persistence
                .submit(
                    "opaque-a",
                    QUEUE_CAPACITY as u64 + 1,
                    "prompt_complete",
                    serde_json::json!({"sourceSeq": QUEUE_CAPACITY as u64 + 1}),
                )
                .unwrap();
            producer_submitted.store(true, Ordering::Release);
        });
        std::thread::sleep(Duration::from_millis(30));
        assert!(!submitted.load(Ordering::Acquire));
        assert_eq!(
            persistence
                .health("opaque-a")
                .unwrap()
                .unwrap()
                .pending_count,
            QUEUE_CAPACITY
        );
        target.release();
        producer.join().unwrap();
        assert!(submitted.load(Ordering::Acquire));
        persistence.shutdown().await.unwrap();
        let health = persistence.health("opaque-a").unwrap().unwrap();
        assert_eq!(health.pending_count, 0);
        assert_eq!(
            health.last_persisted_source_seq,
            health.last_accepted_source_seq
        );
        assert_eq!(target.records.lock()["opaque-a"].len(), QUEUE_CAPACITY + 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_duplicate_decreasing_and_unmapped_submissions_fail_before_append() {
        let target = Arc::new(FakeTarget::new());
        let persistence = ordered(Arc::clone(&target));
        assert_eq!(
            persistence
                .submit("opaque-a", 0, "message_chunk", Value::Null)
                .unwrap_err()
                .code,
            SOURCE_SEQUENCE_INVALID
        );
        assert_eq!(persistence.retained_worker_count(), 0);
        let unmapped_error = persistence
            .submit("unmapped-secret", 1, "message_chunk", Value::Null)
            .unwrap_err();
        assert_eq!(unmapped_error.code, "CONVERSATION_BINDING_NOT_FOUND");
        assert!(!unmapped_error.to_string().contains("unmapped-secret"));
        assert_eq!(persistence.retained_worker_count(), 0);
        persistence
            .submit(
                "opaque-a",
                2,
                "message_chunk",
                serde_json::json!({"sourceSeq": 2}),
            )
            .unwrap();
        for invalid in [2, 1] {
            let error = persistence
                .submit(
                    "opaque-a",
                    invalid,
                    "message_chunk",
                    serde_json::json!({"sourceSeq": invalid, "secret": "never-log"}),
                )
                .unwrap_err();
            assert_eq!(error.code, SOURCE_SEQUENCE_INVALID);
            assert!(!error.to_string().contains("never-log"));
            assert!(!error.to_string().contains("opaque-a"));
        }
        persistence.flush_all().await.unwrap_err();
        assert_eq!(target.append_count.load(Ordering::Acquire), 1);
        let _ = persistence.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn append_failure_is_stable_and_flush_never_claims_success() {
        let target = Arc::new(FakeTarget {
            fail_after: Some(1),
            ..FakeTarget::new()
        });
        let persistence = ordered(Arc::clone(&target));
        for source_seq in 1..=2 {
            persistence
                .submit(
                    "opaque-a",
                    source_seq,
                    "message_chunk",
                    serde_json::json!({"sourceSeq": source_seq, "prompt": "do-not-log"}),
                )
                .unwrap();
        }
        let error = persistence.flush_all().await.unwrap_err();
        assert_eq!(error.code, WRITER_UNHEALTHY);
        assert!(!error.to_string().contains("do-not-log"));
        assert!(!error.to_string().contains("opaque-a"));
        let health = persistence.health("opaque-a").unwrap().unwrap();
        assert_eq!(
            health.last_error_code,
            Some("CONVERSATION_EVENT_APPEND_FAILED")
        );
        assert_eq!(health.last_accepted_source_seq, 2);
        assert_eq!(health.last_persisted_source_seq, 1);
        let shutdown_error = persistence.shutdown().await.unwrap_err();
        assert_eq!(shutdown_error.code, WRITER_UNHEALTHY);
        assert!(!shutdown_error.to_string().contains("do-not-log"));
        assert!(!shutdown_error.to_string().contains("opaque-a"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_workers_retire_only_after_their_queue_is_drained() {
        let target = Arc::new(FakeTarget::new());
        let persistence = ordered(Arc::clone(&target));
        persistence
            .submit(
                "opaque-a",
                1,
                "message_chunk",
                serde_json::json!({"sourceSeq": 1}),
            )
            .unwrap();
        persistence.flush_all().await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(persistence.active_worker_count(), 0);
        let health = persistence.health("opaque-a").unwrap().unwrap();
        assert_eq!(health.pending_count, 0);
        assert_eq!(health.last_accepted_source_seq, 1);
        assert_eq!(health.last_persisted_source_seq, 1);
        persistence
            .submit(
                "opaque-a",
                2,
                "prompt_complete",
                serde_json::json!({"sourceSeq": 2}),
            )
            .unwrap();
        persistence.flush_all().await.unwrap();
        assert_eq!(target.records.lock()["opaque-a"].len(), 2);
        let restarted_health = persistence.health("opaque-a").unwrap().unwrap();
        assert_eq!(restarted_health.last_accepted_source_seq, 2);
        assert_eq!(restarted_health.last_persisted_source_seq, 2);
        persistence.shutdown().await.unwrap();
    }
}
