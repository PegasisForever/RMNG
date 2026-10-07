# Runbook: testing RMNG changes on CT 101

CT 101 is the disposable test deployment. Every command here was run against it on
2026-09-05 while swapping the per-clone assistant to the pi coding agent.

**The production fleet is never a deploy target. Never deploy to it.** Publishing an image
is fine and updating CT 101 is fine, but putting a build onto a production container needs
the operator to ask for that deploy by name. Reading a production container stays fine.
(The production containers are CT 204, CT 205 and CT 206 —
[RUNBOOK-GEN1-TO-GEN2.md](archive/RUNBOOK-GEN1-TO-GEN2.md) lists them.)

## Access

CT 101 is a Proxmox container on the host at `10.0.0.100`. It runs Docker, and the
control-server is a container named `rmng` published on `10.0.0.178`.

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
curl -s http://10.0.0.178:9000/api/state | python3 -m json.tool
```

## Publish an image and recreate

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
the clone daemon, with no clone recreate needed. On a clone that still has the retired
`agent-wrapper`, the same pass stops it, masks its unit, and deletes its files.

## Create a clone

```sh
curl -s -XPOST http://10.0.0.178:9000/api/clone -H 'content-type: application/json' \
  -d '{"linear":{"displayName":"pi probe"},"preset":"work",
       "codexAccount":"hello@talktomedi.com","headless":false}'
```

Poll `GET /api/state` and read the `operations` array, not `ops`. Provisioning takes a few
minutes and ends when the operation leaves `running`. The clone appears under `hosts`.

Two traps here.

1. The template image lags the repo. It is rebuilt by `scripts/publish-template.sh`, so a
   change to `template/setup/*.sh` does not reach a new clone until that runs. The
   reconciler is what fixes an existing clone.
2. The chat panel needs Settings → Presets → Assistant filled in: `url` (the pi-web server) and
   `serverUrl` (this server as the assistant reaches it, here `http://10.0.0.178:9000`).
   Without them `POST /api/chat/:id` answers `409` and says which address is missing.

## Drive a real turn

The chat panel talks to an outside assistant, not to anything inside the clone. Send
through the dashboard API. It exercises the whole path: the first message creates the
clone's chat on the assistant, and the control-server follows that chat's event stream.

```sh
curl -s -XPOST http://10.0.0.178:9000/api/chat/pi-probe \
  -H 'content-type: application/json' \
  -d '{"text":"Take a screenshot and say in one sentence what is on screen."}'
```

Then poll `GET /api/chat/pi-probe` until `busy` is false and read the last message.

The assistant reaches the clone only through the `rmng` CLI. To isolate it from the chat
side, run the call it would make, from the assistant's machine:

```sh
rmng --server http://10.0.0.178:9000 desktop pi-probe screenshot
```

## Read what happened

Chat failures land in the control-server log as `chat:` lines. A send that does not reach
the assistant also leaves a `⚠` notice in the chat itself.

```sh
$SSH root@10.0.0.100 'pct exec 101 -- docker logs --since 10m rmng 2>&1 | grep "chat:"'
```

## Verify before you claim it works

Two checks caught real bugs during the pi swap.

1. Ownership of anything the reconciler writes. Docker's tar extract invents a missing
   parent directory as `root:root`, which silently breaks the agent's own writes.
2. A fresh clone, not just an updated one. The provisioning path and the reconcile path
   are different code.

Silence is not success. A chat that sends cleanly can still fail every turn.
