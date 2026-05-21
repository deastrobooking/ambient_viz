const http = require('http');
const fs = require('fs');
const path = require('path');
const { EventEmitter } = require('events');

const STATIC_ROOT = path.resolve(__dirname, '..', '..', 'static');
const PORT = parseInt(process.env.PORT || '8080', 10);
const HOST = process.env.HOST || '0.0.0.0';
const SOURCES = (process.env.AMBIENT_INPUTS || 'mock')
  .split(',').map(s => s.trim()).filter(s => s && s !== 'none');

const inputBus = new EventEmitter();
const inputState = Object.create(null);

function publish(name, value) {
  const prev = inputState[name];
  if (prev && prev.value === value) return;
  const entry = { name, value, ts: Date.now() };
  inputState[name] = entry;
  inputBus.emit('change', entry);
}

for (const src of SOURCES) {
  try {
    require(`./inputs/${src}`)({ publish });
    console.log(`input source: ${src}`);
  } catch (e) {
    console.error(`failed to load input source '${src}': ${e.message}`);
  }
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
  let urlPath = req.url || '/';
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
  if (req.url === '/events') return handleSSE(req, res);
  if (req.method !== 'GET' && req.method !== 'HEAD') {
    res.writeHead(405); res.end('method not allowed'); return;
  }
  serveStatic(req, res);
});

server.listen(PORT, HOST, () => {
  const hostShown = HOST === '0.0.0.0' ? 'localhost' : HOST;
  console.log(`ambient_viz server listening on http://${hostShown}:${PORT}`);
  console.log(`static root: ${STATIC_ROOT}`);
  console.log(`inputs: ${SOURCES.length ? SOURCES.join(', ') : 'none'}`);
});
