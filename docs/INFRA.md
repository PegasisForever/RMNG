# Provisioned infrastructure (Proxmox)

The Proxmox node is **`pegaswarm`** — standalone (no cluster), PVE 9.2, the AMD Radeon Pro
**W6800** box. SSH `root@10.0.0.100`. Snapshot of the live `pct list` below (2026-06-28);
re-check with `ssh root@10.0.0.100 pct list`.

> RMNG runs two CTs: **CT 106 `rmng-build`** — the *staging* control-server (build box **and**
> control plane) — and **CT 105 `rmng-template`** — the golden template the control-server
> CoW-clones from. The `pega-*` infra and clones below belong to the **old-stack** control-server
> on CT 101 and coexist with RMNG. (The earlier `ng-*`/`poc-*`/`e2e*` rigs are gone.)

## RMNG — staging (build + control plane)

| CT | Name | IP | GPU | onboot | /srv/rmng-sock | Role |
|---|---|---|---|---|---|---|
| **106** | **rmng-build** | 10.0.0.79 | ✓ | yes | ✓ | **Staging control-server.** Source at `/root/RMNG`; builds the self-contained `control-server` (full toolchain) **and** runs it (`rmng-control-server.service` — dashboard `http://10.0.0.79:9000`, video `:9001`, MCP `:9002/:9003`). Same runtime as a production deploy CT (it ran `cs-deploy-ct.sh`) plus the build tools, so it orchestrates **real clones** over SSH to the node. Does **not** run GNOME/capture. Also builds the patched gnome-shell deb (`/root/rmng-shell-build`). |

This replaces the old hand-built `ng-build` (CT 132). "Staging vs production": identical runtime
to a `provision-deploy-ct.sh` CT, just with the toolchain still present.

## RMNG — clones (orchestrated by CT 106)

| CT | Name | IP | GPU | onboot | /srv/rmng-sock | Role |
|---|---|---|---|---|---|---|
| **105** | **rmng-template** | 10.0.0.39 | ✓ | no | ✓ | The **golden template**: a full clone image (headless GNOME + `clone-daemon` + `agent-wrapper` + patched gnome-shell + standalone `claude` CLI), built by `POST /api/template/bootstrap`. CoW clones (`POST /api/clone`) snapshot it. Currently also registered as a selectable host (the first real clone). User `rmng`/`rmng` (passwordless sudo), timezone America/Toronto. |

CoW clones the control-server provisions appear here too — single-NIC on `vmbr0`, GPU
passthrough, the `/srv/rmng-sock` media-socket bind-mount. Each clone's `clone-daemon` connects
to CT 106's control-server over that socket; no per-clone subnet/tailnet.

## Shared / old-stack infra (coexists with RMNG)

| CT | Name | IP(s) | Role |
|---|---|---|---|
| 101 | pega-control | 10.0.0.20 / **10.60.0.1** | The **old control-server** (React Router/Bun) **and** the Tailscale subnet router for the internal `10.60.0.0/24`. |
| 104 | pega-dev-template | 10.0.0.62 / 10.60.0.154 | Golden **dev template** the old stack CoW-clones from (patched g-r-d/gnome-shell for the old RDP client). |
| 114 | pega-infer | 10.0.0.42 / **10.60.0.10** | GPU **inference** CT (llama.cpp / Qwen). The needs-human detector calls `http://10.60.0.10:8080` — reachable from on-subnet clones, **not** from CT 106. |
| 113 | pega-dev-172 | old-stack DHCP | Live old-stack clone (Linear DEV-172). |
| 116 | pega-hh-11 | 10.0.0.220 / 10.60.0.63 | Live old-stack clone (Linear HH-11). |
| 117 | pega-dev-169 | 10.0.0.159 / 10.60.0.160 | Live old-stack clone (Linear DEV-169). |
| 119 | pega-we-609 | old-stack DHCP | Live old-stack clone (Linear WE-609). |
| 120 | pega-we-598 | 10.0.0.204 / 10.60.0.110 | Live old-stack clone (Linear WE-598). |

> The `pega-*` clones are served by the **old** control-server (CT 101) over RDP/g-r-d on the
> `10.60.0.x` subnet behind the CT-101 router — they have no `/srv/rmng-sock` mount. RMNG clones
> connect to their control-server over the bind-mounted socket and need no subnet/tailnet.

## Unrelated CTs on the same node

`100 turbo-cache`, `102 aws-jump-host`, `108–112 dev-lxc-haoran-1..5`, `118
talktomedi-dashboard` — other projects/users; not part of this stack.

## Reaching things

- **RMNG dashboard:** `http://10.0.0.79:9000`.
- **RMNG viewer (from your laptop):** `RMNG_VIDEO=10.0.0.79:9001 cargo run -p viewer` — streams
  the selected clone.
- **Build / iterate:** `ssh root@10.0.0.100 pct exec 106 -- …` (source `/root/RMNG`). Rebuild +
  restart: re-run `cs-build-ct.sh` in the CT, then `systemctl restart rmng-control-server`.
- **Proxmox node:** `ssh root@10.0.0.100` (`pct list`, `pct config <id>`, `pct exec <id> -- …`).
- **Detector inference:** `http://10.60.0.10:8080` (only from on-subnet clones).
- The control-server reaches the node for orchestration over its own ed25519 key (authorized on
  the node by `provision-build-ct.sh` / `provision-deploy-ct.sh`); `proxmox.ssh` = `root@10.0.0.100`.

## CT roles by node config

- **GPU passthrough** (`/dev/dri/renderD128`): every RMNG + `pega-*` CT (VA-API).
- **`/srv/rmng-sock` bind-mount:** the RMNG control-server CT (106) + every RMNG clone (105, …).
- **Two NICs (`10.0.0.x` + `10.60.0.x`):** old-stack CTs behind the CT-101 subnet router;
  RMNG CTs are single-NIC on `vmbr0`.
