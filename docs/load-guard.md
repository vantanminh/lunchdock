# Load guard and overload protection

A small server runs the panel, its database, Traefik, your builds and your apps side by side. When a build or a burst of traffic uses up the memory or the CPU, the kernel starts swapping and then kills processes. Rudder keeps the panel answering in that situation, so you can still open it and stop what is causing the load.

## What protects the panel

| Layer | What it does |
| --- | --- |
| Load guard | Reads memory, CPU and pressure of the panel server every 3 seconds. Holds new builds while the server is busy and stops running builds before memory runs out. |
| OOM priority | App, database and stack containers start with `oom_score_adj=300`; the panel, its database and Traefik with `-900`. When memory runs out the kernel kills an app, never the panel first. |
| CPU priority | The panel and its database get 8× the CPU weight of an app, Traefik 2×. CPU weight only matters when the CPU is contended: then the panel gets the little it needs at once. |
| Request priority | Requests from people who are signed in and from API tokens always reach the panel. Anonymous requests (webhooks, sign-in pages, scanners) share 64 slots and get `503` with `Retry-After: 5` when those are busy. |
| Direct port | The panel listens on port 3000 as well as behind Traefik. If Traefik is overwhelmed by traffic to your apps, open `http://<server-ip>:3000`. |

## Levels

| Level | When | What happens |
| --- | --- | --- |
| **Normal** (`ok`) | Enough free memory and CPU. | Builds start as usual. |
| **Busy** | Free memory is below the reserve (15% by default), CPU has stayed above the limit (90% by default) for about 15 seconds, or processes keep waiting for memory. | New builds wait in the queue, and so do push deploys of stacks that build. Running builds continue. |
| **Critical** | Free memory is below half the reserve (7.5% by default) twice in a row, or the server is thrashing. | Running builds and stack builds on the panel server are stopped. Their deployment says `stopped by the load guard: only N% of the server's memory was free`. |

The guard relaxes only after the server is clearly better (5 points above the memory reserve, 10 points below the CPU limit), so it does not flip back and forth.

A build held only because of CPU starts anyway after 20 minutes, so a server whose apps keep the CPU busy still deploys. A build held because of memory waits until there is memory. A push deploy of a stack waits up to 30 minutes and is then skipped with an event in the activity list.

Every change of level is written to the activity list (`guard.busy`, `guard.critical`, `guard.ok`, `guard.builds_stopped`), so an alert rule of the *event* kind can send it to Telegram, Discord, Slack, a webhook or email.

## The Load page

**Operate → Load** shows the panel server's memory, CPU, swap, load and pressure, the guard level and why, the build queue, and how the panel, its database and Traefik are protected. The overview shows the same summary and the services that use the most CPU and memory, each with a **Stop** button.

Admins can:

- **Pause builds**: every new build waits until you resume them. Use it before a traffic peak or while you investigate.
- **Cancel all builds**: queued builds are cancelled and running builds on the panel server are stopped.
- **Change the thresholds**: the memory reserve (5–50%), the CPU limit (50–100%), and how many builds run at the same time on one server (1–8). Turning the guard off stops it from holding and stopping builds; the readings stay visible.

A red banner on every page says when the server is critical, with a link to the Load page.

## When the panel is already unreachable

1. Open `http://<server-ip>:3000` instead of the panel domain. It bypasses Traefik.
2. On the Load page, pause builds and cancel the running ones.
3. Stop the service at the top of the CPU or memory list.
4. If the panel still does not answer, `docker stop` the heaviest app container over SSH (`docker stats --no-stream` lists them) and the panel will recover.

## Existing installs

The CPU weight of the panel, its database and Traefik is raised every time the panel starts, so existing installs get it right away. The OOM score of a running container cannot be changed:

- The panel container gets `-900` with the next panel update (**System → Updates**).
- Postgres and Traefik get it when `install.sh` creates their containers; it leaves running ones alone. Their CPU weight, which matters most, is already raised. To give Traefik the low OOM score now, run `docker rm -f rudder-traefik` and re-run `install.sh` with the same options (your apps are unreachable for a few seconds while Traefik starts again).
- App containers get `300` the next time they are deployed.

The Load page lists each protected container and says whether it is fully protected.

On extra servers, the agent's systemd unit has `OOMScoreAdjust=-900` and `CPUWeight=1000` for new installs. For an agent installed earlier, run `systemctl edit rudder-agent`, add those two lines under `[Service]`, and restart it.

## API

| Method | Path | |
| --- | --- | --- |
| `GET` | `/api/v1/load` | Guard level, readings, build queue and protection (any signed-in user) |
| `PUT` | `/api/v1/load/settings` | `enabled`, `memory_reserve_percent`, `cpu_percent`, `build_concurrency` (admin) |
| `POST` | `/api/v1/load/pause` | `{"paused": true}` or `false` (admin) |
| `POST` | `/api/v1/load/cancel-builds` | Cancel queued builds and stop running ones (admin) |
