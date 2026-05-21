# ambient_viz server

Serves `../static/` over HTTP and streams GPIO input changes to the
browser over Server-Sent Events at `/events`. Runs on the Raspberry Pi
alongside Chromium. The visualizer itself still works opened directly
via `file://.../static/index.html`; the server is only needed when you
want GPIO-driven control.

## Run

```sh
node src/index.js
# or
npm start
```

Open <http://localhost:8080/>.

No npm dependencies for the skeleton — it uses only Node stdlib. Real
GPIO drivers will pull in their own deps once we add them.

## Configuration (env vars)

| Var | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | HTTP listen port |
| `HOST` | `0.0.0.0` | Bind address |
| `AMBIENT_INPUTS` | `mock` | Comma-separated list of input sources to load from `src/inputs/`. Use `none` for no inputs. |

Example on the Pi once real drivers exist:

```sh
AMBIENT_INPUTS=toggles,keypad,mpr121,humidity,pir node src/index.js
```

## Wire format

Each input source publishes to a shared bus: `{name, value, ts}`. The
SSE endpoint snapshots all known input state on connect, then streams
`event: change` messages as values update.

Names planned (mock today, real drivers later):

- `tog.power`, `tog.invert`, ... — toggle switches (boolean)
- `key.last` — last keypress from the 4×4 matrix (string)
- `pir.motion` — D203S motion edge (0/1)
- `humidity` — HR202 reading via ADC (0..1 normalized)
- `touch.mask` — MPR121 12-channel touch bitmask (int, bit n = pad n)

## Adding a real driver

Each driver lives in `src/inputs/<name>.js` and exports:

```js
module.exports = ({ publish }) => { /* read hardware, call publish(name, value) */ };
```

See `inputs/mock.js` for the contract.
