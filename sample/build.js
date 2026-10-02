'use strict';

const C = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  cyan: '\x1b[36m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  red: '\x1b[31m',
  gray: '\x1b[90m',
  link: '\x1b]8;;https://example.com/releases/v2.3.0\x1b\\',
  linkEnd: '\x1b]8;;\x1b\\',
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

const ok = (msg) => console.log(`${C.green}✅${C.reset} ${msg}`);
const skip = (msg) => console.log(`${C.yellow}⏭️ ${C.reset} ${msg}`);
const warning = (msg) => console.log(`${C.yellow}⚠️  ${C.reset}${C.yellow}${msg}${C.reset}`);
const step = (n, msg) =>
  console.log(`\n${C.bold}${C.cyan}${n}️⃣  ${msg}${C.reset}`);
const bar = (pct) => {
  const full = 24;
  const fill = Math.round((pct / 100) * full);
  return (
    `${C.cyan}[${'█'.repeat(fill)}${C.gray}${'░'.repeat(full - fill)}]${C.reset} ${pct}%`
  );
};

async function main() {
  console.log(
    `${C.bold}🏗️  demo-app build pipeline${C.reset} ${C.dim}v2.3.0 · node ${process.version}${C.reset}`
  );

  step('1', 'install dependencies');
  await sleep(500);
  console.log(`${C.gray}⏳ resolving 214 packages…${C.reset}`);
  await sleep(400);
  console.log(`${C.green}➕${C.reset} added 214 packages, audited 215 packages in 4.1s`);
  warning('2 deprecated packages (request@2.88.2,har-validator@5.1.5)');

  step('2', 'lint');
  await sleep(400);
  warning('no-unused-vars: src/cache.ts:42:7');
  warning('no-unused-vars: src/auth.ts:88:14');
  warning('eqeqeq: src/util.ts:12:9');
  console.log(`${C.green}✔️  lint finished — 0 errors, 3 warnings${C.reset}`);

  step('3', 'build');
  for (const pct of [12, 34, 58, 79, 96, 100]) {
    console.log(`⏳ ${bar(pct)}`);
    await sleep(250);
  }
  console.log(`${C.green}✅${C.reset} 412 modules transformed`);
  table([
    ['File', 'Size', 'Status'],
    ['dist/index.js', '182 KiB', '✅'],
    ['dist/vendors.js', '1.2 MiB', '✅'],
    ['dist/styles.css', '42 KiB', '✅'],
    ['dist/logo.png', '18 KiB', '📦'],
  ]);
  console.log(`${C.dim}gzip: 96.4 KiB · built in 3.4s${C.reset}`);
  ok('bundle written to dist/');

  step('4', 'deploy');
  console.log(`🌏 targets: 🇩🇪 fra-1 · 🇺🇸 iad-1 · 🇯🇵 tyo-1`);
  await sleep(500);
  console.log(`${C.gray}⏳ uploading dist/ → s3://demo-artifacts/v2.3.0 …${C.reset}`);
  await sleep(600);
  ok('fra-1 healthy in 1.2s');
  ok('iad-1 healthy in 0.9s');
  ok('tyo-1 healthy in 1.1s');
  skip('cn-bei-2 not enabled for this release');

  table([
    ['Stage', 'Result', 'Time'],
    ['install', '✅ 214 pkgs', '4.1s'],
    ['lint', '⚠️ 3 warnings', '0.8s'],
    ['build', '✅ 412 modules', '3.4s'],
    ['deploy', '✅ 3 regions', '3.6s'],
  ]);
  console.log(
    `${C.gray}🔗${C.reset} release notes: ${C.link}${C.cyan}${C.underline}https://example.com/releases/v2.3.0${C.reset}${C.linkEnd}`
  );
  console.log(`${C.bold}🎉 done in 11.9s${C.reset}`);
  process.exit(0);
}

main();
