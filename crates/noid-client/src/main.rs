mod api;
mod cli;
mod config;
mod console;
mod exec;
mod update;

use anyhow::Result;
use clap::Parser;

use cli::{AuthAction, Cli, Command};
use config::{ClientConfig, ServerSection};

fn main() -> Result<()> {
    let cli = Cli::parse();

    let exit_code = match cli.command {
        Command::Auth { action } => {
            match action {
                AuthAction::Setup { url, token } => cmd_auth_setup(&url, &token)?,
            }
            0
        }
        Command::Use { name } => {
            config::write_active_vm(&name)?;
            println!("Active VM set to '{name}'");
            0
        }
        Command::Current => {
            cmd_current()?;
            0
        }
        Command::Whoami => {
            cmd_whoami()?;
            0
        }
        Command::Create { name, cpus, mem } => {
            cmd_create(&name, cpus, mem)?;
            0
        }
        Command::Destroy { name } => {
            let name = config::resolve_vm_name(name.as_deref())?;
            cmd_destroy(&name)?;
            0
        }
        Command::List => {
            cmd_list()?;
            0
        }
        Command::Info { name } => {
            let name = config::resolve_vm_name(name.as_deref())?;
            cmd_info(&name)?;
            0
        }
        Command::Exec { name, command } => {
            let name = config::resolve_vm_name(name.as_deref())?;
            if command.is_empty() {
                anyhow::bail!("no command specified");
            }
            cmd_exec(&name, &command)?
        }
        Command::Console { name } => {
            let name = config::resolve_vm_name(name.as_deref())?;
            cmd_console(&name)?;
            0
        }
        Command::Checkpoint { name, label } => {
            let name = config::resolve_vm_name(name.as_deref())?;
            cmd_checkpoint(&name, label.as_deref())?;
            0
        }
        Command::Checkpoints { name } => {
            let name = config::resolve_vm_name(name.as_deref())?;
            cmd_checkpoints(&name)?;
            0
        }
        Command::Update => {
            update::self_update()?;
            0
        }
        Command::Restore {
            name,
            checkpoint_id,
            new_name,
        } => {
            let name = config::resolve_vm_name(name.as_deref())?;
            cmd_restore(&name, &checkpoint_id, new_name.as_deref())?;
            0
        }
    };

    std::process::exit(exit_code);
}

fn api_client() -> Result<api::ApiClient> {
    let config = ClientConfig::load()?;
    let server = config.server()?;
    Ok(api::ApiClient::new(server))
}

fn cmd_auth_setup(url: &str, token: &str) -> Result<()> {
    let mut config = ClientConfig::load()?;
    config.server = Some(ServerSection {
        url: url.to_string(),
        token: token.to_string(),
    });
    config.save()?;
    println!("Configuration saved.");

    // Verify connection
    let api = api::ApiClient::new(config.server.as_ref().unwrap());
    match api.whoami() {
        Ok(who) => println!("Authenticated as '{}' (id: {})", who.name, who.user_id),
        Err(e) => eprintln!("Warning: could not verify connection: {e}"),
    }
    Ok(())
}

fn cmd_current() -> Result<()> {
    let config = ClientConfig::load()?;
    let server = config.server()?;
    println!("Server: {}", server.url);

    match config::read_active_vm() {
        Some(name) => println!("Active VM: {name}"),
        None => println!("Active VM: (none — run `noid use <name>`)"),
    }
    Ok(())
}

fn cmd_whoami() -> Result<()> {
    let api = api_client()?;
    let who = api.whoami()?;
    println!("User: {}", who.name);
    println!("ID:   {}", who.user_id);
    Ok(())
}

fn cmd_create(name: &str, cpus: u32, mem: u32) -> Result<()> {
    let api = api_client()?;
    let info = api.create_vm(name, cpus, mem)?;
    println!("VM '{}' created (state: {})", info.name, info.state);
    Ok(())
}

fn cmd_destroy(name: &str) -> Result<()> {
    let api = api_client()?;
    api.destroy_vm(name)?;
    println!("VM '{name}' destroyed");
    Ok(())
}

fn cmd_list() -> Result<()> {
    let api = api_client()?;
    let vms = api.list_vms()?;
    if vms.is_empty() {
        println!("No VMs found.");
        return Ok(());
    }

    use tabled::{Table, Tabled};

    #[derive(Tabled)]
    struct VmRow {
        name: String,
        state: String,
        cpus: u32,
        #[tabled(rename = "mem (MiB)")]
        mem: u32,
        created: String,
    }

    let rows: Vec<VmRow> = vms
        .iter()
        .map(|vm| VmRow {
            name: vm.name.clone(),
            state: vm.state.clone(),
            cpus: vm.cpus,
            mem: vm.mem_mib,
            created: vm.created_at.clone(),
        })
        .collect();

    println!("{}", Table::new(rows));
    Ok(())
}

fn cmd_info(name: &str) -> Result<()> {
    let api = api_client()?;
    let info = api.get_vm(name)?;
    println!("Name:    {}", info.name);
    println!("State:   {}", info.state);
    println!("CPUs:    {}", info.cpus);
    println!("Memory:  {} MiB", info.mem_mib);
    println!("Created: {}", info.created_at);
    Ok(())
}

fn cmd_exec(name: &str, command: &[String]) -> Result<i32> {
    let api = api_client()?;

    // Try WebSocket first, fall back to HTTP POST
    match exec::exec_ws(&api, name, command) {
        Ok(code) => Ok(code),
        Err(_ws_err) => {
            // Fallback to HTTP POST exec
            let resp = api.exec_vm(name, command)?;
            if !resp.stdout.is_empty() {
                print!("{}", resp.stdout);
            }
            if resp.timed_out {
                eprintln!("exec timed out");
                Ok(124)
            } else {
                Ok(resp.exit_code.unwrap_or(0))
            }
        }
    }
}

fn cmd_console(name: &str) -> Result<()> {
    let api = api_client()?;
    console::attach_console(&api, name)
}

fn cmd_checkpoint(name: &str, label: Option<&str>) -> Result<()> {
    let api = api_client()?;
    let info = api.create_checkpoint(name, label)?;
    println!(
        "Checkpoint '{}' created{}",
        info.id,
        info.label
            .as_ref()
            .map(|l| format!(" (label: {l})"))
            .unwrap_or_default()
    );
    Ok(())
}

fn cmd_checkpoints(name: &str) -> Result<()> {
    let api = api_client()?;
    let checkpoints = api.list_checkpoints(name)?;
    if checkpoints.is_empty() {
        println!("No checkpoints for VM '{name}'.");
        return Ok(());
    }

    use tabled::{Table, Tabled};

    #[derive(Tabled)]
    struct CpRow {
        id: String,
        label: String,
        created: String,
    }

    let rows: Vec<CpRow> = checkpoints
        .iter()
        .map(|cp| CpRow {
            id: cp.id.clone(),
            label: cp.label.clone().unwrap_or("-".into()),
            created: cp.created_at.clone(),
        })
        .collect();

    println!("{}", Table::new(rows));
    Ok(())
}

fn cmd_restore(name: &str, checkpoint_id: &str, new_name: Option<&str>) -> Result<()> {
    let api = api_client()?;
    let info = api.restore_vm(name, checkpoint_id, new_name)?;
    println!(
        "VM '{}' restored from checkpoint '{checkpoint_id}'",
        info.name
    );
    Ok(())
}
