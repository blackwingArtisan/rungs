// Local demo console on :8788. Serves the dashboard and streams a live demo
// run (`GET /api/run`, server-sent events) so you can watch the board react.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { HERE } from './near.mjs';

const WEB = path.join(HERE, '..', 'web');
const PORT = Number(process.env.PORT || 8788);
const TYPES = { '.html': 'text/html; charset=utf-8', '.json': 'application/json', '.js': 'text/javascript' };
let running = null;

http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  if (url.pathname === '/api/run') {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    const send = (line) => res.write(`data: ${JSON.stringify(line)}\n\n`);
    if (running) return send('a demo run is already in progress'), res.end();
    running = spawn(process.execPath, ['demo.mjs'], { cwd: HERE, env: { ...process.env, PACE: url.searchParams.get('pace') || '1200' } });
    let buf = '';
    const onData = (chunk) => {
      buf += chunk;
      const lines = buf.split('\n');
      buf = lines.pop();
      for (const l of lines) if (l.trim() && !/^\s+at /.test(l) && !/ProviderError|cause|^\}/.test(l)) send(l);
    };
    running.stdout.on('data', onData);
    running.stderr.on('data', onData);
    running.on('close', () => { running = null; send('__done__'); res.end(); });
    return;
  }
  const file = path.join(WEB, url.pathname === '/' ? 'index.html' : path.normalize(url.pathname));
  if (!file.startsWith(WEB) || !fs.existsSync(file)) return res.writeHead(404).end('not found');
  res.writeHead(200, { 'content-type': TYPES[path.extname(file)] || 'application/octet-stream' });
  fs.createReadStream(file).pipe(res);
}).listen(PORT, () => console.log(`Rungs console on http://localhost:${PORT}`));
