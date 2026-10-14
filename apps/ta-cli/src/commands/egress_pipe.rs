// egress_pipe.rs — host end of the Windows egress relay (v0.17.12.9)
//
// An AppContainer cannot reach host loopback, and TA grants no loopback
// exemption, so the per-goal egress proxy is carried across the boundary by a
// named pipe. This is the host (outside the container) half: it serves a pipe
// whose DACL admits only the launching user and this goal's container SID, and
// forwards every accepted connection to the proxy's loopback port. The
// in-container half is `ta_runtime::windows_relay` (run by `ta-sandbox-exec`).
//
// The relay moves bytes only. Hostname policy and the audit log stay in the
// proxy, so a bug here cannot widen what the agent may reach.

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

/// A running host relay. Stops when dropped.
pub struct HostPipeRelay {
    pipe: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn create_instance(
    pipe: &str,
    security: &mut ta_runtime::windows_pipe::PipeSecurity,
    first: bool,
) -> std::io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    // First instance: fail if the name already exists, so a squatter cannot
    // stand in for the relay. Remote clients are never served.
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: `security` owns a valid SECURITY_ATTRIBUTES that outlives this call.
    unsafe { options.create_with_security_attributes_raw(pipe, security.as_raw()) }
}

impl HostPipeRelay {
    /// Create the pipe and start forwarding to `127.0.0.1:proxy_port`.
    /// Returns once the pipe exists, so the agent can be launched straight away.
    pub fn start(goal_id: &str, proxy_port: u16) -> Result<Self, String> {
        use ta_runtime::windows_policy::{
            container_name, new_pipe_nonce, pipe_name, pipe_sddl, PipePurpose,
        };
        let container = container_name(goal_id);
        let container_sid = ta_runtime::sandbox_windows::container_sid_string(&container)
            .map_err(|e| format!("cannot derive the SID of AppContainer {container}: {e}"))?;
        let owner_sid = ta_runtime::windows_pipe::current_user_sid_string()?;
        let sddl = pipe_sddl(&container_sid, &owner_sid)?;
        let mut security = ta_runtime::windows_pipe::PipeSecurity::from_sddl(&sddl)?;
        let pipe = pipe_name(PipePurpose::Egress, goal_id, &new_pipe_nonce());

        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let thread_pipe = pipe.clone();
        let thread = std::thread::Builder::new()
            .name("ta-egress-pipe".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("cannot start the relay runtime: {e}")));
                        return;
                    }
                };
                rt.block_on(async move {
                    let mut server = match create_instance(&thread_pipe, &mut security, true) {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = ready_tx.send(Err(format!(
                                "cannot create the egress pipe {thread_pipe}: {e}"
                            )));
                            return;
                        }
                    };
                    let _ = ready_tx.send(Ok(()));
                    loop {
                        tokio::select! {
                            _ = &mut stop_rx => break,
                            conn = server.connect() => {
                                if let Err(e) = conn {
                                    tracing::warn!(pipe = %thread_pipe, error = %e,
                                        "egress pipe accept failed; relay stopping");
                                    break;
                                }
                                let connected = server;
                                server = match create_instance(&thread_pipe, &mut security, false) {
                                    Ok(s) => s,
                                    Err(e) => {
                                        tracing::error!(pipe = %thread_pipe, error = %e,
                                            "cannot create the next egress pipe instance; relay stopping");
                                        break;
                                    }
                                };
                                tokio::spawn(forward(connected, proxy_port));
                            }
                        }
                    }
                });
            })
            .map_err(|e| format!("cannot start the egress relay thread: {e}"))?;

        match ready_rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self {
                pipe,
                stop: Some(stop_tx),
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the egress relay did not report ready within 10s".to_string()),
        }
    }

    /// The pipe name the in-container relay connects to.
    pub fn pipe_name(&self) -> &str {
        &self.pipe
    }
}

async fn forward(mut pipe: NamedPipeServer, proxy_port: u16) {
    match tokio::net::TcpStream::connect(("127.0.0.1", proxy_port)).await {
        Ok(mut tcp) => {
            if let Err(e) = tokio::io::copy_bidirectional(&mut pipe, &mut tcp).await {
                // A reset when either side closes is normal at the end of a connection.
                tracing::debug!(error = %e, "egress relay connection ended");
            }
        }
        Err(e) => tracing::warn!(
            port = proxy_port,
            error = %e,
            "egress relay cannot reach the egress proxy; the agent's request is dropped"
        ),
    }
}

impl Drop for HostPipeRelay {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
