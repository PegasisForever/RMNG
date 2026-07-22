//! `rmng` — fleet management for the RMNG control-server over the port-2 web API.
//!
//! Exit codes: 0 ok · 1 API/transport error · 2 usage (clap) · 3 operation ended in
//! Error · 4 `--wait`/`wait` timeout. `exec` instead passes through the executed
//! command's own exit code (125 when docker reports no code).

mod args;
mod commands;
mod output;
mod wait;

use args::{Cli, Cmd, resolve_server};
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
            // Only nudge toward `--server`/env when the server was actually unreachable;
            // an API error (e.g. 404 "no host 'x'") from a reachable server should not.
            if control_client::is_transport_error(&e) {
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
    match &cli.cmd {
        Cmd::Ps => commands::ps(client, cli.json).await,
        Cmd::Select { host } => commands::select(client, host, cli.json).await,
        Cmd::Clone {
            image,
            hostname,
            group,
            preset,
            wait,
        } => {
            commands::clone(
                client,
                image,
                hostname,
                group.as_deref(),
                preset.as_deref(),
                wait,
                cli.json,
            )
            .await
        }
        Cmd::Rm { host, yes, wait } => commands::rm(client, host, *yes, wait, cli.json).await,
        Cmd::Archive { host, wait } => commands::archive(client, host, wait, cli.json).await,
        Cmd::Restore { host, wait } => commands::restore(client, host, wait, cli.json).await,
        Cmd::Image(cmd) => commands::image(client, cmd, cli.json).await,
        Cmd::Account(cmd) => commands::account(client, cmd, cli.json).await,
        Cmd::Ops => commands::ops(client, cli.json).await,
        Cmd::Wait { op_id, timeout } => commands::wait_cmd(client, op_id, *timeout, cli.json).await,
        Cmd::Ssh { host } => commands::ssh_cmd(client, host).await,
        Cmd::Desktop { clone, cmd } => commands::desktop(client, clone, cmd, cli.json).await,
        Cmd::Exec {
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
                cli.json,
            )
            .await
        }
    }
}
