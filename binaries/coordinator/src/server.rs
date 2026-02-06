use std::{collections::BTreeSet, sync::Arc, time::Duration};

use dora_core::config::{NodeId, OperatorId};
use dora_message::{
    BuildId,
    cli_to_coordinator::{BuildRequest, CliControl, ControlRequest, StartRequest},
    common::DaemonId,
    coordinator_to_cli::{
        CliAndDefaultDaemonIps, ControlRequestReply, DataflowIdAndName, DataflowInfo, DataflowList,
        DataflowListEntry, DataflowResult, DataflowStatus, NodeInfo, NodeMetricsInfo,
    },
    tarpc::context::Context,
};
use eyre::{bail, eyre};
use petname::petname;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::{
    CachedResult, RunningDataflow, build_dataflow, dataflow_result, handle_destroy,
    reload_dataflow, resolve_name, retrieve_logs, start_dataflow, state::CoordinatorState,
    stop_dataflow,
};

pub(crate) struct ControlServer {
    pub(crate) state: Arc<CoordinatorState>,
}

/// Dispatch a `ControlRequest` to the corresponding `ControlServer` method.
///
/// Some variants (`WaitForBuild`, `WaitForSpawn`, `Stop`, `StopByName`,
/// `LogSubscribe`, `BuildLogSubscribe`) still need access to the reply_sender
/// directly and are **not** handled here – the caller must deal with them
/// before calling this function.
pub(crate) async fn handle_control_request(
    server: ControlServer,
    request: ControlRequest,
) -> eyre::Result<ControlRequestReply> {
    let ctx = Context::current();
    match request {
        ControlRequest::Build(request) => {
            let build_id = server.build(ctx, request).await?;
            Ok(ControlRequestReply::DataflowBuildTriggered { build_id })
        }
        ControlRequest::Start(request) => {
            let uuid = server.start(ctx, request).await?;
            Ok(ControlRequestReply::DataflowStartTriggered { uuid })
        }
        ControlRequest::Check { dataflow_uuid } => server.check(ctx, dataflow_uuid).await,
        ControlRequest::Reload {
            dataflow_id,
            node_id,
            operator_id,
        } => {
            let uuid = server
                .reload(ctx, dataflow_id, node_id, operator_id)
                .await?;
            Ok(ControlRequestReply::DataflowReloaded { uuid })
        }
        ControlRequest::Logs {
            uuid,
            name,
            node,
            tail,
        } => {
            let data = server.logs(ctx, uuid, name, node, tail).await?;
            Ok(ControlRequestReply::Logs(data))
        }
        ControlRequest::Info { dataflow_uuid } => {
            let info = server.info(ctx, dataflow_uuid).await?;
            Ok(ControlRequestReply::DataflowInfo {
                uuid: info.uuid,
                name: info.name,
                descriptor: info.descriptor,
            })
        }
        ControlRequest::Destroy => {
            server.destroy(ctx).await?;
            Ok(ControlRequestReply::DestroyOk)
        }
        ControlRequest::List => {
            let list = server.list(ctx).await?;
            Ok(ControlRequestReply::DataflowList(list))
        }
        ControlRequest::DaemonConnected => {
            let connected = server.daemon_connected(ctx).await?;
            Ok(ControlRequestReply::DaemonConnected(connected))
        }
        ControlRequest::ConnectedMachines => {
            let daemons = server.connected_machines(ctx).await?;
            Ok(ControlRequestReply::ConnectedDaemons(daemons))
        }
        ControlRequest::CliAndDefaultDaemonOnSameMachine => {
            let ips = server.cli_and_default_daemon_on_same_machine(ctx).await?;
            Ok(ControlRequestReply::CliAndDefaultDaemonIps {
                default_daemon: ips.default_daemon,
                cli: ips.cli,
            })
        }
        ControlRequest::GetNodeInfo => {
            let infos = server.get_node_info(ctx).await?;
            Ok(ControlRequestReply::NodeInfoList(infos))
        }
        // These are handled directly by the caller because they need
        // the raw reply_sender or the TCP connection.
        ControlRequest::WaitForBuild { .. }
        | ControlRequest::WaitForSpawn { .. }
        | ControlRequest::Stop { .. }
        | ControlRequest::StopByName { .. }
        | ControlRequest::LogSubscribe { .. }
        | ControlRequest::BuildLogSubscribe { .. } => {
            Err(eyre!("request {request:?} must be handled by the caller"))
        }
    }
}

impl CliControl for ControlServer {
    async fn build(self, context: Context, request: BuildRequest) -> eyre::Result<BuildId> {
        // assign a random build id
        let build_id = BuildId::generate();

        let result = build_dataflow(
            request,
            build_id,
            &self.state.clock,
            &self.state.daemon_connections,
        )
        .await;
        match result {
            Ok(build) => {
                self.state.running_builds.insert(build_id, build);
                Ok(build_id)
            }
            Err(err) => Err(err),
        }
    }

    async fn wait_for_build(self, context: Context, build_id: BuildId) -> eyre::Result<()> {
        if let Some(build) = self.state.running_builds.get_mut(&build_id) {
            // TODO: register a oneshot and await the result
            todo!("register reply sender and await build completion")
        } else if let Some(result) = self.state.finished_builds.get_mut(&build_id) {
            // TODO: register a oneshot and await the result
            todo!("register reply sender and await cached result")
        } else {
            Err(eyre!("unknown build id {build_id}"))
        }
    }

    async fn start(self, context: Context, request: StartRequest) -> eyre::Result<Uuid> {
        let StartRequest {
            build_id,
            session_id,
            dataflow,
            name,
            local_working_dir,
            uv,
            write_events_to,
        } = request;

        let name = name.or_else(|| petname(2, "-"));

        if let Some(name) = name.as_deref() {
            // check that name is unique
            if self
                .state
                .running_dataflows
                .iter()
                .any(|d| d.value().name.as_deref() == Some(name))
            {
                bail!("there is already a running dataflow with name `{name}`");
            }
        }
        let dataflow = start_dataflow(
            build_id,
            session_id,
            dataflow,
            local_working_dir,
            name,
            &self.state.daemon_connections,
            &self.state.clock,
            uv,
            write_events_to,
        )
        .await?;

        let uuid = dataflow.uuid;
        self.state.running_dataflows.insert(uuid, dataflow);
        Ok(uuid)
    }

    async fn wait_for_spawn(self, context: Context, dataflow_id: Uuid) -> eyre::Result<()> {
        if let Some(dataflow) = self.state.running_dataflows.get_mut(&dataflow_id) {
            // TODO: register a oneshot and await the spawn result
            todo!("register reply sender and await spawn completion")
        } else {
            Err(eyre!("unknown dataflow {dataflow_id}"))
        }
    }

    async fn reload(
        self,
        context: Context,
        dataflow_id: Uuid,
        node_id: NodeId,
        operator_id: Option<OperatorId>,
    ) -> eyre::Result<Uuid> {
        reload_dataflow(
            &self.state.running_dataflows,
            dataflow_id,
            node_id,
            operator_id,
            &self.state.daemon_connections,
            self.state.clock.new_timestamp(),
        )
        .await?;
        Ok(dataflow_id)
    }

    async fn check(
        self,
        context: Context,
        dataflow_uuid: Uuid,
    ) -> eyre::Result<ControlRequestReply> {
        let status = match self.state.running_dataflows.get(&dataflow_uuid) {
            Some(_) => ControlRequestReply::DataflowSpawned {
                uuid: dataflow_uuid,
            },
            None => ControlRequestReply::DataflowStopped {
                uuid: dataflow_uuid,
                result: self
                    .state
                    .dataflow_results
                    .get(&dataflow_uuid)
                    .map(|r| dataflow_result(r.value(), dataflow_uuid, &self.state.clock))
                    .unwrap_or_else(|| {
                        DataflowResult::ok_empty(dataflow_uuid, self.state.clock.new_timestamp())
                    }),
            },
        };
        Ok(status)
    }

    async fn stop(
        self,
        context: Context,
        dataflow_uuid: Uuid,
        grace_duration: Option<Duration>,
        force: bool,
    ) -> eyre::Result<ControlRequestReply> {
        if let Some(result) = self.state.dataflow_results.get(&dataflow_uuid) {
            let reply = ControlRequestReply::DataflowStopped {
                uuid: dataflow_uuid,
                result: dataflow_result(result.value(), dataflow_uuid, &self.state.clock),
            };
            return Ok(reply);
        }

        let dataflow = stop_dataflow(
            &self.state.running_dataflows,
            dataflow_uuid,
            &self.state.daemon_connections,
            self.state.clock.new_timestamp(),
            grace_duration,
            force,
        )
        .await?;

        // TODO: Instead of pushing a reply sender, await the dataflow stop via a channel
        todo!("await dataflow stop completion and return DataflowStopped reply")
    }

    async fn stop_by_name(
        self,
        context: Context,
        name: String,
        grace_duration: Option<Duration>,
        force: bool,
    ) -> eyre::Result<ControlRequestReply> {
        let dataflow_uuid = resolve_name(
            name,
            &self.state.running_dataflows,
            &self.state.archived_dataflows,
        )?;

        if let Some(result) = self.state.dataflow_results.get(&dataflow_uuid) {
            let reply = ControlRequestReply::DataflowStopped {
                uuid: dataflow_uuid,
                result: dataflow_result(result.value(), dataflow_uuid, &self.state.clock),
            };
            return Ok(reply);
        }

        let dataflow = stop_dataflow(
            &self.state.running_dataflows,
            dataflow_uuid,
            &self.state.daemon_connections,
            self.state.clock.new_timestamp(),
            grace_duration,
            force,
        )
        .await?;

        // TODO: Instead of pushing a reply sender, await the dataflow stop via a channel
        todo!("await dataflow stop completion and return DataflowStopped reply")
    }

    async fn logs(
        self,
        context: Context,
        uuid: Option<Uuid>,
        name: Option<String>,
        node: String,
        tail: Option<usize>,
    ) -> eyre::Result<Vec<u8>> {
        let dataflow_uuid = if let Some(uuid) = uuid {
            Ok(uuid)
        } else if let Some(name) = name {
            resolve_name(
                name,
                &self.state.running_dataflows,
                &self.state.archived_dataflows,
            )
        } else {
            Err(eyre!("No uuid"))
        }?;

        retrieve_logs(
            &self.state.running_dataflows,
            &self.state.archived_dataflows,
            dataflow_uuid,
            node.into(),
            &self.state.daemon_connections,
            self.state.clock.new_timestamp(),
            tail,
        )
        .await
    }

    async fn destroy(self, context: Context) -> eyre::Result<()> {
        tracing::info!("Received destroy command");

        handle_destroy(&self.state).await
    }

    async fn list(self, context: Context) -> eyre::Result<DataflowList> {
        let mut dataflows: Vec<_> = self.state.running_dataflows.iter().collect();
        dataflows.sort_by_key(|d| (d.value().name.clone(), d.value().uuid));

        let running = dataflows.into_iter().map(|d| DataflowListEntry {
            id: DataflowIdAndName {
                uuid: d.value().uuid,
                name: d.value().name.clone(),
            },
            status: DataflowStatus::Running,
        });
        let finished_failed = self.state.dataflow_results.iter().map(|r| {
            let uuid = *r.key();
            let name = self
                .state
                .archived_dataflows
                .get(&uuid)
                .and_then(|d| d.name.clone());
            let id = DataflowIdAndName { uuid, name };
            let status = if r.value().values().all(|r| r.is_ok()) {
                DataflowStatus::Finished
            } else {
                DataflowStatus::Failed
            };
            DataflowListEntry { id, status }
        });

        Ok(DataflowList(running.chain(finished_failed).collect()))
    }

    async fn info(self, context: Context, dataflow_uuid: Uuid) -> eyre::Result<DataflowInfo> {
        if let Some(dataflow) = self.state.running_dataflows.get(&dataflow_uuid) {
            Ok(DataflowInfo {
                uuid: dataflow.uuid,
                name: dataflow.name.clone(),
                descriptor: dataflow.descriptor.clone(),
            })
        } else {
            Err(eyre!("No running dataflow with uuid `{dataflow_uuid}`"))
        }
    }

    async fn daemon_connected(self, context: Context) -> eyre::Result<bool> {
        Ok(!self.state.daemon_connections.is_empty())
    }

    async fn connected_machines(self, context: Context) -> eyre::Result<BTreeSet<DaemonId>> {
        Ok(self.state.daemon_connections.keys().collect())
    }

    async fn cli_and_default_daemon_on_same_machine(
        self,
        context: Context,
    ) -> eyre::Result<CliAndDefaultDaemonIps> {
        let mut default_daemon_ip = None;
        if let Some(default_id) = self.state.daemon_connections.unnamed().next() {
            if let Some(connection) = self.state.daemon_connections.get(&default_id) {
                if let Ok(addr) = connection.stream.peer_addr() {
                    default_daemon_ip = Some(addr.ip());
                }
            }
        }
        Ok(CliAndDefaultDaemonIps {
            default_daemon: default_daemon_ip,
            cli: None, // filled later
        })
    }

    async fn get_node_info(self, context: Context) -> eyre::Result<Vec<NodeInfo>> {
        let mut node_infos = Vec::new();
        for r in self.state.running_dataflows.iter() {
            let dataflow = r.value();
            for (node_id, _node) in &dataflow.nodes {
                // Get the specific daemon this node is running on
                if let Some(daemon_id) = dataflow.node_to_daemon.get(node_id) {
                    // Get metrics if available
                    let metrics = dataflow.node_metrics.get(node_id).map(|m| {
                        NodeMetricsInfo {
                            pid: m.pid,
                            cpu_usage: m.cpu_usage,
                            // Use 1000 for MB (megabytes) instead of 1024 (mebibytes)
                            memory_mb: m.memory_bytes as f64 / 1000.0 / 1000.0,
                            disk_read_mb_s: m.disk_read_bytes.map(|b| b as f64 / 1000.0 / 1000.0),
                            disk_write_mb_s: m.disk_write_bytes.map(|b| b as f64 / 1000.0 / 1000.0),
                        }
                    });

                    node_infos.push(NodeInfo {
                        dataflow_id: dataflow.uuid,
                        dataflow_name: dataflow.name.clone(),
                        node_id: node_id.clone(),
                        daemon_id: daemon_id.clone(),
                        metrics,
                    });
                }
            }
        }
        Ok(node_infos)
    }
}
