use noid_core::auth;
use noid_core::db::{Db, UserRecord};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::transport::{RequestContext, ResponseBuilder};

/// Authenticated request -- carries the user context.
pub struct AuthenticatedRequest {
    pub ctx: RequestContext,
    pub user: UserRecord,
}

/// Attempt to authenticate a request. Returns None if auth not required.
pub fn authenticate(
    ctx: &RequestContext,
    db: &Mutex<Db>,
    rate_limiter: &auth::RateLimiter,
) -> Result<UserRecord, ResponseBuilder> {
    let token = ctx
        .headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| ResponseBuilder::error(401, "missing or invalid Authorization header"))?;

    let rate_key = auth::token_rate_key(token);
    if rate_limiter.check(&rate_key).is_err() {
        return Err(ResponseBuilder::error(
            429,
            "too many authentication failures, try again later",
        ));
    }

    let db = db.lock().unwrap_or_else(|e| e.into_inner());
    match db.authenticate_user(token) {
        Ok(Some(user)) => Ok(user),
        Ok(None) => {
            drop(db);
            rate_limiter.record_failure(&rate_key);
            Err(ResponseBuilder::error(401, "invalid token"))
        }
        Err(e) => Err(ResponseBuilder::error(
            500,
            &format!("authentication error: {e}"),
        )),
    }
}

/// Fields collected for request logging.
struct LogEntry<'a> {
    request_id: &'a str,
    user: Option<&'a str>,
    method: &'a str,
    path: &'a str,
    status: u16,
    start: Instant,
    remote_addr: &'a str,
    forwarded_for: &'a Option<String>,
}

/// Route a request to the appropriate handler. Returns (handler_name, response).
pub fn route(ctx: RequestContext, state: &Arc<crate::ServerState>) -> (String, ResponseBuilder) {
    let start = Instant::now();
    let method = ctx.method.clone();
    let path = ctx.path.clone();
    let remote = ctx.remote_addr.clone();
    let forwarded = ctx.forwarded_for.clone();
    let request_id = uuid::Uuid::new_v4().to_string()[..8].to_string();

    // Unauthenticated endpoints
    match (method.as_str(), path.as_str()) {
        ("GET", "/healthz") => {
            let resp = crate::handlers::healthz();
            log_request(&LogEntry {
                request_id: &request_id,
                user: None,
                method: &method,
                path: &path,
                status: resp.status,
                start,
                remote_addr: &remote,
                forwarded_for: &forwarded,
            });
            return ("healthz".into(), resp);
        }
        ("GET", "/version") => {
            let resp = crate::handlers::version();
            log_request(&LogEntry {
                request_id: &request_id,
                user: None,
                method: &method,
                path: &path,
                status: resp.status,
                start,
                remote_addr: &remote,
                forwarded_for: &forwarded,
            });
            return ("version".into(), resp);
        }
        _ => {}
    }

    // Authenticate
    let user = match authenticate(&ctx, &state.db, &state.rate_limiter) {
        Ok(u) => u,
        Err(resp) => {
            log_request(&LogEntry {
                request_id: &request_id,
                user: None,
                method: &method,
                path: &path,
                status: resp.status,
                start,
                remote_addr: &remote,
                forwarded_for: &forwarded,
            });
            return ("auth_failed".into(), resp);
        }
    };

    let user_name = user.name.clone();

    let auth_req = AuthenticatedRequest { ctx, user };

    let resp = route_authenticated(auth_req, state);
    log_request(&LogEntry {
        request_id: &request_id,
        user: Some(&user_name),
        method: &method,
        path: &path,
        status: resp.status,
        start,
        remote_addr: &remote,
        forwarded_for: &forwarded,
    });

    (request_id, resp)
}

fn route_authenticated(
    req: AuthenticatedRequest,
    state: &Arc<crate::ServerState>,
) -> ResponseBuilder {
    let method = req.ctx.method.clone();
    let path = req.ctx.path.clone();

    // Strip query string for matching
    let path = path.split('?').next().unwrap_or(&path).to_string();

    match (method.as_str(), path.as_str()) {
        ("GET", "/v1/whoami") => crate::handlers::whoami(&req),
        ("GET", "/v1/capabilities") => crate::handlers::capabilities(state),
        ("POST", "/v1/vms") => crate::handlers::create_vm(req, state),
        ("GET", "/v1/vms") => crate::handlers::list_vms(&req, state),
        _ => {
            // Try VM-scoped routes: /v1/vms/{name}...
            if let Some(rest) = path.strip_prefix("/v1/vms/") {
                route_vm_scoped(&method, rest, req, state)
            } else {
                ResponseBuilder::error(404, "not found")
            }
        }
    }
}

fn route_vm_scoped(
    method: &str,
    rest: &str,
    req: AuthenticatedRequest,
    state: &Arc<crate::ServerState>,
) -> ResponseBuilder {
    // Parse: {name} or {name}/sub or {name}/sub/more
    let (vm_name, sub) = match rest.find('/') {
        Some(pos) => (&rest[..pos], &rest[pos + 1..]),
        None => (rest, ""),
    };

    if vm_name.is_empty() {
        return ResponseBuilder::error(400, "missing VM name in path");
    }

    if noid_core::storage::validate_name(vm_name, "VM").is_err() {
        return ResponseBuilder::error(400, "invalid VM name");
    }

    match (method, sub) {
        ("GET", "") => crate::handlers::get_vm(&req, state, vm_name),
        ("DELETE", "") => crate::handlers::destroy_vm(&req, state, vm_name),
        ("POST", "checkpoints") => crate::handlers::create_checkpoint(req, state, vm_name),
        ("GET", "checkpoints") => crate::handlers::list_checkpoints(&req, state, vm_name),
        ("POST", "restore") => crate::handlers::restore_vm(req, state, vm_name),
        ("POST", "exec") => crate::handlers::exec_vm(req, state, vm_name),
        ("GET", "exec") => {
            // WebSocket upgrade for streaming exec
            ResponseBuilder::error(426, "WebSocket upgrade required for GET /exec")
        }
        ("GET", "console") => {
            // WebSocket upgrade for console
            ResponseBuilder::error(426, "WebSocket upgrade required for GET /console")
        }
        _ => ResponseBuilder::error(404, "not found"),
    }
}

fn log_request(entry: &LogEntry) {
    let duration = entry.start.elapsed().as_millis();
    let user_str = entry.user.unwrap_or("-");
    let fwd = entry.forwarded_for.as_deref().unwrap_or("-");
    eprintln!(
        "[{}] {} {} {} -> {} ({}ms) remote={} fwd={}",
        entry.request_id,
        user_str,
        entry.method,
        entry.path,
        entry.status,
        duration,
        entry.remote_addr,
        fwd
    );
}
