(async () => {
  const out = (value) => JSON.stringify(value);
  if (location.origin !== __ORIGIN__) return out({ state: 'pending' });
  const userId = __USER_ID__;
  let user;
  try { user = JSON.parse(localStorage.getItem('user') || 'null'); }
  catch (_) { return out({ state: 'pending' }); }
  if (!user || user.id == null) return out({ state: 'pending' });
  if (String(user.id) !== userId) return out({ state: 'identity_mismatch' });
  if (typeof user.checked_in !== 'boolean' || !Number.isSafeInteger(user.last_login_time)) {
    return out({ state: 'unconfirmed' });
  }
  if (user.last_login_time < __STARTED_AT__) {
    return out({ state: 'pending' });
  }
  // Nothing but an amount leaves this function: the response's token, cookie
  // and any other field stay in the page, and only a same-origin endpoint is
  // asked with the session the browser already owns.
  const get = async (path) => {
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), 8000);
    try {
      const response = await fetch(path, {
        credentials: 'same-origin', headers: { 'New-Api-User': userId },
        signal: abort.signal, redirect: 'error', cache: 'no-store',
      });
      if (!response.ok) return null;
      return await response.json();
    } catch (_) { return null; }
    finally { clearTimeout(timer); }
  };
  const body = await get('/api/user/self');
  if (body?.success !== true) return out({ state: 'pending' });
  if (body.data?.id == null) return out({ state: 'unconfirmed' });
  if (String(body.data.id) !== userId) return out({ state: 'identity_mismatch' });
  if (!Number.isSafeInteger(body.data.last_login_time) ||
      body.data.last_login_time !== user.last_login_time) {
    return out({ state: 'unconfirmed' });
  }
  const checkedIn = typeof body.data.checked_in === 'boolean'
    ? body.data.checked_in : user.checked_in;
  const quota = typeof body.data.quota === 'number' && Number.isFinite(body.data.quota)
    ? Math.trunc(body.data.quota) : null;
  // The panel's own log is the strongest source for the granted amount. Only a
  // row created after this attempt started can be this check-in's reward, and
  // only a positive quota is a grant; older or non-check-in rows are ignored.
  let logQuota = null;
  let logAt = null;
  if (checkedIn) {
    const now = Math.floor(Date.now() / 1000);
    const logs = await get('/api/log/self?p=1&page_size=100' +
      `&start_timestamp=${__STARTED_AT__}&end_timestamp=${now}`);
    const rows = Array.isArray(logs?.data) ? logs.data
      : Array.isArray(logs?.data?.data) ? logs.data.data : [];
    for (const row of rows) {
      const at = Number(row?.created_at);
      const value = typeof row?.quota === 'number' && Number.isFinite(row.quota)
        ? Math.trunc(row.quota) : null;
      if (!Number.isSafeInteger(at) || at < __STARTED_AT__) continue;
      if (value === null || value <= 0) continue;
      const content = `${row?.content ?? ''} ${row?.type ?? ''}`;
      if (!/签到|check[\s_-]?in|每日|daily/i.test(content)) continue;
      if (logAt === null || at < logAt) {
        logAt = at;
        logQuota = value;
      }
    }
  }
  return out({ state: 'signed_in', user_id: userId,
    last_login_time: user.last_login_time, checked_in: checkedIn,
    quota, log_quota: logQuota, log_at: logAt });
})()
