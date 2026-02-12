use noid_types::*;
use std::sync::Arc;

use crate::router::AuthenticatedRequest;
use crate::transport::ResponseBuilder;
use crate::ServerState;

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
            default_mem_mib: 128,
        },
    )
}

pub fn create_vm(req: AuthenticatedRequest, state: &Arc<ServerState>) -> ResponseBuilder {
    let body: CreateVmRequest = match serde_json::from_slice(&req.ctx.body) {
        Ok(b) => b,
        Err(e) => return ResponseBuilder::error(400, &format!("invalid request body: {e}")),
    };

    match state.backend.create(&req.user.id, &body.name, body.cpus, body.mem_mib) {
        Ok(info) => ResponseBuilder::json(201, &info),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") {
                ResponseBuilder::error(409, &msg)
            } else {
                ResponseBuilder::error(500, &msg)
            }
        }
    }
}

pub fn list_vms(req: &AuthenticatedRequest, state: &Arc<ServerState>) -> ResponseBuilder {
    match state.backend.list(&req.user.id) {
        Ok(vms) => ResponseBuilder::json(200, &vms),
        Err(e) => ResponseBuilder::error(500, &e.to_string()),
    }
}

pub fn get_vm(
    req: &AuthenticatedRequest,
    state: &Arc<ServerState>,
    name: &str,
) -> ResponseBuilder {
    match state.backend.get(&req.user.id, name) {
        Ok(Some(info)) => ResponseBuilder::json(200, &info),
        Ok(None) => ResponseBuilder::error(404, &format!("VM '{name}' not found")),
        Err(e) => ResponseBuilder::error(500, &e.to_string()),
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
            let msg = e.to_string();
            if msg.contains("not found") {
                ResponseBuilder::no_content()
            } else {
                ResponseBuilder::error(500, &msg)
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
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                ResponseBuilder::error(404, &msg)
            } else {
                ResponseBuilder::error(500, &msg)
            }
        }
    }
}

pub fn list_checkpoints(
    req: &AuthenticatedRequest,
    state: &Arc<ServerState>,
    name: &str,
) -> ResponseBuilder {
    match state.backend.list_checkpoints(&req.user.id, name) {
        Ok(cps) => ResponseBuilder::json(200, &cps),
        Err(e) => ResponseBuilder::error(500, &e.to_string()),
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
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") {
                ResponseBuilder::error(409, &msg)
            } else if msg.contains("not found") {
                ResponseBuilder::error(404, &msg)
            } else {
                ResponseBuilder::error(500, &msg)
            }
        }
    }
}

pub fn exec_vm(
    req: AuthenticatedRequest,
    state: &Arc<ServerState>,
    name: &str,
) -> ResponseBuilder {
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
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                ResponseBuilder::error(404, &msg)
            } else {
                ResponseBuilder::error(500, &msg)
            }
        }
    }
}
