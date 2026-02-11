use communication_layer_request_reply::TcpConnection;
use dora_core::descriptor::Descriptor;
use dora_message::{
    BuildId,
    cli_to_coordinator::{BuildRequest, CliControlClient, ControlRequest},
    common::{GitSource, LogMessage},
    id::NodeId,
    tarpc,
};
use eyre::Context;
use std::{
    collections::BTreeMap,
    net::{SocketAddr, TcpStream},
};

use crate::common::rpc;
use crate::output::print_log_message;
use crate::session::DataflowSession;

pub fn build_distributed_dataflow(
    client: &CliControlClient,
    dataflow: Descriptor,
    git_sources: &BTreeMap<NodeId, GitSource>,
    dataflow_session: &DataflowSession,
    local_working_dir: Option<std::path::PathBuf>,
    uv: bool,
) -> eyre::Result<BuildId> {
    let build_id = rpc(client.build(
        tarpc::context::current(),
        BuildRequest {
            session_id: dataflow_session.session_id,
            dataflow,
            git_sources: git_sources.clone(),
            prev_git_sources: dataflow_session.git_sources.clone(),
            local_working_dir,
            uv,
        },
    ))?;
    eprintln!("dataflow build triggered: {build_id}");
    Ok(build_id)
}

pub fn wait_until_dataflow_built(
    build_id: BuildId,
    client: &CliControlClient,
    coordinator_socket: SocketAddr,
    log_level: log::LevelFilter,
) -> eyre::Result<BuildId> {
    // subscribe to build log messages (TCP streaming)
    let mut log_session = TcpConnection {
        stream: TcpStream::connect(coordinator_socket)
            .wrap_err("failed to connect to dora coordinator")?,
    };
    log_session
        .send(
            &serde_json::to_vec(&ControlRequest::BuildLogSubscribe {
                build_id,
                level: log_level,
            })
            .wrap_err("failed to serialize message")?,
        )
        .wrap_err("failed to send build log subscribe request to coordinator")?;
    std::thread::spawn(move || {
        while let Ok(raw) = log_session.receive() {
            let parsed: eyre::Result<LogMessage> =
                serde_json::from_slice(&raw).context("failed to parse log message");
            match parsed {
                Ok(log_message) => {
                    print_log_message(log_message, false, true);
                }
                Err(err) => {
                    tracing::warn!("failed to parse log message: {err:?}")
                }
            }
        }
    });

    rpc(client.wait_for_build(tarpc::context::current(), build_id))?;
    eprintln!("dataflow build finished successfully");
    Ok(build_id)
}
