# Dashboard & UI Mode

Blacksmith includes an optional web dashboard for monitoring multiple blacksmith instances across projects and machines.

## Architecture

Two components:

- **`blacksmith serve`** — Exposes a JSON API from each running blacksmith instance (feature-gated behind `--features serve`).
- **`blacksmith-ui`** — A separate binary that aggregates data from all instances and serves a web dashboard.

```
blacksmith serve (per-project)          blacksmith-ui (single dashboard)
├── JSON API on :8420                   ├── Web UI on :8080
├── UDP multicast heartbeat ──────────► ├── UDP multicast listener
│                                       ├── HTTP poller (every 10s)
│   ◄──────────────────────────────────── polls /api/* endpoints
└── SSE for live transcripts            └── Embedded static frontend
```

Instances auto-discover each other via UDP multicast on the LAN. Remote instances can be added manually.

## Building

```bash
# Build blacksmith with serve support
cargo build --release --features serve

# Build the dashboard
cargo build --release -p blacksmith-ui
```

The `serve` feature is opt-in so default builds stay fast. The `blacksmith-ui` crate is a separate workspace member.

## Running

### 1. Start instance APIs

In each project directory where blacksmith is running:

```bash
blacksmith serve
blacksmith serve --port 8420           # explicit port (default: 8420)
blacksmith serve --bind 0.0.0.0        # bind address (default: 0.0.0.0)
```

This starts the JSON API server and (by default) a UDP multicast heartbeat beacon.

### 2. Start the dashboard

```bash
blacksmith-ui
```

Open http://localhost:8080 in a browser.

### 3. Add instances

Instances on the same LAN are auto-discovered via UDP multicast. For remote instances, either:

- Use the "Add Project" form in the sidebar, or
- Configure `blacksmith-ui.toml` (see below)

## Configuration

### Instance-side: `.blacksmith/config.toml`

```toml
[serve]
port = 8420                             # HTTP API port (default: 8420)
bind = "0.0.0.0"                        # Bind address (default: 0.0.0.0)
heartbeat = true                        # UDP multicast beacon (default: true)
heartbeat_address = "239.66.83.77:8421" # Multicast group (default: 239.66.83.77:8421)
# api_advertise = "http://myhost:8420"  # Override advertised URL (for NAT/proxy)
```

### Dashboard-side: `blacksmith-ui.toml`

Place this in the directory where you run `blacksmith-ui`:

```toml
[dashboard]
port = 8080              # Dashboard port (default: 8080)
bind = "127.0.0.1"       # Bind address (default: 127.0.0.1)
poll_interval_secs = 10  # How often to poll instances (default: 10)

# Manually configured instances (in addition to auto-discovered ones)
[[projects]]
name = "my-project"
url = "http://192.168.1.50:8420"

[[projects]]
name = "other-project"
url = "http://192.168.1.51:8420"
```

Runtime-added instances (via the UI form or POST /api/instances) are persisted to `.blacksmith-ui-instances.json` and survive restarts.

## Dashboard Features

### Overview (no project selected)

- **Sidebar** — Project list with online/offline status dots and worker counts.
- **Aggregate cards** — Open beads, in-progress, total workers, instances online, cost today.
- **Global metrics panel** — Cost today/this week, bead velocity (per day), worker utilization with progress bar, session outcome breakdown (success/failed/timed out).

### Project Detail (click a project)

- **Status bar** — Online/offline, iteration count, worker count, uptime.
- **ETA panel** — Remaining beads, parallel/serial ETA, worker count slider for what-if estimates, critical path bead list.
- **Bead list** — Filterable by status (all/open/in_progress/closed), expandable details.
- **Active sessions** — Worker assignments with status, bead, duration, and "View Transcript" button.
- **Metrics summary** — Average cost, tokens, duration, turns, cost today, beads closed today.
- **Stop button** — Sends stop signal to the instance (with confirmation dialog).

### Transcript Viewer

Click "View Transcript" on any active session to open an overlay:

- **Live sessions** — Streams turns via SSE in real-time.
- **Completed sessions** — Loads full transcript.
- Turns colored by role (assistant, user, tool, system).
- Client-side text search with highlighting.
- Auto-scroll that pauses when you scroll up.

## API Endpoints

### Instance API (`blacksmith serve`)

| Endpoint | Description |
|---|---|
| `GET /api/health` | Liveness probe (returns `{"ok": true}`) |
| `GET /api/status` | Coordinator state, workers, iterations |
| `GET /api/project` | Project name, repo path, config summary |
| `GET /api/beads` | Bead listing with filters |
| `GET /api/beads/:id` | Single bead detail |
| `GET /api/sessions` | Session list with metadata |
| `GET /api/sessions/:id` | Session metadata + metrics |
| `GET /api/sessions/:id/stream` | SSE: live transcript |
| `GET /api/metrics/summary` | Aggregated stats and averages |
| `GET /api/metrics/timeseries` | Cost, tokens, duration over time |
| `GET /api/improvements` | Self-improvement records |
| `GET /api/estimate` | ETA with optional `?workers=N` override |
| `POST /api/stop` | Touch STOP file |

### Dashboard API (`blacksmith-ui`)

| Endpoint | Description |
|---|---|
| `GET /api/health` | Liveness probe |
| `GET /api/instances` | List all known instances |
| `POST /api/instances` | Add a runtime instance (`{url, name?}`) |
| `GET /api/aggregate` | Cross-project aggregate stats |
| `GET /api/global-metrics` | Cost, velocity, utilization, outcomes |
| `GET /api/instances/:url/poll-data` | Cached poll data for one instance |
| `GET /api/instances/:url/estimate` | Proxy to instance estimate endpoint |
| `POST /api/instances/:url/stop` | Proxy stop to instance |
| `GET /api/instances/:url/sessions/:id/stream` | Proxy SSE transcript |
| `GET /api/instances/:url/sessions/:id/transcript` | Proxy full transcript |

## Troubleshooting

**Instance not appearing in sidebar?**
- Check that `blacksmith serve` is running in the project directory.
- For LAN discovery, ensure your firewall allows UDP multicast on `239.66.83.77:8421`.
- For remote instances, add them manually via the UI or `blacksmith-ui.toml`.
- Set `heartbeat = false` in config if you don't want multicast (manual-only discovery).

**`blacksmith serve` command not available?**
- Rebuild with `cargo build --release --features serve`.

**Dashboard shows instance as offline (gray dot)?**
- The instance is unreachable or hasn't sent a heartbeat in 90 seconds.
- Check the instance URL is correct and the port is accessible.

**`api_advertise` — when do I need it?**
- When the instance is behind NAT or a reverse proxy, the auto-detected address won't be reachable from the dashboard. Set `api_advertise` to the externally-reachable URL.
