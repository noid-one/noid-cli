use noid_types::*;
use std::sync::Arc;

use crate::router::AuthenticatedRequest;
use crate::transport::ResponseBuilder;
use crate::ServerState;

/// Map a backend error to an HTTP response. Known error patterns (not found,
/// already exists) get specific status codes; all others become 500s.
/// The error message is always passed through to the client.
fn map_backend_error(e: &anyhow::Error) -> ResponseBuilder {
    let msg = e.to_string();
    if msg.contains("not found") {
        ResponseBuilder::error(404, &msg)
    } else if msg.contains("already exists") {
        ResponseBuilder::error(409, &msg)
    } else {
        eprintln!("internal error: {e:#}");
        ResponseBuilder::error(500, &msg)
    }
}

pub fn healthz() -> ResponseBuilder {
    ResponseBuilder::json(200, &serde_json::json!({"status": "ok"}))
}

pub fn version() -> ResponseBuilder {
    ResponseBuilder::json(
        200,
        &VersionInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: 1,
        },
    )
}

pub fn whoami(req: &AuthenticatedRequest) -> ResponseBuilder {
    ResponseBuilder::json(
        200,
        &WhoamiResponse {
            user_id: req.user.id.clone(),
            name: req.user.name.clone(),
        },
    )
}

pub fn capabilities(state: &Arc<ServerState>) -> ResponseBuilder {
    ResponseBuilder::json(
        200,
        &Capabilities {
            api_version: 1,
            max_exec_output_bytes: 1048576,
            exec_timeout_secs: state.config.exec_timeout_secs,
            console_timeout_secs: state.config.console_timeout_secs,
            max_vm_name_length: 64,
            default_cpus: 1,
            default_mem_mib: 256,
        },
    )
}

pub fn create_vm(req: AuthenticatedRequest, state: &Arc<ServerState>) -> ResponseBuilder {
    let body: CreateVmRequest = match serde_json::from_slice(&req.ctx.body) {
        Ok(b) => b,
        Err(e) => return ResponseBuilder::error(400, &format!("invalid request body: {e}")),
    };

    match state
        .backend
        .create(&req.user.id, &body.name, body.cpus, body.mem_mib)
    {
        Ok(info) => ResponseBuilder::json(201, &info),
        Err(e) => map_backend_error(&e),
    }
}

pub fn list_vms(req: &AuthenticatedRequest, state: &Arc<ServerState>) -> ResponseBuilder {
    match state.backend.list(&req.user.id) {
        Ok(vms) => ResponseBuilder::json(200, &vms),
        Err(e) => map_backend_error(&e),
    }
}

pub fn get_vm(req: &AuthenticatedRequest, state: &Arc<ServerState>, name: &str) -> ResponseBuilder {
    match state.backend.get(&req.user.id, name) {
        Ok(Some(info)) => ResponseBuilder::json(200, &info),
        Ok(None) => ResponseBuilder::error(404, &format!("VM '{name}' not found")),
        Err(e) => map_backend_error(&e),
    }
}

pub fn destroy_vm(
    req: &AuthenticatedRequest,
    state: &Arc<ServerState>,
    name: &str,
) -> ResponseBuilder {
    match state.backend.destroy(&req.user.id, name) {
        Ok(()) => ResponseBuilder::no_content(),
        Err(e) => {
            if e.to_string().contains("not found") {
                ResponseBuilder::no_content()
            } else {
                map_backend_error(&e)
            }
        }
    }
}

pub fn create_checkpoint(
    req: AuthenticatedRequest,
    state: &Arc<ServerState>,
    name: &str,
) -> ResponseBuilder {
    let body: CheckpointRequest = match serde_json::from_slice(&req.ctx.body) {
        Ok(b) => b,
        Err(_) => CheckpointRequest { label: None },
    };

    match state
        .backend
        .checkpoint(&req.user.id, name, body.label.as_deref())
    {
        Ok(info) => ResponseBuilder::json(201, &info),
        Err(e) => map_backend_error(&e),
    }
}

pub fn list_checkpoints(
    req: &AuthenticatedRequest,
    state: &Arc<ServerState>,
    name: &str,
) -> ResponseBuilder {
    match state.backend.list_checkpoints(&req.user.id, name) {
        Ok(cps) => ResponseBuilder::json(200, &cps),
        Err(e) => map_backend_error(&e),
    }
}

pub fn restore_vm(
    req: AuthenticatedRequest,
    state: &Arc<ServerState>,
    name: &str,
) -> ResponseBuilder {
    let body: RestoreRequest = match serde_json::from_slice(&req.ctx.body) {
        Ok(b) => b,
        Err(e) => return ResponseBuilder::error(400, &format!("invalid request body: {e}")),
    };

    match state.backend.restore(
        &req.user.id,
        name,
        &body.checkpoint_id,
        body.new_name.as_deref(),
    ) {
        Ok(info) => ResponseBuilder::json(200, &info),
        Err(e) => map_backend_error(&e),
    }
}

pub fn exec_vm(req: AuthenticatedRequest, state: &Arc<ServerState>, name: &str) -> ResponseBuilder {
    let body: ExecRequest = match serde_json::from_slice(&req.ctx.body) {
        Ok(b) => b,
        Err(e) => return ResponseBuilder::error(400, &format!("invalid request body: {e}")),
    };

    if body.command.is_empty() {
        return ResponseBuilder::error(400, "command cannot be empty");
    }

    match state.backend.exec_full(&req.user.id, name, &body.command) {
        Ok((stdout, result)) => ResponseBuilder::json(
            200,
            &ExecResponse {
                stdout,
                exit_code: result.exit_code,
                timed_out: result.timed_out,
                truncated: result.truncated,
            },
        ),
        Err(e) => map_backend_error(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthz_returns_200() {
        let resp = healthz();
        assert_eq!(resp.status, 200);
        let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(body["status"], "ok");
    }

    #[test]
    fn version_returns_api_version_1() {
        let resp = version();
        assert_eq!(resp.status, 200);
        let body: VersionInfo = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(body.api_version, 1);
        assert!(!body.version.is_empty());
    }

    #[test]
    fn map_backend_error_not_found_gives_404() {
        let err = anyhow::anyhow!("VM 'test' not found");
        let resp = map_backend_error(&err);
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn map_backend_error_already_exists_gives_409() {
        let err = anyhow::anyhow!("VM 'test' already exists");
        let resp = map_backend_error(&err);
        assert_eq!(resp.status, 409);
    }

    #[test]
    fn map_backend_error_unknown_passes_message() {
        let err = anyhow::anyhow!("cp failed: No space left on device");
        let resp = map_backend_error(&err);
        assert_eq!(resp.status, 500);
        let body: ErrorResponse = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(body.error, "cp failed: No space left on device");
    }
}
