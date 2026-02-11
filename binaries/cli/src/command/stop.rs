use super::{Executable, default_tracing};
use crate::common::{
    connect_to_coordinator_rpc, handle_dataflow_result, query_running_dataflows, rpc,
};
use dora_core::topics::{DORA_COORDINATOR_PORT_CONTROL_DEFAULT, LOCALHOST};
use dora_message::{
    cli_to_coordinator::CliControlClient,
    coordinator_to_cli::ControlRequestReply,
    tarpc,
};
use duration_str::parse;
use eyre::{Context, bail};
use std::net::IpAddr;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, clap::Args)]
/// Stop a running dataflow. If no id or name is provided, you will be able to choose between the running dataflows.
///
/// You could specify the strategy to stop the dataflow with `--grace-duration` or `--force`.
pub struct Stop {
    /// UUID of the dataflow that should be stopped
    uuid: Option<Uuid>,
    /// Name of the dataflow that should be stopped
    #[clap(long)]
    name: Option<String>,
    /// Kill the dataflow if it doesn't stop after the given duration
    ///
    /// Specifically, it does the following:
    /// 1. Sends `Event::Stop` to all nodes of the dataflow.
    /// 2. After DURATION, performs a soft kill (sending SIGTERM, or Ctrl-Break on Windows).
    /// 3. If the dataflow is still running after DURATION * 0.5, terminates all its processes.
    #[clap(
        long,
        value_name = "DURATION",
        group = "strategy",
        verbatim_doc_comment
    )]
    #[arg(value_parser = parse)]
    grace_duration: Option<Duration>,
    /// Force stop the dataflow by immediately terminating all its processes
    #[clap(short, long, action, group = "strategy")]
    force: bool,
    /// Address of the dora coordinator
    #[clap(long, value_name = "IP", default_value_t = LOCALHOST)]
    coordinator_addr: IpAddr,
    /// Port number of the coordinator control server
    #[clap(long, value_name = "PORT", default_value_t = DORA_COORDINATOR_PORT_CONTROL_DEFAULT)]
    coordinator_port: u16,
}

impl Executable for Stop {
    fn execute(self) -> eyre::Result<()> {
        default_tracing()?;
        let rpc_port = self.coordinator_port + 1;
        let client = connect_to_coordinator_rpc(self.coordinator_addr, rpc_port)
            .wrap_err("could not connect to dora coordinator")?;
        match (self.uuid, self.name) {
            (Some(uuid), _) => stop_dataflow(uuid, self.grace_duration, self.force, &client),
            (None, Some(name)) => {
                stop_dataflow_by_name(name, self.grace_duration, self.force, &client)
            }
            (None, None) => {
                stop_dataflow_interactive(self.grace_duration, self.force, &client)
            }
        }
    }
}

fn stop_dataflow_interactive(
    grace_duration: Option<Duration>,
    force: bool,
    client: &CliControlClient,
) -> eyre::Result<()> {
    let list = query_running_dataflows(client).wrap_err("failed to query running dataflows")?;
    let active = list.get_active();
    if active.is_empty() {
        eprintln!("No dataflows are running");
    } else {
        let selection = inquire::Select::new("Choose dataflow to stop:", active).prompt()?;
        stop_dataflow(selection.uuid, grace_duration, force, client)?;
    }

    Ok(())
}

fn stop_dataflow(
    uuid: Uuid,
    grace_duration: Option<Duration>,
    force: bool,
    client: &CliControlClient,
) -> Result<(), eyre::ErrReport> {
    let result = rpc(client.stop(tarpc::context::current(), uuid, grace_duration, force))?;
    match result {
        ControlRequestReply::DataflowStopped { uuid, result } => {
            handle_dataflow_result(result, Some(uuid))
        }
        other => bail!("unexpected stop dataflow reply: {other:?}"),
    }
}

fn stop_dataflow_by_name(
    name: String,
    grace_duration: Option<Duration>,
    force: bool,
    client: &CliControlClient,
) -> Result<(), eyre::ErrReport> {
    let result = rpc(client.stop_by_name(tarpc::context::current(), name, grace_duration, force))?;
    match result {
        ControlRequestReply::DataflowStopped { uuid, result } => {
            handle_dataflow_result(result, Some(uuid))
        }
        other => bail!("unexpected stop dataflow reply: {other:?}"),
    }
}
