# ambient_viz server

Serves `../static/` over HTTP and streams kiosk sensor inputs to the
browser over Server-Sent Events at `/events`. Runs on the Pi alongside
Chromium and a Python sensor sidecar (see `../python/`); receives sensor
publications over a local POST `/ingest` and fans them out as SSE.

The visualizer itself still works opened directly via
`file://.../static/index.html`; the server is only needed when you want
GPIO-driven control.

## Run

```sh
npm install
node src/index.js
# or
npm start
```

Open <http://localhost:8080/>.

`serialport` is used when `DAISY_SERIAL` is set for Daisy CDC song-position and
control bridging. Mock/SSE-only testing still goes through the same package
install path.

## Configuration (env vars)

| Var | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | HTTP listen port |
| `HOST` | `0.0.0.0` | Bind address |
| `MOCK` | `0` | Set to `1` to enable the in-process mock source (fake sensor data, no Python needed). Useful for Mac-side SSE plumbing tests. |
| `INGEST_TOKEN` | (none) | If set, `POST /ingest` requires header `X-Ingest-Token: <value>`. Without this var, `/ingest` accepts any localhost request unauthenticated. |
| `MOTION_PRESENCE` | `0` (off) | **Feature flag for the AM312 motion sensors.** Off → room occupancy is derived purely from the ToF distance feed, exactly as without any PIR (the reliable fallback). Set `1`/`true` to let the OR'd `motion` channel **augment** occupancy: motion forces the room "occupied" and holds it for `MOTION_HOLD_S` after motion stops, so a still visitor the ToF cone misses still counts as present. Augment-only — motion never *clears* occupancy, so distance still owns the "empty" baseline. Drives all three presence triggers: the entry bell fires on the empty→occupied edge (motion onset or ToF approach), voice-on-leave on the confirmed-empty edge, the toll while occupied. With the flag off, each reverts to its pure-distance path (the bell to its sustained-approach gate). Even when on, if no `motion` events arrive (AM312 absent/miswired) occupancy silently reverts to distance. |
| `MOTION_HOLD_S` | `20` | How long (s) after motion stops the room still counts as occupied. Bridges the AM312's ~2 s internal drop and its blindness to a perfectly still person. Only consulted when `MOTION_PRESENCE` is on. |
| `VOICE_TOLL` | `1` (on) | The periodic "active room" voice: while occupied **and** there's recent AM312 motion, murmur a random phrase every `VOICE_TOLL_MIN_S`–`VOICE_TOLL_MAX_S` (default 300–600 s), skipping `VOICE_TOLL_SKIP_PROB` (0.25) of them. Self-disables with no motion. `VOICE_TOLL=0` turns it off. Also: `VOICE_TOLL_ACTIVE_S` (30, the "recent motion" window) and `VOICE_MIN_GAP_S` (20, min gap between any two spoken phrases). |

## Architecture

```
[Python sidecar]  ── POST /ingest ──►  [Node SSE server]  ── /events ──►  Browser
                    JSON {name,value}                       SSE change frames
```

The Python sidecar reads sensors (per `hardware-handoff.md` in the repo
root) and POSTs each value change to `/ingest`. Node holds the latest
state per name and broadcasts changes to all connected SSE clients.

## Endpoints

### `GET /events` (SSE)

Server-Sent Events stream. On connect, snapshots the latest value of
every known input as a `change` event, then sends `event: ready` and
continues streaming `change` events as values update. 15 s keepalive
comments through any proxies.

Frame format:

```
event: change
data: {"name":"distance_cm","value":42.3,"ts":1747800000000}

```

### `POST /ingest` (localhost only)

Accepts publications from the Python sensor layer. Request body is
either a single object or an array:

```json
{"name": "distance_cm", "value": 42.3}
```

Restricted to loopback connections (`127.0.0.1` / `::1`). If
`INGEST_TOKEN` is set, the `X-Ingest-Token` header must match.

Response: `200 OK` with `{"accepted": <n>}`.

## Wire vocabulary

Event names emitted by the Python drivers (and by `MOCK=1`) per
`hardware-handoff.md`:

| Name | Type | Source | Meaning |
|---|---|---|---|
| `motion` | boolean | AM312 ×2 (GPIO4 + GPIO23), OR'd | True while *either* PIR cone holds detection; suppressed for 60 s post-boot. Ignored unless `MOTION_PRESENCE` is on |
| `distance_cm` | number | VL53L1X (I²C 0x29) | Smoothed distance to closest target in cone; `null` ≈ no target |
| `breath_detected` | timestamp (ms) | HR202 + TLC555 (GPIO17) | Most-recent breath puff; bumps monotonically. Visualizer compares to `Date.now()` for recency. |
| `touch_mask` | int (0..4095) | MPR121 (I²C 0x5A, IRQ GPIO27) | 12-bit channel state; bit `n` set ⇒ channel `n` touched |

Adding new event names: just have the Python driver POST under a new
name. Nothing in the Node server needs to change. The browser-side
`window.AMBIENT_INPUTS` will pick it up automatically.

## Browser-side access

The visualizer's inline client (`static/index.html`) populates
`window.AMBIENT_INPUTS` as values arrive:

```js
const dist = window.AMBIENT_INPUTS.distance_cm ?? 200;
const isTouched = (window.AMBIENT_INPUTS.touch_mask ?? 0) !== 0;
const lastBreathAgoMs = Date.now() - (window.AMBIENT_INPUTS.breath_detected ?? 0);
```

`window.AMBIENT_INPUTS.__meta` exposes connection state (`connected`,
`lastEventAt`, `error`).
