use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use dashmap::DashMap;
use dora_core::uhlc::HLC;
use dora_message::{
    BuildId, DataflowId, common::DaemonId, coordinator_to_daemon::CoordinatorToDaemonClient,
    daemon_to_coordinator::DataflowDaemonResult,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    ArchivedDataflow, CachedResult, DaemonConnections, Event, RunningBuild, RunningDataflow,
};

pub struct CoordinatorState {
    pub clock: Arc<HLC>,
    pub running_builds: DashMap<BuildId, RunningBuild>,
    pub finished_builds: DashMap<BuildId, CachedResult>,
    pub running_dataflows: DashMap<DataflowId, RunningDataflow>,
    pub dataflow_results: DashMap<DataflowId, BTreeMap<DaemonId, DataflowDaemonResult>>,
    pub archived_dataflows: DashMap<DataflowId, ArchivedDataflow>,
    pub daemon_connections: DaemonConnections,
    pub daemon_events_tx: mpsc::Sender<Event>,
    pub abort_handle: futures::stream::AbortHandle,
}
