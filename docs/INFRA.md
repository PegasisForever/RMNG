# Provisioned infrastructure (Proxmox)

The Proxmox node is **`pegaswarm`** — standalone (no cluster), PVE 9.2, the AMD Radeon Pro
**W6800** box. SSH `root@10.0.0.100`. Snapshot of the live `pct list` below (2026-06-25);
re-check with `ssh root@10.0.0.100 pct list`.

> CT 132 `ng-build` is the persistent build/dev box for RMNG. The other RMNG CTs
> (`ng-control-e2e`, `e2eclone`, `ng-build-e2e`, the two `poc-*`) are **disposable test rigs**
> — fine to delete and recreate. The `pega-*` infra and clones below belong to the **current
> (old-stack) control-server** on CT 101 and coexist with RMNG.

## RMNG — build & dev

| CT | Name | IP(s) | GPU | onboot | Role |
|---|---|---|---|---|---|
| **132** | **ng-build** | 10.0.0.31 | ✓ | no | **The build + dev/test CT.** Source at `/root/RMNG`; builds the self-contained `control-server`; runs headless GNOME as `pega` (uid 1000) for running the GPU bins. ~12 G warm target cache. This is the rsync+cargo target in [DEPLOY.md](DEPLOY.md). Also where the patched gnome-shell deb is built (`/root/ng-shell-build`). |

## RMNG — E2E / PoC rigs (disposable)

| CT | Name | IP(s) | ng-sock | onboot | Role |
|---|---|---|---|---|---|
| 122 | ng-control-e2e | 10.0.0.182 | ✓ | yes | Deployed RMNG **control-server** from the full E2E run (dashboard `http://10.0.0.182:9000`). |
| 123 | e2eclone | 10.0.0.190 | ✓ | no | RMNG **clone** bootstrapped by CT 122 in the E2E run (clone-daemon + agent-wrapper). |
| 115 | ng-build-e2e | 10.0.0.27 | — | yes | Throwaway E2E build CT. |
| 130 | poc-headless | 10.0.0.70 | — | yes | Phase-0 PoC rig (headless GNOME capture). |
| 131 | poc-media | 10.0.0.97 | — | yes | Phase-0 PoC rig (media plane). |

The `ng-sock` column = has the `/srv/ng-sock` clone-socket bind-mount (the marker of a
RMNG control-server/clone pair). The two `poc-*` rigs are leftover Phase-0 prototypes
(safe to delete).

## Shared / old-stack infra (coexists with RMNG)

| CT | Name | IP(s) | Role |
|---|---|---|---|
| 101 | pega-control | 10.0.0.20 / **10.60.0.1** | The **current/old control-server** (React Router/Bun) **and** the Tailscale subnet router for the internal `10.60.0.0/24`. |
| 104 | pega-dev-template | 10.0.0.62 / 10.60.0.154 | Golden **dev template** that the old stack CoW-clones from; carries the patched g-r-d/gnome-shell used by the old RDP client. |
| 114 | pega-infer | 10.0.0.42 / **10.60.0.10** | GPU **inference** CT (llama.cpp / Qwen). The needs-human detector calls `http://10.60.0.10:8080` — reachable from clones on the internal subnet, **not** from the build CT. |
| 116 | pega-hh-11 | 10.0.0.220 / 10.60.0.63 | Live old-stack clone (Linear HH-11). |
| 117 | pega-dev-169 | 10.0.0.159 / 10.60.0.160 | Live old-stack clone (Linear DEV-169). |
| 120 | pega-we-598 | 10.0.0.204 / 10.60.0.110 | Live old-stack clone (Linear WE-598). |

> The `pega-*` clones (116/117/120) are served by the **old** control-server (CT 101) over
> RDP/g-r-d — they have no `/srv/ng-sock` mount and live on the `10.60.0.x` subnet behind the
> CT-101 router. RMNG clones, by contrast, connect to their control-server over the
> bind-mounted `/srv/ng-sock` socket and need no per-clone subnet/tailnet.

## Unrelated CTs on the same node

`100 turbo-cache`, `102 aws-jump-host`, `108–112 dev-lxc-haoran-1..5`, `118
talktomedi-dashboard` — other projects/users; not part of this stack.

## Reaching things

- **Build/iterate:** `ssh root@10.0.0.31` (source `/root/RMNG`). Run GPU bins as `pega`
  with the session env in [DEPLOY.md](DEPLOY.md).
- **RMNG dashboard (E2E deploy):** `http://10.0.0.182:9000`.
- **Proxmox node:** `ssh root@10.0.0.100` (`pct list`, `pct config <id>`, `pct exec <id> -- …`).
- **Detector inference:** `http://10.60.0.10:8080` (only from on-subnet clones).
- The control-server reaches the node for orchestration over its own ed25519 key (authorized
  on the node by `provision-deploy-ct.sh`); set `proxmox.ssh` to `root@10.0.0.100`.

## CT roles by node config

- **GPU passthrough** (`/dev/dri/renderD128`): every RMNG + `pega-*` CT (VA-API).
- **`/srv/ng-sock` bind-mount:** only RMNG control-server/clone CTs (122, 123).
- **Two NICs (`10.0.0.x` + `10.60.0.x`):** old-stack CTs behind the CT-101 subnet router;
  RMNG CTs are single-NIC on `vmbr0`.
