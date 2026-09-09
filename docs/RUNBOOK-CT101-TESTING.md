# Runbook: testing RMNG changes on CT 101

CT 101 is the disposable test deployment. Every command here was run against it on
2026-09-05 while swapping the per-clone assistant to the pi coding agent.

**CT 105 is the production fleet. Never deploy to it.** Publishing an image is fine and
updating CT 101 is fine, but putting a build onto CT 105 needs the operator to ask for
that deploy by name. Reading CT 105 stays fine.

## Access

CT 101 is a Proxmox container on the host at `10.0.0.100`. It runs Docker, and the
control-server is a container named `rmng` published on `10.0.0.182`.

The local `gcr-ssh-agent` cannot sign with the RSA key and there is no askpass, so plain
`ssh root@10.0.0.100` fails with `Permission denied`. Bypass the agent:

```sh
SSH="ssh -i ~/.ssh/id_rsa -o IdentityAgent=none -o IdentitiesOnly=yes"
$SSH root@10.0.0.100 'pct exec 101 -- docker ps --format "{{.Names}}\t{{.Status}}"'
```

Confirm the address before trusting it, because the lease moves:

```sh
$SSH root@10.0.0.100 'pct exec 101 -- hostname -I'
```

Three shells, nested. Each one wraps the next.

1. Proxmox host: `$SSH root@10.0.0.100 '<cmd>'`
2. Inside CT 101: `$SSH root@10.0.0.100 'pct exec 101 -- <cmd>'`
3. Inside a clone as the agent user: add `docker exec -u rmng <clone> bash -lc '<cmd>'`

Drop `-u rmng` and you are root, which cannot see the clone user's tmux socket or systemd
user units. The clone user has passwordless sudo, so `-u rmng` is rarely limiting.

The control-server container has no `python3`. Pipe its JSON to your own box instead:

```sh
curl -s http://10.0.0.182:9000/api/state | python3 -m json.tool
```

## Fast path: swap one binary

The clone binaries are standalone files inside each clone, so a change to `agent-wrapper`
or `clone-daemon` does not need an image rebuild. This takes about a minute instead of
five.

```sh
cd agent-wrapper && bun build --compile src/server.ts --outfile /tmp/agent-wrapper
scp -i ~/.ssh/id_rsa -o IdentityAgent=none -o IdentitiesOnly=yes \
    /tmp/agent-wrapper root@10.0.0.100:/tmp/agent-wrapper
$SSH root@10.0.0.100 'pct push 101 /tmp/agent-wrapper /root/agent-wrapper \
  && pct exec 101 -- bash -lc "docker cp /root/agent-wrapper claude-test2:/opt/rmng/bin/agent-wrapper \
     && docker exec claude-test2 chmod 755 /opt/rmng/bin/agent-wrapper"'
```

Restart the unit as the clone user. `XDG_RUNTIME_DIR` is required, because these are
`systemd --user` units:

```sh
$SSH root@10.0.0.100 'pct exec 101 -- docker exec -u rmng claude-test2 bash -lc \
  "export XDG_RUNTIME_DIR=/run/user/1000; systemctl --user restart agent-wrapper.service"'
```

Compare the checksum after the copy. A silent `docker cp` failure looks exactly like a
change that did not take effect.

The reconciler will not revert this. It compares a payload stamp against the control-server
container's own copy, so an unchanged stamp means no re-push. Publish a new image and the
stamp changes, which overwrites your hand-pushed binary within about a minute.

## Full path: publish an image and recreate

```sh
scripts/publish-server.sh
```

That builds from the repo root, stamps the git SHA, and pushes `pegasis0/rmng:YYYYMMDD`
plus `:latest`. The Rust compile is the long pole at two to four minutes. Watch for the
line `>> published pegasis0/rmng:<date>`.

The dated tag is today's date every time, so a second publish on the same day overwrites
the first. Rollback by tag stops being possible for that day.

Pull and retag inside CT 101, then confirm the revision matches your `HEAD`:

```sh
$SSH root@10.0.0.100 'pct exec 101 -- bash -lc "docker pull -q pegasis0/rmng:20260905 \
  && docker tag pegasis0/rmng:20260905 rmng:latest \
  && docker image inspect rmng:latest --format \"{{index .Config.Labels \\\"org.opencontainers.image.revision\\\"}}\""'
```

Recreate the container. Never use `docker compose`, which prefixes the volume names and
loses the completed setup. Read the live run config first, because `RUST_LOG` drifts:

```sh
$SSH root@10.0.0.100 "pct exec 101 -- docker inspect rmng --format '{{json .Config.Env}}'"
```

Then recreate with that exact value. The `/srv/rmng-homes` bind MUST stay `:shared`
(and the host path must be a shared mount — see below), or clone creation fails:
the server creates each clone's ZFS dataset from inside its own mount namespace,
and only a shared bind propagates that mount to the host mount namespace where
the Docker daemon resolves bind sources. Without it `docker create` fails with
`bind source path does not exist: /srv/rmng-homes/<id>`.

```sh
$SSH root@10.0.0.100 'pct exec 101 -- bash -lc "docker rm -f rmng >/dev/null 2>&1; \
  docker run -d --name rmng --privileged --pid=host --restart unless-stopped \
  -p 445:445 -p 2222:2222 -p 9000:9000 -p 9001:9001 -p 9005:9005 \
  -v /var/run/docker.sock:/var/run/docker.sock -v rmng-data:/data -v rmng-sock:/srv/rmng-sock \
  -v /srv/rmng-homes:/srv/rmng-homes:shared \
  -e RUST_LOG=info,tower_http=warn,clip=debug,rmng_control_server::mediaplane=debug rmng:latest"'
```

After a CT reboot (or if `/srv/rmng-homes` was ever recreated as a plain directory),
re-establish the shared mount BEFORE recreating, then verify propagation:

```sh
$SSH root@10.0.0.100 'pct exec 101 -- bash -lc "mount --bind /srv/rmng-homes /srv/rmng-homes \
  && mount --make-shared /srv/rmng-homes"'
# Verify: a dataset created inside the container must be visible on the host.
$SSH root@10.0.0.100 'pct exec 101 -- bash -lc "docker exec rmng zfs create \
  -o mountpoint=/srv/rmng-homes/probe rpool/rmng-homes/probe \
  && ls -d /srv/rmng-homes/probe \
  && docker exec rmng zfs destroy rpool/rmng-homes/probe"'
```

Restarting resets in-memory state and drops every dashboard connection. Within about a
minute the reconciler pushes fresh clone binaries into every running clone and restarts
their units, with no clone recreate needed.

## Create a clone

```sh
curl -s -XPOST http://10.0.0.182:9000/api/clone -H 'content-type: application/json' \
  -d '{"image":"pegasis0/rmng-template:latest","hostname":"pi-probe",
       "codexAccount":"hello@talktomedi.com","headless":false}'
```

Poll `GET /api/state` and read the `operations` array, not `ops`. Provisioning takes a few
minutes and ends when the operation leaves `running`. The clone appears under `hosts`.

Two traps here.

1. The template image lags the repo. It is rebuilt by `scripts/publish-template.sh`, so a
   change to `template/setup/*.sh` does not reach a new clone until that runs. The
   reconciler is what fixes an existing clone.
2. A clone needs a Codex account for its assistant. Without one the wrapper answers `503`
   and the dashboard shows `agent prompt HTTP 503`.

## Drive a real turn

Prefer the dashboard API over talking to the wrapper directly. It exercises the whole path,
including the Rust chat proxy and the persisted transcript.

```sh
curl -s -XPOST http://10.0.0.182:9000/api/chat/pi-probe \
  -H 'content-type: application/json' \
  -d '{"text":"Take a screenshot and say in one sentence what is on screen."}'
```

Then poll `GET /api/chat/pi-probe` until `busy` is false and read the last message. A turn
with a screenshot takes 20 to 60 seconds.

To isolate the wrapper from the Rust side, hit it directly inside the clone. Open `/events`
before you post, because the reply rides the stream:

```sh
$SSH root@10.0.0.100 'pct exec 101 -- docker exec -u rmng pi-probe bash -lc \
  "curl -sS -XPOST localhost:4096/prompt -H \"content-type: application/json\" \
   -d \"{\\\"text\\\":\\\"say ok\\\"}\""'
```

## Read what happened

The wrapper logs to the systemd user journal. Its startup lines carry the model, the loaded
extensions, and the tool list:

```sh
$SSH root@10.0.0.100 'pct exec 101 -- docker exec -u rmng pi-probe bash -lc \
  "journalctl --user -u agent-wrapper.service -n 30 --no-pager -o cat"'
```

A healthy start looks like this:

```
agent-wrapper listening on http://0.0.0.0:4096 (model openai-codex/gpt-5.6-luna, thinking xhigh)
extensions: <inline:rmng-mcp>, <inline:rmng-service-tier> | tools: read, bash, ..., desktop_screenshot, ...
provider request: model gpt-5.6-luna, effort xhigh, service_tier priority
```

The tool list is a snapshot taken before the MCP adapter's first sync. On a cold cache it
shows only `mcp` and `mcpScript`, and the `desktop_*` tools arrive during the first session.

## Verify before you claim it works

Four checks caught real bugs during the pi swap.

1. Ownership of anything the reconciler writes. Docker's tar extract invents a missing
   parent directory as `root:root`, which silently breaks the agent's own writes.
2. A fresh clone, not just an updated one. The provisioning path and the reconcile path
   are different code.
3. The compiled binary, not just `bun run`. Bundling breaks dynamic imports that work fine
   from source.
4. The dashboard path, not just the wrapper. They are separate hops.

Silence is not success. A wrapper that starts cleanly can still fail every request.

## Test pi subclones

The `pi-subagents` fork uses `rmng clone create --seed` to copy its project and runtime into each worker clone.
The fork lives at [`pi-subagents-rmng/`](../pi-subagents-rmng/README-RMNG.md) in this repository.
Run `pi-subagents-rmng/scripts/rmng/install.sh` from the repository root to install it in a clone.
The seed option requires the matching control-server and fleet command binaries.
A successful create operation includes the completed seed copies.
The operation log reports copy duration in the `seed` step.
Total operation time also includes container creation, startup, and account setup.
Compare file hashes and metadata before starting pi when testing an exact copy.
Pi creates runtime caches and refreshes the Git index after it starts.
The terminal plugin runs separately from the dashboard assistant's fixed extension list.

On 2026-09-05, serial `cp` copied the 357,684-entry `/home/rmng/Dev` tree in 103.82 seconds.
The deployed eight-worker copy took 42.17 seconds, and total clone creation took 51.65 seconds.
The test clone is `dev-seed-fast-20260905`.
The comparison found no missing entries, extra entries, or changed metadata or file contents.

### Repeated copy comparison

On 2026-09-05, three rounds compared copy tools against the same 357,684-entry `Dev` tree in CT 101.
Each copy started with an empty destination in `dev-seed-verify-20260905`.
The benchmark rotated execution order: `cp`, rsync, rclone, then rsync, rclone, `cp`, then rclone, `cp`, rsync.
Copies ran sequentially, and verification ran after all timed copies.
The benchmark did not clear caches or control other workloads.

| Copy configuration | Round 1 seconds | Round 2 seconds | Round 3 seconds | Median seconds | Median processor seconds |
| --- | ---: | ---: | ---: | ---: | ---: |
| `cp`, 8 workers | 36.94 | 31.74 | 24.68 | 31.74 | 94.61 |
| rsync 3.4.1, 8 processes | 60.90 | 50.01 | 38.79 | 50.01 | 197.66 |
| rclone 1.75.1, 32 transfers | 96.10 | 71.97 | 54.59 | 71.97 | 447.84 |

The repeated benchmark used a Python implementation of the seed copy's partitioning, eight-worker schedule, and directory metadata restoration.
Both `cp` and rsync used that implementation.
These timings include planning and directory metadata restoration, but exclude clone creation.
Processor time sums user and system time for copy processes and the planner.
It excludes the Docker command transport processes.
The earlier 42.17-second measurement used the deployed Rust implementation.

The `cp` jobs used `-a --parents --reflink=auto`.
The rsync jobs used `-aHAXSU --numeric-ids --relative --whole-file --inplace --checksum-choice=none`.
The rclone copy used `--metadata --links --create-empty-src-dirs --local-metadata-restore-special-bits`.
It also used `--inplace --no-check-dest --ignore-checksum --transfers 32 --checkers 32 --retries 1 --low-level-retries 1 --config /dev/null`.

All tools became faster across rounds, so individual elapsed times do not establish fixed performance guarantees.
The `cp` implementation won every round and used less processor time.
Its slowest run also beat the fastest rsync and rclone runs.
Keep the eight-worker `cp` seed copy for this workload.

Separate preservation tests passed for `cp` and rsync.
The rclone test lost hardlink relationships and access control lists.
It also failed to copy a named pipe and an ordinary file named `literal.rclonelink`.
Full comparisons of the third-round `cp` and rsync copies found zero differences across all 357,684 entries.
Both comparisons checked file hashes and metadata.
The rclone metadata comparison found changes only on the destination root directory: owner, group, permissions, and modification time.
That comparison checked sizes but did not hash rclone's output.
The [benchmark report](benchmarks/ct101-dev-copy-20260905.json) records every run and verification result.
