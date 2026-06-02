const http = require('http');
const fs = require('fs');
const path = require('path');
const { EventEmitter } = require('events');

const STATIC_ROOT = path.resolve(__dirname, '..', '..', 'static');
const PORT = parseInt(process.env.PORT || '8080', 10);
const HOST = process.env.HOST || '0.0.0.0';
const MOCK = process.env.MOCK === '1' || process.env.MOCK === 'true';
const INGEST_TOKEN = process.env.INGEST_TOKEN || '';
const INGEST_MAX_BYTES = 65536;

const inputBus = new EventEmitter();
const inputState = Object.create(null);

function publish(name, value) {
  if (typeof name !== 'string' || !name) return;
  const prev = inputState[name];
  if (prev && prev.value === value) return;
  const entry = { name, value, ts: Date.now() };
  inputState[name] = entry;
  inputBus.emit('change', entry);
}

if (MOCK) {
  require('./inputs/mock')({ publish });
  console.log('mock source: enabled');
}

// Daisy song-position tail (PLAN_USB_COMPOSITE Phase D). Enable with DAISY=1
// (defaults to /dev/ttyACM0) or by setting DAISY_SERIAL to the port path.
const DAISY = process.env.DAISY === '1' || process.env.DAISY === 'true' || !!process.env.DAISY_SERIAL;
if (DAISY) {
  require('./inputs/daisy-position')({ publish });
  console.log('daisy-position source: enabled');
}

const sseClients = new Set();
inputBus.on('change', (entry) => {
  const payload = `event: change\ndata: ${JSON.stringify(entry)}\n\n`;
  for (const res of sseClients) {
    try { res.write(payload); } catch { /* client gone */ }
  }
});

function handleSSE(req, res) {
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache, no-transform',
    'Connection': 'keep-alive',
    'X-Accel-Buffering': 'no',
  });
  for (const entry of Object.values(inputState)) {
    res.write(`event: change\ndata: ${JSON.stringify(entry)}\n\n`);
  }
  res.write(`event: ready\ndata: {}\n\n`);
  sseClients.add(res);
  const heartbeat = setInterval(() => {
    try { res.write(':keepalive\n\n'); } catch { /* */ }
  }, 15000);
  req.on('close', () => {
    clearInterval(heartbeat);
    sseClients.delete(res);
  });
}

// POST /ingest — accepts publications from the Python sensor sidecar.
// Restricted to loopback connections; optionally requires INGEST_TOKEN.
// Body: either a single {name, value} or an array of those.
function isLoopback(req) {
  const a = req.socket.remoteAddress || '';
  return a === '127.0.0.1'
      || a === '::1'
      || a === '::ffff:127.0.0.1'
      || a.startsWith('127.');
}

function handleIngest(req, res) {
  if (!isLoopback(req)) {
    res.writeHead(403); res.end('localhost only'); return;
  }
  if (INGEST_TOKEN && req.headers['x-ingest-token'] !== INGEST_TOKEN) {
    res.writeHead(401); res.end('bad token'); return;
  }
  let total = 0;
  const chunks = [];
  req.on('data', (chunk) => {
    total += chunk.length;
    if (total > INGEST_MAX_BYTES) {
      res.writeHead(413); res.end('payload too large');
      req.destroy();
      return;
    }
    chunks.push(chunk);
  });
  req.on('end', () => {
    if (res.writableEnded) return;
    let parsed;
    try { parsed = JSON.parse(Buffer.concat(chunks).toString('utf8')); }
    catch { res.writeHead(400); res.end('bad json'); return; }
    const items = Array.isArray(parsed) ? parsed : [parsed];
    let accepted = 0;
    for (const item of items) {
      if (item && typeof item === 'object' && typeof item.name === 'string') {
        publish(item.name, item.value);
        accepted++;
      }
    }
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ accepted }));
  });
}

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js':   'application/javascript; charset=utf-8',
  '.css':  'text/css; charset=utf-8',
  '.svg':  'image/svg+xml',
  '.png':  'image/png',
  '.jpg':  'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.json': 'application/json; charset=utf-8',
  '.mp3':  'audio/mpeg',
  '.ico':  'image/x-icon',
  '.txt':  'text/plain; charset=utf-8',
};

function safeJoin(root, urlPath) {
  const clean = urlPath.split('?')[0].split('#')[0];
  let decoded;
  try { decoded = decodeURIComponent(clean); }
  catch { return null; }
  const resolved = path.resolve(root, '.' + decoded);
  if (resolved !== root && !resolved.startsWith(root + path.sep)) return null;
  return resolved;
}

function serveStatic(req, res) {
  let urlPath = (req.url || '/').split('?')[0].split('#')[0];
  if (urlPath === '/' || urlPath === '') urlPath = '/index.html';
  const filePath = safeJoin(STATIC_ROOT, urlPath);
  if (!filePath) { res.writeHead(403); res.end('forbidden'); return; }
  fs.stat(filePath, (err, st) => {
    if (err || !st.isFile()) { res.writeHead(404); res.end('not found'); return; }
    const ext = path.extname(filePath).toLowerCase();
    const ctype = MIME[ext] || 'application/octet-stream';
    const range = req.headers.range;
    // Range support — iOS Safari requires it for <audio>.
    if (range) {
      const m = /^bytes=(\d*)-(\d*)$/.exec(range);
      if (m) {
        let start = m[1] === '' ? Math.max(0, st.size - parseInt(m[2], 10)) : parseInt(m[1], 10);
        let end = m[2] === '' ? st.size - 1 : Math.min(parseInt(m[2], 10), st.size - 1);
        if (Number.isFinite(start) && Number.isFinite(end) && start >= 0 && start <= end && end < st.size) {
          res.writeHead(206, {
            'Content-Type': ctype,
            'Content-Length': end - start + 1,
            'Content-Range': `bytes ${start}-${end}/${st.size}`,
            'Accept-Ranges': 'bytes',
          });
          fs.createReadStream(filePath, { start, end }).pipe(res);
          return;
        }
        res.writeHead(416, { 'Content-Range': `bytes */${st.size}` });
        res.end();
        return;
      }
    }
    res.writeHead(200, {
      'Content-Type': ctype,
      'Content-Length': st.size,
      'Accept-Ranges': 'bytes',
    });
    if (req.method === 'HEAD') { res.end(); return; }
    fs.createReadStream(filePath).pipe(res);
  });
}

const server = http.createServer((req, res) => {
  if (req.url === '/events' && req.method === 'GET') return handleSSE(req, res);
  if (req.url === '/ingest' && req.method === 'POST') return handleIngest(req, res);
  if (req.method !== 'GET' && req.method !== 'HEAD') {
    res.writeHead(405); res.end('method not allowed'); return;
  }
  serveStatic(req, res);
});

server.listen(PORT, HOST, () => {
  const hostShown = HOST === '0.0.0.0' ? 'localhost' : HOST;
  console.log(`ambient_viz server listening on http://${hostShown}:${PORT}`);
  console.log(`static root: ${STATIC_ROOT}`);
  console.log(`ingest token: ${INGEST_TOKEN ? 'required' : 'disabled (localhost-only)'}`);
});
