(async () => {
  const out = (value) => JSON.stringify(value);
  if (location.origin !== __ORIGIN__) return out({ state: 'pending' });
  const request = async (path) => {
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), 8000);
    try {
      const response = await fetch(path, {
        credentials: 'same-origin', signal: abort.signal,
        redirect: 'error', cache: 'no-store',
      });
      if (!response.ok) return null;
      return await response.json();
    } finally { clearTimeout(timer); }
  };
  try {
    const status = await request('/api/status');
    const clientId = status?.data?.github_client_id;
    if (status?.success !== true || status.data?.github_oauth !== true ||
        typeof clientId !== 'string' || !/^[A-Za-z0-9]+$/.test(clientId)) {
      return out({ state: 'oauth_failed' });
    }
    const state = await request('/api/oauth/state?mode=login');
    if (state?.success !== true || typeof state.data !== 'string' || !state.data) {
      return out({ state: 'oauth_failed' });
    }
    const authorize = new URL('https://github.com/login/oauth/authorize');
    authorize.searchParams.set('client_id', clientId);
    authorize.searchParams.set('state', state.data);
    authorize.searchParams.set('scope', 'user:email');
    localStorage.setItem('oauth_mode', 'login');
    // Password, 2FA and the authorization consent remain human actions. No URL,
    // state, code or session credential is returned to the desktop or its logs.
    const popup = window.open(authorize.href, '_blank');
    return out({ state: popup ? 'started' : 'needs_user' });
  } catch (_) { return out({ state: 'oauth_failed' }); }
})()
