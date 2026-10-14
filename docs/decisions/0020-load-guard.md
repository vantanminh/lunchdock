---
created_at: "2026-10-14T00:56:24.814564980+00:00"
doc: docs/decisions/0020-load-guard.md
id: "0020"
notes: null
status: accepted
title: "Keep the panel reachable under load: a load guard and control-plane priority"
type: decision
updated_at: "2026-10-14T00:56:24.814567665+00:00"
verify: null
---

# Keep the panel reachable under load: a load guard and control-plane priority

Date: 2026-10-14

## Status

Accepted

## Context

Rudder runs the panel, its PostgreSQL, Traefik, the builds and the apps on one small server. When a build or a burst of traffic to an app used up the memory or the CPU, the kernel started swapping and then killed whatever it judged largest. The panel answered slowly or not at all, and sometimes the OOM killer picked the panel or its database. With the panel gone there was no way to stop the build or the app that caused it short of SSH.

Builds are the usual trigger: `docker build` and Nixpacks run on the panel host for every server, and a single `npm install` or a Rust build can take gigabytes. App traffic is the other: an app that gets popular can grow until nothing else fits.

## Decision

The panel protects itself in four layers.

1. **Kernel priority.** Workload containers (apps, databases, stack containers) are created with `oom_score_adj=300`, so the OOM killer picks them first. The installer starts the panel and its database with `--oom-score-adj -900 --cpu-shares 8192` and a memory reservation, and Traefik with `-900` and `--cpu-shares 2048`; a panel self-update keeps those values and adds them to a panel container that lacks them. On start, and every ten minutes, the panel also raises the CPU weight and memory reservation of those containers through the Docker API, because an existing install cannot change its OOM score without being recreated. The agent's systemd unit gets `OOMScoreAdjust=-900` and `CPUWeight=1000`.
2. **Load guard.** Every three seconds the panel reads `/proc` of its host: available memory, CPU, load, and pressure stall information when the kernel has it. A pure state machine in `rudder-core` turns the readings into `ok`, `busy` or `critical`, with smoothing (CPU is averaged over about 15 seconds; critical needs two readings in a row) and hysteresis (it relaxes only after a margin). While the host is `busy` (free memory below the reserve, CPU above the limit, or memory stalls), queued builds and push deploys of stacks that build wait. When it is `critical` (free memory below half the reserve, or heavy memory stalls) running builds and stack builds on the panel host are stopped, with the reason written to the deployment. A build held only because of CPU starts anyway after 20 minutes, so a server whose apps keep the CPU busy still deploys. Memory holds have no limit.
3. **Request priority.** Requests that carry a session cookie or a bearer token always reach a handler. Anonymous requests (webhooks, sign-in, scanners) share 64 slots and get an immediate 503 with `Retry-After` when they are all busy.
4. **Rescue controls.** Admins can pause every new build, cancel queued and running builds, change the thresholds and the number of builds at a time, and stop the services that use the most CPU or memory, from the overview and the Load page. The panel also stays reachable on port 3000 directly, bypassing Traefik.

## Alternatives Considered

1. A cgroup slice for all apps with a hard memory limit. It is the strongest guarantee, but it needs write access to the host's cgroup tree, differs between the cgroupfs and systemd drivers, and cannot be applied from inside the panel container.
2. Running builds in a resource-limited BuildKit container builder. It bounds the build, but changes caching, needs `--load` for every image, and Nixpacks drives its own build.
3. Limits on every app by default. It would surprise people whose apps need the memory, and does not help when many small apps add up.

## Consequences

Positive:

- The panel, its database and Traefik get CPU first and are the last processes the kernel kills.
- A build that would run the server out of memory is stopped with a clear message instead of taking the server down.
- People can see why builds wait and act from the panel, even while the server is overloaded.

Tradeoffs:

- An app can now be killed by the OOM killer where before the panel might have been.
- Builds can wait during long busy periods; the overview says why and lets an admin cancel or turn the guard off.
- Existing installs get the lower OOM score for the panel only after the next panel update or a re-run of `install.sh`; the Load page shows which containers are not yet protected.
