use crate::{
    DaemonConnections,
    tcp_utils::{tcp_receive, tcp_send},
};

use dora_core::{descriptor::DescriptorExt, uhlc::HLC};
use dora_message::{
    BuildId, SessionId,
    common::DaemonId,
    coordinator_to_daemon::{DaemonCoordinatorEvent, SpawnDataflowNodes, Timestamped},
    daemon_to_coordinator::DaemonCoordinatorReply,
    descriptor::{Descriptor, ResolvedNode},
    id::NodeId,
};
use eyre::{ContextCompat, WrapErr, bail, eyre};
use itertools::Itertools;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use uuid::{NoContext, Timestamp, Uuid};

/// Plan a dataflow spawn without sending any commands to daemons yet.
///
/// Resolves nodes, generates a UUID, and determines which daemon handles
/// each node group. The actual spawn commands are prepared as serialized
/// messages but not sent.
#[tracing::instrument(skip(daemon_connections, clock))]
pub(super) fn plan_dataflow(
    build_id: Option<BuildId>,
    session_id: SessionId,
    dataflow: &Descriptor,
    local_working_dir: Option<PathBuf>,
    daemon_connections: &DaemonConnections,
    clock: &HLC,
    uv: bool,
    write_events_to: Option<PathBuf>,
) -> eyre::Result<DataflowPlan> {
    let nodes = dataflow.resolve_aliases_and_set_defaults()?;
    let uuid = Uuid::new_v7(Timestamp::now(NoContext));

    let nodes_by_daemon = nodes
        .values()
        .into_group_map_by(|n| n.deploy.as_ref().and_then(|d| d.machine.as_ref()));

    let mut daemons = BTreeSet::new();
    let mut node_to_daemon = BTreeMap::new();
    let mut daemon_messages: Vec<(DaemonId, Vec<u8>)> = Vec::new();

    for (machine, nodes_on_machine) in &nodes_by_daemon {
        let spawn_nodes = nodes_on_machine.iter().map(|n| n.id.clone()).collect();
        tracing::debug!(
            "Spawning dataflow `{uuid}` on machine `{machine:?}` (nodes: {spawn_nodes:?})"
        );

        let daemon_id = resolve_daemon_for_machine(daemon_connections, machine.map(|m| m.as_str()))
            .wrap_err_with(|| format!("failed to resolve daemon for machine `{machine:?}`"))?;

        let spawn_command = SpawnDataflowNodes {
            build_id,
            session_id,
            dataflow_id: uuid,
            local_working_dir: local_working_dir.clone(),
            nodes: nodes.clone(),
            dataflow_descriptor: dataflow.clone(),
            spawn_nodes,
            uv,
            write_events_to: write_events_to.clone(),
        };
        let message = serde_json::to_vec(&Timestamped {
            inner: DaemonCoordinatorEvent::Spawn(spawn_command),
            timestamp: clock.new_timestamp(),
        })?;

        daemon_messages.push((daemon_id.clone(), message));
        daemons.insert(daemon_id.clone());

        // Map each node on this machine to its daemon
        for node in nodes_on_machine {
            node_to_daemon.insert(node.id.clone(), daemon_id.clone());
        }
    }

    Ok(DataflowPlan {
        uuid,
        daemons,
        nodes,
        node_to_daemon,
        daemon_messages,
    })
}

/// Send the prepared spawn commands to the daemons and wait for acknowledgments.
pub(super) async fn execute_dataflow_plan(
    uuid: Uuid,
    daemon_messages: &[(DaemonId, Vec<u8>)],
    daemon_connections: &DaemonConnections,
) -> eyre::Result<()> {
    for (daemon_id, message) in daemon_messages {
        send_spawn_to_daemon(daemon_connections, daemon_id, message)
            .await
            .wrap_err_with(|| format!("failed to spawn dataflow on daemon `{daemon_id}`"))?;
    }

    tracing::info!("successfully triggered dataflow spawn `{uuid}`",);

    Ok(())
}

fn resolve_daemon_for_machine(
    daemon_connections: &DaemonConnections,
    machine: Option<&str>,
) -> Result<DaemonId, eyre::ErrReport> {
    let daemon_id = match machine {
        Some(machine) => daemon_connections
            .get_matching_daemon_id(machine)
            .wrap_err_with(|| format!("no matching daemon for machine id {machine:?}"))?
            .clone(),
        None => daemon_connections
            .unnamed()
            .next()
            .wrap_err("no unnamed daemon connections")?
            .clone(),
    };
    Ok(daemon_id)
}

async fn send_spawn_to_daemon(
    daemon_connections: &DaemonConnections,
    daemon_id: &DaemonId,
    message: &[u8],
) -> Result<(), eyre::ErrReport> {
    // Clone the Arc<Mutex<TcpStream>> and immediately drop the DashMap lock.
    let stream = daemon_connections
        .get_stream(daemon_id)
        .wrap_err_with(|| format!("no daemon connection for daemon `{daemon_id}`"))?;
    let mut stream = stream.lock().await;
    tcp_send(&mut stream, message)
        .await
        .wrap_err("failed to send spawn message to daemon")?;

    let reply_raw = tcp_receive(&mut stream)
        .await
        .wrap_err("failed to receive spawn reply from daemon")?;
    match serde_json::from_slice(&reply_raw)
        .wrap_err("failed to deserialize spawn reply from daemon")?
    {
        DaemonCoordinatorReply::TriggerSpawnResult(result) => result
            .map_err(|e| eyre!(e))
            .wrap_err("daemon returned an error")?,
        _ => bail!("unexpected reply"),
    }
    Ok(())
}

pub struct DataflowPlan {
    pub uuid: Uuid,
    pub daemons: BTreeSet<DaemonId>,
    pub nodes: BTreeMap<NodeId, ResolvedNode>,
    pub node_to_daemon: BTreeMap<NodeId, DaemonId>,
    pub daemon_messages: Vec<(DaemonId, Vec<u8>)>,
}
