'use strict';

const C = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  underline: '\x1b[4m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  magenta: '\x1b[35m',
  cyan: '\x1b[36m',
  gray: '\x1b[90m',
  brand: '\x1b[38;2;86;156;214m',
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ts = () => new Date().toTimeString().slice(0, 8);
const rand = (n) => Math.floor(Math.random() * n);
const pad = (s, n) => s + ' '.repeat(Math.max(0, n - Array.from(s).length));

function table(rows) {
  const widths = [];
  for (const row of rows) {
    row.forEach((cell, i) => {
      const w = Array.from(cell).length;
      if (w > (widths[i] || 0)) widths[i] = w;
    });
  }
  const line = (l, m, r) =>
    l + widths.map((w) => '─'.repeat(w + 2)).join(m) + r;
  const row = (cells) =>
    '│ ' +
    cells
      .map((c, i) => pad(c, widths[i]))
      .join(' │ ') +
    ' │';
  console.log(line('╭', '┬', '╮'));
  console.log(row(rows[0]));
  console.log(line('├', '┼', '┤'));
  for (const cells of rows.slice(1)) console.log(row(cells));
  console.log(line('╰', '┴', '╯'));
}

function log(level, color, icon, msg) {
  console.log(
    `${C.gray}${ts()}${C.reset} ${C.bold}${color}${level.padEnd(5)}${C.reset} ${icon} ${msg}`
  );
}

const info = (icon, msg) => log('INFO', C.green, icon, msg);
const warn = (icon, msg) => log('WARN', C.yellow, icon, msg);
const error = (icon, msg) => log('ERROR', C.red, icon, msg);
const debug = (icon, msg) => log('DEBUG', C.cyan, icon, msg);

const METHODS = ['GET', 'GET', 'GET', 'POST', 'PUT', 'DELETE'];
const ROUTES = [
  '/api/users',
  '/api/users/:id',
  '/api/orders',
  '/api/session',
  '/api/health',
  '/static/app.js',
];

process.emitWarning('url.parse() is deprecated', {
  type: 'DeprecationWarning',
  code: 'DEP0170',
  detail: 'Use new URL() instead',
});

async function main() {
  console.log(
    `${C.brand}${C.bold}🚀 demo-api${C.reset} ${C.dim}v1.4.2 · ${process.version} · pid ${process.pid}${C.reset}`
  );
  console.log(
    `${C.dim}👩‍💻 team-core presence: alice · bob · 👋🏽 sam just connected${C.reset}`
  );

  table([
    ['Setting', 'Value', 'Status'],
    ['port', '3000', '✅'],
    ['env', 'development', '⚙️'],
    ['database', 'postgres://localhost:5432/demo', '🔌'],
    ['cache', 'redis://localhost:6379', '⚠️'],
    ['workers', '4', '✅'],
    ['telemetry', 'disabled', '🚫'],
  ]);

  await sleep(400);
  info('🔌', 'Connecting to postgres://localhost:5432/demo …');
  await sleep(600);
  warn('⏳', 'Database handshake took 1.4s — consider a connection pool');
  await sleep(300);
  info('✅', 'Database ready, 12 migrations applied');

  info('🌏', 'Edge regions online: 🇩🇪 fra-1 · 🇺🇸 iad-1 · 🇯🇵 tyo-1');
  await sleep(400);
  info('⚡', `Ready in ⏱️ 2.1s — listening on http://localhost:3000`);

  let served = 0;
  let crashed = false;
  for (;;) {
    await sleep(700 + rand(700));
    served += 1;
    if (served === 12 && !crashed) {
      crashed = true;
      error(
        '💥',
        'Uncaught TypeError: Cannot read properties of undefined (reading \'id\')'
      );
      console.log(
        `${C.red}    at authenticate (src/auth/session.js:42:19)${C.reset}`
      );
      console.log(
        `${C.red}    at async handler (src/routes/user.js:87:5)${C.reset}`
      );
      console.log(
        `${C.red}    at async Layer.handle (node_modules/express/lib/router/layer.js:152:17)${C.reset}`
      );
      await sleep(500);
      info('🩹', 'Session middleware reloaded, worker recovered');
      continue;
    }
    const roll = Math.random();
    const method = METHODS[rand(METHODS.length)];
    const path = ROUTES[rand(ROUTES.length)];
    const ms = 1 + rand(420);
    const time = `${ms}ms`;
    if (roll > 0.94) {
      warn('🟡', `${method} ${path} → 429 too many requests ${time}`);
      debug('🔍', 'rate limiter: bucket=ip:10.0.0.7 window reset in 12s');
    } else if (roll > 0.9) {
      warn('🟡', `${method} ${path} → 404 not found ${time}`);
    } else if (roll > 0.87) {
      debug('🔍', `cache hit ratio 0.94 · pool idle 3/8 · gc pause 4.2ms`);
    } else {
      info('🟢', `${method} ${path} → ${roll > 0.75 ? 201 : 200} ${time}`);
    }
  }
}

process.on('SIGINT', () => {
  info('👋', 'Shutting down — connections drained ✓');
  process.exit(0);
});

main();
