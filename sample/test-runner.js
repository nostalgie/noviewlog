'use strict';

const C = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  red: '\x1b[31m',
  cyan: '\x1b[36m',
  gray: '\x1b[90m',
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
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
    '│ ' + cells.map((c, i) => pad(c, widths[i])).join(' │ ') + ' │';
  console.log(line('╭', '┬', '╮'));
  console.log(row(rows[0]));
  console.log(line('├', '┼', '┤'));
  for (const cells of rows.slice(1)) console.log(row(cells));
  console.log(line('╰', '┴', '╯'));
}

const pass = (suite, name, ms) =>
  console.log(`${C.green}✔️ ${C.reset} ${C.gray}${suite} ›${C.reset} ${name} ${C.dim}${ms}ms${C.reset}`);
const fail = (suite, name) =>
  console.log(`${C.red}✖️ ${C.reset} ${C.gray}${suite} ›${C.reset} ${C.bold}${name}${C.reset}`);
const skipTest = (suite, name) =>
  console.log(`${C.yellow}⏭️ ${C.reset} ${C.gray}${suite} ›${C.reset} ${name} ${C.dim}skipped${C.reset}`);

function failureDetail(t) {
  console.log(`${C.red}${C.bold}🛑 FAIL ${t.title}${C.reset}`);
  console.log(`${C.red}AssertionError:${C.reset}`);
  console.log(`${C.red}  - expected: ${t.expected}${C.reset}`);
  console.log(`${C.green}  + received: ${t.received}${C.reset}`);
  for (const line of t.stack) console.log(`${C.gray}    at ${line}${C.reset}`);
  console.log('');
}

async function suite(name, tests) {
  for (const t of tests) {
    await sleep(120 + Math.floor(Math.random() * 150));
    if (t.outcome === 'pass') pass(name, t.name, t.ms);
    else if (t.outcome === 'skip') skipTest(name, t.name);
    else {
      fail(name, t.name);
      if (t.detail) failureDetail(t.detail);
    }
  }
}

async function main() {
  console.log(
    `${C.bold}🧪 unit suite${C.reset} ${C.dim}21 tests · node ${process.version}${C.reset}`
  );
  console.log(`${C.gray}⏳ spinning up test database (ephemeral)…${C.reset}`);
  await sleep(500);
  console.log(`${C.green}✅${C.reset} test database ready, seed loaded`);

  await suite('core/parser', [
    { name: 'parses ansi colors', outcome: 'pass', ms: 12 },
    { name: 'parses truecolor', outcome: 'pass', ms: 9 },
    { name: 'splits records on newlines', outcome: 'pass', ms: 7 },
    { name: 'keeps crlf intact', outcome: 'pass', ms: 6 },
    { name: 'detects severity words', outcome: 'pass', ms: 8 },
  ]);

  await suite('core/filters', [
    { name: 'exclude wins over include', outcome: 'pass', ms: 11 },
    { name: 'literal match', outcome: 'pass', ms: 5 },
    { name: 'regex match', outcome: 'pass', ms: 6 },
    { name: 'severity mode errors', outcome: 'pass', ms: 10 },
  ]);
  console.log(`${C.yellow}⚠️  WARNING deprecation: expect().equal() — use expect().toBe()${C.reset}`);

  await suite('core/viewport', [
    { name: 'wraps long lines', outcome: 'pass', ms: 14 },
    { name: 'vs16 adds no width', outcome: 'pass', ms: 8 },
    {
      name: 'emoji advance counts one cell',
      outcome: 'fail',
      detail: {
        title: 'core/viewport › emoji advance counts one cell',
        expected: '1',
        received: '2',
        stack: [
          'Context.<anonymous> (test/viewport.test.js:88:29)',
          'processImmediate (node:internal/timers:478:21)',
        ],
      },
    },
    { name: 'renders 256-color', outcome: 'pass', ms: 9 },
  ]);

  await suite('core/pty', [
    { name: 'spawns echo', outcome: 'pass', ms: 45 },
    { name: 'resizes grid', outcome: 'pass', ms: 22 },
    { name: 'drains flood without loss', outcome: 'pass', ms: 130 },
    { name: 'conpty passthrough', outcome: 'skip' },
  ]);

  await suite('core/buffer', [
    { name: 'trims scrollback', outcome: 'pass', ms: 16 },
    { name: 'preserves selection ids', outcome: 'pass', ms: 11 },
    { name: 'appends live lines', outcome: 'pass', ms: 9 },
    {
      name: 'reflows after resize',
      outcome: 'fail',
      detail: {
        title: 'core/buffer › reflows after resize',
        expected: '80',
        received: '79',
        stack: [
          'Context.<anonymous> (test/buffer.test.js:142:17)',
          'async Promise.all (index 0)',
          'processTicksAndRejections (node:internal/process/task_queues:95:5)',
        ],
      },
    },
  ]);

  await sleep(300);
  console.log(
    `${C.red}${C.bold}UnhandledPromiseRejection:${C.reset} This error originated by throwing inside of an async function without a catch block (test/buffer.test.js:201)`
  );
  console.log(`${C.gray}    (Use node --trace-warnings … to show where the warning was created)${C.reset}`);

  table([
    ['File', 'Passed ✔️', 'Failed ✖️', 'Skipped ⏭️', 'Time'],
    ['core/parser.test.js', '5', '0', '0', '0.4s'],
    ['core/filters.test.js', '4', '0', '0', '0.3s'],
    ['core/viewport.test.js', '3', '1', '0', '0.5s'],
    ['core/pty.test.js', '3', '0', '1', '1.9s'],
    ['core/buffer.test.js', '3', '1', '0', '0.4s'],
    ['total', '18', '2', '1', '3.5s'],
  ]);

  console.log(`\n1️⃣ rerun failed tests only: npm test -- --only-failures`);
  console.log(`2️⃣ coverage report: coverage/index.html`);
  console.log(`${C.red}${C.bold}💥 2 of 21 tests failed — exiting with code 1${C.reset}`);
  process.exit(1);
}

main();
