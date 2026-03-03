# Windows Standup LAN Sync — Implementation Prompt

## Context

You are adding LAN synchronization to an existing Windows standup break reminder app. The app already has: system tray icon, work timer, break countdown overlay (fullscreen semi-transparent window on all monitors), sleep/wake handling (`WM_POWERBROADCAST`), and CLI args via `clap`.

This document specifies the **LAN sync protocol** to add. The same protocol is implemented on macOS — both platforms must interoperate.

## Protocol Specification

### Transport

- **UDP Broadcast** to `255.255.255.255:43210`
- Bind to `0.0.0.0:43210` with `SO_REUSEADDR` (and `SO_REUSEPORT` if available)
- `SO_BROADCAST` must be enabled on the send socket
- Heartbeat every **3 seconds**
- Peer timeout: **9 seconds** (3 missed heartbeats → remove peer)
- On state change to `breaking`: send 3 rapid heartbeats at 0ms, 50ms, 100ms intervals

### Node Identity

- Generate a random `u64` on each launch as `node_id`
- No persistence needed

### Message Format

Single JSON message type, UTF-8 encoded, one per UDP packet:

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

Fields:
- `v` (u8): Protocol version, must be `1`. Ignore messages with different version.
- `id` (u64): Random node ID for this session.
- `state` (string enum): `"working"` | `"breaking"` | `"idle"`
- `secs_to_break` (u32): Seconds until next break. Only meaningful when state=working.
- `break_secs` (u32): This node's break duration in seconds.
- `work_secs` (u32): This node's work interval in seconds.
- `peers` (u8): Number of peers this node currently sees (including self).

### Sync Algorithm

**Rule 0 — Join (first contact):**
When `synced == false` and first peer heartbeat is received:
- If peer is `working`: adopt their `work_secs`, `break_secs`, and `secs_to_break`. Reset work timer. Set `synced = true`.
- If peer is `breaking`: immediately start break. After break, adopt their `work_secs`/`break_secs`.
- Only fires once per launch. Subsequent sync uses Rules 1-3.

**Rule 1 — Follow earlier break:**
While working, if received `peer.secs_to_break < my_secs_to_break` and difference > 5s:
- Reset work timer to `peer.secs_to_break` seconds.

**Rule 2 — Peer breaking → immediate break:**
While working, if any peer has `state == "breaking"`:
- Immediately trigger break countdown.
- Use `max(my_break_secs, peer.break_secs)` as break duration.

**Rule 3 — Ignore sync while breaking:**
While in break state, do not accept any timer adjustments. Only send heartbeats with `state="breaking"`.

### Edge Cases

- **ESC to skip break**: Local only. Node restarts work timer, does not notify peers. Peers continue their own break.
- **Node goes offline**: Removed from peer list after 9s. No impact on remaining nodes.
- **Sleep/wake**: After wake, existing wake handler runs first (re-establishes timers). Then normal heartbeat sync adjusts timing.
- **`--solo` flag**: Disables all LAN sync. No socket opened, no heartbeats sent/received.
- **Firewall**: Windows Firewall will likely prompt on first run. The app needs inbound UDP on port 43210.

## Windows Implementation Guide

### Architecture

Add a `LanSync` module/struct with:

```
LanSync {
    socket: UdpSocket,          // bound to 0.0.0.0:43210, SO_BROADCAST
    node_id: u64,               // random, generated at startup
    peers: HashMap<u64, PeerInfo>,  // tracked peers
    synced: bool,               // true after first sync
    enabled: bool,              // false if --solo
}

PeerInfo {
    state: PeerState,
    secs_to_break: u32,
    break_secs: u32,
    work_secs: u32,
    peers: u8,
    last_seen: Instant,
}
```

### Threading Model

Two approaches, pick based on your app's current architecture:

**Option A: Dedicated thread + channel (recommended)**
- Spawn a thread for UDP recv loop
- Use `mpsc::channel` to send parsed messages to the main/timer thread
- Main thread polls the channel on each timer tick (every 1s during work, or use a dedicated 100ms poll timer)
- Heartbeat sending: can be on the same thread, or use `SetTimer` on main thread every 3s

**Option B: Non-blocking poll on WM_TIMER**
- Set socket to non-blocking mode
- On a 200ms `WM_TIMER`, call `recv_from` in a loop until `WouldBlock`
- Simpler but slightly higher latency

### Socket Setup (Winsock2)

```rust
use std::net::UdpSocket;

let socket = UdpSocket::bind("0.0.0.0:43210")?;
socket.set_broadcast(true)?;
socket.set_nonblocking(true)?; // if using Option B

// Send heartbeat:
socket.send_to(json_bytes, "255.255.255.255:43210")?;

// Receive:
let mut buf = [0u8; 1024];
match socket.recv_from(&mut buf) {
    Ok((len, addr)) => { /* parse JSON from &buf[..len] */ }
    Err(e) if e.kind() == WouldBlock => { /* no data */ }
    Err(e) => { /* real error */ }
}
```

### Integration Points

1. **Startup**: Create `LanSync` (unless `--solo`). Start heartbeat timer. Start recv thread/timer.
2. **Work timer tick** (menubar update): Calculate current `secs_to_break` from work timer fire date. Include in heartbeat.
3. **Receive handler**: Parse message → apply Rules 0/1/2/3 → may call `reset_work_timer(secs)` or `trigger_break()`.
4. **Break start**: Send 3 rapid heartbeats. Then continue normal 3s heartbeat with `state="breaking"`.
5. **Break end**: Switch heartbeat back to `state="working"`, recalculate `secs_to_break`.
6. **Shutdown**: Stop sending heartbeats. Peers will time out in 9s.

### System Tray Integration

- Show peer count in tray tooltip: `"Standup - 18:32 remaining [3 peers]"`
- Or in the tray menu: add a disabled item `"Synced with 3 peers"` / `"Solo mode"` / `"No peers found"`

### CLI Args

Add to existing clap args:
```
--solo          Disable LAN sync (run standalone)
--port <PORT>   LAN sync port (default: 43210)
```

### Testing

- Run two instances on the same machine (both bind to 0.0.0.0:43210 with SO_REUSEADDR)
- Verify: second instance syncs to first within 3s
- Verify: when first breaks, second breaks within 200ms
- Verify: ESC on one does not affect the other
- Verify: killing one instance, the other continues normally after 9s timeout
- Cross-platform: run one on macOS, one on Windows, verify interop

### Dependencies

- `serde` + `serde_json` for message serialization (likely already in the project)
- `rand` for node_id generation (or use `std::hash` of timestamp + pid as poor-man's random)
- No additional Windows-specific crates needed — `std::net::UdpSocket` covers everything

## Priority

1. **MVP**: Heartbeat send/recv + Rule 2 (break sync) — this alone is highly useful
2. **Phase 2**: Rules 0 and 1 (timer convergence + join sync)
3. **Phase 3**: Peer count display, `--solo`/`--port` flags, menu status
