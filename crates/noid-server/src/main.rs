mod config;
mod console;
mod handlers;
mod router;
mod transport;
mod ws_exec;

use anyhow::Result;
use clap::{Parser, Subcommand};
use noid_core::auth;
use noid_core::backend::{FirecrackerBackend, VmBackend};
use noid_core::db::Db;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use config::ServerConfig;

/// noid server — manages Firecracker microVMs
#[derive(Parser)]
#[command(name = "noid-server", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the noid server
    Serve {
        /// Path to server config file
        #[arg(long, default_value = "server.toml")]
        config: String,
    },
    /// Add a new user
    AddUser {
        /// Username
        name: String,
    },
    /// Rotate a user's token
    RotateToken {
        /// Username
        name: String,
    },
    /// List all users
    ListUsers,
    /// Remove a user and all their VMs
    RemoveUser {
        /// Username
        name: String,
    },
}

pub struct ServerState {
    pub backend: Arc<dyn VmBackend>,
    pub db: Mutex<Db>,
    pub config: ServerConfig,
    pub rate_limiter: auth::RateLimiter,
    pub ws_session_count: AtomicUsize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Serve { config: config_path } => cmd_serve(&config_path),
        Command::AddUser { name } => cmd_add_user(&name),
        Command::RotateToken { name } => cmd_rotate_token(&name),
        Command::ListUsers => cmd_list_users(),
        Command::RemoveUser { name } => cmd_remove_user(&name),
    }
}

fn cmd_serve(config_path: &str) -> Result<()> {
    let config = ServerConfig::load(config_path)?;

    let db = Db::open()?;
    let backend = Arc::new(FirecrackerBackend::new(
        Db::open()?,
        config.kernel.clone(),
        config.rootfs.clone(),
        config.exec_timeout_secs,
    ));

    let state = Arc::new(ServerState {
        backend,
        db: Mutex::new(db),
        config: config.clone(),
        rate_limiter: auth::RateLimiter::new(),
        ws_session_count: AtomicUsize::new(0),
    });

    let server = tiny_http::Server::http(&config.listen)
        .map_err(|e| anyhow::anyhow!("failed to bind {}: {e}", config.listen))?;

    eprintln!("noid-server listening on {}", config.listen);

    for mut request in server.incoming_requests() {
        let state = state.clone();
        let trust_fwd = config.trust_forwarded_for;

        // Check if this is a WebSocket upgrade
        let is_upgrade = request
            .headers()
            .iter()
            .any(|h| {
                h.field.as_str().as_str().eq_ignore_ascii_case("upgrade")
                    && h.value.as_str().eq_ignore_ascii_case("websocket")
            });

        if is_upgrade {
            std::thread::spawn(move || {
                handle_ws_upgrade(request, state, trust_fwd);
            });
        } else {
            std::thread::spawn(move || {
                let ctx = transport::from_tiny_http(&mut request, trust_fwd);
                let (_, resp) = router::route(ctx, &state);
                let response = transport::to_tiny_http_response(resp);
                let _ = request.respond(response);
            });
        }
    }

    Ok(())
}

fn handle_ws_upgrade(
    mut request: tiny_http::Request,
    state: Arc<ServerState>,
    trust_fwd: bool,
) {
    let ctx = transport::from_tiny_http(&mut request, trust_fwd);

    // Authenticate first
    let user = match router::authenticate(&ctx, &state.db, &state.rate_limiter) {
        Ok(u) => u,
        Err(resp) => {
            let response = transport::to_tiny_http_response(resp);
            let _ = request.respond(response);
            return;
        }
    };

    let path = ctx.path.split('?').next().unwrap_or(&ctx.path).to_string();

    // Parse VM name from path
    let rest = match path.strip_prefix("/v1/vms/") {
        Some(r) => r,
        None => {
            let resp = transport::ResponseBuilder::error(404, "not found");
            let _ = request.respond(transport::to_tiny_http_response(resp));
            return;
        }
    };

    let (vm_name, endpoint) = match rest.find('/') {
        Some(pos) => (&rest[..pos], &rest[pos + 1..]),
        None => {
            let resp = transport::ResponseBuilder::error(404, "not found");
            let _ = request.respond(transport::to_tiny_http_response(resp));
            return;
        }
    };

    // Check WS session limit
    let current = state.ws_session_count.load(Ordering::Relaxed);
    if current >= state.config.max_ws_sessions {
        let resp = transport::ResponseBuilder::error(503, "too many WebSocket sessions");
        let _ = request.respond(transport::to_tiny_http_response(resp));
        return;
    }
    state.ws_session_count.fetch_add(1, Ordering::Relaxed);

    let vm_name = vm_name.to_string();
    let endpoint = endpoint.to_string();

    // We need to compute the Sec-WebSocket-Accept header
    let ws_key = ctx.headers.get("sec-websocket-key").cloned().unwrap_or_default();
    let accept_key = compute_ws_accept(&ws_key);

    let response = tiny_http::Response::new(
        tiny_http::StatusCode(101),
        vec![
            tiny_http::Header::from_bytes(b"Upgrade", b"websocket").unwrap(),
            tiny_http::Header::from_bytes(b"Connection", b"Upgrade").unwrap(),
            tiny_http::Header::from_bytes(
                b"Sec-WebSocket-Accept",
                accept_key.as_bytes(),
            )
            .unwrap(),
        ],
        std::io::Cursor::new(vec![]),
        Some(0),
        None,
    );

    // Get the underlying TCP stream by upgrading
    let stream = request.upgrade("websocket", response);

    match endpoint.as_str() {
        "console" => {
            console::handle_console_ws(stream, &state, &user, &vm_name);
        }
        "exec" => {
            ws_exec::handle_exec_ws(stream, &state, &user, &vm_name);
        }
        _ => {
            // Unknown endpoint — just close
        }
    }

    state.ws_session_count.fetch_sub(1, Ordering::Relaxed);
}

fn compute_ws_accept(key: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(b"258EAFA5-E914-47DA-95CA-5AB5DC11D65B");
    base64_encode(&hasher.finalize())
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((n >> 18) & 63) as usize] as char);
        result.push(CHARS[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 63) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 63) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

// --- User management commands ---

fn cmd_add_user(name: &str) -> Result<()> {
    let db = Db::open()?;
    if db.get_user_by_name(name)?.is_some() {
        anyhow::bail!("user '{name}' already exists");
    }
    let token = auth::generate_token();
    let hash = auth::hash_token(&token);
    let id = uuid::Uuid::new_v4().to_string();
    db.insert_user(&id, name, &hash)?;
    println!("{token}");
    eprintln!("User '{name}' created (id: {id})");
    Ok(())
}

fn cmd_rotate_token(name: &str) -> Result<()> {
    let db = Db::open()?;
    let token = auth::generate_token();
    let hash = auth::hash_token(&token);
    if !db.update_user_token(name, &hash)? {
        anyhow::bail!("user '{name}' not found");
    }
    println!("{token}");
    eprintln!("Token rotated for user '{name}'");
    Ok(())
}

fn cmd_list_users() -> Result<()> {
    let db = Db::open()?;
    let users = db.list_users()?;
    if users.is_empty() {
        println!("No users.");
        return Ok(());
    }
    println!("{:<36}  {:<20}  CREATED", "ID", "NAME");
    for u in &users {
        println!("{:<36}  {:<20}  {}", u.id, u.name, u.created_at);
    }
    Ok(())
}

fn cmd_remove_user(name: &str) -> Result<()> {
    let db = Db::open()?;
    match db.delete_user(name)? {
        Some(user_id) => {
            let _ = noid_core::storage::delete_user_storage(&user_id);
            eprintln!("User '{name}' removed (id: {user_id})");
        }
        None => {
            anyhow::bail!("user '{name}' not found");
        }
    }
    Ok(())
}
