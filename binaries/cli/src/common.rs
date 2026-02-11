use crate::{LOCALHOST, formatting::FormatDataflowError};
use dora_core::{
    descriptor::{Descriptor, source_is_url},
    topics::DORA_COORDINATOR_PORT_CONTROL_DEFAULT,
};
use dora_download::download_file;
use dora_message::{
    cli_to_coordinator::CliControlClient,
    coordinator_to_cli::{DataflowList, DataflowResult},
    tarpc::{self, client, tokio_serde},
};
use eyre::{Context, ContextCompat, bail};
use std::{
    env::current_dir,
    future::Future,
    net::IpAddr,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Call a tarpc RPC method, unwrapping the two Result layers:
///
/// 1. Transport-level error (e.g. `RpcError`)
/// 2. `Result<T, String>` from the application (coordinator) layer
pub(crate) async fn rpc<T, E: std::error::Error + Send + Sync + 'static>(
    future: impl Future<Output = Result<Result<T, String>, E>>,
) -> eyre::Result<T> {
    future
        .await
        .context("RPC transport error")?
        .map_err(|e| eyre::eyre!(e))
}

pub(crate) fn handle_dataflow_result(
    result: DataflowResult,
    uuid: Option<Uuid>,
) -> Result<(), eyre::Error> {
    if result.is_ok() {
        Ok(())
    } else {
        Err(match uuid {
            Some(uuid) => {
                eyre::eyre!("Dataflow {uuid} failed:\n{}", FormatDataflowError(&result))
            }
            None => {
                eyre::eyre!("Dataflow failed:\n{}", FormatDataflowError(&result))
            }
        })
    }
}

pub(crate) async fn query_running_dataflows(
    client: &CliControlClient,
) -> eyre::Result<DataflowList> {
    rpc(client.list(tarpc::context::current())).await
}

pub(crate) async fn resolve_dataflow_identifier_interactive(
    client: &CliControlClient,
    name_or_uuid: Option<&str>,
) -> eyre::Result<Uuid> {
    if let Some(uuid) = name_or_uuid.and_then(|s| Uuid::parse_str(s).ok()) {
        return Ok(uuid);
    }

    let list = query_running_dataflows(client)
        .await
        .wrap_err("failed to query running dataflows")?;
    let active: Vec<dora_message::coordinator_to_cli::DataflowIdAndName> = list.get_active();
    if let Some(name) = name_or_uuid {
        let Some(dataflow) = active.iter().find(|it| it.name.as_deref() == Some(name)) else {
            bail!("No dataflow with name `{name}` is running");
        };
        return Ok(dataflow.uuid);
    }
    Ok(match &active[..] {
        [] => bail!("No dataflows are running"),
        [entry] => entry.uuid,
        _ => {
            inquire::Select::new("Choose dataflow:", active)
                .prompt()?
                .uuid
        }
    })
}

#[derive(Debug, clap::Args)]
pub(crate) struct CoordinatorOptions {
    /// Address of the dora coordinator
    #[clap(long, value_name = "IP", default_value_t = LOCALHOST)]
    pub coordinator_addr: IpAddr,
    /// Port number of the coordinator control server
    #[clap(long, value_name = "PORT", default_value_t = DORA_COORDINATOR_PORT_CONTROL_DEFAULT)]
    pub coordinator_port: u16,
}

impl CoordinatorOptions {
    pub async fn connect_rpc(&self) -> eyre::Result<CliControlClient> {
        connect_to_coordinator_rpc(self.coordinator_addr, self.coordinator_port).await
    }
}

/// Connect to the coordinator's tarpc RPC service.
///
/// The RPC port is conventionally `control_port + 1`.
pub(crate) async fn connect_to_coordinator_rpc(
    addr: IpAddr,
    control_port: u16,
) -> eyre::Result<CliControlClient> {
    let rpc_port = control_port + 1;
    let transport =
        tarpc::serde_transport::tcp::connect((addr, rpc_port), tokio_serde::formats::Json::default)
            .await
            .context("failed to connect tarpc client to coordinator")?;
    let client = CliControlClient::new(client::Config::default(), transport).spawn();
    Ok(client)
}

pub(crate) async fn resolve_dataflow(dataflow: String) -> eyre::Result<PathBuf> {
    let dataflow = if source_is_url(&dataflow) {
        // try to download the shared library
        let target_path = current_dir().context("Could not access the current dir")?;
        download_file(&dataflow, &target_path)
            .await
            .wrap_err("failed to download dataflow yaml file")?
    } else {
        PathBuf::from(dataflow)
    };
    Ok(dataflow)
}

pub(crate) async fn local_working_dir(
    dataflow_path: &Path,
    dataflow_descriptor: &Descriptor,
    client: &CliControlClient,
    coordinator_addr: IpAddr,
) -> eyre::Result<Option<PathBuf>> {
    Ok(
        if dataflow_descriptor
            .nodes
            .iter()
            .all(|n| n.deploy.as_ref().map(|d| d.machine.as_ref()).is_none())
            && cli_and_daemon_on_same_machine(client, coordinator_addr).await?
        {
            Some(
                dunce::canonicalize(dataflow_path)
                    .context("failed to canonicalize dataflow file path")?
                    .parent()
                    .context("dataflow path has no parent dir")?
                    .to_owned(),
            )
        } else {
            None
        },
    )
}

pub(crate) async fn cli_and_daemon_on_same_machine(
    client: &CliControlClient,
    coordinator_addr: IpAddr,
) -> eyre::Result<bool> {
    let result =
        rpc(client.cli_and_default_daemon_on_same_machine(tarpc::context::current())).await?;

    // Determine the CLI's outgoing IP toward the coordinator.
    // Uses a UDP socket to query the OS routing table (no packets sent).
    let cli_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect((coordinator_addr, 1))?;
            s.local_addr()
        })
        .ok()
        .map(|a| a.ip());

    Ok(result.default_daemon.is_some() && result.default_daemon == cli_ip)
}

pub(crate) fn write_events_to() -> Option<PathBuf> {
    std::env::var("DORA_WRITE_EVENTS_TO")
        .ok()
        .map(PathBuf::from)
}
