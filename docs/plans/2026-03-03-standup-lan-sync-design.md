# Standup LAN Sync Protocol Design

## Overview

Multiple standup instances (2-10 devices) on the same LAN discover each other and synchronize break timing via UDP broadcast. No central server, no leader election.

## Transport

- **Method**: UDP Broadcast (`255.255.255.255:43210`)
- **Heartbeat interval**: 3 seconds
- **Peer timeout**: 9 seconds (3 missed heartbeats = offline)
- **Immediate event**: state change to `breaking` triggers 3 rapid packets at 0/50/100ms
- **Node ID**: random `u64` per launch, no persistence

## Message Format

Single message type — Heartbeat (JSON, UTF-8):

```json
{
  "v": 1,
  "id": 12345678901234,
  "state": "working",
  "secs_to_break": 342,
  "break_secs": 300,
  "work_secs": 1500,
  "peers": 3
}
```

| Field | Type | Description |
|---|---|---|
| `v` | u8 | Protocol version, currently `1` |
| `id` | u64 | Random node ID |
| `state` | enum | `"working"` / `"breaking"` / `"idle"` |
| `secs_to_break` | u32 | Seconds until next break (meaningful when working) |
| `break_secs` | u32 | This node's break duration config |
| `work_secs` | u32 | This node's work duration config |
| `peers` | u8 | Number of online peers this node sees (including self) |

## Sync Algorithm

### Rule 0: Join — adopt group config on first contact

When a new node first receives peer heartbeats:
- If any peer is `working`: adopt the config (work_secs, break_secs) and timing (secs_to_break) of the peer with the earliest break. Mark `synced = true`.
- If all peers are `breaking`: immediately trigger break. After break ends, adopt the most common work_secs/break_secs from peers.
- Only applies on first contact (`synced == false`). Subsequent sync uses rules 1-3.
- Runtime config only — CLI args are not modified.

### Rule 1: Follow earlier break

While working, if a peer's `secs_to_break < my secs_to_break` and the difference > 5s:
- Reset work timer to `peer.secs_to_break`
- Log: `"synced to peer {id}, break in {secs}s"`

The 5s threshold prevents jitter near break time.

### Rule 2: Peer breaking → immediate break

While working, if any peer's state is `"breaking"`:
- Immediately trigger `show_countdown()`
- Use `max(my.break_secs, peer.break_secs)` for break duration

### Rule 3: Ignore sync while breaking

While in break state:
- Only send heartbeats (state="breaking"), reject all timer adjustments
- Other working nodes will see this and trigger Rule 2

## Edge Cases

- **New node joins**: First heartbeat triggers Rule 0 for the newcomer; existing nodes may follow Rule 1 if newcomer has earlier break
- **Node goes offline**: Removed after 9s timeout, no impact on others
- **All nodes break simultaneously**: Each counts down independently (break_secs may differ), restarts work timer on completion, re-converges via heartbeats
- **ESC skip**: Local only, does not propagate — node restarts work timer, others continue their break
- **Laptop rejoins with different config**: Rule 0 adopts group config on first contact
- **Sleep/wake**: Existing wake handler fires, then LAN sync adjusts timing via normal heartbeat rules
- **`--solo` flag**: Disables LAN sync entirely, runs standalone

## Latency Guarantees

- Break synchronization: < 150ms (normal) / < 3s (extreme packet loss fallback)
- Timer convergence: < 3s (one heartbeat cycle)
- New node join: < 3s

## UI Integration

- Menubar shows peer count: `☕ 18:32 [3]` (3 peers online)
- Menu shows sync status item: "Synced with 3 peers" or "Solo mode"
