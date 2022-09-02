use super::OperatorEvent;
use dora_node_api::{communication::Publisher, config::DataId};
use eyre::{bail, eyre, Context};
use pyo3::{pyclass, types::IntoPyDict, types::PyBytes, Py, Python};
use std::{
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    sync::Arc,
    thread,
};
use tokio::sync::mpsc::Sender;

fn traceback(err: pyo3::PyErr) -> eyre::Report {
    Python::with_gil(|py| {
        eyre::Report::msg(format!(
            "{}\n{err}",
            err.traceback(py)
                .expect("PyError should have a traceback")
                .format()
                .expect("Traceback could not be formatted")
        ))
    })
}

pub fn spawn(
    path: &Path,
    events_tx: Sender<OperatorEvent>,
    inputs: flume::Receiver<dora_node_api::Input>,
    publishers: HashMap<DataId, Box<dyn Publisher>>,
) -> eyre::Result<()> {
    if !path.exists() {
        bail!("No python file exists at {}", path.display());
    }
    let path = path
        .canonicalize()
        .wrap_err_with(|| format!("no file found at `{}`", path.display()))?;
    let path_cloned = path.clone();

    let send_output = SendOutputCallback {
        publishers: Arc::new(publishers),
    };

    let init_operator = move |py: Python| {
        if let Some(parent_path) = path.parent() {
            let parent_path = parent_path
                .to_str()
                .ok_or_else(|| eyre!("module path is not valid utf8"))?;
            let sys = py.import("sys").wrap_err("failed to import `sys` module")?;
            let sys_path = sys
                .getattr("path")
                .wrap_err("failed to import `sys.path` module")?;
            let sys_path_append = sys_path
                .getattr("append")
                .wrap_err("`sys.path.append` was not found")?;
            sys_path_append
                .call1((parent_path,))
                .wrap_err("failed to append module path to python search path")?;
        }

        let module_name = path
            .file_stem()
            .ok_or_else(|| eyre!("module path has no file stem"))?
            .to_str()
            .ok_or_else(|| eyre!("module file stem is not valid utf8"))?;
        let module = py.import(module_name).map_err(traceback)?;
        let operator_class = module
            .getattr("Operator")
            .wrap_err("no `Operator` class found in module")?;

        let locals = [("Operator", operator_class)].into_py_dict(py);
        let operator = py
            .eval("Operator()", None, Some(locals))
            .map_err(traceback)?;
        Result::<_, eyre::Report>::Ok(Py::from(operator))
    };

    let python_runner = move || {
        let operator =
            Python::with_gil(init_operator).wrap_err("failed to init python operator")?;

        while let Ok(input) = inputs.recv() {
            let status_enum = Python::with_gil(|py| {
                operator
                    .call_method1(
                        py,
                        "on_input",
                        (
                            input.id.to_string(),
                            PyBytes::new(py, &input.data),
                            send_output.clone(),
                        ),
                    )
                    .map_err(traceback)
            })?;
            let status_val = Python::with_gil(|py| status_enum.getattr(py, "value"))
                .wrap_err("on_input must have enum return value")?;
            let status: i32 = Python::with_gil(|py| status_val.extract(py))
                .wrap_err("on_input has invalid return value")?;
            match status {
                0 => {}     // ok
                1 => break, // stop
                other => bail!("on_input returned invalid status {other}"),
            }
        }

        Python::with_gil(|py| {
            let operator = operator.as_ref(py);
            if operator
                .hasattr("drop_operator")
                .wrap_err("failed to look for drop_operator")?
            {
                operator.call_method0("drop_operator")?;
            }
            Result::<_, eyre::Report>::Ok(())
        })?;

        Result::<_, eyre::Report>::Ok(())
    };

    thread::spawn(move || {
        let closure = AssertUnwindSafe(|| {
            python_runner()
                .wrap_err_with(|| format!("error in Python module at {}", path_cloned.display()))
        });

        match catch_unwind(closure) {
            Ok(Ok(())) => {
                let _ = events_tx.blocking_send(OperatorEvent::Finished);
            }
            Ok(Err(err)) => {
                let _ = events_tx.blocking_send(OperatorEvent::Error(err));
            }
            Err(panic) => {
                let _ = events_tx.blocking_send(OperatorEvent::Panic(panic));
            }
        }
    });

    Ok(())
}

#[pyclass]
#[derive(Clone)]
struct SendOutputCallback {
    publishers: Arc<HashMap<DataId, Box<dyn Publisher>>>,
}

#[allow(unsafe_op_in_unsafe_fn)]
mod callback_impl {
    use super::SendOutputCallback;
    use eyre::{eyre, Context};
    use pyo3::{pymethods, PyResult};

    #[pymethods]
    impl SendOutputCallback {
        fn __call__(&mut self, output: &str, data: &[u8]) -> PyResult<()> {
            match self.publishers.get(output) {
                Some(publisher) => publisher.publish(data).context("publish failed"),
                None => Err(eyre!(
                    "unexpected output {output} (not defined in dataflow config)"
                )),
            }
            .map_err(|err| err.into())
        }
    }
}
