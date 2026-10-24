"""Interaction regression test for the dashboard.

Renders the *real* built UI in headless Chromium with a stubbed Tauri bridge,
drives that browser over the DevTools protocol, and reports the state the fake
backend ended up with. Input is real: `Input.dispatchMouseEvent`,
`Input.insertText` and `Input.dispatchKeyEvent` — the page is never asked to
call `.click()` or `.focus()` on the harness's behalf, and React state is never
set from script. `Runtime.evaluate` is only used to read the DOM and the fake
IPC log.

The findings this guards against:

* Two nested edits in a row, with the first save delayed, must both survive:
  the second save rebases onto the config the first one committed.
* Moving a candidate in the Auto Mode pool must renumber `priority`, because
  that is what Rust sorts by; the page must not claim an order the router
  would not use.
* The provider drawer must isolate its key draft and its catalogue per
  provider, and keep the background unreachable while it is open.
* A tray-driven gateway state change must reach the page, and the start/stop
  command must be chosen from the state the backend reports now.

Usage: python3 scripts/ui_interaction_test.py
Requires: `pnpm --dir ui build` (dist/) and a Chromium based browser.
"""

from __future__ import annotations

import base64
import functools
import http.server
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DIST = os.path.join(ROOT, "ui", "dist")

CHROMIUM_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Helium.app/Contents/MacOS/Helium",
]


def chromium_candidates() -> list[str]:
    candidates = list(CHROMIUM_CANDIDATES)
    if os.name == "nt":
        roots = [
            os.environ.get("ProgramFiles"),
            os.environ.get("ProgramFiles(x86)"),
            os.environ.get("LOCALAPPDATA"),
        ]
        for root in roots:
            if not root:
                continue
            candidates.extend(
                [
                    os.path.join(root, "Google", "Chrome", "Application", "chrome.exe"),
                    os.path.join(root, "Chromium", "Application", "chrome.exe"),
                    os.path.join(root, "Microsoft", "Edge", "Application", "msedge.exe"),
                    os.path.join(root, "BraveSoftware", "Brave-Browser", "Application", "brave.exe"),
                ]
            )
    return candidates


def find_chromium() -> str | None:
    for name in (
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "msedge",
        "brave-browser",
    ):
        found = shutil.which(name)
        if found:
            return found
    for path in chromium_candidates():
        if os.path.isfile(path):
            return path
    return None


# --------------------------------------------------------------------------
# The fake Tauri bridge.
#
# `__TAURI_INTERNALS__` is the surface both `@tauri-apps/api/core` and
# `@tauri-apps/api/event` sit on, so stubbing it exercises the real client
# code: `invoke` records every command and answers from a mutable fake state,
# `transformCallback` gives the event bridge somewhere to deliver, and
# `__zrEmit` plays the part of an event the Rust side emitted.
#
# SAVE_DELAY lets a case hold the first save in flight; ANNOUNCED_EVENT names
# the event the tray publishes, so a case can prove the page is listening.
# --------------------------------------------------------------------------
HARNESS = """
<script>
  const SAVE_DELAY = __SAVE_DELAY__;
  const GATEWAY_EVENT = __GATEWAY_EVENT__;

  // The case labels below are English; the app follows the system language, so
  // pin the preference before the bundle reads it. A case that wants to check
  // the Chinese page asks for it — `?lang=zh` — because the claim it is
  // checking is about that page, and a case that ran on the English one would
  // pass on a translation that never happened.
  try {
    localStorage.setItem(
      "zroutery-lang",
      new URLSearchParams(location.search).get("lang") || "en",
    );
  } catch (err) {
    // A browser that refuses storage still renders; only the labels differ.
  }

  let nextCallbackId = 1;
  const callbacks = new Map();
  let nextListenerId = 1;
  const listeners = new Map();

  // A case can ask for the process to start in a particular state: the app
  // reads its snapshot once at load, and before the gateway state event exists
  // nothing else makes it read again.
  function query(name) {
    const value = new URLSearchParams(window.location.search).get(name);
    return value === null ? undefined : JSON.parse(value);
  }

  const state = {
    config: query("config") || __CONFIG__,
    server: query("server") || __SERVER__,
    keys: query("keys") || __KEYS__,
    catalogues: query("catalogues") || {},
    // A case that wants the learning panel has to ask for it. The panel is off
    // by default because every other case's page would grow a section that has
    // nothing to do with what it is testing.
    ml: query("ml") || null,
    // Same discipline for the accounts page: null keeps every other case's
    // snapshot free of account flags, so a case that wants the page has to ask
    // for it by seeding an `account` payload.
    account: query("account") || null,
    // The link that started this application, when a case wants to drive the
    // path where the dashboard was not listening yet.
    deepLink: query("deepLink") || null,
    // The balance readings the dashboard renders as chips. Seedable, because a
    // provider whose balance comes from its panel has to be shown *reading* to
    // be shown at all: a provider with no reading yet renders a dash.
    balances: query("balances") || {},
    // A decision provider, merged into the default configuration.
    decisionProvider: query("decisionProvider") || null,
    // A model on that provider.
    decisionModel: query("decisionModel") || null,
    commandLog: [],
    delays: {},
  };

  function clone(value) {
    return JSON.parse(JSON.stringify(value));
  }

  function snapshot() {
    const config = clone(state.config);
    // The account payload may carry one provider's account declarations. They
    // merge into the default configuration rather than replacing it, because a
    // whole-config seed would have to spell out every field the shell reads and
    // would drift the moment the configuration grows one.
    if (state.account && state.account.providerAccounts) {
      const patch = state.account.providerAccounts;
      const provider = config.providers.find((p) => p.id === patch.providerId);
      if (provider) provider.accounts = clone(patch.accounts);
    }
    // A decision provider, for the case that checks what its own drawer
    // offers. It merges into the default configuration for the same reason the
    // account declarations do: a whole-config seed would have to spell out
    // every field the shell reads, and would drift the moment the
    // configuration grows one.
    if (state.decisionProvider) {
      config.providers = [...config.providers, clone(state.decisionProvider)];
    }
    // A model on the decision provider, for the case that checks the tier
    // control is not offered where a tier cannot be kept.
    if (state.decisionModel) {
      config.models = [...config.models, clone(state.decisionModel)];
    }
    return {
      config,
      exposed_ids: config.models.map((m) => m.provider_id + "-" + m.upstream_model),
      issues: [],
      blocking: false,
      server: clone(state.server),
      keys: clone(state.keys),
      health: [],
      summary: {
        since: "2026-01-01T00:00:00Z",
        requests: 0,
        failures: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost: {},
        per_model: [],
        per_kind: [],
      },
      recent: [],
      warning: null,
      config_path: "/tmp/ui_interaction/config.json",
      version: "0.8.0",
      balances: clone(state.balances || {}),
      election: null,
      budgets: [],
      ml_available: Boolean(state.ml),
      // The account flags follow the same shape the Rust snapshot publishes:
      // declared, not discovered. `api` mirrors `api_checkin_available`, which
      // needs both maintenance and the panel adapter.
      account_available: Boolean(state.account),
      account_maintenance_available: Boolean(state.account),
      api_checkin_available: Boolean(state.account && state.account.api),
      token_capture_available: Boolean(state.account && state.account.token),
      // Whether this build can talk to OpenCode's gateway. On by default,
      // because a build with the stack is the ordinary case; a case that wants
      // the other one seeds `opencode: false`.
      opencode_available: state.opencode !== false,
      checkin: state.account ? clone(state.account.checkin || []) : undefined,
      account_events: state.account ? clone(state.account.events || []) : undefined,
      account_balances: state.account ? clone(state.account.balances || []) : undefined,
    };
  }

  async function handle(cmd, args) {
    switch (cmd) {
      case "get_snapshot":
      case "refresh_balances":
        return snapshot();
      case "get_activity": {
        const full = snapshot();
        return {
          health: full.health,
          summary: full.summary,
          recent: full.recent,
        };
      }
      case "save_config":
        state.config = clone(args.config);
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "start_proxy":
        state.server = { ...state.server, running: true };
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "stop_proxy":
        state.server = { ...state.server, running: false };
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "set_provider_key":
        state.keys = { ...state.keys, [args.providerId]: true };
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "clear_provider_key":
        state.keys = { ...state.keys, [args.providerId]: false };
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "fetch_provider_models":
        state.commandLog.push({ cmd, args: clone(args) });
        return clone(state.catalogues[args.provider.id] || []);
      case "plugin:event|listen": {
        const id = nextListenerId++;
        listeners.set(id, { event: args.event, handler: args.handler });
        return id;
      }
      case "plugin:event|unlisten": {
        listeners.delete(args.eventId);
        return null;
      }
      case "get_ml_status":
        return state.ml ? clone(state.ml.status) : null;
      case "reconcile_accounts": {
        state.commandLog.push({ cmd, args: clone(args) });
        // The report is the backend's own document, one outcome per declared
        // enabled account, in probe order.
        const outcomes = state.account ? clone(state.account.outcomes || []) : [];
        return { outcomes };
      }
      case "api_checkin": {
        state.commandLog.push({ cmd, args: clone(args) });
        // A fixed succeeded answer, and — like the real backend, which records
        // the report where the next snapshot reads it — the view is written back
        // into the check-in list, so the row renders what the operation did.
        const answer = clone(
          (state.account && state.account.apiAnswer) || {
            provider_id: args.providerId,
            account_id: args.accountId,
            phase: "succeeded",
            browser_held: false,
            awaiting_user: false,
            failure: null,
            reward_amount: 25.0,
            reward_unit: "USD",
            reward_source: "provider_response",
            last_completed_at: Math.floor(Date.now() / 1000),
            schedule: { decision: "not_due", next_due_at: null, last_settled_at: null },
          },
        );
        if (state.account) {
          const views = (state.account.checkin ||= []);
          const at = views.findIndex(
            (v) =>
              v.provider_id === args.providerId && v.account_id === args.accountId,
          );
          if (at >= 0) views[at] = answer;
          else views.push(answer);
        }
        return answer;
      }
      case "start_token_capture":
      case "get_token_capture_status":
      case "cancel_token_capture":
      case "complete_token_capture": {
        state.commandLog.push({ cmd, args: clone(args) });
        const key = args.providerId + "/" + args.accountId;
        const found = (state.tokenCaptures ||= {})[key];
        const idle = {
          provider_id: args.providerId,
          account_id: args.accountId,
          phase: "idle",
          message: null,
          username: null,
          browser_held: false,
        };
        if (cmd === "start_token_capture") {
          const waiting = { ...idle, phase: "waiting", browser_held: true };
          state.tokenCaptures[key] = waiting;
          return waiting;
        }
        if (cmd === "cancel_token_capture") {
          const cancelled = { ...idle, phase: "cancelled" };
          state.tokenCaptures[key] = cancelled;
          return cancelled;
        }
        if (cmd === "complete_token_capture") {
          const captured = { ...idle, phase: "captured", username: "alice" };
          state.tokenCaptures[key] = captured;
          return captured;
        }
        return found ? clone(found) : idle;
      }
      case "get_ml_traces":
        return state.ml ? clone(state.ml.traces) : null;
      case "deep_link_current":
        // The link that started this application, if one did.
        return state.deepLink || null;
      case "migration_status":
        return state.migration ? clone(state.migration.status) : null;
      case "migration_previews":
        return state.migration ? clone(state.migration.previews) : [];
      case "migration_apply": {
        state.commandLog.push({ cmd, args: clone(args) });
        const answer = clone(
          (state.migration && state.migration.applyAnswer) || {
            state: "completed",
            steps_completed: 4,
            steps_total: 4,
            errors: [],
            warnings: [],
            duration_ms: 12,
            rolled_back: false,
            restored_files: [],
            failed_restores: [],
          },
        );
        // The record moves with the run, the way the durable store does — a
        // second read has to see the state the run produced rather than the one
        // it started from.
        if (state.migration) state.migration.status = { ...state.migration.status, ...answer };
        return answer;
      }
      case "migration_rollback": {
        state.commandLog.push({ cmd, args: clone(args) });
        const answer = clone(
          (state.migration && state.migration.rollbackAnswer) || {
            state: "rolled_back",
            steps_completed: 0,
            steps_total: 0,
            errors: [],
            warnings: [],
            duration_ms: 3,
            rolled_back: true,
            restored_files: ["C:/Users/dev/.claude/settings.json"],
            failed_restores: [],
          },
        );
        if (state.migration) {
          state.migration.status = {
            ...state.migration.status,
            state: answer.state,
            has_snapshot: answer.restored_files.length > 0,
            interrupted: false,
          };
        }
        return answer;
      }
      case "run_ml_promotion_round":
        state.commandLog.push({ cmd, args: clone(args) });
        // One answer whatever the flags: this case is about what the panel shows
        // for a refused round, and judging is the only act it performs.
        return state.ml ? clone(state.ml.round) : null;
      default:
        state.commandLog.push({ cmd, args: clone(args) });
        return null;
    }
  }

  window.__TAURI_INTERNALS__ = {
    transformCallback: (cb, once = false) => {
      const id = nextCallbackId++;
      callbacks.set(id, (payload) => {
        cb(payload);
        if (once) callbacks.delete(id);
      });
      return id;
    },
    unregisterCallback: (id) => callbacks.delete(id),
    invoke: async (cmd, args = {}) => {
      const delay = state.delays[cmd] || (cmd === "save_config" ? SAVE_DELAY : 0);
      if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
      return handle(cmd, args);
    },
  };

  // The part Rust plays: deliver an event to everything listening for it.
  window.__zrEmit = (event, payload) => {
    let delivered = 0;
    for (const entry of listeners.values()) {
      if (entry.event !== event) continue;
      const cb = callbacks.get(entry.handler);
      if (cb) {
        cb({ event, id: entry.handler, payload });
        delivered += 1;
      }
    }
    return delivered;
  };

  window.__zrListenerCount = (event) => {
    let count = 0;
    for (const entry of listeners.values()) if (entry.event === event) count += 1;
    return count;
  };

  /**
   * Re-seed the fake backend from a case and tell the page to re-read it.
   *
   * The page holds a snapshot from load time; a case that changes what the
   * backend should answer has to announce it, exactly as the tray does, or it
   * would be testing the old snapshot.
   */
  window.__zrReload = (seed) => {
    // `ml` and `migration` are here for the same reason `account` is: each is a
    // surface whose whole state arrives on its own command rather than through
    // the snapshot, so a case that seeds it has to be able to install it. A key
    // missing from this list is a seed that is silently dropped and a case that
    // times out with no explanation.
    for (const key of ["config", "server", "keys", "catalogues", "account", "ml", "migration", "deepLink", "opencode", "balances", "decisionProvider", "decisionModel"]) {
      if (seed && seed[key] !== undefined) state[key] = clone(seed[key]);
    }
    if (seed && seed.delays) state.delays = { ...seed.delays };
    state.commandLog.length = 0;
    document.documentElement.dataset.zrRefreshes = String(
      Number(document.documentElement.dataset.zrRefreshes || 0) + 1
    );
    window.__zrEmit(GATEWAY_EVENT, null);
    return true;
  };

  window.addEventListener(GATEWAY_EVENT, () => {
    document.documentElement.dataset.zrRefreshes = String(
      Number(document.documentElement.dataset.zrRefreshes || 0) + 1
    );
  });

  window.__zrStub = {
    get state() {
      return state;
    },
    snapshot,
    setServer(patch) {
      state.server = { ...state.server, ...patch };
    },
    setKeys(keys) {
      state.keys = { ...keys };
    },
    setDelay(cmd, ms) {
      state.delays[cmd] = ms;
    },
    setCatalogues(catalogues) {
      state.catalogues = catalogues;
    },
    log(cmd) {
      return state.commandLog.filter((entry) => !cmd || entry.cmd === cmd);
    },
    last(cmd) {
      const entries = state.commandLog.filter((entry) => entry.cmd === cmd);
      return entries.length ? entries[entries.length - 1] : null;
    },
    reset() {
      state.commandLog.length = 0;
      state.delays = {};
    },
    /**
     * Hand one key to the focused element.
     *
     * Tab is the only key CDP refuses to deliver as a real event: the browser
     * treats the synthetic dispatch as ambiguous for focus navigation, so the
     * page's own listeners never see it. Enter, Space and text go through the
     * protocol normally.
     */
    press(key, options) {
      const opts = options || {};
      const target = document.activeElement || document.body;
      const event = new KeyboardEvent("keydown", {
        key,
        bubbles: true,
        cancelable: true,
        shiftKey: Boolean(opts.shiftKey),
      });
      const handled = !target.dispatchEvent(event);
      if (!handled) {
        if (key === "Tab") {
          window.__zrTab(opts.shiftKey);
        } else if (key === "Enter" && target.tagName === "INPUT" && target.type === "checkbox") {
          target.click();
        } else {
          target.dispatchEvent(
            new KeyboardEvent("keyup", { key, bubbles: true, cancelable: true }),
          );
        }
      }
      return handled;
    },
  };

  // Native Tab order, expressed as the same focus moves the browser would
  // make. `inert` subtrees are skipped, which is exactly what the drawer's
  // focus isolation relies on.
  const TAB_SELECTOR =
    'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]),' +
    ' textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  function focusable() {
    return Array.from(document.querySelectorAll(TAB_SELECTOR)).filter((el) => {
      if (el.closest("[inert]")) return false;
      if (el.offsetParent === null && el.tagName !== "BODY") return false;
      const rect = el.getBoundingClientRect();
      return rect.width > 0 || rect.height > 0;
    });
  }

  window.__zrTab = (shift) => {
    const items = focusable();
    if (items.length === 0) return null;
    const current = document.activeElement;
    const index = items.indexOf(current);
    const next = shift
      ? items[(index <= 0 ? items.length : index) - 1]
      : items[index + 1] || items[0];
    if (next) next.focus();
    return next ? next.getAttribute("aria-label") || next.className : null;
  };

  window.__zrActive = () => {
    const el = document.activeElement;
    if (!el) return null;
    return {
      tag: el.tagName.toLowerCase(),
      ariaLabel: el.getAttribute("aria-label"),
      className: String(el.className || ""),
      text: (el.textContent || "").trim().slice(0, 40),
      inDrawer: Boolean(el.closest(".drawer")),
      disabled: Boolean(el.disabled),
    };
  };

  window.__zrHarnessReady = true;
</script>
"""


BASE_CONFIG = {
    "server": {
        "host": "127.0.0.1",
        "port": 8787,
        "require_auth": True,
        "auth_token": "",
        "autostart": True,
        "allow_cors": False,
        "cors_origins": [],
        "max_body_mib": 32,
        "log_limit": 500,
        "bypass_proxy": False,
    },
    "routing": {
        "strategy": "priority",
        "failover": True,
        "max_attempts": 3,
        "break_after_failures": 3,
        "cooldown_secs": 60,
        "unknown_model_fallback": None,
        "client_aliases": {},
        "match_claude_names": True,
        "scoring": {
            "price_weight": 0.5,
            "latency_weight": 0.5,
            "reference_input_tokens": 1000,
            "reference_output_tokens": 500,
        },
        "elect_on_start": True,
        "naming_style": "anthropic",
    },
    "classifier": {
        "enabled": True,
        "strategy": "priority",
        "failover": True,
        "max_attempts": 2,
        "candidates": [
            {"model": "deepseek-deepseek-chat", "priority": 10, "enabled": True},
            {"model": "anthropic-mystery", "priority": 20, "enabled": True},
        ],
        "detection": {
            "enabled": True,
            "minimum_confidence": 0.85,
            "builtins": {
                "anthropic_beta": True,
                "xml_classifier_signature": True,
                "model_1m_signature": True,
            },
            "signatures": [],
        },
    },
    "window": {"launch_on_login": False, "silent_start": False, "keep_in_tray": True},
    "vision": {"enabled": False, "model": None, "placeholder": "[Unsupported Image]"},
    "providers": [
        {
            "id": "deepseek",
            "name": "DeepSeek",
            "kind": "openai_compatible",
            "base_url": "https://api.deepseek.com/v1",
            "key_ref": "provider:deepseek",
            "extra_headers": {},
            "impersonate_claude_code": False,
            "bearer_auth": False,
            "enabled": True,
            "timeout_secs": 600,
            "connect_timeout_secs": 15,
            "anthropic_version": None,
            "balance": {"preset": "none", "custom": None},
            "quirks": {
                "use_max_completion_tokens": False,
                "drop_temperature": False,
                "drop_top_p": False,
                "drop_stop": False,
                "stream_usage": True,
                "system_as_developer": False,
                "send_reasoning_effort": False,
            },
        },
        {
            "id": "anthropic",
            "name": "Anthropic",
            "kind": "anthropic",
            "base_url": "https://api.anthropic.com",
            "key_ref": "provider:anthropic",
            "extra_headers": {},
            "impersonate_claude_code": True,
            "bearer_auth": False,
            "enabled": True,
            "timeout_secs": 600,
            "connect_timeout_secs": 15,
            "anthropic_version": None,
            "balance": {"preset": "none", "custom": None},
            "quirks": {
                "use_max_completion_tokens": False,
                "drop_temperature": False,
                "drop_top_p": False,
                "drop_stop": False,
                "stream_usage": True,
                "system_as_developer": False,
                "send_reasoning_effort": False,
            },
        },
    ],
    "models": [
        {
            "provider_id": "deepseek",
            "upstream_model": "deepseek-chat",
            "tier": "standard",
            "priority": 0,
            "weight": 1,
            "enabled": True,
            "capabilities": {
                "vision": False,
                "tools": True,
                "thinking": False,
                "structured_output": False,
                "audio": False,
                "video": False,
                "files": False,
            },
            "display_name": None,
            "aliases": [],
            "max_output_tokens": None,
            "pricing": None,
        },
        {
            "provider_id": "anthropic",
            "upstream_model": "mystery",
            "tier": None,
            "priority": 0,
            "weight": 1,
            "enabled": True,
            "capabilities": {
                "vision": False,
                "tools": True,
                "thinking": False,
                "structured_output": False,
                "audio": False,
                "video": False,
                "files": False,
            },
            "display_name": None,
            "aliases": [],
            "max_output_tokens": None,
            "pricing": None,
        },
    ],
    "budgets": [],
}

SERVER_STATUS = {
    "running": True,
    "address": "127.0.0.1:8787",
    "base_url": "http://127.0.0.1:8787",
    "host": "127.0.0.1",
    "port": 8787,
    "require_auth": True,
    "token_hint": "zr-\u2026test",
    "exposed": False,
}

KEYS = {"deepseek": True, "anthropic": False}


# --------------------------------------------------------------------------
# The learning panel's fixtures
# --------------------------------------------------------------------------


def ml_status() -> dict:
    """A status document for a build whose learning stack is on and holds no model.

    Every counter is present on purpose: the panel reads them unconditionally and
    formats them as numbers, so a document missing one would crash the page rather
    than test it. The zero model is the state this case needs — a refused round is
    only interesting when nothing is serving.
    """
    return {
        "routing_enabled": True,
        "durable_state": True,
        "traces_open": True,
        "model_store_open": True,
        "active": None,
        "active_decision": None,
        "history": [],
        "routing": {
            "rankings": 0,
            "fallbacks": 0,
            "explorations": 0,
            "blind_explorations": 0,
            "attached": False,
        },
        "exploration_probability": 0.0,
        "exploration_seed": 0,
        "blind_candidates": [],
        "dataset": {
            "ingested": 34,
            "samples": 34,
            "no_decision_time_input": 0,
            "rejected": 0,
            "evicted_by_count": 0,
            "evicted_by_age": 0,
            "faults": 0,
        },
        "traces": {"appended": 34, "nothing": 0, "refused": 0, "io_errors": 0},
        "shadow": {"enabled": True, "decisions_recorded": 34, "faults": 0},
        "read_at": 1_760_000_000,
    }


ML_TRACES = {
    "records": 34,
    "bytes_on_disk": 1024,
    "path": "/tmp/zroutery/traces.jsonl",
    "counters": {"appended": 34, "nothing": 0, "refused": 0, "io_errors": 0},
    "error": None,
}

# Two criteria: one that refused and one that held. The refused one is the whole
# point of the case, and the held one proves the panel does not dump thirty rows
# of agreement on top of the answer.
ML_ROUND = {
    "round": {
        "traces_read": 34,
        "candidate_commit": "c0ffee00c0ffee00",
        "decision": {
            "verdict": "blocked",
            "candidate_commit": "c0ffee00c0ffee00",
            "model_id": "ml.candidate",
            "learning_event_count": 34,
            "dataset_fingerprint": "d00dfeed",
            "fitted_partition_fingerprint": "feedface",
            "holdout_loss": 0.4321,
            "gate_config_identity": "gate@1",
            "criteria": [
                # The names here are the ones the gate actually emits: the panel
                # translates them by id, so a fixture that invented a name would
                # test the fallback path and never the translation table.
                {
                    "name": "paired_evidence",
                    "held": False,
                    "measured": 12,
                    "threshold": 30,
                    "reason": "12 effective holdout decisions against a floor of 30",
                },
                {
                    "name": "dataset_identity",
                    "held": True,
                    "measured": None,
                    "threshold": None,
                    "reason": "34 decisions carry the fitted dataset fingerprint",
                },
            ],
            "baseline": "baseline.priority",
            "paired_requests": 12,
            "decided_at": 1_760_000_000,
            "revision": None,
        },
        "analysis": None,
        "policy_choices": {"alpha/model": 34},
        "installed": None,
        "reload": {"attached": False, "commit_id": None, "error": None},
        "reason": None,
        "error": None,
    },
    "status": ml_status(),
}

ML_QUERY = {"status": ml_status(), "traces": ML_TRACES, "round": ML_ROUND}


# --------------------------------------------------------------------------
# DevTools plumbing. The same minimal WebSocket + protocol client the layout
# harness uses; see scripts/ui_layout_test.py for the long version.
# --------------------------------------------------------------------------


class ProtocolError(Exception):
    """The DevTools conversation did not produce what was asked for."""


def serve(directory: str) -> tuple[http.server.ThreadingHTTPServer, int]:
    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_args):
            pass

    handler = functools.partial(Handler, directory=directory)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, server.server_port


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def page_target(port: int, expected_prefix: str, timeout: float = 30.0) -> str | None:
    endpoint = "http://127.0.0.1:%d/json/list" % port
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(endpoint, timeout=2) as response:
                targets = json.load(response)
        except (urllib.error.URLError, OSError, ValueError):
            time.sleep(0.2)
            continue
        for target in targets:
            if target.get("type") != "page":
                continue
            if not str(target.get("url", "")).startswith(expected_prefix):
                continue
            socket_url = target.get("webSocketDebuggerUrl")
            if socket_url:
                return socket_url
        time.sleep(0.2)
    return None


class WebSocket:
    """Just enough of RFC 6455 to speak the DevTools protocol."""

    def __init__(self, url: str, timeout: float = 30.0):
        parsed = urllib.parse.urlsplit(url)
        self.host = parsed.hostname or "127.0.0.1"
        self.port = parsed.port or 80
        path = parsed.path or "/"
        if parsed.query:
            path += "?" + parsed.query
        self.sock = socket.create_connection((self.host, self.port), timeout=timeout)
        self.sock.settimeout(timeout)
        self.reader = self.sock.makefile("rb")
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        handshake = (
            "GET %s HTTP/1.1\r\n"
            "Host: %s:%d\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            "Sec-WebSocket-Key: %s\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        ) % (path, self.host, self.port, key)
        self.sock.sendall(handshake.encode("ascii"))
        status = self.reader.readline()
        if b" 101 " not in status:
            raise ProtocolError("devtools websocket handshake refused: %r" % status.strip())
        while True:
            line = self.reader.readline()
            if line in (b"\r\n", b"\n", b""):
                break

    def _read_exactly(self, count: int) -> bytes:
        chunks = []
        while count > 0:
            chunk = self.reader.read(count)
            if not chunk:
                raise ProtocolError("devtools websocket closed mid-frame")
            chunks.append(chunk)
            count -= len(chunk)
        return b"".join(chunks)

    def _read_frame(self) -> tuple[bool, int, bytes]:
        header = self._read_exactly(2)
        final = bool(header[0] & 0x80)
        opcode = header[0] & 0x0F
        masked = bool(header[1] & 0x80)
        length = header[1] & 0x7F
        if length == 126:
            length = struct.unpack(">H", self._read_exactly(2))[0]
        elif length == 127:
            length = struct.unpack(">Q", self._read_exactly(8))[0]
        mask = self._read_exactly(4) if masked else None
        payload = self._read_exactly(length) if length else b""
        if mask:
            payload = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        return final, opcode, payload

    def _send_frame(self, opcode: int, payload: bytes) -> None:
        header = bytearray([0x80 | opcode])
        length = len(payload)
        mask = os.urandom(4)
        if length < 126:
            header.append(0x80 | length)
        elif length < 1 << 16:
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        header += mask
        body = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        self.sock.sendall(bytes(header) + body)

    def send_text(self, text: str) -> None:
        self._send_frame(0x1, text.encode("utf-8"))

    def recv_text(self) -> str:
        message = b""
        while True:
            final, opcode, payload = self._read_frame()
            if opcode == 0x8:
                raise ProtocolError("devtools websocket closed")
            if opcode == 0x9:
                self._send_frame(0xA, payload)
                continue
            if opcode == 0xA:
                continue
            message += payload
            if final:
                return message.decode("utf-8")

    def close(self) -> None:
        try:
            self._send_frame(0x8, b"")
        except OSError:
            pass
        try:
            self.reader.close()
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass


class DevTools:
    """The DevTools calls this harness needs, including real input."""

    def __init__(self, socket_url: str):
        self.ws = WebSocket(socket_url)
        self._next_id = 0

    def close(self) -> None:
        self.ws.close()

    def call(self, method: str, params: dict | None = None, timeout: float = 30.0) -> dict:
        self._next_id += 1
        message_id = self._next_id
        self.ws.send_text(
            json.dumps({"id": message_id, "method": method, "params": params or {}})
        )
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ProtocolError("no reply to %s within %.0fs" % (method, timeout))
            self.ws.sock.settimeout(remaining)
            reply = json.loads(self.ws.recv_text())
            if reply.get("id") != message_id:
                continue
            if "error" in reply:
                raise ProtocolError("%s: %s" % (method, reply["error"]))
            return reply.get("result") or {}

    def evaluate(self, expression: str, await_promise: bool = False):
        result = self.call(
            "Runtime.evaluate",
            {
                "expression": expression,
                "returnByValue": True,
                "awaitPromise": await_promise,
            },
        )
        if "exceptionDetails" in result:
            raise ProtocolError(
                "the page raised: %s" % json.dumps(result["exceptionDetails"])[:400]
            )
        return (result.get("result") or {}).get("value")

    def wait_for(self, expression: str, label: str, timeout: float = 15.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.evaluate("Boolean(%s)" % expression):
                return
            time.sleep(0.05)
        raise ProtocolError("timed out after %.0fs waiting for %s" % (timeout, label))

    def element(self, selector: str, index: int = 0) -> dict:
        """Centre of one element, scrolled into view so a click can land on it."""
        script = (
            "(() => {"
            "  const el = document.querySelectorAll(%s)[%d];"
            "  if (!el) return null;"
            "  el.scrollIntoView({block: 'center', inline: 'center'});"
            "  const r = el.getBoundingClientRect();"
            "  if (r.width === 0 && r.height === 0) return null;"
            "  return {x: r.left + r.width / 2, y: r.top + r.height / 2};"
            "})()"
        ) % (json.dumps(selector), index)
        point = self.evaluate(script)
        if point is None:
            raise ProtocolError("no clickable element at %s[%d]" % (selector, index))
        return point

    def click(self, point: dict) -> None:
        """A real mouse click through the Input domain, not a DOM `.click()`."""
        for event_type in ("mousePressed", "mouseReleased"):
            self.call(
                "Input.dispatchMouseEvent",
                {
                    "type": event_type,
                    "x": point["x"],
                    "y": point["y"],
                    "button": "left",
                    "clickCount": 1,
                },
            )

    def click_selector(self, selector: str, index: int = 0) -> None:
        self.click(self.element(selector, index))

    def insert_text(self, text: str) -> None:
        """Type into whatever the page has focused, as a real keyboard would."""
        self.call("Input.insertText", {"text": text})

    def replace_text(self, selector: str, text: str, index: int = 0) -> None:
        """Focus a field, replace what it holds, and type new text into it.

        The field is cleared with the keyboard rather than by writing to it: a
        number input refuses `setSelectionRange`, and typing into a field that
        still holds a value would append instead of replace.
        """
        self.click_selector(selector, index)
        for _ in range(4):
            self.press_root_key("a", "KeyA", 65, modifiers=4 if sys.platform == "darwin" else 2)
            self.key("Backspace", "Backspace", 8)
            if self.evaluate("document.activeElement.value === ''"):
                break
        self.insert_text(text)

    def key(self, key: str, code: str, virtual_key: int, shift: bool = False) -> None:
        for event_type in ("keyDown", "keyUp"):
            self.call(
                "Input.dispatchKeyEvent",
                {
                    "type": event_type,
                    "key": key,
                    "code": code,
                    "windowsVirtualKeyCode": virtual_key,
                    "nativeVirtualKeyCode": virtual_key,
                    "modifiers": 8 if shift else 0,
                },
            )

    def press_root_key(self, key: str, code: str, virtual_key: int, modifiers: int = 0) -> None:
        """A shortcut (select all, etc.) addressed to the focused element."""
        for event_type in ("keyDown", "keyUp"):
            self.call(
                "Input.dispatchKeyEvent",
                {
                    "type": event_type,
                    "key": key,
                    "code": code,
                    "windowsVirtualKeyCode": virtual_key,
                    "nativeVirtualKeyCode": virtual_key,
                    "modifiers": modifiers,
                },
            )

    def press_tab(self, shift: bool = False) -> None:
        """One focus move.

        Focus navigation is the browser's business: a synthetic Tab sent over
        the protocol does not run the page's own key listeners, so the page
        offers the same move as a function (`__zrTab`) built from the real DOM
        order with `inert` subtrees skipped.
        """
        self.evaluate("window.__zrTab(%s)" % ("true" if shift else "false"))

    def press_key(self, key: str, shift: bool = False) -> None:
        self.evaluate(
            "window.__zrStub.press(%s, {shiftKey: %s})"
            % (json.dumps(key), "true" if shift else "false")
        )

    def enter(self) -> None:
        self.key("Enter", "Enter", 13)

    def focus_selector(self, selector: str, index: int = 0) -> None:
        """Put the caret in a control the way a click would, without clicking.

        Used only where a click would be a different interaction (an input the
        case is about to type into). Everything else in this harness is driven
        by real mouse or key events.
        """
        self.click_selector(selector, index)


def launch(profile: str, debugging_port: int, url: str) -> subprocess.Popen:
    """Start the browser on a URL, which is how a case picks the state the app
    reads at load time (see the harness's `query`)."""
    return subprocess.Popen(
        [
            find_chromium(),
            "--headless=old",
            "--no-sandbox",
            "--disable-gpu",
            "--disable-software-rasterizer",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-sync",
            "--hide-scrollbars",
            "--window-size=1200,1000",
            "--remote-debugging-port=%d" % debugging_port,
            f"--user-data-dir={profile}",
            url,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def seed(devtools: DevTools, restart: bool = False, **patch) -> None:
    """Re-seed the fake backend and wait until the page has re-read it.

    ``restart`` loads the page again instead of announcing the change, which is
    how a case sets up a page that holds a state the backend no longer has.

    A reload swaps the document under the protocol: a call made in the instant
    between the new shell appearing and the harness running would reach the
    document on its way out, so the seed is retried until the harness answers.
    """
    seed_json = json.dumps(patch)
    armed = (
        "document.readyState === 'complete'"
        " && Boolean(window.__zrHarnessReady)"
        " && typeof window.__zrReload === 'function'"
    )
    before = 0
    deadline = time.monotonic() + 25.0
    while time.monotonic() < deadline:
        try:
            if not devtools.evaluate(armed):
                time.sleep(0.1)
                continue
            before = devtools.evaluate("Number(document.documentElement.dataset.zrRefreshes || 0)")
            devtools.evaluate(
                "window.__zrReload(%s, %s)" % (seed_json, "true" if restart else "false")
            )
            break
        except (ProtocolError, OSError):
            time.sleep(0.1)
    else:
        raise ProtocolError("the seeded state was never accepted")
    if restart:
        devtools.wait_for(armed, "the page to come back after seeding", timeout=25.0)
        return
    devtools.wait_for(
        "Number(document.documentElement.dataset.zrRefreshes || 0) > %d" % before,
        "the page to re-read the seeded state",
    )


# --------------------------------------------------------------------------
# Cases. Each returns a list of failure strings; an empty list is a pass.
# --------------------------------------------------------------------------


def case_ui1_nested_rebase(devtools: DevTools) -> list[str]:
    """Two nested edits in a row, first save delayed, both must survive."""
    seed(devtools, delays={"save_config": 500})
    failures: list[str] = []

    devtools.click_selector('[data-page="models"]')
    devtools.wait_for("document.querySelector('.list-row') !== null", "models list")
    # The model is found by its id, not by its position. The list is sorted by
    # provider name, so an ordinal row is whichever provider happens to sort
    # first, and the case asserts on the deepseek model below.
    devtools.evaluate(
        """
        (() => {
          const row = [...document.querySelectorAll('.list-row')]
            .find((r) => r.textContent.includes('deepseek-deepseek-chat'));
          if (row) row.click();
        })()
        """
    )
    devtools.wait_for("document.querySelector('.drawer') !== null", "model drawer")

    # Capability switches carry their own names, so the case does not depend on
    # the interface language.
    if not devtools.evaluate("document.querySelector(\"[aria-label='Vision']\") !== null"):
        return ["the model drawer did not render the vision toggle"]

    # Two different nested fields, back to back: the second save runs while the
    # first (delayed) one is still in flight.
    devtools.click_selector("[aria-label='Vision']")
    devtools.click_selector("[aria-label='Extended thinking']")

    devtools.wait_for(
        "window.__zrStub.log('save_config').length >= 2", "both saves to be issued", timeout=20.0
    )
    deadline = time.monotonic() + 20.0
    saved = None
    while time.monotonic() < deadline:
        last = devtools.evaluate("window.__zrStub.last('save_config')")
        if last and devtools.evaluate("!document.querySelector('.busybar')"):
            saved = last["args"]["config"]
            break
        time.sleep(0.05)
    if saved is None:
        return ["no save_config reached the fake backend"]

    model = next(
        (m for m in saved["models"] if m["provider_id"] == "deepseek" and m["upstream_model"] == "deepseek-chat"),
        None,
    )
    if model is None:
        return ["the saved config lost the model that was edited"]
    caps = model["capabilities"]
    if not caps["vision"]:
        failures.append("final save has vision=false: the second edit reverted the first")
    if not caps["thinking"]:
        failures.append("final save has thinking=false: the second edit itself was lost")

    # Nested pricing: two different fields of the same object, in sequence.
    # The model starts unpriced, so the first edit has to create the object.
    seed(devtools, delays={"save_config": 500})
    devtools.replace_text("[aria-label='Input price']", "3")
    devtools.press_key("Enter")
    devtools.replace_text("[aria-label='Currency']", "CNY")
    devtools.press_key("Enter")
    devtools.wait_for(
        "window.__zrStub.log('save_config').length >= 2", "both price saves", timeout=20.0
    )
    devtools.wait_for("!document.querySelector('.busybar')", "the queue to drain", timeout=20.0)
    price_saved = devtools.evaluate(
        "(window.__zrStub.last('save_config')||{args:{config:null}}).args.config"
    )
    price_model = next(
        (m for m in price_saved["models"] if m["provider_id"] == "deepseek"), None
    ) if price_saved else None
    if not price_model or not price_model["pricing"]:
        failures.append("the price edit did not land at all")
    else:
        if price_model["pricing"]["currency"] != "CNY":
            failures.append(
                "currency reverted to %r: the second price save carried a stale pricing object"
                % price_model["pricing"]["currency"]
            )
        if price_model["pricing"]["input_per_mtok"] != 3:
            failures.append("input price is %r, expected 3" % price_model["pricing"]["input_per_mtok"])

    devtools.evaluate("window.__zrStub.reset()")
    return failures




# The drawer and veil animate in. A browser that paints no frames leaves that
# animation on its first keyframe, where the drawer is still translated 16px and
# half transparent, so nothing inside it can be hit. Interaction is what this
# harness measures, so animation timing is taken out of the picture; the layout
# harness measures the same surfaces with the animation running.
NO_ANIMATION = """
<style>
  *, *::before, *::after {
    animation-duration: 0s !important;
    animation-delay: 0s !important;
    transition-duration: 0s !important;
    transition-delay: 0s !important;
  }
</style>
"""


def stage_page(stage: str) -> None:
    """Copy dist/ and inject the harness into index.html."""
    shutil.copytree(DIST, stage, dirs_exist_ok=True)
    index = os.path.join(stage, "index.html")
    with open(index) as fh:
        page = fh.read()
    harness = (
        NO_ANIMATION
        + HARNESS.replace("__SAVE_DELAY__", "0")
        .replace("__GATEWAY_EVENT__", json.dumps("zroutery://gateway-state-changed"))
        .replace("__CONFIG__", json.dumps(BASE_CONFIG))
        .replace("__SERVER__", json.dumps(SERVER_STATUS))
        .replace("__KEYS__", json.dumps(KEYS))
    )
    page = page.replace("</body>", harness + "</body>")
    with open(index, "w") as fh:
        fh.write(page)


def reload(devtools: DevTools) -> None:
    """Start every case from the same freshly loaded page.

    The first load renders in the system language, so once the harness has had
    a chance to record the English preference the page is loaded again; every
    case then sees the same labels.

    Waiting for the shell is not enough: the previous document satisfies the
    same query until the new one commits, and a case run against the document
    that is on its way out would have no harness. Readiness therefore also
    requires a harness marker, and a reload that loses it is retried.
    """
    devtools.call("Page.enable")
    ready = (
        "document.readyState === 'complete'"
        " && Boolean(window.__zrHarnessReady)"
        " && typeof window.__zrReload === 'function'"
        " && document.querySelector('.nav-item') !== null"
    )
    # At most three loads: the first may still be in the system language, which
    # the harness rewrites for the second, and a load that somehow misses the
    # harness is retried rather than run without it.
    loaded = False
    for attempt in range(3):
        devtools.evaluate("window.location.reload()")
        deadline = time.monotonic() + 25.0
        loaded = False
        while time.monotonic() < deadline:
            try:
                if devtools.evaluate(ready):
                    loaded = True
                    break
            except (ProtocolError, OSError):
                pass
            time.sleep(0.1)
        if not loaded:
            continue
        if attempt == 0 and not devtools.evaluate(
            "/[\\u4e00-\\u9fff]/.test(document.body.textContent)"
        ):
            return
        if attempt > 0:
            return
    raise ProtocolError("the dashboard never came back after a reload")


def case_ui2_candidate_priority(devtools: DevTools) -> list[str]:
    """Moving a candidate up must renumber priority, which is what Rust sorts by."""
    seed(devtools)
    failures: list[str] = []

    devtools.click_selector('[data-page="routing"]')
    # The routing page splits its three subjects into tabs, so the auto-review
    # section is only mounted once its own tab is selected — and once it is, its
    # Edit button is the only `linky` on the page rather than the second.
    devtools.wait_for("Boolean(document.querySelector('#tab-auto'))", "the routing tabs")
    devtools.click_selector("#tab-auto")
    devtools.wait_for("document.querySelector('.flow-row') !== null", "auto review tab")
    devtools.click_selector(".section-head button.linky", 0)
    devtools.wait_for("document.querySelector('.drawer table.table') !== null", "auto drawer")

    before = devtools.evaluate(
        "Array.from(document.querySelectorAll('.drawer table.table tbody tr td.mono'))"
        ".map((td) => td.textContent.trim())"
    )
    if before[:2] != ["deepseek-deepseek-chat", "anthropic-mystery"]:
        failures.append("unexpected pool order before the move: %r" % before)

    # Move the second entry up with a real click on its ↑ button.
    devtools.click_selector(
        "button[aria-label='Move anthropic-mystery up']"
    )
    devtools.wait_for(
        "window.__zrStub.log('save_config').length >= 1", "the move to be saved", timeout=15.0
    )
    time.sleep(0.4)
    saved = devtools.evaluate(
        "(window.__zrStub.last('save_config')||{args:{config:null}}).args.config"
    )
    if not saved:
        return failures + ["no save_config reached the fake backend"]

    candidates = saved["classifier"]["candidates"]
    by_model = {c["model"]: c for c in candidates}
    moved = by_model.get("anthropic-mystery")
    stayed = by_model.get("deepseek-deepseek-chat")
    if moved is None or stayed is None:
        return failures + ["the move lost a candidate: %r" % candidates]
    if not moved["priority"] < stayed["priority"]:
        failures.append(
            "Rust would still try %s first: priorities are %s"
            % (
                "deepseek-deepseek-chat",
                {c["model"]: c["priority"] for c in candidates},
            )
        )

    # The page must draw the same order it just saved, and label it with the
    # priority rule rather than the array position.
    after = devtools.evaluate(
        "Array.from(document.querySelectorAll('.drawer table.table tbody tr td.mono'))"
        ".map((td) => td.textContent.trim())"
    )
    if after[:2] != ["anthropic-mystery", "deepseek-deepseek-chat"]:
        failures.append("the drawer still lists %r after the move" % after)

    devtools.evaluate("window.__zrStub.reset()")
    return failures


def case_ui3_drawer_isolation(devtools: DevTools) -> list[str]:
    """A key typed for one provider must not be saved to another.

    Neither provider has a key when the app loads, which is the state the case
    needs and the state the app reads once at startup.
    """
    failures: list[str] = []
    devtools.evaluate(
        "window.__zrStub.setCatalogues({"
        "'deepseek': [{id: 'deepseek-catalogue-only', pricing: null}],"
        "'anthropic': [{id: 'claude-only', pricing: null}]})"
    )
    key_selector = ".drawer input[type='password']"

    devtools.click_selector('[data-page="providers"]')
    devtools.wait_for("document.querySelector('.list-row') !== null", "providers list")

    # Open provider A and type a key that only A should ever see.
    devtools.click_selector(".list-row", 0)
    devtools.wait_for("document.querySelector('.drawer') !== null", "provider A drawer")
    if devtools.evaluate("document.querySelector(%s) === null" % json.dumps(key_selector)):
        return ["the first provider rendered no key field to type into"]
    devtools.click_selector(key_selector)
    devtools.insert_text("DUMMY_PROVIDER_A_ONLY")
    typed = devtools.evaluate("document.querySelector(%s).value" % json.dumps(key_selector))
    if typed != "DUMMY_PROVIDER_A_ONLY":
        failures.append("real typing did not reach the key field: %r" % typed)

    # While the drawer is open the background must not be reachable at all.
    escaped: list[str] = []
    for _ in range(30):
        devtools.press_tab(False)
        active = devtools.evaluate("window.__zrActive()")
        if not active or not active["inDrawer"]:
            escaped.append(active)
    if escaped:
        failures.append("keyboard focus left the open drawer: %r" % escaped[:2])
    # Focus landing behind the drawer — a click on the veil's edge, or a script
    # that moves it — is pulled back instead of resting on a covered control.
    landed = devtools.evaluate(
        "(() => {"
        "  const row = document.querySelectorAll('.list-row')[1];"
        "  row.focus();"
        "  return window.__zrActive();"
        "})()"
    )
    if landed and not landed["inDrawer"]:
        failures.append("focus stayed on the background row behind the drawer: %r" % landed)

    # Switch providers the way a user can once the background is unreachable:
    # close A (the veil) and open B. The drawer instance changes with it, and
    # nothing of A may travel.
    devtools.click_selector(".drawer-veil")
    devtools.wait_for("document.querySelector('.drawer') === null", "provider A drawer to close")
    devtools.click_selector(".list-row", 1)
    devtools.wait_for("document.querySelector('.drawer') !== null", "provider B drawer")

    title = devtools.evaluate("document.querySelector('.drawer-title').textContent.trim()")
    if "Anthropic" not in title:
        failures.append("expected the second provider's drawer, got %r" % title)
    draft_after = devtools.evaluate(
        "(() => { const el = document.querySelector(%s);"
        " return el ? el.value : '<no key field>'; })()" % json.dumps(key_selector)
    )
    if draft_after != "":
        failures.append("the previous provider's key draft survived the switch: %r" % draft_after)

    # Saving a key for B must carry B's id and B's own draft.
    if devtools.evaluate("document.querySelector(%s) === null" % json.dumps(key_selector)):
        failures.append("the second provider rendered no key field")
    else:
        devtools.click_selector(key_selector)
        devtools.insert_text("DUMMY_PROVIDER_B_ONLY")
        devtools.press_key("Enter")
        devtools.wait_for(
            "window.__zrStub.log('set_provider_key').length >= 1",
            "the key save",
            timeout=15.0,
        )
        call = devtools.evaluate("window.__zrStub.last('set_provider_key')")
        if call["args"]["providerId"] != "anthropic":
            failures.append("the key was saved to %r" % call["args"]["providerId"])
        if call["args"]["apiKey"] != "DUMMY_PROVIDER_B_ONLY":
            failures.append(
                "the key saved for the second provider was %r" % call["args"]["apiKey"]
            )

    # The catalogue importer lives on the Models page now, and the same hazard
    # applies to its provider switch: the answer for the provider that was
    # selected first must not land after another one is selected. The provider
    # drawer is still open, and the veil is the way back out.
    devtools.click_selector(".drawer-veil")
    devtools.wait_for("document.querySelector('.drawer') === null", "the provider drawer to close")
    devtools.click_selector('[data-page="models"]')
    devtools.wait_for("Boolean(document.querySelector('.list-row'))", "the models page")
    devtools.evaluate("window.__zrStub.setDelay('fetch_provider_models', 1500)")
    browse = devtools.evaluate(
        """
        (() => {
          const button = [...document.querySelectorAll('.page button')]
            .find((b) => b.textContent.includes("Browse the provider's catalogue"));
          if (!button) return null;
          const r = button.getBoundingClientRect();
          return {x: r.left + r.width / 2, y: r.top + r.height / 2};
        })()
        """
    )
    if browse is None:
        failures.append("the models page offers no catalogue browser")
        return failures
    devtools.click(browse)
    devtools.wait_for(
        "Boolean(window.__zrStub.last('fetch_provider_models'))",
        "the catalogue request",
        timeout=15.0,
    )
    # Switch providers while the first answer is still in flight.
    devtools.click_selector("[aria-label='Provider']")
    devtools.wait_for("Boolean(document.querySelector('.select-menu'))", "the provider menu")
    devtools.evaluate(
        """
        (() => {
          const item = [...document.querySelectorAll('.select-menu [role="option"]')]
            .find((o) => o.textContent.includes('Anthropic'));
          if (item) item.click();
        })()
        """
    )
    time.sleep(1.8)
    leaked = devtools.evaluate(
        "document.querySelector('.page').innerText.includes('deepseek-catalogue-only')"
    )
    if leaked:
        failures.append(
            "the catalogue A discovered was rendered under provider B on the models page"
        )

    devtools.evaluate("window.__zrStub.reset()")
    return failures


def case_ui4_tray_state(devtools: DevTools) -> list[str]:
    """A tray state change reaches the page, and the button issues the right command."""
    seed(devtools, server={**SERVER_STATUS, "running": True})
    failures: list[str] = []

    # The page must be listening for the tray's announcement.
    listeners = devtools.evaluate(
        "window.__zrListenerCount('zroutery://gateway-state-changed')"
    )
    if not listeners:
        failures.append("the page never subscribed to the tray's state-change event")

    devtools.click_selector(".bar-action", 0)
    devtools.wait_for("document.querySelector('.popover') !== null", "gateway menu")
    if not devtools.evaluate("document.querySelectorAll('.popover .menu-item').length >= 3"):
        failures.append("the gateway menu did not render its actions")
    if not devtools.evaluate("Boolean(document.querySelector('.popover .dot-ok'))"):
        failures.append("the running gateway did not render a running dot")

    # The tray stops the gateway behind the page's back and announces it. The
    # dot is painted from `server.running`, so its class is the language
    # independent view of the state the page is showing.
    devtools.evaluate("window.__zrStub.setServer({running: false})")
    delivered = devtools.evaluate("window.__zrEmit('zroutery://gateway-state-changed', null)")
    if not delivered:
        failures.append("the announcement reached no listener")
    try:
        devtools.wait_for(
            "Boolean(document.querySelector('.popover .dot-danger'))",
            "the open menu to follow the tray",
            timeout=10.0,
        )
    except ProtocolError:
        failures.append(
            "the page kept the old gateway state: the menu still shows %r"
            % devtools.evaluate("document.querySelector('.popover').textContent")[:80]
        )

    # The button must now start, not stop.
    devtools.click_selector(".popover .menu-item", 2)
    devtools.wait_for(
        "window.__zrStub.log('start_proxy').length + window.__zrStub.log('stop_proxy').length >= 1",
        "a gateway command",
        timeout=15.0,
    )
    time.sleep(0.3)
    starts = devtools.evaluate("window.__zrStub.log('start_proxy').length")
    stops = devtools.evaluate("window.__zrStub.log('stop_proxy').length")
    if starts != 1 or stops != 0:
        failures.append(
            "after the tray stopped the gateway the toggle issued start=%d stop=%d" % (starts, stops)
        )
    devtools.wait_for(
        "Boolean(document.querySelector('.bar-action .dot-ok'))",
        "the main bar to follow the command",
        timeout=10.0,
    )

    # The command is chosen from the state Rust reports, not from the boolean
    # the page happens to be drawing: the menu is opened while the page still
    # believes the gateway runs, and the backend has already stopped it. No
    # event announces this one.
    seed(devtools, server={**SERVER_STATUS, "running": True}, restart=True)
    devtools.evaluate("window.__zrStub.reset(); window.__zrStub.setServer({running: false})")
    devtools.click_selector(".bar-action", 0)
    devtools.wait_for("document.querySelector('.popover') !== null", "gateway menu")
    if not devtools.evaluate("Boolean(document.querySelector('.popover .dot-ok'))"):
        failures.append("the popover did not render the state the page was holding")
    devtools.click_selector(".popover .menu-item", 2)
    devtools.wait_for(
        "window.__zrStub.log('start_proxy').length + window.__zrStub.log('stop_proxy').length >= 1",
        "a gateway command from a changed backend",
        timeout=15.0,
    )
    starts = devtools.evaluate("window.__zrStub.log('start_proxy').length")
    stops = devtools.evaluate("window.__zrStub.log('stop_proxy').length")
    if starts != 1 or stops != 0:
        failures.append(
            "with the backend already stopped the toggle issued start=%d stop=%d" % (starts, stops)
        )

    devtools.evaluate("window.__zrStub.reset()")
    return failures


def case_ui5_refused_round_shows_its_criteria(devtools: DevTools) -> list[str]:
    """A refused promotion must name the criterion that refused it.

    The backend has always sent the gate's per-criterion evidence; the panel used
    to render one word, ``blocked``, which is the verdict and not the reason. An
    operator who reads "blocked" cannot tell a model that was judged and lost from
    a body that was too small to judge, and those send them to different places.

    The click is a real mouse event on the button the operator would press, and
    every assertion is read out of the rendered text rather than from component
    state, so a panel that received the data and did not draw it fails here.
    """
    failures: list[str] = []
    seed(devtools)
    # The learned panel is the first of the three routing subjects, so the path
    # to it is a page and then a tab. Clicking the page alone leaves the panel
    # unmounted and the judge button nowhere.
    devtools.click_selector('[data-page="routing"]')
    devtools.wait_for(
        "Boolean(document.querySelector('#tab-ml'))", "the learned-routing tab"
    )
    devtools.click_selector("#tab-ml")
    devtools.wait_for(
        "document.body.innerText.includes('Judge a candidate')", "the learning panel"
    )

    def box_for(expression: str) -> dict | None:
        # Scrolled into view first: the panel is taller than the viewport, and a
        # click at the coordinates of an element below the fold lands on whatever
        # is there instead.
        return devtools.evaluate(
            "(() => {"
            "  const el = %s;"
            "  if (!el) return null;"
            "  el.scrollIntoView({ block: 'center' });"
            "  const r = el.getBoundingClientRect();"
            "  return { x: r.left + r.width / 2, y: r.top + r.height / 2 };"
            "})()" % expression
        )

    judge = box_for(
        "Array.from(document.querySelectorAll('button'))"
        ".find((b) => b.textContent.trim() === 'Judge a candidate')"
    )
    if not judge:
        return ["the round's judge button is not on the page"]
    devtools.click(judge)

    devtools.wait_for(
        "window.__zrStub.log('run_ml_promotion_round').length >= 1",
        "the round to reach the backend",
        timeout=15.0,
    )
    # The criteria sit behind a disclosure that is closed until someone asks why,
    # so the verdict is the first thing read and the evidence is one click away.
    guard = box_for(
        "Array.from(document.querySelectorAll('.disclosure-head'))"
        ".find((b) => b.textContent.includes('Gate criteria'))"
    )
    if not guard:
        return ["the refused round has no gate-criteria disclosure"]
    devtools.click(guard)
    devtools.wait_for(
        "document.body.innerText.includes('Paired evidence')",
        "the refused round's criteria",
        timeout=10.0,
    )

    body = devtools.evaluate("document.body.innerText")
    for needle in (
        "blocked",
        # The gate reports `paired_evidence`; the panel must name it in words a
        # reader recognises, from the id, not print the id.
        "Paired evidence",
        # Both numbers, and the unit the criterion is counted in. A threshold
        # rendered as "+30" would mean a delta, which this is not.
        "12 measured / 30 required",
        "12 effective holdout decisions against a floor of 30",
    ):
        if needle not in body:
            failures.append("the refused round never showed %r" % needle)

    # The criterion that held is evidence, not the answer. Its own prose would
    # restate the pass beside a refusal, so the panel drops the reason on a row
    # that held rather than printing agreement under it.
    if "34 decisions carry the fitted dataset fingerprint" in body:
        failures.append("a passing criterion printed its reason beside the refusal")

    header = devtools.evaluate(
        "(() => {"
        "  const head = Array.from(document.querySelectorAll('.disclosure-head'))"
        "    .find((b) => b.textContent.includes('Gate criteria'));"
        "  return head ? head.textContent : '';"
        "})()"
    )
    if not header or "1/2" not in header:
        failures.append("the disclosure did not count the criteria that held: %r" % header)

    devtools.evaluate("window.__zrStub.reset()")
    return failures


def connect(debugging_port: int, url: str) -> DevTools:
    """Attach to the page the browser opened, retrying while it starts."""
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        socket_url = page_target(debugging_port, url, timeout=5.0)
        if socket_url:
            return DevTools(socket_url)
        time.sleep(0.2)
    raise ProtocolError("the browser exposed no DevTools page target")


# The accounts-page fixture: one provider with two declared accounts, a probe
# answer covering both, one check-in view, and a balance reading. `api` turns on
# the direct check-in button, which is the command this case exists to exercise.
# The accounts-page fixture: one provider with two declared accounts, a probe
# answer covering both, one check-in view, and a balance reading. `api` turns on
# the direct check-in button, which is the command this case exists to exercise.
# The declarations ride inside the account payload and merge into the default
# configuration — see `snapshot()` in the harness; a whole-config seed would have
# to spell out every field the shell reads and would drift the moment the
# configuration grew one.
ACCOUNT_QUERY = {
    "account": {
        "api": True,
        "token": True,
        "providerAccounts": {
            "providerId": "deepseek",
            "accounts": [
                {
                    "account_id": "main",
                    "enabled": True,
                    "key_ref": "provider:deepseek:account:main",
                    "maintenance": {
                        "checkin_enabled": True,
                        "checkin_interval_secs": 86400,
                        "checkin_path": "/console/personal",
                        "checkin_trigger_selector": "button.checkin",
                        "login_path": None,
                        "browser_executable": "",
                    },
                },
                {
                    "account_id": "spare",
                    "enabled": False,
                    "key_ref": "",
                    "maintenance": {
                        "checkin_enabled": False,
                        "checkin_interval_secs": None,
                        "checkin_path": None,
                        "checkin_trigger_selector": None,
                        "login_path": None,
                        "browser_executable": "",
                    },
                },
            ],
        },
        "outcomes": [
            ["deepseek", "main", "refreshed"],
            ["deepseek", "spare", "unsupported"],
        ],
        "balances": [
            {
                "provider_id": "deepseek",
                "account_id": "main",
                "status": "active",
                "remaining": 12.5,
                "total": 100.0,
                "used": 87.5,
                "unit": "USD",
                "last_sync": 1_700_000_000,
                "last_failure": None,
            }
        ],
        "checkin": [
            {
                "provider_id": "deepseek",
                "account_id": "main",
                "phase": "idle",
                "browser_held": False,
                "awaiting_user": False,
                "failure": None,
                "reward_amount": None,
                "reward_unit": None,
                "reward_source": None,
                "last_completed_at": None,
                "schedule": {
                    "decision": "not_due",
                    "next_due_at": None,
                    "last_settled_at": None,
                },
            }
        ],
        "events": [],
    }
}


def case_ui6_accounts_direct_checkin(devtools: DevTools) -> list[str]:
    """The accounts page renders declarations as readings, and direct check-in acts.

    Three things the page owes the operator, all asserted out of rendered text:
    a probed account shows its status and balance rather than nothing (the old
    data layer hid single-account providers entirely, which is how a probe could
    run and the panel stay empty); the direct check-in button calls the backend
    with the right account and renders the phase and reward that came back; and
    the probe's report distinguishes an unsupported account from a refreshed one
    instead of merging them into one verdict.
    """
    failures: list[str] = []
    seed(devtools, **ACCOUNT_QUERY)
    devtools.click_selector('[data-page="accounts"]')
    devtools.wait_for(
        "Boolean(document.querySelector('.account-maint'))", "the accounts page rows"
    )

    # Two declared accounts, rendered regardless of how many the provider has.
    # The single-account blackout is the defect that motivated the page.
    rows = devtools.evaluate("document.querySelectorAll('.account-maint').length")
    if rows != 2:
        failures.append("expected 2 account rows, saw %d" % rows)

    # The probe reading: status and balance in the row's own text. The balance
    # is the panel's own sentence now, not the wire vocabulary's.
    body = devtools.evaluate("document.querySelector('.page').innerText")
    for needle in ("active", "12.5 USD left of 100"):
        if needle not in body:
            failures.append("the probed reading %r is not rendered" % needle)

    # The direct check-in: a real press on the button the operator would use,
    # then the phase and reward the backend answered, rendered in the row.
    devtools.evaluate(
        """
        (() => {
          const buttons = [...document.querySelectorAll('.account-maint .controls button')];
          const direct = buttons.find((b) => b.textContent.includes('Check in (direct)'));
          direct.click();
        })()
        """
    )
    devtools.wait_for(
        "window.__zrStub.state.commandLog.some("
        "(e) => e.cmd === 'api_checkin' && e.args.providerId === 'deepseek'"
        " && e.args.accountId === 'main')",
        "the direct check-in command",
    )
    devtools.wait_for(
        "document.querySelector('.page').innerText.includes('checked in')",
        "the answered phase",
    )
    if "reward 25 USD" not in devtools.evaluate("document.querySelector('.page').innerText"):
        failures.append("the observed reward is not rendered after the check-in")

    # The probe report, with its four-way outcome kept apart.
    devtools.evaluate(
        """
        (() => {
          const buttons = [...document.querySelectorAll('.page button')];
          const probe = buttons.find((b) => b.textContent.includes('Probe every account'));
          probe.click();
        })()
        """
    )
    devtools.wait_for(
        "window.__zrStub.state.commandLog.some((e) => e.cmd === 'reconcile_accounts')",
        "the reconcile command",
    )
    body = devtools.evaluate("document.querySelector('.page').innerText")
    # The two outcomes render as two different sentences — the refreshed row
    # says "Probed", the unsupported one says no backend could ask it — which is
    # the distinction the report exists to preserve.
    if "Probed" not in body or "not probed" not in body:
        failures.append(
            "the probe report does not keep the refreshed and unsupported rows apart"
        )
    return failures


# The cutover seed, in the one shape a record starts in. A fresh record is
# `detected`, which is the only state that admits a run — the state machine takes
# Detected -> Prepared once — so this is what the run path has to be driven from.
def _cutover_query(status, **extra):
    return {
        "migration": {
            "status": status,
            "previews": [
                {
                    "agent": agent,
                    "config_path": path,
                    "fingerprint": "fp-" + agent,
                    "blocked_reason": None,
                    "steps": [
                        {
                            "kind": kind,
                            "description": description,
                            "reversible": kind == "copy_config",
                            "params": params,
                        }
                        for kind, description, params in (
                            # The descriptions here deliberately leave the
                            # values out, because the gate is that the window
                            # words the step with them. A fixture whose
                            # description already named the port and the file
                            # would make the window's sentence and the
                            # backend's sentence the same string, and a gate
                            # that cannot tell them apart is a gate that
                            # cannot fail.
                            (
                                "stop_external",
                                "check whether the client is still running",
                                {
                                    "subject": agent,
                                    "port": None,
                                    "path": None,
                                    "url": None,
                                },
                            ),
                            (
                                "start_zroutery",
                                "check that the port is free",
                                {
                                    "subject": None,
                                    "port": 8787,
                                    "path": None,
                                    "url": None,
                                },
                            ),
                            (
                                "copy_config",
                                "install the staged document",
                                {
                                    "subject": None,
                                    "port": None,
                                    "path": path,
                                    "url": None,
                                },
                            ),
                            (
                                "verify_endpoint",
                                "confirm the endpoint answers as Zroutery",
                                {
                                    "subject": None,
                                    "port": None,
                                    "path": None,
                                    "url": "http://127.0.0.1:8787/health",
                                },
                            ),
                        )
                    ],
                }
                for agent, path in (
                    ("claude", "C:/Users/dev/.claude/settings.json"),
                    ("codex", "C:/Users/dev/.codex/config.toml"),
                    ("opencode", "C:/Users/dev/.config/opencode/opencode.json"),
                )
            ]
            + [
                {
                    "agent": "gemini",
                    "config_path": None,
                    "fingerprint": "fp-gemini",
                    "blocked_reason": "this build has no migration adapter for gemini",
                    "steps": [],
                }
            ],
            **extra,
        }
    }


# A provider that publishes a catalogue, so the drawer's catalogue table has a
# row in it and the tier dropdown on that row can be read.
PROVIDER_CATALOGUE_QUERY = {
    "catalogues": {
        "deepseek": [
            {
                "id": "deepseek-chat",
                "pricing": {"input": 0.001, "output": 0.002},
                "free": False,
            }
        ]
    }
}

CUTOVER_QUERY = _cutover_query(
    {
        "state": "detected",
        "interrupted": False,
        "has_snapshot": False,
        "snapshot_files": [],
        "durable": True,
        "persist_error": None,
        "owned_processes": [],
    }
)

# The same page after a run that failed partway. This is the one state that
# permits a rollback, so it is the state the undo has to be driven from, and the
# answer that matters is the one where the machine reached RolledBack and no
# byte came back: the defect this whole surface is built against is a panel that
# reads `state` and tells the user their file was restored.
CUTOVER_FAILED_QUERY = _cutover_query(
    {
        "state": "failed",
        "interrupted": True,
        "has_snapshot": True,
        "snapshot_files": ["C:/Users/dev/.claude/settings.json"],
        "durable": True,
        "persist_error": None,
        "owned_processes": [],
    },
    rollbackAnswer={
        "state": "rolled_back",
        "steps_completed": 0,
        "steps_total": 0,
        "errors": [],
        "warnings": [],
        "duration_ms": 3,
        "rolled_back": False,
        "restored_files": [],
        "failed_restores": [],
    },
)


def case_ui7_cutover_confirmation_and_honest_rollback(devtools) -> list[str]:
    """Running a cutover, driven the way an operator drives it.

    Four properties, each of which the page can get wrong in a way a rendering
    fixture would not notice:

    1. A client this build cannot migrate renders a reason and *no* button, so
       the page never offers a control it cannot honour.
    2. Running a plan goes through a confirmation that names what it replaces.
    3. Nothing runs before the confirmation is answered.
    4. The run carries the preview's fingerprint to the backend. Without that
       argument the dialog is decoration: the plan the user read and the plan
       that runs are then only assumed to be the same one.
    """
    failures: list[str] = []
    seed(devtools, **CUTOVER_QUERY)
    devtools.click_selector('[data-page="cutover"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the cutover rows")

    # (1) Every agent is a row; the unmigratable one explains itself and offers
    # nothing to click.
    if devtools.evaluate("document.querySelectorAll('.page .list-row').length") != 5:
        failures.append(
            "expected 4 plan rows plus the record row, saw %d"
            % devtools.evaluate("document.querySelectorAll('.page .list-row').length")
        )
    body = devtools.evaluate("document.querySelector('.page').innerText")
    if "no migration adapter for gemini" not in body:
        failures.append("the client this build cannot migrate does not say why")

    gemini_buttons = devtools.evaluate(
        """
        (() => {
          const rows = [...document.querySelectorAll('.page .list-row')];
          const row = rows.find((r) => r.textContent.includes('gemini'));
          return row ? row.querySelectorAll('button').length : -1;
        })()
        """
    )
    if gemini_buttons != 0:
        failures.append(
            "the blocked client renders %d button(s); it must render none" % gemini_buttons
        )

    # (5) The plan is worded in the reader's language, with the values the
    # backend sent. The fixture's descriptions deliberately omit those values,
    # so a page that rendered the backend's English sentence instead of its own
    # would show a sentence with no port and no file in it — which is what
    # this checks for, and why the sentences are read out of the table's cells
    # rather than out of the page: the page also shows the client's own path,
    # and a whole-page search would find it there and pass for the wrong
    # reason.
    cells = devtools.evaluate(
        """
        [...document.querySelectorAll('.page table td')].map((td) => td.textContent.trim())
        """
    )
    for expected in (
        "claude is still running",
        "port 8787 is free",
        "install the staged document over C:/Users/dev/.claude/settings.json",
        "confirm http://127.0.0.1:8787/health answers as Zroutery",
    ):
        if not any(expected in cell for cell in cells):
            failures.append(
                "the plan does not word the step with the values the backend sent: %r is in no "
                "cell of %r" % (expected, cells)
            )
    for backend_sentence in (
        "check whether the client is still running",
        "check that the port is free",
    ):
        if any(backend_sentence in cell for cell in cells):
            failures.append(
                "the plan shows the backend's own sentence (%r) instead of wording the step"
                % backend_sentence
            )

    # (2) One live run button per migratable client, and none for the blocked one.
    # Checked before anything runs: once the stub has moved the record to
    # `completed` the page correctly retires them all, so asserting afterwards
    # would assert the opposite of what is wanted.
    enabled_now = devtools.evaluate(
        """
        [...document.querySelectorAll('.page button')]
          .filter((b) => b.textContent.includes('Run this cutover'))
          .filter((b) => !b.disabled)
          .length
        """
    )
    if enabled_now != 3:
        failures.append(
            "a record in `detected` must offer one live run button per migratable client (three of "
            "the four agents; gemini has no adapter); saw %d" % enabled_now
        )

    # (3) and (4) The run goes through a confirmation that carries the fingerprint.
    devtools.evaluate(
        """
        (() => {
          const rows = [...document.querySelectorAll('.page .list-row')];
          const row = rows.find((r) => r.textContent.includes('claude'));
          [...row.querySelectorAll('button')]
            .find((b) => b.textContent.includes('Run this cutover'))
            .click();
        })()
        """
    )
    devtools.wait_for("Boolean(document.querySelector('.confirm'))", "the cutover confirmation")
    if "Replace Claude Code's configuration?" not in devtools.evaluate(
        "document.querySelector('.confirm').innerText"
    ):
        failures.append("the confirmation does not name what it is about to replace")
    if devtools.evaluate(
        "window.__zrStub.state.commandLog.some((e) => e.cmd === 'migration_apply')"
    ):
        failures.append("the run was sent before the confirmation was answered")

    devtools.evaluate(
        """
        (() => {
          [...document.querySelectorAll('.confirm-actions button')]
            .find((b) => b.textContent.includes('Run this cutover'))
            .click();
        })()
        """
    )
    devtools.wait_for(
        "window.__zrStub.state.commandLog.some((e) => e.cmd === 'migration_apply')",
        "the cutover command",
    )
    sent = devtools.evaluate(
        "JSON.stringify(window.__zrStub.state.commandLog.filter((e) => e.cmd === 'migration_apply')[0].args)"
    )
    if "fp-claude" not in sent:
        failures.append(
            "the confirmed run did not carry the preview's fingerprint, so the plan the user read "
            "is not provably the plan that ran: %s" % sent
        )

    # (4) The record admits one run, so there is no second one to offer.
    enabled = devtools.evaluate(
        """
        [...document.querySelectorAll('.page button')]
          .filter((b) => b.textContent.includes('Run this cutover'))
          .filter((b) => !b.disabled)
          .length
        """
    )
    if enabled != 0:
        failures.append(
            "%d run button(s) are still live after the record ran; the machine takes Detected -> "
            "Prepared once" % enabled
        )
    return failures


def case_ui8_cutover_reports_a_bare_rollback_honestly(devtools) -> list[str]:
    """The undo, driven from the only state that permits one.

    The record is seeded `failed` with a snapshot, because `Failed -> RolledBack`
    is the only transition the state machine allows and a fixture in any other
    state would let the page pass while offering no undo at all.

    The answer the stub gives back is the one that matters: the machine reached
    `RolledBack` and no byte came back. `rollback_with` ignores a refused
    transition and restores the files anyway, and `MigrationResult::rolled_back`
    exists precisely because the state machine cannot tell the two apart — so a
    panel that reads `state` tells a user their configuration was restored when
    it was not. This is the assertion the page exists for.

    Split from UI-7 because a case is seeded by its URL and a reload re-seeds
    from that URL: one case cannot hold two different records.
    """
    failures: list[str] = []
    seed(devtools, **CUTOVER_FAILED_QUERY)
    devtools.click_selector('[data-page="cutover"]')
    devtools.wait_for(
        "document.querySelector('.page').innerText.includes('failed')", "the failed record"
    )

    # The snapshot is what makes a rollback possible, and the record row has to
    # say there is one rather than leaving the button unexplained.
    if "1 file(s) captured" not in devtools.evaluate("document.querySelector('.page').innerText"):
        failures.append("the record row does not say what the snapshot holds")

    # A record that has run offers no second run: the machine takes
    # Detected -> Prepared once, so a second run would be refused by the engine
    # only after the snapshot was taken.
    enabled = devtools.evaluate(
        """
        [...document.querySelectorAll('.page button')]
          .filter((b) => b.textContent.includes('Run this cutover'))
          .filter((b) => !b.disabled)
          .length
        """
    )
    if enabled != 0:
        failures.append(
            "%d run button(s) are still live on a record that has already run" % enabled
        )

    # The rollback goes through a confirmation that names what it will overwrite.
    devtools.evaluate(
        """
        (() => {
          const row = [...document.querySelectorAll('.page .list-row')].find((r) =>
            r.textContent.includes('failed'),
          );
          [...row.querySelectorAll('button')].find((b) => b.textContent.includes('Roll back')).click();
        })()
        """
    )
    devtools.wait_for("Boolean(document.querySelector('.confirm'))", "the rollback confirmation")
    if "Put the configuration file back?" not in devtools.evaluate(
        "document.querySelector('.confirm').innerText"
    ):
        failures.append("the rollback confirmation does not name what it will overwrite")
    if devtools.evaluate(
        "window.__zrStub.state.commandLog.some((e) => e.cmd === 'migration_rollback')"
    ):
        failures.append("the rollback was sent before the confirmation was answered")

    devtools.evaluate(
        """
        (() => {
          [...document.querySelectorAll('.confirm-actions button')]
            .find((b) => b.textContent.includes('Roll back'))
            .click();
        })()
        """
    )
    devtools.wait_for(
        "window.__zrStub.state.commandLog.some((e) => e.cmd === 'migration_rollback')",
        "the rollback command",
    )
    devtools.wait_for(
        "document.querySelector('.page').innerText.includes('Last run')", "the run report"
    )
    body = devtools.evaluate("document.querySelector('.page').innerText")
    if "Nothing was restored" not in body:
        failures.append(
            "a rollback that restored no file is not reported as having restored none, even though "
            "the state machine says RolledBack — the page is reading state instead of rolled_back"
        )
    return failures


def case_ui9_confirm_dialog_takes_focus_and_gives_it_back(devtools) -> list[str]:
    """A modal that takes focus must give it back.

    Two halves, and the second one is the one that was missing. The dialog
    focuses its confirm button so a keyboard user starts inside it, and when
    it closes the focus has to go back to the button that opened it.

    Without the second half, dismissing the dialog with Escape or with Cancel
    leaves the focus on the document body, and the next Tab starts from the
    top of the page — a long way from the control the user had just used, and
    nothing on screen says that is what happened. `Drawer` has always done
    this; the dialog now does too.
    """
    failures: list[str] = []
    seed(devtools, **CUTOVER_QUERY)
    devtools.click_selector('[data-page="cutover"]')
    devtools.wait_for(
        "Boolean(document.querySelector('.page .list-row'))", "the cutover rows"
    )

    # A real mouse click, so the button genuinely holds the focus when the
    # dialog opens. A DOM `.click()` does not move the focus, and the case
    # would then be testing a restore to whatever happened to be focused
    # before — which is the defect this is about.
    point = devtools.evaluate(
        """
        (() => {
          const rows = [...document.querySelectorAll('.page .list-row')];
          const row = rows.find((r) => r.textContent.includes('claude'));
          const button = [...row.querySelectorAll('button')]
            .find((b) => b.textContent.includes('Run this cutover'));
          if (!button) return null;
          button.scrollIntoView({block: 'center', inline: 'center'});
          const r = button.getBoundingClientRect();
          return {x: r.left + r.width / 2, y: r.top + r.height / 2};
        })()
        """
    )
    if point is None:
        failures.append("the cutover page offered no run button to press")
        return failures
    devtools.click(point)
    devtools.wait_for("Boolean(document.querySelector('.confirm'))", "the confirm dialog")

    inside = devtools.evaluate(
        "document.querySelector('.confirm').contains(document.activeElement)"
    )
    if not inside:
        failures.append(
            "the dialog opened without taking the focus; a keyboard user would be typing into "
            "the page behind it"
        )

    devtools.press_key("Escape")
    devtools.wait_for("document.querySelector('.confirm') === null", "the dialog to close")

    active = devtools.evaluate(
        "document.activeElement === document.body ? 'body' : (document.activeElement.textContent || '').trim()"
    )
    if active == "body":
        failures.append(
            "the dialog closed and left the focus on the document body; the next Tab would start "
            "from the top of the page instead of where the user was"
        )
    elif "Run this cutover" not in active:
        failures.append(
            "the dialog closed and did not give the focus back to the button that opened it; "
            "it went to %r instead" % active
        )
    return failures


def case_ui10_the_menu_moves_with_the_arrow_keys(devtools) -> list[str]:
    """A menu role is a promise about the behaviour.

    The popover claimed `role="menu"` with `role="menuitem"` children, and a
    screen reader announced it as a menu while the arrow keys did what arrow
    keys do everywhere else: move the page. The role has to be backed by the
    behaviour, so the arrow keys move between the items, the focus starts
    inside the menu, and Escape gives the focus back to the trigger.
    """
    failures: list[str] = []
    seed(devtools, **CUTOVER_QUERY)
    devtools.click_selector(".mainbar .popover-anchor .bar-action")
    devtools.wait_for("Boolean(document.querySelector('.popover'))", "the menu")

    inside = devtools.evaluate(
        "document.querySelector('.popover').contains(document.activeElement)"
    )
    if not inside:
        failures.append("the menu opened without taking the focus")

    first = devtools.evaluate(
        "document.activeElement.getAttribute('role') || ''"
    )
    if first != "menuitem":
        failures.append(
            "the menu did not start on a menu item; focus was on %r" % first
        )

    # Down has to move, and it has to move to the *next* item rather than
    # staying put.
    before = devtools.evaluate("document.activeElement.textContent.trim()")
    devtools.press_key("ArrowDown")
    after = devtools.evaluate("document.activeElement.textContent.trim()")
    if before == after:
        failures.append(
            "ArrowDown did not move the focus between menu items; it stayed on %r" % before
        )

    # Up has to come back.
    devtools.press_key("ArrowUp")
    back = devtools.evaluate("document.activeElement.textContent.trim()")
    if back != before:
        failures.append(
            "ArrowUp did not return the focus to the previous item; it went to %r instead of %r"
            % (back, before)
        )

    devtools.press_key("Escape")
    devtools.wait_for("document.querySelector('.popover') === null", "the menu to close")
    return failures


def case_ui11_a_confirmation_inside_a_drawer_does_not_fight_it(devtools) -> list[str]:
    """A modal on top of a modal, which is the case that used to recurse.

    The provider drawer and the confirm dialog both pull the focus back when it
    lands outside them, and their roots are not nested: the drawer is a panel,
    the confirmation is rendered by the page. Two guards that each pull the
    focus to their own root, with roots that differ, fight each other — the
    confirmation focuses its button, the drawer pulls it back, the confirmation
    pulls it again, and the recursion only ends when the stack does.

    So a confirmation raised from inside a drawer is the case to drive, and it
    is the path to both of the app's irreversible actions.

    The second half is the same sequence: Escape there must close the
    confirmation and leave the drawer open. One keypress, one thing closed.
    """
    failures: list[str] = []
    seed(devtools, **CUTOVER_QUERY)
    devtools.click_selector('[data-page="providers"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the provider rows")

    # Open the drawer with a real click, so it holds the focus the way it does
    # for a person.
    point = devtools.evaluate(
        """
        (() => {
          const row = document.querySelector('.page .list-row');
          if (!row) return null;
          const r = row.getBoundingClientRect();
          return {x: r.left + r.width / 2, y: r.top + r.height / 2};
        })()
        """
    )
    if point is None:
        failures.append("the providers page offered no row to open")
        return failures
    devtools.click(point)
    devtools.wait_for("Boolean(document.querySelector('.drawer'))", "the provider drawer")

    # Now raise a confirmation from inside it.
    devtools.evaluate(
        """
        (() => {
          const drawer = document.querySelector('.drawer');
          const button = [...drawer.querySelectorAll('button')]
            .find((b) => b.textContent.includes('Remove provider'));
          if (button) button.click();
        })()
        """
    )
    devtools.wait_for("Boolean(document.querySelector('.confirm'))", "the confirmation")

    inside = devtools.evaluate(
        "document.querySelector('.confirm').contains(document.activeElement)"
    )
    if not inside:
        failures.append(
            "the confirmation opened without taking the focus; the drawer and the confirmation are "
            "pulling it from each other"
        )
    if not devtools.evaluate("Boolean(document.querySelector('.drawer'))"):
        failures.append("the drawer closed when the confirmation opened")

    # Escape closes the confirmation, and only the confirmation.
    devtools.press_key("Escape")
    devtools.wait_for("document.querySelector('.confirm') === null", "the confirmation to close")
    if not devtools.evaluate("Boolean(document.querySelector('.drawer'))"):
        failures.append(
            "one Escape closed the confirmation AND the drawer; one keypress should close one thing"
        )
    return failures


def case_ui12_a_deep_link_opens_the_page_it_names(devtools) -> list[str]:
    """A `zroutery://` link opens the page it names.

    The desktop shell brings the window forward and hands the URL over; the
    window turns the URL into a page. A link that named a page this build does
    not have must open none, and a link in another scheme must be ignored
    entirely — the shell only emits its own, but a link is user input and the
    boundary is where that matters.
    """
    failures: list[str] = []
    seed(devtools, **CUTOVER_QUERY)

    delivered = devtools.evaluate(
        "window.__zrEmit('zroutery://deep-link', 'zroutery://providers')"
    )
    if not delivered:
        failures.append(
            "nothing was listening for the deep-link event, so a link could not open anything"
        )
    devtools.wait_for(
        "document.querySelector('.page').innerText.includes('端点') "
        "|| document.querySelector('.page').innerText.includes('Base URL')",
        "the providers page",
    )
    if not devtools.evaluate(
        "Boolean(document.querySelector('[data-page=\"providers\"]')?.getAttribute('aria-current'))"
    ):
        failures.append("the link did not move the navigation to the page it named")

    # A link that names a page this build does not have opens none.
    before = devtools.evaluate(
        "document.querySelector('.nav-item.active')?.getAttribute('data-page')"
    )
    devtools.evaluate(
        "window.__zrEmit('zroutery://deep-link', 'zroutery://no-such-page')"
    )
    after = devtools.evaluate(
        "document.querySelector('.nav-item.active')?.getAttribute('data-page')"
    )
    if after != before:
        failures.append(
            "a link naming a page this build does not have moved the dashboard to %r" % after
        )

    # A link in another scheme is not ours to act on.
    devtools.evaluate(
        "window.__zrEmit('zroutery://deep-link', 'https://example.com/providers')"
    )
    other = devtools.evaluate(
        "document.querySelector('.nav-item.active')?.getAttribute('data-page')"
    )
    if other != before:
        failures.append("a link in another scheme moved the dashboard to %r" % other)
    return failures


def case_ui13_a_catalogue_row_says_no_tier_rather_than_a_dash(devtools) -> list[str]:
    """The catalogue import's batch tier, and the empty string it used to mean.

    Radix cannot show an option whose value is the empty string *and* show its
    placeholder at the same time: the placeholder is what an empty value draws.
    So an option that meant "no tier" and carried the empty string was never
    shown — the dropdown said a generic dash while the option itself was
    marked as the chosen one.

    The fix gives that option a sentinel of its own, and the control now says
    what it means. Catalogue import lives on the Models page, so the case opens
    the import panel there and reads the batch tier.
    """
    failures: list[str] = []
    seed(devtools, **PROVIDER_CATALOGUE_QUERY)
    devtools.click_selector('[data-page="models"]')
    devtools.wait_for("Boolean(document.querySelector('.list-row'))", "the model rows")

    devtools.evaluate(
        """
        (() => {
          const b = [...document.querySelectorAll('button')]
            .find((x) => x.textContent.includes("Browse the provider's catalogue"));
          if (b) b.click();
        })()
        """
    )
    devtools.wait_for(
        "Boolean(document.querySelector('[aria-label=\"Add selected as\"]'))",
        "the batch tier control",
        timeout=15.0,
    )
    tier = devtools.evaluate(
        "(() => { const t = document.querySelector('[aria-label=\"Add selected as\"]');"
        " return t ? t.textContent.trim() : null; })()"
    )
    if tier is None:
        failures.append("the catalogue import carried no batch tier control")
    elif "no tier" not in tier:
        failures.append(
            "the tier dropdown says %r where it should say which tier the batch is added as; an "
            "option whose value is the empty string can never be shown" % tier
        )
    return failures


def case_ui14_a_select_inside_a_drawer_keeps_its_focus(devtools) -> list[str]:
    """A dropdown inside a drawer, opened and navigated with the keyboard.

    Radix portals a select's menu to the document body, which puts it outside
    the drawer's own root — and the drawer pulls the focus back whenever it
    lands outside. Two focus managers, two roots, and the menu is in neither.

    The question is whether the keyboard can still move through the options.
    This drives it: open a drawer, open a dropdown in it, press the arrow key
    that moves to the next option, and read where the focus ended up. It has to
    be on a menu item — on the drawer, the menu is open but the keyboard has
    been thrown out of it.
    """
    failures: list[str] = []
    seed(devtools, **CUTOVER_QUERY)
    devtools.click_selector('[data-page="providers"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the provider rows")
    devtools.click_selector(".page .list-row")
    devtools.wait_for("Boolean(document.querySelector('.drawer'))", "the provider drawer")

    # Open a dropdown that lives inside the drawer.
    point = devtools.evaluate(
        """
        (() => {
          const trigger = document.querySelector('.drawer [role="combobox"]');
          if (!trigger) return null;
          // The provider drawer is taller than the window; a coordinate
          // measured before scrolling lands on whatever the control's
          // off-screen position happens to overlap.
          trigger.scrollIntoView({block: 'center'});
          const r = trigger.getBoundingClientRect();
          return {x: r.left + r.width / 2, y: r.top + r.height / 2};
        })()
        """
    )
    if point is None:
        failures.append("the drawer offered no dropdown to open")
        return failures
    devtools.click(point)
    devtools.wait_for("Boolean(document.querySelector('.select-menu'))", "the dropdown menu")

    # The arrow key that moves to the next option.
    devtools.press_key("ArrowDown")
    landed = devtools.evaluate(
        """
        (() => {
          const active = document.activeElement;
          if (!active) return "nothing";
          const inMenu = Boolean(active.closest('.select-menu'));
          const inDrawer = Boolean(active.closest('.drawer'));
          if (inMenu) return "menu";
          if (inDrawer) return "drawer";
          return "elsewhere:" + (active.tagName || "?");
        })()
        """
    )
    if landed != "menu":
        failures.append(
            "the focus left the open dropdown and landed %r; the drawer pulled it back out of a "
            "menu that Radix had portalled outside the drawer's own root" % landed
        )
    return failures


def case_ui15_the_link_that_started_the_application_opens_its_page(devtools) -> list[str]:
    """A link that *started* the application, which is a different path.

    A link that arrives while the application is already running is delivered
    as an event, and the dashboard is listening. A link that started the
    application was announced before the dashboard had finished loading, so
    there was no listener and the event went to nobody — the shell keeps the
    link, and the dashboard asks for it when it mounts.

    The case drives the second path: the application is already on the
    overview, and the link says the providers page.
    """
    failures: list[str] = []
    # The harness has already loaded the page with this case's query, which
    # carries the link. That is the path: the application came up because of
    # the link, so the dashboard asked for it as it mounted.
    devtools.wait_for("Boolean(document.querySelector('.nav-item'))", "the shell")
    devtools.wait_for(
        "document.querySelector('.nav-item.active') && "
        "document.querySelector('.nav-item.active').getAttribute('data-page') === 'providers'",
        "the providers page",
        timeout=15.0,
    )
    active = devtools.evaluate(
        "document.querySelector('.nav-item.active')?.getAttribute('data-page')"
    )
    if active != "providers":
        failures.append(
            "the link that started the application did not open its page; the navigation says %r"
            % active
        )
    return failures


def case_ui16_a_build_without_the_opencode_stack_offers_its_kinds(devtools) -> list[str]:
    """A build that cannot talk to OpenCode must not offer OpenCode.

    The two OpenCode provider kinds and the `opencode_cli` client profile are
    configuration rather than code, so they survive every build — but the
    request shaping that makes OpenCode's gateway accept a request does not, and
    the gateway checks a fixed fingerprint and rejects anything else. A provider
    of those kinds in a build without the shaping is a provider whose every
    request is refused.

    So the kinds and the profile are offered only where they work. The case
    reads the dropdown in both builds.
    """
    failures: list[str] = []

    def kinds_on_screen() -> str:
        return devtools.evaluate(
            """
            (() => {
              const section = [...document.querySelectorAll('section')]
                .find((s) => s.textContent.includes('API'));
              if (!section) return '';
              const items = [...section.querySelectorAll('[role="radio"]')];
              return items.map((i) => i.textContent.trim()).join('|');
            })()
            """
        )

    # The build with the stack offers the kinds.
    seed(devtools, **{**CUTOVER_QUERY})
    devtools.click_selector('[data-page="providers"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the provider rows")
    with_stack = kinds_on_screen()
    if "OpenCode" not in with_stack:
        failures.append(
            "a build with the OpenCode stack does not offer its kinds: %r" % with_stack
        )

    # The build without it does not.
    seed(devtools, **{**CUTOVER_QUERY, "opencode": False})
    devtools.click_selector('[data-page="providers"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the provider rows")
    without_stack = kinds_on_screen()
    if "OpenCode" in without_stack:
        failures.append(
            "a build without the OpenCode stack still offers its kinds: %r" % without_stack
        )
    return failures


def case_ui17_a_panel_backed_balance_is_shown_and_askable(devtools: DevTools) -> list[str]:
    """A relay whose balance comes from its panel shows the figure, and can be asked.

    This is the NewAPI case, and it is the one the balance work was for. The
    relay serves `/v1` and publishes no balance there, so the figure lives in
    the panel that owns the wallet — and only the account's own credential can
    read it. A dashboard that hid that provider's balance, or that disabled
    the button that fetches it, would be a dashboard telling the operator the
    relay has nothing to report when the relay never claimed to.

    Two readings, and both are about what the operator is shown:

    * the chip carries the figure the panel reported, and
    * the button that asks the panel is enabled, because there is something to
      ask.
    """
    failures: list[str] = []

    # The accounts-page fixture already declares two accounts on the DeepSeek
    # provider, and a provider with an account of its own is a provider whose
    # balance comes from a panel. Reusing it rather than spelling out a third
    # declaration is deliberate: the fixture convention is that an account
    # carries its maintenance block, and a hand-written one that left it out
    # would crash the drawer for a reason that has nothing to do with this case.
    seed(
        devtools,
        **ACCOUNT_QUERY,
        balances={
            "deepseek": {
                "checked_at": "2026-10-21T00:00:00Z",
                "balance": {
                    "currency": "USD",
                    "remaining": 12.5,
                    "total": 100.0,
                    "used": 87.5,
                },
                "error": None,
            }
        },
    )
    devtools.click_selector('[data-page="providers"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the provider rows")

    body = devtools.evaluate("document.querySelector('.page').innerText")
    if "12.50 USD" not in body:
        failures.append("the panel's figure is not rendered on the provider row: %r" % body)

    # The button that asks the panel is enabled. A disabled button here would
    # be the exact defect the balance work fixed, and it would look like a
    # provider that simply has nothing to report. It lives in the provider's
    # own drawer, so the drawer is opened first — by name, because the row
    # that carries the panel's reading is the one under test and an index
    # would be an accident waiting to happen.
    devtools.evaluate(
        """
        (() => {
          const rows = [...document.querySelectorAll('.list-row')];
          const row = rows.find((r) => r.textContent.includes('DeepSeek'));
          if (row) row.click();
        })()
        """
    )
    devtools.wait_for("document.querySelector('.drawer') !== null", "the provider drawer")
    enabled = devtools.evaluate(
        """
        (() => {
          const drawer = document.querySelector('.drawer');
          if (!drawer) return null;
          const buttons = [...drawer.querySelectorAll('button')];
          const check = buttons.find((b) => b.textContent.includes('Check now'));
          return check ? !check.disabled : null;
        })()
        """
    )
    if enabled is None:
        failures.append("there is no button that asks for the balance")
    elif not enabled:
        failures.append("the button that asks the panel for the balance is disabled")

    return failures


# The backend's own sentences and the Chinese the interface owes for each.
#
# These are the *reassurance* class: a failure that says what is still in
# place and what to do about it. Every one of them is a message the desktop
# can put in front of an operator, and every one of them used to arrive in
# English on a Chinese page — the sentences the interface does not restate are
# technical strings that name a setting, and those are fine in English.
#
# The Chinese fragment is what must appear; the English must not.
BACKEND_ERRORS = (
    (
        "cannot preserve C:/Users/dev/.zroutery/config.json as "
        "C:/Users/dev/.zroutery/config.json.bak: access denied; it was left as "
        "it is, so fix or move it and start again",
        "配置无法另存为",
    ),
    (
        "cannot preserve C:/Users/dev/.zroutery/config.json: "
        "C:/Users/dev/.zroutery/config.json.bak already exists",
        "因为该文件已经存在",
    ),
    (
        "the copy of C:/Users/dev/.zroutery/config.json in "
        "C:/Users/dev/.zroutery/config.json.bak does not match it; "
        "C:/Users/dev/.zroutery/config.json was left as it is",
        "的副本与它不一致",
    ),
    (
        "the copy of C:/Users/dev/.zroutery/config.json in "
        "C:/Users/dev/.zroutery/config.json.bak cannot be read back (access "
        "denied); C:/Users/dev/.zroutery/config.json was left as it is",
        "的副本无法回读",
    ),
    (
        "the change to 127.0.0.1:8787 was refused and the previous "
        "configuration and gateway are still in place (access denied)",
        "之前的配置与网关仍在原位",
    ),
    (
        "cannot use the new address: access denied; the gateway is still "
        "listening on 127.0.0.1:8787 and the configuration was not changed",
        "网关仍在",
    ),
    (
        "cannot use the new address: access denied; the configuration was not changed",
        "配置未被改动",
    ),
    (
        "cannot listen on 127.0.0.1:8787: address already in use; the previous "
        "gateway was restored",
        "之前的网关已恢复",
    ),
    (
        "cannot listen on 127.0.0.1:8787: address already in use; the previous "
        "gateway could not be restored either: access denied",
        "之前的网关也未能恢复",
    ),
    (
        "no rollback snapshot could be taken of C:/Users/dev/.claude/settings.json "
        "— the file could not be read, so a cutover could not be undone. Fix the "
        "file's permissions and try again.",
        "无法为",
    ),
    (
        "no account `acct-0` on provider `deepseek`",
        "上不存在账户",
    ),
    (
        "cannot bind 127.0.0.1:8787: cannot assign requested address",
        "无法绑定",
    ),
)


def case_ui18_a_backend_failure_is_restated_in_chinese(devtools) -> list[str]:
    """The backend's own sentences are re-stated in the reader's language.

    The interface restates a handful of the desktop's English messages and
    passes the rest through unchanged, and the ones it restates are the ones
    that carry advice: what is still in place, and what to do about it. A
    failure an operator has to act on arriving in English, on a page written
    in Chinese, is a page that reads as half-made.

    So the page is put in Chinese and each of the desktop's own sentences is
    shown to it, and what has to appear is the Chinese — and what must not
    appear is the English the backend wrote.

    The drawer is left open across the whole table: the seed re-reads the
    snapshot without remounting the page, so the drawer survives and each
    message only costs a re-read.
    """
    failures: list[str] = []

    # The page is in Chinese, because the claim is about the Chinese page.
    # The harness pins the language from `?lang=`, and this case asks for it:
    # a case that ran on the English page would pass on a translation that
    # never happened.
    seed(devtools, **CUTOVER_QUERY)
    devtools.click_selector('[data-page="providers"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the provider rows")
    devtools.evaluate(
        """
        (() => {
          const rows = [...document.querySelectorAll('.page .list-row')];
          const row = rows.find((r) => r.textContent.includes('DeepSeek'));
          if (row) row.click();
        })()
        """
    )
    devtools.wait_for("document.querySelector('.drawer') !== null", "the provider drawer")

    for message, expected in BACKEND_ERRORS:
        seed(
            devtools,
            balances={
                "deepseek": {
                    "checked_at": "2026-10-21T00:00:00Z",
                    "balance": None,
                    "error": message,
                }
            },
        )
        body = devtools.evaluate("document.querySelector('.drawer').innerText")
        if expected not in body:
            failures.append(
                "the backend's sentence is not restated in Chinese: %r is missing from %r"
                % (expected, body)
            )
        if message[:28] in body:
            failures.append(
                "the backend's own English is still shown: %r" % message[:40]
            )

    return failures


# A decision provider, merged into the default configuration by the harness.
#
# A whole-configuration seed, which the harness normally avoids, because the
# configuration grows a field and the seed would drift. This is spelled out
# here because a decision provider *is* a different kind of provider, and the
# case is about what the drawer offers for one — so the seed has to be one,
# and it has to say so.
DECISION_PROVIDER = {
    "id": "jev",
    "name": "Jev",
    "kind": "decision",
    "base_url": "https://api.typesafe.ai",
    "key_ref": "provider:jev",
    "extra_headers": {},
    "client_profile": "native",
    "bearer_auth": False,
    "enabled": True,
    "timeout_secs": 600,
    "connect_timeout_secs": 15,
    "anthropic_version": None,
    "balance": {"preset": "none", "custom": None},
    "decision": {"path": "/v1/systemone", "threshold": 0.85},
    "quirks": {},
}

# A model on the decision provider.
#
# A decision model cannot serve main traffic, so the dashboard must not offer
# a tier for it: the router would exclude the model from every tier pool, and
# a tier the router silently ignores is a promise the dashboard cannot keep.
DECISION_MODEL = {
    "provider_id": "jev",
    "upstream_model": "jev",
    "tier": None,
    "priority": 0,
    "weight": 1,
    "enabled": True,
    "capabilities": {
        "vision": False,
        "tools": False,
        "thinking": False,
        "structured_output": False,
        "audio": False,
        "video": False,
        "files": False,
    },
    "display_name": None,
    "aliases": [],
    "max_output_tokens": None,
    "pricing": None,
}


def case_ui19_a_decision_provider_offers_its_own_settings(devtools) -> list[str]:
    """A decision endpoint is asked a question, so it has settings of its own.

    The provider drawer's sections are chosen by what the provider *is*: a
    relay publishes a balance, a decision endpoint answers a typed question.
    A decision provider therefore carries the path its endpoint answers on and
    the confidence the answer must carry before Auto review acts on it — and
    a relay carries neither, because neither means anything for it.

    So the drawer shows the decision section for a decision provider and not
    for a relay, and the values it shows are the ones that were saved.
    """
    failures: list[str] = []

    seed(devtools, decisionProvider=DECISION_PROVIDER)
    devtools.click_selector('[data-page="providers"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the provider rows")
    devtools.evaluate(
        """
        (() => {
          const rows = [...document.querySelectorAll('.page .list-row')];
          const row = rows.find((r) => r.textContent.includes('Jev'));
          if (row) row.click();
        })()
        """
    )
    devtools.wait_for("document.querySelector('.drawer') !== null", "the provider drawer")

    body = devtools.evaluate("document.querySelector('.drawer').innerText")
    if "Required confidence" not in body:
        failures.append("the decision provider's own settings are not in the drawer: %r" % body)
    if "/v1/systemone" not in body:
        failures.append("the decision path is not shown: %r" % body)
    # The summary's *Endpoint* row is where an operator checks the address, and
    # for a decision provider that address is the base url plus the path — the
    # base alone is the half of it that is not called.
    if "https://api.typesafe.ai/v1/systemone" not in body:
        failures.append(
            "the endpoint row does not say the url the decision endpoint is called at: %r"
            % body
        )
    # The model on it is not offered a tier. The router excludes a decision
    # model from every tier pool, so a tier the dashboard let the operator pick
    # would be one the router silently ignores.
    #
    # The provider's drawer is closed first: a drawer left open over the
    # models page would be the drawer the case found, and it is the wrong one.
    devtools.evaluate(
        """
        (() => {
          const close = document.querySelector('.drawer-head button');
          if (close) close.click();
        })()
        """
    )
    seed(devtools, decisionProvider=DECISION_PROVIDER, decisionModel=DECISION_MODEL)
    devtools.click_selector('[data-page="models"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the model rows")
    devtools.evaluate(
        """
        (() => {
          const rows = [...document.querySelectorAll('.page .list-row')];
          const row = rows.find((r) => r.textContent.includes('jev'));
          if (row) row.click();
        })()
        """
    )
    devtools.wait_for("document.querySelector('.drawer') !== null", "the model drawer")
    model = devtools.evaluate("document.querySelector('.drawer').innerText")
    if "Why no tier" not in model or "cannot answer chat" not in model:
        failures.append("the decision model's tier is not explained: %r" % model)
    # The drawer no longer draws a dead tier control; it explains the absence
    # instead. Either answer is honest, so the case accepts both: no tier
    # control at all, or one that is disabled.
    locked = devtools.evaluate(
        """
        (() => {
          const drawer = document.querySelector('.drawer');
          if (!drawer) return null;
          const triggers = [...drawer.querySelectorAll('[role="combobox"]')];
          return triggers.every(
            (t) => t.disabled || t.getAttribute('data-disabled') === 'true'
          );
        })()
        """
    )
    if locked is not True:
        failures.append(
            "the decision model's drawer offers a tier control it cannot honour: %r" % locked
        )

    # And a relay carries neither, because neither means anything for it.
    seed(devtools, **CUTOVER_QUERY)
    devtools.click_selector('[data-page="providers"]')
    devtools.wait_for("Boolean(document.querySelector('.page .list-row'))", "the provider rows")
    devtools.evaluate(
        """
        (() => {
          const rows = [...document.querySelectorAll('.page .list-row')];
          const row = rows.find((r) => r.textContent.includes('DeepSeek'));
          if (row) row.click();
        })()
        """
    )
    devtools.wait_for("document.querySelector('.drawer') !== null", "the provider drawer")
    relay = devtools.evaluate("document.querySelector('.drawer').innerText")
    if "Required confidence" in relay:
        failures.append("a relay is offered the decision settings, which mean nothing for it")

    return failures


def case_ui20_the_auto_mode_editor_names_the_decision_answer(devtools) -> list[str]:
    """The Auto Mode's editor says how a verdict arrives, and names both ways.

    The editor told the operator that a verdict is parsed *only* from
    `<block>yes|no</block>`. That was true while every candidate answered in
    prose, and it stopped being true when a decision model could answer with a
    probability instead — so the sentence the operator reads was wrong about
    the thing it describes.

    So the editor names both: the tag a language model answers with, and the
    probability a decision model answers with.
    """
    failures: list[str] = []

    seed(devtools, **CUTOVER_QUERY)
    devtools.click_selector('[data-page="routing"]')
    devtools.wait_for("Boolean(document.querySelector('#tab-auto'))", "the auto mode tab")
    devtools.click_selector("#tab-auto")
    devtools.wait_for("Boolean(document.querySelector('.page .linky'))", "the edit control")
    devtools.evaluate(
        """
        (() => {
          // The inactive tab's panel is still mounted, so the first match for
          // the edit control may be a hidden one — and clicking a hidden
          // button does nothing. `offsetParent` is null for a hidden element
          // and set for a visible one.
          //
          // The label is the harness's own language, which is English: the
          // harness pins it, and a case that looked for the Chinese label
          // would find nothing and time out.
          const edit = [...document.querySelectorAll('.page .linky')]
            .find((b) => b.textContent.includes('Edit') && b.offsetParent !== null);
          if (edit) edit.click();
        })()
        """
    )
    devtools.wait_for("document.querySelector('.drawer') !== null", "the auto mode drawer")

    body = devtools.evaluate("document.querySelector('.drawer').innerText")
    if "<block>yes|no</block>" not in body:
        failures.append("the editor no longer says how a language model answers: %r" % body)
    if "probability" not in body:
        failures.append(
            "the editor does not say how a decision model answers: %r" % body
        )
    return failures


def case_ui21_the_auto_mode_pool_marks_a_decision_candidate(devtools) -> list[str]:
    """A decision candidate is in the pool by name, and says what it is.

    A decision model looks like any other row in the Auto Mode's pool, but it
    is asked a typed question and answers with a probability instead of a tag.
    The choice between one and a chat model is a choice between the two ways a
    verdict arrives, so the menu that offers it and the row it lands in both
    say which one it is — before and after it is added.
    """
    failures: list[str] = []

    seed(devtools, decisionProvider=DECISION_PROVIDER, decisionModel=DECISION_MODEL)
    devtools.click_selector('[data-page="routing"]')
    devtools.wait_for("Boolean(document.querySelector('#tab-auto'))", "the auto mode tab")
    devtools.click_selector("#tab-auto")
    devtools.wait_for("Boolean(document.querySelector('.page .linky'))", "the edit control")
    devtools.evaluate(
        """
        (() => {
          // The inactive tab's panel stays mounted, so click the visible edit
          // control rather than the first one in the document.
          const edit = [...document.querySelectorAll('.page .linky')]
            .find((b) => b.textContent.includes('Edit') && b.offsetParent !== null);
          if (edit) edit.click();
        })()
        """
    )
    devtools.wait_for("document.querySelector('.drawer') !== null", "the auto mode drawer")

    # The add menu says which kind of model an entry is, before it is added.
    # The harness pins English, so the label it looks for is the English one.
    devtools.click_selector('.drawer [aria-label^="Add a model"]')
    devtools.wait_for(
        "Boolean(document.querySelector('.select-menu [role=\"option\"]'))",
        "the add-model menu",
    )
    labels = devtools.evaluate(
        """
        [...document.querySelectorAll('.select-menu [role="option"]')]
          .map((option) => option.textContent)
        """
    )
    jev = next((label for label in labels if "jev-jev" in label), None)
    if jev is None:
        failures.append("the decision model is not offered by the add menu: %r" % labels)
    elif "Decision model" not in jev:
        failures.append("the add menu does not say what the decision model is: %r" % jev)

    # Add it, through a real click on the menu item, and read the row it lands
    # in: the badge and the menu label are the same fact on either side of the
    # choice.
    point = devtools.evaluate(
        """
        (() => {
          const item = [...document.querySelectorAll('.select-menu [role="option"]')]
            .find((option) => option.textContent.includes('jev-jev'));
          if (!item) return null;
          const rect = item.getBoundingClientRect();
          return {x: rect.left + rect.width / 2, y: rect.top + rect.height / 2};
        })()
        """
    )
    if point is None:
        failures.append("the decision model's menu item has no place to click")
        return failures
    devtools.click(point)
    devtools.wait_for(
        "Boolean([...document.querySelectorAll('.drawer .table tbody tr')]"
        ".find((row) => row.textContent.includes('jev-jev')))",
        "the decision candidate in the pool",
    )
    row = devtools.evaluate(
        """
        (() => {
          const row = [...document.querySelectorAll('.drawer .table tbody tr')]
            .find((candidate) => candidate.textContent.includes('jev-jev'));
          return row ? row.innerText : null;
        })()
        """
    )
    if row is None:
        failures.append("the decision model was not added to the pool")
    elif "Decision model" not in row:
        failures.append("the pool row does not say the candidate is a decision model: %r" % row)

    return failures


def case_ui22_one_click_adds_the_zen_jev_preset(devtools) -> list[str]:
    """One control, one save, and the Auto mode pool is on Zen's free Jev.

    The preset is three facts — the decision provider, the model that names it
    and the pool member that reaches it — and they are only useful together. The
    case reads the single save the button issues: the provider is keyless and
    points at the System One path, the model is declared, the candidate is first
    (priorities shifted, not overwritten) and Auto review is on. The second half
    is the idempotence: once the candidate is there, the button must be gone.
    """
    failures: list[str] = []

    devtools.click_selector('[data-page="routing"]')
    devtools.wait_for("Boolean(document.querySelector('#tab-auto'))", "the auto review tab")
    devtools.click_selector("#tab-auto")
    devtools.wait_for("Boolean(document.querySelector('.page .linky'))", "the edit control")
    devtools.evaluate(
        """
        (() => {
          const edit = [...document.querySelectorAll('.page .linky')]
            .find((b) => b.textContent.includes('Edit') && b.offsetParent !== null);
          if (edit) edit.click();
        })()
        """
    )
    devtools.wait_for("document.querySelector('.drawer') !== null", "the auto review drawer")

    button = (
        "[...document.querySelectorAll('.drawer .controls button')]"
        ".find((b) => b.textContent.includes(\"OpenCode Zen's free Jev\"))"
    )
    devtools.wait_for("Boolean(%s)" % button, "the Jev preset button")
    point = devtools.evaluate(
        """
        (() => {
          const b = %s;
          if (!b) return null;
          const r = b.getBoundingClientRect();
          return {x: r.left + r.width / 2, y: r.top + r.height / 2};
        })()
        """
        % button
    )
    if point is None:
        return ["the Jev preset button has no place to click"]
    devtools.click(point)

    devtools.wait_for("Boolean(window.__zrStub.last('save_config'))", "the preset save")
    devtools.wait_for("!document.querySelector('.busybar')", "the save to drain", timeout=20.0)
    config = devtools.evaluate(
        "(window.__zrStub.last('save_config')||{args:{config:null}}).args.config"
    )
    if config is None:
        return ["the preset issued no save_config"]

    provider = next((p for p in config["providers"] if p.get("kind") == "decision"), None)
    if provider is None:
        failures.append("the preset did not create a decision provider")
        return failures
    expected = {
        "base_url": "https://opencode.ai/zen",
        "key_ref": "",
        "client_profile": "native",
    }
    for field, value in expected.items():
        if provider.get(field) != value:
            failures.append("provider.%s is %r, expected %r" % (field, provider.get(field), value))
    if provider.get("extra_headers", {}).get("x-opencode-client") != "zroutery":
        failures.append("the provider does not identify itself to Zen: %r" % provider.get("extra_headers"))
    decision = provider.get("decision") or {}
    if decision.get("path") != "/v1/systemone":
        failures.append("decision.path is %r" % decision.get("path"))
    if decision.get("threshold") != 0.6:
        failures.append("decision.threshold is %r, expected 0.6" % decision.get("threshold"))

    model = next(
        (
            m
            for m in config["models"]
            if m["provider_id"] == provider["id"] and m["upstream_model"] == "jev-1.13-free"
        ),
        None,
    )
    if model is None:
        failures.append("the preset did not declare the jev-1.13-free model")

    candidates = config["classifier"]["candidates"]
    jev_id = "%s-jev-1.13-free" % provider["id"]
    jev = next((c for c in candidates if c["model"] == jev_id), None)
    if jev is None:
        failures.append("the preset did not add %s to the pool" % jev_id)
    else:
        if jev["priority"] != 10:
            failures.append("Jev is at priority %r, not first" % jev["priority"])
        if not jev["enabled"]:
            failures.append("the Jev candidate is disabled")
        others = [c["priority"] for c in candidates if c["model"] != jev_id]
        if any(p <= 10 for p in others):
            failures.append("existing candidates were not shifted behind Jev: %r" % others)
    if not config["classifier"]["enabled"]:
        failures.append("the preset left Auto review off")

    # Idempotence: the candidate is in the pool, so the button must be gone.
    devtools.wait_for(
        "!Boolean(%s)" % button, "the preset button to disappear once it has landed"
    )

    return failures


def case_ui23_rename_moves_the_references_it_changed(devtools) -> list[str]:
    """Renaming a model is an identity change, and the references move with it.

    The Auto review pool and the vision model name a model by its exposed id.
    Renaming used to leave both pointing at an id that no longer existed: the
    pool quietly skipped the candidate and the vision fallback silently degraded
    to the placeholder. The rename now commits the model and both references in
    one save, keeps the drawer on the renamed model, and refuses a name another
    model already answers to.
    """
    failures: list[str] = []

    config = json.loads(json.dumps(BASE_CONFIG))
    config["vision"]["model"] = "deepseek-deepseek-chat"
    # An alias on another model is the collision the name check has to see.
    for model in config["models"]:
        if model["provider_id"] == "anthropic":
            model["aliases"] = ["deepseek-chat-v2"]
    seed(devtools, config=config)

    devtools.click_selector('[data-page="models"]')
    devtools.wait_for("Boolean(document.querySelector('.list-row'))", "the models list")
    # By id, not by position: the list sorts by provider name, and the case
    # renames and then asserts on the deepseek model.
    devtools.evaluate(
        """
        (() => {
          const row = [...document.querySelectorAll('.list-row')]
            .find((r) => r.textContent.includes('deepseek-deepseek-chat'));
          if (row) row.click();
        })()
        """
    )
    devtools.wait_for("Boolean(document.querySelector('.drawer'))", "the model drawer")

    # The add-a-model field on the same page carries the same accessible name,
    # so the rename is driven inside the drawer.
    devtools.replace_text(".drawer [aria-label='Model name']", "deepseek-chat-v3")
    devtools.press_key("Enter")
    devtools.wait_for(
        "Boolean(window.__zrStub.last('save_config'))", "the rename save", timeout=20.0
    )
    devtools.wait_for("!document.querySelector('.busybar')", "the save to drain", timeout=20.0)
    saved = devtools.evaluate(
        "(window.__zrStub.last('save_config')||{args:{config:null}}).args.config"
    )
    if saved is None:
        return ["the rename issued no save_config"]

    renamed = next(
        (
            m
            for m in saved["models"]
            if m["provider_id"] == "deepseek" and m["upstream_model"] == "deepseek-chat-v3"
        ),
        None,
    )
    if renamed is None:
        failures.append("the model was not renamed")
    candidates = [c["model"] for c in saved["classifier"]["candidates"]]
    if "deepseek-deepseek-chat-v3" not in candidates:
        failures.append("the auto review candidate was not moved: %r" % candidates)
    if "deepseek-deepseek-chat" in candidates:
        failures.append("the old id is still in the pool: %r" % candidates)
    if saved["vision"]["model"] != "deepseek-deepseek-chat-v3":
        failures.append("vision.model is %r" % saved["vision"]["model"])

    # The drawer follows the new identity instead of closing on the old one.
    title = devtools.evaluate(
        "(document.querySelector('.drawer .drawer-head')||{}).innerText || ''"
    )
    if "deepseek-deepseek-chat-v3" not in title:
        failures.append("the drawer did not follow the rename: %r" % title)

    # A name another model already answers to is refused before the save.
    before = devtools.evaluate("window.__zrStub.log('save_config').length")
    devtools.replace_text(".drawer [aria-label='Model name']", "chat-v2")
    devtools.press_key("Enter")
    devtools.wait_for(
        "Boolean(document.querySelector('.drawer .field-hint-danger'))",
        "the collision to be named",
        timeout=10.0,
    )
    hint = devtools.evaluate(
        "(document.querySelector('.drawer .field-hint-danger')||{}).innerText || ''"
    )
    if "deepseek-chat-v2" not in hint:
        failures.append("the collision hint does not name the taken id: %r" % hint)
    after = devtools.evaluate("window.__zrStub.log('save_config').length")
    if after != before:
        failures.append("a colliding rename reached the backend: %d -> %d" % (before, after))

    return failures


def case_ui24_catalogue_import_adds_several_models(devtools) -> list[str]:
    """The models page can import several catalogue entries in one save.

    The provider drawer could add one entry at a time; this is the bulk path.
    The case selects two rows, adds them in one save, and reads the fields that
    used to be dropped: the context window from the catalogue and the tier the
    batch was assigned.
    """
    failures: list[str] = []

    seed(
        devtools,
        catalogues={
            "deepseek": [
                {"id": "deepseek-reasoner", "pricing": None, "free": False, "context_window": 1000000},
                {"id": "deepseek-coder", "pricing": None, "free": True},
            ]
        },
    )
    devtools.click_selector('[data-page="models"]')
    devtools.wait_for("Boolean(document.querySelector('.list-row'))", "the models list")

    # The add section defaults to the first provider; open its catalogue.
    devtools.wait_for(
        "Boolean([...document.querySelectorAll('button')]"
        ".find((b) => b.textContent.includes(\"Browse the provider's catalogue\")))",
        "the catalogue button",
    )
    devtools.evaluate(
        """
        (() => {
          const b = [...document.querySelectorAll('button')]
            .find((x) => x.textContent.includes("Browse the provider's catalogue"));
          if (b) b.click();
        })()
        """
    )
    devtools.wait_for(
        "Boolean(document.querySelector(\"input[aria-label='deepseek-reasoner']\"))",
        "the catalogue rows",
        timeout=15.0,
    )
    devtools.click_selector("input[aria-label='deepseek-reasoner']")
    devtools.click_selector("input[aria-label='deepseek-coder']")
    devtools.wait_for(
        "Boolean([...document.querySelectorAll('button')]"
        ".find((b) => b.textContent.includes('Add selected (2)')))",
        "the batch count",
    )
    removed = devtools.evaluate("window.__zrStub.log('save_config').length")
    devtools.evaluate(
        """
        (() => {
          const b = [...document.querySelectorAll('button')]
            .find((x) => x.textContent.includes('Add selected (2)'));
          if (b) b.click();
        })()
        """
    )
    devtools.wait_for(
        "window.__zrStub.log('save_config').length > %d" % removed,
        "the import save",
        timeout=20.0,
    )
    devtools.wait_for("!document.querySelector('.busybar')", "the save to drain", timeout=20.0)
    saved = devtools.evaluate(
        "(window.__zrStub.last('save_config')||{args:{config:null}}).args.config"
    )
    if saved is None:
        return ["the import issued no save_config"]

    by_name = {m["upstream_model"]: m for m in saved["models"]}
    reasoner = by_name.get("deepseek-reasoner")
    coder = by_name.get("deepseek-coder")
    if reasoner is None or coder is None:
        failures.append("the import did not add both models: %r" % sorted(by_name))
    else:
        if reasoner.get("context_window") != 1000000:
            failures.append(
                "the catalogue context window was dropped: %r" % reasoner.get("context_window")
            )
        if reasoner.get("tier") is not None:
            failures.append("the batch should land unclassified by default: %r" % reasoner.get("tier"))
    saves = devtools.evaluate("window.__zrStub.log('save_config').length")
    if saves != removed + 1:
        failures.append("the batch took %d saves, not one" % (saves - removed))

    return failures


def case_ui25_the_browser_token_capture_runs_and_cancels(devtools) -> list[str]:
    """The account row can ask the browser for a panel token, and stop it.

    Nothing secret is involved in this harness: it proves the control exists on
    an account row, starts the command with the account's ids, shows the phase
    the backend reports, and lets the operator cancel. The token itself never
    reaches the page — there is no field for it — so the case also checks that
    the view text does not grow a token-shaped string.
    """
    failures: list[str] = []

    seed(devtools, **ACCOUNT_QUERY)
    devtools.click_selector('[data-page="accounts"]')
    devtools.wait_for(
        "Boolean([...document.querySelectorAll('button')]"
        ".find((b) => b.textContent.includes('Get token in browser')))",
        "the capture button",
    )
    devtools.evaluate(
        """
        (() => {
          const b = [...document.querySelectorAll('button')]
            .find((x) => x.textContent.includes('Get token in browser'));
          if (b) b.click();
        })()
        """
    )
    devtools.wait_for(
        "Boolean(window.__zrStub.log('start_token_capture').length)",
        "the capture command",
        timeout=15.0,
    )
    call = devtools.evaluate("window.__zrStub.last('start_token_capture')")
    if not call or call["args"]["providerId"] != "deepseek" or call["args"]["accountId"] != "main":
        failures.append("the capture was not asked for the account that owns the button: %r" % (call,))

    devtools.wait_for(
        "Boolean([...document.querySelectorAll('*')]"
        ".find((e) => e.childElementCount === 0"
        " && e.textContent.includes('Sign in in the browser window')))",
        "the waiting phase",
        timeout=15.0,
    )
    body = devtools.evaluate("document.body.innerText")
    if "panel-token" in body or "eyJ" in body:
        failures.append("a token-shaped string reached the page")

    # Cancel: the backend reports `cancelled`, and the start control returns.
    devtools.evaluate(
        """
        (() => {
          const b = [...document.querySelectorAll('button')]
            .find((x) => x.textContent.trim() === 'Cancel');
          if (b) b.click();
        })()
        """
    )
    devtools.wait_for(
        "Boolean(window.__zrStub.log('cancel_token_capture').length)",
        "the cancel command",
        timeout=15.0,
    )
    devtools.wait_for(
        "Boolean([...document.querySelectorAll('button')]"
        ".find((b) => b.textContent.includes('Get token in browser')))",
        "the start control to come back",
        timeout=15.0,
    )
    return failures


CASES = (
    {"label": "UI-1 two nested edits in sequence (first save delayed)", "run": case_ui1_nested_rebase},
    {"label": "UI-2 moved candidate renumbers priority", "run": case_ui2_candidate_priority},
    {
        "label": "UI-3 provider drawer isolates key draft and background",
        "run": case_ui3_drawer_isolation,
        "query": {"keys": {}},
    },
    {"label": "UI-4 tray state change updates page and command", "run": case_ui4_tray_state},
    {
        "label": "UI-5 refused promotion names the criterion that refused it",
        "run": case_ui5_refused_round_shows_its_criteria,
        "query": {"ml": ML_QUERY},
    },
    {
        "label": "UI-6 accounts page shows readings and direct check-in acts",
        "run": case_ui6_accounts_direct_checkin,
        "query": ACCOUNT_QUERY,
    },
    {
        "label": "UI-7 cutover confirms a run behind the preview's fingerprint",
        "run": case_ui7_cutover_confirmation_and_honest_rollback,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-8 a rollback that restored nothing says so",
        "run": case_ui8_cutover_reports_a_bare_rollback_honestly,
        "query": CUTOVER_FAILED_QUERY,
    },
    {
        "label": "UI-9 a confirm dialog takes the focus and gives it back",
        "run": case_ui9_confirm_dialog_takes_focus_and_gives_it_back,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-10 a menu moves with the arrow keys",
        "run": case_ui10_the_menu_moves_with_the_arrow_keys,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-11 a confirmation inside a drawer does not fight it",
        "run": case_ui11_a_confirmation_inside_a_drawer_does_not_fight_it,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-12 a deep link opens the page it names",
        "run": case_ui12_a_deep_link_opens_the_page_it_names,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-13 a catalogue row says no tier rather than a dash",
        "run": case_ui13_a_catalogue_row_says_no_tier_rather_than_a_dash,
        "query": PROVIDER_CATALOGUE_QUERY,
    },
    {
        "label": "UI-14 a select inside a drawer keeps its focus",
        "run": case_ui14_a_select_inside_a_drawer_keeps_its_focus,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-15 the link that started the application opens its page",
        "run": case_ui15_the_link_that_started_the_application_opens_its_page,
        "query": {**CUTOVER_QUERY, "deepLink": "zroutery://providers"},
    },
    {
        "label": "UI-16 a build without the opencode stack offers its kinds",
        "run": case_ui16_a_build_without_the_opencode_stack_offers_its_kinds,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-17 a panel backed balance is shown and askable",
        "run": case_ui17_a_panel_backed_balance_is_shown_and_askable,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-18 a backend failure is restated in chinese",
        "run": case_ui18_a_backend_failure_is_restated_in_chinese,
        "query": {**CUTOVER_QUERY, "lang": "zh"},
    },
    {
        "label": "UI-19 a decision provider offers its own settings",
        "run": case_ui19_a_decision_provider_offers_its_own_settings,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-20 the auto mode editor names the decision answer",
        "run": case_ui20_the_auto_mode_editor_names_the_decision_answer,
        "query": CUTOVER_QUERY,
    },
    {
        "label": "UI-21 the auto mode pool marks a decision candidate",
        "run": case_ui21_the_auto_mode_pool_marks_a_decision_candidate,
    },
    {
        "label": "UI-22 one click adds the Zen Jev preset",
        "run": case_ui22_one_click_adds_the_zen_jev_preset,
    },
    {
        "label": "UI-23 rename moves the references it changed",
        "run": case_ui23_rename_moves_the_references_it_changed,
    },
    {
        "label": "UI-24 catalogue import adds several models in one save",
        "run": case_ui24_catalogue_import_adds_several_models,
        "query": {"catalogues": {"deepseek": []}},
    },
    {
        "label": "UI-25 the browser token capture runs and cancels",
        "run": case_ui25_the_browser_token_capture_runs_and_cancels,
        "query": ACCOUNT_QUERY,
    },
)


def main() -> int:
    # The console has to be able to print what the gate found.
    #
    # A failure message that names a Chinese sentence cannot be reported on a
    # console that cannot encode it, and the encoding error hides the finding
    # behind a crash: the gate reports a crash instead of a failure, which is
    # a report about the harness rather than about the product. This case
    # reads the page in Chinese on purpose, so it needs this.
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass

    if not os.path.exists(os.path.join(DIST, "index.html")):
        print("ui/dist is missing; run `pnpm --dir ui build` first")
        return 2
    browser = find_chromium()
    if not browser:
        print("no Chromium based browser found; the interaction test did not run")
        return 2

    stage = tempfile.mkdtemp(prefix="zr-interaction-")
    stage_page(stage)
    server, port = serve(stage)
    profile = tempfile.mkdtemp(prefix="zr-interaction-profile-")
    debugging_port = free_port()
    url = "http://127.0.0.1:%d/" % port
    browser_proc = launch(profile, debugging_port, url)

    exit_code = 0
    try:
        devtools = connect(debugging_port, url)
        try:
            only = None
            for arg in sys.argv[1:]:
                if arg.startswith("--only="):
                    only = arg.split("=", 1)[1]
            for spec in CASES:
                label, case = spec["label"], spec["run"]
                if only and only not in label:
                    continue
                failures = []
                # A browser that died mid-case is retried from a clean process:
                # each case starts by loading the page and seeding the backend,
                # so nothing of the dead run carries over.
                for attempt in range(3):
                    if spec.get("query") or attempt > 0:
                        # A case that needs the process to start in another state
                        # gets a browser of its own; the app reads its snapshot
                        # once, at load.
                        query = spec.get("query") or {}
                        params = "&".join(
                            "%s=%s" % (name, urllib.parse.quote(json.dumps(value), safe=""))
                            for name, value in query.items()
                        )
                        case_url = "%s?%s" % (url, params) if params else url
                        browser_proc.kill()
                        try:
                            browser_proc.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            pass
                        try:
                            devtools.close()
                        except OSError:
                            pass
                        browser_proc = launch(profile, debugging_port, case_url)
                        devtools = connect(debugging_port, case_url)
                    try:
                        reload(devtools)
                        failures = case(devtools)
                        break
                    except (ProtocolError, OSError, ValueError, socket.timeout) as error:
                        failures = ["the case could not be driven: %s" % error]
                        if attempt == 2:
                            break
                if failures:
                    exit_code = 1
                    print("FAIL %s" % label)
                    for problem in failures:
                        print("       %s" % problem)
                else:
                    print("PASS %s" % label)
        finally:
            devtools.close()
    except (ProtocolError, OSError, ValueError, socket.timeout) as error:
        print("the interaction test could not run: %s" % error)
        exit_code = 1
    finally:
        browser_proc.kill()
        try:
            browser_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        server.shutdown()
        shutil.rmtree(profile, ignore_errors=True)
        shutil.rmtree(stage, ignore_errors=True)

    return exit_code


if __name__ == "__main__":
    sys.exit(main())
