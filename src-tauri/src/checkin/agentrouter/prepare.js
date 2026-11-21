(async () => {
  const out = (value) => JSON.stringify(value);
  if (location.origin !== __ORIGIN__) return out({ state: 'pending' });
  const userId = __USER_ID__;
  try {
    const cached = JSON.parse(localStorage.getItem('user') || 'null');
    if (cached?.id != null && String(cached.id) !== userId) {
      return out({ state: 'identity_mismatch' });
    }
  } catch (_) { return out({ state: 'needs_user' }); }
  const request = async (path) => {
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), 8000);
    try {
      const response = await fetch(path, {
        credentials: 'same-origin',
        headers: { 'New-Api-User': userId },
        signal: abort.signal,
        redirect: 'error',
        cache: 'no-store',
      });
      const json = await response.json().catch(() => null);
      return { status: response.status, ok: response.ok, json };
    } finally { clearTimeout(timer); }
  };
  try {
    const self = await request('/api/user/self');
    let quota = null;
    if (self.ok && self.json?.success === true && self.json.data?.id != null) {
      if (String(self.json.data.id) !== userId) return out({ state: 'identity_mismatch' });
      if (Number.isSafeInteger(self.json.data.quota)) quota = self.json.data.quota;
    } else if (self.status !== 401) {
      return out({ state: 'needs_user' });
    }
    // A raw logout GET does NOT run the SPA's localStorage cleanup. A 401 from
    // both self and logout proves there is no session left to sign out; a
    // business refusal on a live session is not that proof.
    const logout = await request('/api/user/logout');
    const noSession = self.status === 401 && logout.status === 401;
    if ((!logout.ok || logout.json?.success !== true) && !noSession) {
      return out({ state: 'logout_failed' });
    }
    localStorage.removeItem('user');
    localStorage.removeItem('oauth_mode');
    return out({ state: 'prepared', quota });
  } catch (_) { return out({ state: 'logout_failed' }); }
})()
