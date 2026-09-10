//! Gen-2 happy-path end to end: create → fork → archive → rebase → unarchive → delete.
//!
//! Drives a LIVE server over its web API with the `control-client` typed client — the same
//! calls the CLI and the dialogs make. Needs real Docker + ZFS + one preset, so this never
//! runs in the unit suite; run it on real hardware after a release candidate is deployed:
//!
//! ```sh
//! RMNG_E2E_SERVER=http://rmng-control:9000 RMNG_E2E_PRESET=work \
//!   cargo run -p control-client --bin rmng_e2e
//! ```
//!
//! Env:
//! - `RMNG_E2E_SERVER` — web-API origin, no trailing slash (default `http://localhost:9000`).
//! - `RMNG_E2E_PRESET` (required) — preset to create with. Point it at a preset carrying a
//!   custom Dockerfile to exercise the on-demand image build; the run prints the Dockerfile's
//!   first line as evidence of which image path it took.
//! - `RMNG_E2E_TIMEOUT_S` — per-operation wait budget (default 600; image builds are slow on
//!   a cold cache).
//!
//! Every step asserts the state it leaves behind and aborts on the first failure, best-effort
//! deleting whatever it made (ids are printed either way, for hand cleanup if that misses).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use control_client::{Client, ForkOpts};
use wire::{Operation, OperationStatus};

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

struct Runner {
    client: Client,
    budget: Duration,
    made: Vec<String>,
}

impl Runner {
    /// Poll `/api/state` until the op leaves `Running` (or vanishes after being seen, which
    /// means it landed and pruned — the same rule the CLI waiter uses).
    async fn wait_op(&self, op: &Operation, what: &str) -> Result<()> {
        let start = Instant::now();
        let mut seen = false;
        loop {
            if start.elapsed() > self.budget {
                bail!("{what}: op {} still running after {:?}", op.id, self.budget);
            }
            let st = self.client.state().await?;
            match st.operations.iter().find(|o| o.id == op.id) {
                Some(o) => {
                    seen = true;
                    match o.status {
                        OperationStatus::Done => return Ok(()),
                        OperationStatus::Error => {
                            bail!("{what}: op {} failed: {}", op.id, o.message)
                        }
                        OperationStatus::Running => {}
                    }
                }
                None if seen => return Ok(()),
                None => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    async fn hosts_contain(&self, id: &str) -> Result<bool> {
        Ok(self.client.state().await?.hosts.iter().any(|h| h.id == id))
    }

    /// Poll `/api/state` until the clone-daemon holds its media session (headed
    /// clones only). Separate from op completion: the op succeeds when the
    /// container is up, which says nothing about the agent stack inside.
    async fn wait_connected(&self, id: &str, what: &str) -> Result<()> {
        println!("e2e: waiting for {id} daemon ...");
        let start = Instant::now();
        loop {
            if start.elapsed() > self.budget {
                bail!("{what}: clone '{id}' daemon never registered");
            }
            let connected = self
                .client
                .state()
                .await?
                .hosts
                .iter()
                .find(|h| h.id == id)
                .map(|h| h.daemon_connected)
                .unwrap_or(false);
            if connected {
                println!("e2e: {id} daemon registered");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Poll the clone-daemon's `screenshot` tool until it returns a real frame: valid
    /// JPEG, at least 800x600, with real pixel variance (a black/dead session decodes
    /// to near-zero). No agent turn involved — this calls the desktop MCP directly,
    /// headlessly. A session that never paints fails here, not green.
    async fn wait_frames(&self, id: &str, what: &str) -> Result<()> {
        println!("e2e: waiting for {id} frames ...");
        let start = Instant::now();
        loop {
            if start.elapsed() > self.budget {
                bail!("{what}: clone '{id}' produced no real frames");
            }
            match self.try_frame(id).await {
                Ok((w, h, sd)) => {
                    println!("e2e: {id} frame {w}x{h} stddev {sd:.1}");
                    return Ok(());
                }
                Err(e) => {
                    println!("e2e: {id} no frame yet ({e:#}), retrying ...");
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn try_frame(&self, id: &str) -> Result<(u32, u32, f64)> {
        let content = self
            .client
            .desktop(id, "screenshot", serde_json::json!({}))
            .await
            .with_context(|| format!("clone '{id}' screenshot tool"))?;
        let data = content
            .as_array()
            .context("screenshot content is not an array")?
            .iter()
            .find(|i| i.get("type").and_then(|t| t.as_str()) == Some("image"))
            .context("screenshot content has no image item")?;
        let b64 = data
            .get("data")
            .and_then(|d| d.as_str())
            .context("image item has no data")?;
        use base64::Engine;
        let jpeg = base64::engine::general_purpose::STANDARD.decode(b64)?;
        if jpeg.len() < 2 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
            bail!("not a JPEG ({} bytes)", jpeg.len());
        }
        let img = image::load_from_memory(&jpeg).context("decoding frame JPEG")?;
        let gray = img.to_luma8();
        let (w, h) = gray.dimensions();
        if w < 800 || h < 600 {
            bail!("frame too small ({w}x{h})");
        }
        let n = (w as f64) * (h as f64);
        let mean = gray.pixels().map(|p| p[0] as f64).sum::<f64>() / n;
        let var = gray.pixels().map(|p| (p[0] as f64 - mean).powi(2)).sum::<f64>() / n;
        let sd = var.sqrt();
        if sd < 5.0 {
            bail!("frame has no variance (stddev {sd:.1} — dead session?)");
        }
        Ok((w, h, sd))
    }

    async fn cleanup(&self) {
        for id in &self.made {
            if !self.hosts_contain(id).await.unwrap_or(true) {
                continue;
            }
            match self.client.delete(id).await {
                Ok(op) => {
                    let _ = self.wait_op(&op, &format!("cleanup delete {id}")).await;
                }
                Err(e) => eprintln!("cleanup: delete {id} failed: {e:#}"),
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let server = env("RMNG_E2E_SERVER", "http://localhost:9000");
    let preset = std::env::var("RMNG_E2E_PRESET").context(
        "set RMNG_E2E_PRESET to a preset name (one with a custom Dockerfile exercises the image build)",
    )?;
    let budget = Duration::from_secs(
        env("RMNG_E2E_TIMEOUT_S", "600")
            .parse()
            .context("RMNG_E2E_TIMEOUT_S must be seconds")?,
    );
    let mut r = Runner {
        client: Client::new(server.clone()),
        budget,
        made: vec![],
    };
    let run = run(&mut r, &preset).await;
    if let Err(e) = &run {
        eprintln!("E2E FAIL: {e:#}");
        if !r.made.is_empty() {
            eprintln!("cleaning up: {}", r.made.join(", "));
            r.cleanup().await;
        }
        std::process::exit(1);
    }
    println!("E2E PASS");
    Ok(())
}

async fn run(r: &mut Runner, preset: &str) -> Result<()> {
    // 0. The preset exists — and show which Dockerfile the image build will use.
    let cfg = r.client.config().await.context("GET /api/config")?;
    let found = cfg
        .presets
        .iter()
        .find(|p| p.name == preset)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "preset '{preset}' not in server config (have: {})",
                cfg.presets
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    println!(
        "e2e: preset '{preset}', dockerfile starts: {}",
        found.dockerfile.lines().next().unwrap_or("(empty)")
    );

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let title = format!("e2e-{stamp}");

    // 1. Create a plain clone (builds the preset image on demand on a cold cache).
    println!("e2e: create '{title}' ...");
    let op = r
        .client
        .clone_create_plain(&title, "", Some(preset))
        .await
        .context("POST /api/clone")?;
    r.wait_op(&op, "create").await?;
    let id = op.target.clone();
    r.made.push(id.clone());
    let st = r.client.state().await?;
    let row = st
        .hosts
        .iter()
        .find(|h| h.id == id)
        .ok_or_else(|| anyhow::anyhow!("created clone '{id}' missing from state"))?;
    if !row.managed {
        bail!("created clone '{id}' is not managed");
    }
    println!("e2e: created '{id}'");
    // The op succeeding only means the container is up: wait for the clone-daemon's
    // Hello, which is what makes the clone actually usable (and what an earlier
    // version of this test never checked — brain-dead clones passed green).
    r.wait_connected(&id, "create").await?;
    // Frames, not just a Hello: a registered daemon with a dead session (black
    // output, no dmabuf) used to pass green. Screenshot directly, headlessly.
    r.wait_frames(&id, "create").await?;

    // 2. Fork it with a first message.
    println!("e2e: fork '{id}' ...");
    let op = r
        .client
        .fork_with(
            &id,
            &ForkOpts {
                first_message: Some("e2e fork"),
                ..Default::default()
            },
        )
        .await
        .context("POST /api/fork")?;
    r.wait_op(&op, "fork").await?;
    let fork = op.target.clone();
    r.made.push(fork.clone());
    if !r.hosts_contain(&fork).await? {
        bail!("forked clone '{fork}' missing from state");
    }
    println!("e2e: forked '{fork}'");
    r.wait_connected(&fork, "fork").await?;
    r.wait_frames(&fork, "fork").await?;

    // 3. Archive the fork.
    println!("e2e: archive '{fork}' ...");
    let op = r.client.archive(&fork).await.context("archive")?;
    r.wait_op(&op, "archive").await?;
    let archived = r
        .client
        .state()
        .await?
        .hosts
        .iter()
        .find(|h| h.id == fork)
        .map(|h| h.archived)
        .unwrap_or(false);
    if !archived {
        bail!("fork '{fork}' is not flagged archived after archive");
    }

    // 4. Rebase the ARCHIVED fork (must stay archived on the new image).
    println!("e2e: rebase archived '{fork}' ...");
    let op = r
        .client
        .rebase(&fork, preset, false)
        .await
        .context("rebase")?;
    r.wait_op(&op, "rebase").await?;
    let row = r
        .client
        .state()
        .await?
        .hosts
        .into_iter()
        .find(|h| h.id == fork)
        .ok_or_else(|| anyhow::anyhow!("fork '{fork}' missing from state after rebase"))?;
    if !row.archived {
        bail!("fork '{fork}' lost its archived flag across rebase");
    }
    if row.base_tag.is_none() {
        bail!("fork '{fork}' has no base tag after rebase");
    }

    // 5. Unarchive, then delete both clones.
    println!("e2e: unarchive '{fork}' ...");
    let op = r.client.unarchive(&fork).await.context("unarchive")?;
    r.wait_op(&op, "unarchive").await?;
    for dead in [fork.clone(), id.clone()] {
        println!("e2e: delete '{dead}' ...");
        let op = r.client.delete(&dead).await.context("delete")?;
        r.wait_op(&op, "delete").await?;
        if r.hosts_contain(&dead).await? {
            bail!("clone '{dead}' still in state after delete");
        }
    }
    r.made.clear();
    Ok(())
}
