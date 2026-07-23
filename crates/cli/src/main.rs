//! `rmng` — fleet management for the RMNG control-server over the port-2 web API.
//!
//! Exit codes: 0 ok · 1 API/transport error · 2 usage (clap) · 3 operation ended in
//! Error · 4 `--wait`/`wait` timeout. `exec` instead passes through the executed
//! command's own exit code (125 when docker reports no code).

mod args;
mod commands;
mod output;
mod wait;

use args::{Cli, CloneCmd, Cmd, OpCmd, resolve_server};
use clap::Parser;
use control_client::Client;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let base = resolve_server(cli.server.clone(), std::env::var("RMNG_CONTROL_URL").ok());
    let client = Client::new(&base);
    let code = match run(&cli, &client).await {
        Ok(code) => code,
        Err(e) => {
            let transport = control_client::is_transport_error(&e);
            if cli.json {
                // Under --json, errors are JSON too (on stderr) so an agent parses one shape for
                // success and failure. Exit code is unchanged (1 for a transport/API error).
                let hint = if transport {
                    format!("set --server or $RMNG_CONTROL_URL (server: {})", client.base())
                } else {
                    String::new()
                };
                eprintln!(
                    "{}",
                    serde_json::json!({ "error": { "message": format!("{e:#}"), "hint": hint } })
                );
            } else if transport {
                // Only nudge toward `--server`/env when the server was actually unreachable;
                // an API error (e.g. 404 "no clone 'x'") from a reachable server should not.
                eprintln!("error: {}", commands::connect_hint(client.base(), &e));
            } else {
                eprintln!("error: {e:#}");
            }
            1
        }
    };
    std::process::exit(code as i32);
}

async fn run(cli: &Cli, client: &Client) -> anyhow::Result<u8> {
    let json = cli.json;
    match &cli.cmd {
        Cmd::Clone(cmd) => match cmd {
            CloneCmd::Ls => commands::clone_ls(client, json).await,
            CloneCmd::Create {
                hostname,
                from,
                group,
                no_group,
                preset,
                no_preset,
                headless,
                parent,
                top_level,
                wait,
            } => {
                commands::clone_create(
                    client,
                    hostname,
                    from,
                    group.as_deref(),
                    *no_group,
                    preset.as_deref(),
                    *no_preset,
                    *headless,
                    parent.as_deref(),
                    *top_level,
                    wait,
                    json,
                )
                .await
            }
            CloneCmd::Rm { clone, yes, wait } => {
                commands::clone_rm(client, clone, *yes, wait, json).await
            }
            CloneCmd::Archive { clone, wait } => commands::archive(client, clone, wait, json).await,
            CloneCmd::Restore { clone, wait } => commands::restore(client, clone, wait, json).await,
            CloneCmd::Ssh { clone } => commands::clone_ssh(client, clone, json).await,
            CloneCmd::Exec {
                clone,
                user,
                workdir,
                env,
                cmd,
            } => {
                commands::exec(
                    client,
                    clone,
                    user.as_deref(),
                    workdir.as_deref(),
                    env,
                    cmd,
                    json,
                )
                .await
            }
            CloneCmd::Bind { clone, group, none } => {
                commands::clone_bind(client, clone, group.as_deref(), *none, json).await
            }
            CloneCmd::Select { clone, none } => {
                commands::select(client, clone.as_deref(), *none, json).await
            }
        },
        Cmd::Image(cmd) => commands::image(client, cmd, json).await,
        Cmd::Account(cmd) => commands::account(client, cmd, json).await,
        Cmd::Op(cmd) => match cmd {
            OpCmd::Ls => commands::op_ls(client, json).await,
            OpCmd::Wait { op_id, timeout } => {
                commands::wait_cmd(client, op_id, *timeout, json).await
            }
        },
        Cmd::Desktop { clone, cmd } => commands::desktop(client, clone, cmd, json).await,
    }
}
