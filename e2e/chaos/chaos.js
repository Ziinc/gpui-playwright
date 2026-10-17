// Chaos test for gpui-playwright: random actions, protocol abuse and
// connection churn against a running app. Reports every anomaly it sees.
//
//   GPUI_PLAYWRIGHT=9333 cargo run -p todo &
//   node chaos/chaos.js [--seed N] [--steps N] [--endpoint ws://127.0.0.1:9333/]
//
// Exit code is 1 when the server died or an invariant broke.

const net = require('net');
const { chromium } = require('@playwright/test');

const args = Object.fromEntries(
  process.argv.slice(2).reduce((pairs, arg, i, all) => {
    if (arg.startsWith('--')) pairs.push([arg.slice(2), all[i + 1]]);
    return pairs;
  }, []),
);
const endpoint = args.endpoint ?? 'ws://127.0.0.1:9333/';
const { hostname, port } = new URL(endpoint);
const seed = Number(args.seed ?? Date.now() % 100000);
const steps = Number(args.steps ?? 300);
const HANG_MS = 20_000;

// mulberry32
let state = seed >>> 0;
const random = () => {
  state = (state + 0x6d2b79f5) >>> 0;
  let t = state;
  t = Math.imul(t ^ (t >>> 15), t | 1);
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
};
const pick = (list) => list[Math.floor(random() * list.length)];
const int = (n) => Math.floor(random() * n);

const findings = [];
const finding = (kind, detail) => {
  const key = `${kind}: ${detail}`.slice(0, 300);
  if (!findings.some((f) => f.key === key)) {
    findings.push({ key, kind, detail });
    console.log(`  ! ${key}`);
  }
};

// Errors a well-behaved server may legitimately answer with.
const EXPECTED_ERRORS = [
  /Timeout \d+ms exceeded/,
  /strict mode violation/,
  /not supported by GPUI apps/,
  /cannot use selector/,
  /Not a checkbox or radio button/,
  /Element is not an <input>/,
  /Unknown key/,
  /No window to open as a page/,
  /Target page, context or browser has been closed/,
  /did not change its state/,
  /Taking a screenshot failed/,
  /Node is not an <input>/,
  /no route for/,
  // The script chaos throws these on purpose.
  /Maximum call stack size exceeded/,
  /Cannot read property 'x' of null|cannot read property 'x' of null/i,
  /Error: nope/,
];

async function alive() {
  try {
    const res = await fetch(`http://${hostname}:${port}/`, { signal: AbortSignal.timeout(3000) });
    return res.status === 200;
  } catch {
    return false;
  }
}

function rawExchange(payload, { timeout = 4000 } = {}) {
  return new Promise((resolve) => {
    const socket = net.connect(Number(port), hostname);
    let data = Buffer.alloc(0);
    socket.on('data', (chunk) => (data = Buffer.concat([data, chunk])));
    socket.on('error', () => {});
    // Frames go out after the handshake, as RFC 6455 requires of clients.
    const [head, ...frames] = Array.isArray(payload) ? payload : [payload];
    socket.on('connect', () => {
      socket.write(head);
      setTimeout(() => frames.forEach((frame) => socket.write(frame)), 300);
    });
    setTimeout(() => {
      socket.destroy();
      resolve(data.toString('latin1'));
    }, timeout);
  });
}

function wsFrame(text, { opcode = 1, mask = true } = {}) {
  const payload = Buffer.from(text);
  const header = [0x80 | opcode];
  const maskBit = mask ? 0x80 : 0;
  if (payload.length < 126) header.push(maskBit | payload.length);
  else if (payload.length < 65536) header.push(maskBit | 126, payload.length >> 8, payload.length & 255);
  else {
    header.push(maskBit | 127, 0, 0, 0, 0);
    header.push((payload.length >>> 24) & 255, (payload.length >>> 16) & 255, (payload.length >>> 8) & 255, payload.length & 255);
  }
  const key = Buffer.from([1, 2, 3, 4]);
  const body = mask ? Buffer.from(payload.map((b, i) => b ^ key[i % 4])) : payload;
  return Buffer.concat([Buffer.from(header), mask ? key : Buffer.alloc(0), body]);
}

const upgrade = (agent = 'Playwright/1.64.1 (chaos)') =>
  `GET / HTTP/1.1\r\nHost: ${hostname}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
  `Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nUser-Agent: ${agent}\r\n\r\n`;

async function protocolAbuse() {
  console.log('protocol abuse');
  const cases = [
    ['garbage request', Buffer.from('\x00\x01\x02 not http\r\n\r\n')],
    ['unterminated head', Buffer.from('GET / HTTP/1.1\r\nHost: x\r\n')],
    ['huge head', Buffer.from('GET / HTTP/1.1\r\n' + 'X-Pad: ' + 'a'.repeat(20000) + '\r\n\r\n')],
    ['old client', Buffer.from(upgrade('Playwright/1.40.0'))],
    ['no user agent', Buffer.from(upgrade('').replace('User-Agent: \r\n', ''))],
    ['invalid json', [Buffer.from(upgrade()), wsFrame('{not json')]],
    ['no method', [Buffer.from(upgrade()), wsFrame('{"id":1,"guid":"","params":{}}')]],
    ['unknown guid', [Buffer.from(upgrade()), wsFrame('{"id":1,"guid":"page@nope","method":"click","params":{}}')]],
    ['binary frame', [Buffer.from(upgrade()), wsFrame('\x00\xff', { opcode: 2 })]],
    ['unmasked frame', [Buffer.from(upgrade()), wsFrame('{"id":1,"guid":"","method":"initialize","params":{}}', { mask: false })]],
    ['double initialize', [
      Buffer.from(upgrade()),
      wsFrame('{"id":1,"guid":"","method":"initialize","params":{"sdkLanguage":"javascript"}}'),
      wsFrame('{"id":2,"guid":"","method":"initialize","params":{"sdkLanguage":"javascript"}}'),
    ]],
    ['5MB message', [Buffer.from(upgrade()), wsFrame(JSON.stringify({ id: 1, guid: '', method: 'x', params: { pad: 'a'.repeat(5_000_000) } }))]],
    ['id types', [
      Buffer.from(upgrade()),
      wsFrame('{"id":null,"guid":"","method":"initialize","params":{}}'),
      wsFrame('{"id":{"x":1},"guid":"","method":"initialize","params":{}}'),
    ]],
  ];
  for (const [name, payload] of cases) {
    const response = await rawExchange(payload);
    if (name === 'old client' && !response.includes(' 428 ')) finding('protocol', `old client not rejected with 428: ${response.slice(0, 60)}`);
    if (name === 'double initialize') {
      const creates = (response.match(/"type":"Playwright"/g) ?? []).length;
      if (creates > 1) finding('protocol', 'a second initialize creates a second Playwright root on the same connection');
    }
    if (name === 'id types' && !response.includes('"id":{"x":1}')) finding('protocol', 'non-numeric call id not echoed back');
    if (!(await alive())) {
      finding('crash', `server died after "${name}"`);
      return false;
    }
  }
  return true;
}

const LOCATORS = (page) => [
  () => page.getByRole('button').nth(int(8) - 1),
  () => page.getByRole('checkbox').nth(int(6) - 1),
  () => page.getByRole('textbox'),
  () => page.getByRole(pick(['button', 'checkbox', 'textbox', 'link', 'dialog', 'listitem'])),
  () => page.getByText(pick(['Add', 'Delete', 'left', 'Todos', '', ' ', 'Read the docs'])),
  () => page.getByLabel(pick(['What needs to be done?', 'nope'])),
  () => page.getByTestId(pick(['remaining', 'add', 'new-todo', 'done-1', 'row-2'])),
  () => page.locator(pick(['#add', '[aria-label="Add"]', 'div > span', '//button', 'text=Add', 'internal:role=button >> nth=-1', '>>', '[', '"unterminated', 'internal:has="x"'])),
  () => page.getByRole('checkbox').filter({ hasText: pick(['Read', 'x', '']) }),
  () => page.getByRole('button', { name: pick(['Add', 'Delete', 'Dark', 'Light', /^D/]) }).first(),
];

const TEXTS = [
  '', ' ', 'milk', '🙂🙃 emoji', 'עברית RTL', 'é combining', 'a'.repeat(2000), 'tab\tinside', 'new\nline',
  '"quotes" and \\backslashes', '<b>html</b>', '\u0000nul', 'ß→SS', '中文输入',
];
const KEYS = ['Enter', 'Tab', 'Backspace', 'Delete', 'Escape', 'ArrowLeft', 'ArrowRight', 'Home', 'End', 'ControlOrMeta+a', 'Shift+ArrowLeft', 'a', 'Z', 'F5', 'Hyper', 'Control+Shift+Alt+x', '+', ' '];

async function randomAction(page) {
  const locator = () => pick(LOCATORS(page))();
  const timeout = pick([1, 50, 300]);
  const actions = [
    async () => locator().click({ timeout }),
    async () => locator().dblclick({ timeout }),
    async () => locator().hover({ timeout }),
    async () => locator().fill(pick(TEXTS), { timeout }),
    async () => locator().press(pick(KEYS), { timeout }),
    async () => locator().check({ timeout }),
    async () => locator().uncheck({ timeout }),
    async () => locator().textContent({ timeout }),
    async () => locator().inputValue({ timeout }),
    async () => locator().isChecked({ timeout }),
    async () => locator().count(),
    async () => locator().boundingBox({ timeout }),
    async () => locator().ariaSnapshot({ timeout }),
    async () => locator().allTextContents(),
    // A key per character costs the app a full input update each; long texts
    // go through fill and insertText instead.
    async () => page.keyboard.type(pick(TEXTS).slice(0, 200)),
    async () => page.keyboard.insertText(pick(TEXTS)),
    async () => page.keyboard.press(pick(KEYS)),
    async () => page.keyboard.down(pick(['Shift', 'Control', 'Alt'])),
    async () => page.keyboard.up(pick(['Shift', 'Control', 'Alt'])),
    async () => page.mouse.click(int(700) - 50, int(600) - 50, { clickCount: int(3) + 1 }),
    async () => page.mouse.move(int(600), int(500), { steps: int(5) + 1 }),
    async () => page.mouse.down(),
    async () => page.mouse.up(),
    async () => page.mouse.wheel(int(400) - 200, int(400) - 200),
    async () => page.emulateMedia({ colorScheme: pick(['dark', 'light', 'no-preference']) }),
    async () => page.setViewportSize({ width: pick([200, 560, 1200]), height: pick([150, 480, 900]) }),
    async () => page.title(),
    async () => page.screenshot({ timeout }),
    async () => locator().screenshot({ timeout }),
    async () => page.evaluate('1 + 1'),
    async () => page.evaluate(pick(SCRIPTS)),
    async () => locator().evaluate((e, a) => [e.textContent, e.getAttribute(a)], pick(['title', 'alt', 'x']), { timeout }),
    async () => page.waitForFunction(() => document.querySelectorAll('internal:role=checkbox').length >= 0, null, { timeout }),
    async () => page.goto('https://example.com'),
    async () => page.waitForTimeout(int(30)),
  ];
  const action = pick(actions);
  const label = action.toString().replace('async () => ', '');
  // Every call has a timeout or none of its own to wait on; one stuck this
  // long is a hang.
  let timer;
  const watchdog = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`HANG: ${label} did not settle in ${HANG_MS}ms`)), HANG_MS);
  });
  try {
    return await Promise.race([action(), watchdog]);
  } finally {
    clearTimeout(timer);
  }
}

// Scripts that throw, allocate, recurse or return odd values.
const SCRIPTS = [
  'todos()',
  'null.x',
  'new Array(1e5).fill("x").join("")',
  '(function f() { return f(); })()',
  '({ a: [1, { b: NaN }], d: new Date(0), r: /x/g, n: -0, big: 10n })',
  'Promise.reject(new Error("nope"))',
  'document.body.textContent',
  'todos(1, 2, 3)',
];

async function checkInvariants(page, label) {
  const boxes = await page.getByRole('checkbox').count();
  const deletes = await page.getByRole('button', { name: 'Delete' }).count();
  if (boxes !== deletes) finding('invariant', `${label}: ${boxes} checkboxes but ${deletes} delete buttons`);
  let checked = 0;
  for (let i = 0; i < boxes; i++) if (await page.getByRole('checkbox').nth(i).isChecked()) checked++;
  const remaining = await page.getByTestId('remaining').textContent({ timeout: 2000 });
  const expected = `${boxes - checked} of ${boxes} left`;
  if (remaining !== expected) finding('invariant', `${label}: counter says "${remaining}", elements say "${expected}"`);
}

async function randomWalk() {
  console.log(`random walk: seed ${seed}, ${steps} steps`);
  const browser = await chromium.connect(endpoint);
  let context = await browser.newContext({ viewport: { width: 560, height: 480 } });
  let page = await context.newPage();
  const errors = new Map();
  for (let step = 0; step < steps; step++) {
    try {
      if (random() < 0.15) {
        // Several calls in flight at once.
        await Promise.allSettled(Array.from({ length: int(5) + 2 }, () => randomAction(page)));
      } else if (random() < 0.03) {
        await context.close();
        context = await browser.newContext({ viewport: { width: 560, height: 480 } });
        page = await context.newPage();
      } else {
        await randomAction(page);
      }
    } catch (error) {
      const message = String(error.message).split('\n')[0];
      if (!EXPECTED_ERRORS.some((re) => re.test(error.message))) {
        finding('unexpected error', message);
      }
      errors.set(message.slice(0, 80), (errors.get(message.slice(0, 80)) ?? 0) + 1);
    }
    if (step % 25 === 24) {
      if (!(await alive())) {
        finding('crash', `server died during step ${step} (seed ${seed})`);
        return false;
      }
      await page.keyboard.up('Shift').catch(() => {});
      await page.mouse.up().catch(() => {});
      await checkInvariants(page, `step ${step}`).catch((e) => finding('invariant check failed', String(e.message).split('\n')[0]));
    }
  }
  await browser.close();
  return true;
}

async function connectionChurn() {
  console.log('connection churn');
  // Two clients at once: the second newPage has no free window.
  const [a, b] = await Promise.all([chromium.connect(endpoint), chromium.connect(endpoint)]);
  const pa = await (await a.newContext()).newPage();
  const second = await (await b.newContext()).newPage().catch((e) => e);
  if (!(second instanceof Error)) finding('pages', 'two clients were given the same window');
  // Disconnect with calls in flight.
  const inflight = pa.getByRole('button', { name: 'never there' }).click({ timeout: 5000 }).catch(() => {});
  await new Promise((r) => setTimeout(r, 50));
  await a.close();
  await inflight;
  await b.close();
  // Rapid connect/disconnect.
  for (let i = 0; i < 20; i++) {
    const browser = await chromium.connect(endpoint);
    if (random() < 0.5) await (await browser.newContext()).newPage().catch(() => {});
    await browser.close();
  }
  if (!(await alive())) {
    finding('crash', 'server died during connection churn');
    return false;
  }
  // After all that, the window must be adoptable again.
  const browser = await chromium.connect(endpoint);
  const page = await (await browser.newContext()).newPage().catch((e) => e);
  if (page instanceof Error) finding('pages', `window not released after clients left: ${page.message.split('\n')[0]}`);
  else await checkInvariants(page, 'after churn');
  await browser.close();
  return true;
}

(async () => {
  const ok = (await protocolAbuse()) && (await connectionChurn()) && (await randomWalk());
  console.log(`\n${findings.length} finding(s), seed ${seed}`);
  for (const f of findings) console.log(`- [${f.kind}] ${f.detail}`);
  process.exit(ok && !findings.some((f) => f.kind === 'crash' || f.kind === 'invariant') ? 0 : 1);
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
