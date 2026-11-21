import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const origin = 'https://agentrouter.org';
const scripts = Object.fromEntries(await Promise.all(['prepare', 'oauth', 'observe'].map(async name => [
  name, (await readFile(new URL(`./${name}.js`, import.meta.url), 'utf8'))
    .replaceAll('__ORIGIN__', JSON.stringify(origin))
    .replaceAll('__USER_ID__', '"7"')
    .replaceAll('__STARTED_AT__', '100'),
])));

function browser({ user = null, location = origin, responses = {} } = {}) {
  const storage = new Map(user ? [['user', JSON.stringify(user)]] : []);
  const requests = [];
  const opened = [];
  const context = vm.createContext({
    location: { origin: location },
    localStorage: {
      getItem: key => storage.get(key) ?? null,
      setItem: (key, value) => storage.set(key, value),
      removeItem: key => storage.delete(key),
    },
    window: { open: (url, target) => { opened.push({ url, target }); return {}; } },
    URL, AbortController, setTimeout, clearTimeout,
    fetch: async (path, init) => {
      requests.push({ path, init });
      const reply = responses[path] ?? responses[path.split('?')[0]];
      if (!reply) throw new Error('unexpected request (may contain sensitive details)');
      return { status: reply.status ?? 200, ok: (reply.status ?? 200) < 400,
        json: async () => reply.body };
    },
  });
  return { storage, requests, opened,
    run: async name => JSON.parse(await vm.runInContext(scripts[name], context)) };
}

const self = { success: true, data: { id: 7, last_login_time: 101, quota: 50_000_000,
  access_token: 'must-stay-in-page', password: 'must-not-cross-cdp' } };

test('logout is accepted before clearing the old local result', async () => {
  const page = browser({ user: { id: 7, checked_in: true, last_login_time: 99 }, responses: {
    '/api/user/self': { body: self }, '/api/user/logout': { body: { success: true } },
  } });
  assert.deepEqual(await page.run('prepare'), { state: 'prepared', quota: 50_000_000 });
  assert.equal(page.storage.has('user'), false);
  assert.deepEqual(page.requests.map(r => r.path), ['/api/user/self', '/api/user/logout']);
  assert.equal(page.requests[0].init.headers['New-Api-User'], '7');
});

test('logout HTTP 200 with business failure cannot discard or reuse old state', async () => {
  const user = { id: 7, checked_in: true, last_login_time: 99 };
  const page = browser({ user, responses: {
    '/api/user/self': { body: self }, '/api/user/logout': { body: { success: false } },
  } });
  assert.deepEqual(await page.run('prepare'), { state: 'logout_failed' });
  assert.deepEqual(JSON.parse(page.storage.get('user')), user);
});

test('an expired session still performs a confirmed sign-out before starting', async () => {
  const page = browser({ responses: {
    '/api/user/self': { status: 401, body: { success: false } },
    '/api/user/logout': { body: { success: true } },
  } });
  assert.deepEqual(await page.run('prepare'), { state: 'prepared', quota: null });
});

test('self and logout 401 clear a stale result without requiring a live session', async () => {
  const page = browser({ user: { id: 7, checked_in: true, last_login_time: 99 }, responses: {
    '/api/user/self': { status: 401, body: { success: false } },
    '/api/user/logout': { status: 401, body: { success: false } },
  } });
  assert.deepEqual(await page.run('prepare'), { state: 'prepared', quota: null });
  assert.equal(page.storage.has('user'), false);
});

test('logout 401 after a verified live session is not assumed to be success', async () => {
  const page = browser({ user: { id: 7 }, responses: {
    '/api/user/self': { body: self },
    '/api/user/logout': { status: 401, body: { success: false } },
  } });
  assert.deepEqual(await page.run('prepare'), { state: 'logout_failed' });
  assert.equal(page.storage.has('user'), true);
});

test('a different cached or server identity is never signed out automatically', async () => {
  const cached = browser({ user: { id: 8 } });
  assert.deepEqual(await cached.run('prepare'), { state: 'identity_mismatch' });
  assert.equal(cached.requests.length, 0);
  const server = browser({ responses: {
    '/api/user/self': { body: { success: true, data: { id: 8 } } },
  } });
  assert.deepEqual(await server.run('prepare'), { state: 'identity_mismatch' });
  assert.equal(server.requests.length, 1);
});

test('403 is not assumed to be an expired session belonging to this account', async () => {
  const page = browser({ user: { id: 7 }, responses: {
    '/api/user/self': { status: 403, body: { success: false } },
  } });
  assert.deepEqual(await page.run('prepare'), { state: 'needs_user' });
  assert.equal(page.requests.length, 1);
  assert.equal(page.storage.has('user'), true);
});

test('OAuth uses current public client id and keeps its ticket in the page', async () => {
  const page = browser({ responses: {
    '/api/status': { body: { success: true, data: { github_oauth: true, github_client_id: 'public123' } } },
    '/api/oauth/state?mode=login': { body: { success: true, data: 'ephemeral-ticket' } },
  } });
  const result = await page.run('oauth');
  assert.deepEqual(result, { state: 'started' });
  assert.equal(JSON.stringify(result).includes('ephemeral-ticket'), false);
  const opened = new URL(page.opened[0].url);
  assert.equal(opened.origin, 'https://github.com');
  assert.equal(opened.searchParams.get('client_id'), 'public123');
  assert.equal(opened.searchParams.get('state'), 'ephemeral-ticket');
  assert.equal(page.storage.get('oauth_mode'), 'login');
  assert.equal(page.requests.some(r => r.path === '/api/user/checkin'), false);
});

test('new checked_in true and false are returned only with accepted self identity', async () => {
  for (const checked_in of [true, false]) {
    const page = browser({ user: { id: 7, checked_in, last_login_time: 101,
      password: 'never-return-this' }, responses: { '/api/user/self': { body: self } } });
    const result = await page.run('observe');
    assert.deepEqual(result, { state: 'signed_in', user_id: '7', last_login_time: 101,
      checked_in, quota: 50_000_000, log_quota: null, log_at: null });
    assert.equal(page.requests[0].init.credentials, 'same-origin');
    assert.equal(page.requests[0].init.headers['New-Api-User'], '7');
    assert.equal(JSON.stringify(result).includes('password'), false);
    assert.equal(JSON.stringify(result).includes('access_token'), false);
  }
});

test('a fresh panel log row is the measured reward; old or non-check-in rows are ignored', async () => {
  const page = browser({ user: { id: 7, checked_in: true, last_login_time: 101 },
    responses: {
      '/api/user/self': { body: self },
      '/api/log/self': { body: { success: true, data: [
        { created_at: 99, quota: 9_000_000, content: '每日签到' },
        { created_at: 101, quota: 12_500_000, content: '每日签到' },
        { created_at: 102, quota: 0, content: '签到' },
        { created_at: 103, quota: 8_000_000, content: '充值' },
      ] } },
    } });
  const result = await page.run('observe');
  assert.equal(result.checked_in, true);
  assert.equal(result.log_quota, 12_500_000);
  assert.equal(result.log_at, 101);
  assert.equal(page.requests.some(r => r.path.startsWith('/api/log/self')), true);
});

test('an already-completed login does not ask the log endpoint', async () => {
  const page = browser({ user: { id: 7, checked_in: false, last_login_time: 101 },
    responses: { '/api/user/self': { body: self } } });
  const result = await page.run('observe');
  assert.equal(result.checked_in, false);
  assert.deepEqual(page.requests.map(r => r.path), ['/api/user/self']);
});

test('absent or old login results are pending without a server request', async () => {
  for (const user of [null, { id: 7, checked_in: true, last_login_time: 99 }]) {
    const page = browser({ user });
    assert.deepEqual(await page.run('observe'), { state: 'pending' });
    assert.equal(page.requests.length, 0);
  }
});

test('missing and mistyped login metadata are unconfirmed, not another human login', async () => {
  for (const user of [
    { id: 7, last_login_time: 101 }, { id: 7, checked_in: 'true', last_login_time: 101 },
    { id: 7, checked_in: true, last_login_time: '101' }]) {
    const page = browser({ user });
    assert.deepEqual(await page.run('observe'), { state: 'unconfirmed' });
    assert.equal(page.requests.length, 0);
  }
});

test('local success cannot override server refusal, identity or stale login time', async () => {
  for (const reply of [
    { status: 401, body: { success: false } },
    { body: { success: false } },
    { body: { success: true, data: { id: 8 } } },
    { body: { success: true, data: { id: 7 } } },
    { body: { success: true, data: { id: 7, last_login_time: 99 } } },
  ]) {
    const page = browser({ user: { id: 7, checked_in: true, last_login_time: 101 },
      responses: { '/api/user/self': reply } });
    const result = await page.run('observe');
    assert.notEqual(result.state, 'signed_in');
  }
});

test('neither a lookalike nor OAuth origin receives panel operations', async () => {
  for (const location of ['https://agentrouter.org.evil.test', 'http://agentrouter.org', 'https://github.com']) {
    const page = browser({ location });
    for (const name of ['prepare', 'oauth', 'observe']) {
      assert.deepEqual(await page.run(name), { state: 'pending' });
    }
    assert.equal(page.requests.length, 0);
    assert.equal(page.opened.length, 0);
  }
});
