//! The background database worker.
//!
//! All blocking I/O — connect, schema harvest, selects, commits — runs on this
//! dedicated thread. The UI thread talks to it over a pair of channels and
//! never blocks on the database, so the render loop stays responsive and can
//! animate a spinner while work is in flight.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::config::ConnectionConfig;
use crate::db::query::SelectQuery;
use crate::db::{self, DatabaseEngine};
use crate::error::DbError;
use crate::model::delta::RowMutation;
use crate::model::schema::{Catalog, SchemaName};
use crate::model::value::Value;

/// A request from the UI thread to the worker.
#[derive(Debug)]
pub enum WorkerRequest {
    HarvestSchema,
    /// List the switchable namespaces (Postgres schemas / MySQL databases).
    ListSchemas,
    /// Switch the session default namespace and return its catalog.
    SwitchSchema(SchemaName),
    /// Run a browse query. `id` lets the UI discard stale results when the user
    /// has moved on to a newer query.
    RunSelect {
        id: u64,
        query: SelectQuery,
    },
    /// Run a raw custom query from the query pane.
    RunRawQuery {
        id: u64,
        sql: String,
    },
    Commit(Vec<RowMutation>),
    Shutdown,
}

/// A reply from the worker to the UI thread.
#[derive(Debug)]
pub enum WorkerResponse {
    /// The connection was established; the UI may now request the schema.
    Connected,
    /// A harvested catalog together with the namespace it came from. Shared as
    /// an `Arc` so the worker cache and the UI hold one snapshot, not copies.
    Schema {
        schema: SchemaName,
        catalog: Arc<Catalog>,
    },
    /// The result of [`WorkerRequest::ListSchemas`], with the active namespace.
    SchemaList {
        schemas: Vec<SchemaName>,
        active: SchemaName,
    },
    Rows {
        id: u64,
        rows: Vec<Vec<Value>>,
    },
    /// Result of a raw custom query: columns are discovered from the result.
    RawRows {
        id: u64,
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
        truncated: bool,
    },
    Committed,
    /// An operation failed; the carried string is a user-facing message.
    Failed(String),
}

/// The UI-side endpoints of the worker.
pub struct WorkerHandle {
    tx: Sender<WorkerRequest>,
    rx: Receiver<WorkerResponse>,
}

impl WorkerHandle {
    /// Spawn the worker thread for `config`. The worker attempts to connect
    /// immediately and reports the outcome over the response channel.
    pub fn spawn(config: ConnectionConfig) -> WorkerHandle {
        let (req_tx, req_rx) = mpsc::channel::<WorkerRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<WorkerResponse>();

        thread::spawn(move || run(config, req_rx, resp_tx));

        WorkerHandle {
            tx: req_tx,
            rx: resp_rx,
        }
    }

    /// Send a request to the worker. A send error means the worker thread has
    /// gone away, which the UI treats as a fatal condition on its next poll.
    pub fn send(&self, request: WorkerRequest) {
        // If the worker is gone the channel is closed; the UI will observe the
        // closed response channel and shut down, so dropping the error is safe.
        let _ = self.tx.send(request);
    }

    /// Drain all currently-available responses without blocking.
    pub fn try_recv(&self) -> Vec<WorkerResponse> {
        let mut out = Vec::new();
        while let Ok(resp) = self.rx.try_recv() {
            out.push(resp);
        }
        out
    }
}

/// The outcome of a one-shot connection test.
#[derive(Debug)]
pub enum TestOutcome {
    Ok,
    Failed(String),
}

/// A throwaway connection attempt used by the connection form's "test" action.
///
/// It connects on its own thread and reports the outcome over a channel, then
/// drops the connection immediately — the UI never blocks on the attempt, and a
/// successful test does not become the session connection.
#[derive(Debug)]
pub struct TestHandle {
    rx: Receiver<TestOutcome>,
}

impl TestHandle {
    pub fn spawn(config: ConnectionConfig) -> TestHandle {
        let (tx, rx) = mpsc::channel::<TestOutcome>();
        thread::spawn(move || {
            let outcome = match db::connect(&config) {
                Ok(_engine) => TestOutcome::Ok,
                Err(e) => TestOutcome::Failed(e.to_string()),
            };
            let _ = tx.send(outcome);
        });
        TestHandle { rx }
    }

    /// The outcome if the attempt has finished, else `None`.
    pub fn try_recv(&self) -> Option<TestOutcome> {
        self.rx.try_recv().ok()
    }
}

fn run(config: ConnectionConfig, req_rx: Receiver<WorkerRequest>, resp_tx: Sender<WorkerResponse>) {
    let mut engine: Box<dyn DatabaseEngine> = match db::connect(&config) {
        Ok(engine) => engine,
        Err(e) => {
            let _ = resp_tx.send(WorkerResponse::Failed(e.to_string()));
            return;
        }
    };
    let mut active = match engine.current_schema() {
        Ok(schema) => schema,
        Err(e) => {
            let _ = resp_tx.send(WorkerResponse::Failed(e.to_string()));
            return;
        }
    };
    if resp_tx.send(WorkerResponse::Connected).is_err() {
        return;
    }

    // The worker keeps one catalog per visited namespace so it can resolve table
    // metadata (primary keys, column types) when compiling commits, and so a
    // schema switch is instant once that schema has been loaded before.
    let mut cache: HashMap<SchemaName, Arc<Catalog>> = HashMap::new();

    while let Ok(request) = req_rx.recv() {
        let response = match request {
            WorkerRequest::Shutdown => break,
            WorkerRequest::HarvestSchema => match engine.harvest_schema() {
                Ok(catalog) => {
                    let catalog = Arc::new(catalog);
                    cache.insert(active.clone(), Arc::clone(&catalog));
                    WorkerResponse::Schema {
                        schema: active.clone(),
                        catalog,
                    }
                }
                Err(e) => WorkerResponse::Failed(e.to_string()),
            },
            WorkerRequest::ListSchemas => match engine.list_schemas() {
                Ok(schemas) => WorkerResponse::SchemaList {
                    schemas,
                    active: active.clone(),
                },
                Err(e) => WorkerResponse::Failed(e.to_string()),
            },
            WorkerRequest::SwitchSchema(schema) => {
                match switch_schema(engine.as_mut(), &mut cache, &mut active, schema) {
                    Ok(catalog) => WorkerResponse::Schema {
                        schema: active.clone(),
                        catalog,
                    },
                    Err(e) => WorkerResponse::Failed(e.to_string()),
                }
            }
            WorkerRequest::RunSelect { id, query } => match engine.run_select(&query) {
                Ok(rows) => WorkerResponse::Rows { id, rows },
                Err(e) => WorkerResponse::Failed(e.to_string()),
            },
            WorkerRequest::RunRawQuery { id, sql } => match engine.run_raw(&sql) {
                Ok(result) => WorkerResponse::RawRows {
                    id,
                    columns: result.columns,
                    rows: result.rows,
                    truncated: result.truncated,
                },
                Err(e) => WorkerResponse::Failed(e.to_string()),
            },
            WorkerRequest::Commit(mutations) => match cache.get(&active) {
                Some(catalog) => match engine.commit(&mutations, catalog) {
                    Ok(()) => WorkerResponse::Committed,
                    Err(e) => WorkerResponse::Failed(e.to_string()),
                },
                None => WorkerResponse::Failed("schema is not loaded yet".to_string()),
            },
        };
        if resp_tx.send(response).is_err() {
            break;
        }
    }
}

/// Make `schema` the session default and return its catalog, reusing the cache
/// when possible. The `SET`/`USE` always runs, even on a cache hit, so the
/// connection's default namespace follows the active schema that commits and
/// raw queries resolve against.
fn switch_schema(
    engine: &mut dyn DatabaseEngine,
    cache: &mut HashMap<SchemaName, Arc<Catalog>>,
    active: &mut SchemaName,
    schema: SchemaName,
) -> Result<Arc<Catalog>, DbError> {
    engine.set_schema(&schema)?;
    *active = schema.clone();
    if let Some(cached) = cache.get(&schema) {
        return Ok(Arc::clone(cached));
    }
    let catalog = Arc::new(engine.harvest_schema()?);
    cache.insert(schema, Arc::clone(&catalog));
    Ok(catalog)
}
