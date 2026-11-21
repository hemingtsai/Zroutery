import { describe, expect, it } from "vitest";
import type { CheckinView, Maintenance, Provider } from "./api";
import {
  browserCheckinMissing,
  checkinAllTargets,
  isLoginSettledCheckin,
} from "./accountCheckin";

const maintenance = (overrides: Partial<Maintenance> = {}): Maintenance => ({
  checkin_enabled: true,
  checkin_interval_secs: 86400,
  checkin_path: null,
  checkin_trigger_selector: null,
  login_path: null,
  panel_url: null,
  new_api_user: "7",
  browser_executable: "",
  ...overrides,
});

describe("login-settled browser check-in", () => {
  it("maps only the exact approved relay to the panel", () => {
    expect(isLoginSettledCheckin({ base_url: "https://ps.air-outer.com/api/v1" }, maintenance())).toBe(true);
    for (const base_url of ["http://ps.air-outer.com/v1", "https://ps.air-outer.com.evil.test/v1",
      "https://agentrouter.org.evil.test", "https://user@agentrouter.org"])
      expect(isLoginSettledCheckin({ base_url }, maintenance())).toBe(false);
  });

  it("the explicit panel wins and a provider/model name is irrelevant", () => {
    const provider = { base_url: "https://ps.air-outer.com/v1" };
    expect(isLoginSettledCheckin(provider, maintenance({ panel_url: "https://other.example" }))).toBe(false);
    expect(isLoginSettledCheckin({ base_url: "https://other.example" },
      maintenance({ panel_url: "https://agentrouter.org/" }))).toBe(true);
  });

  it("does not demand a nonexistent check-in button on AgentRouter", () => {
    expect(browserCheckinMissing({ base_url: "https://ps.air-outer.com/v1" }, maintenance())).toBeNull();
    const generic = { base_url: "https://other.example" };
    expect(browserCheckinMissing(generic, maintenance())).toBe("path");
    expect(browserCheckinMissing(generic, maintenance({ checkin_path: "/console/personal" }))).toBe("selector");
    expect(browserCheckinMissing(generic, maintenance({ checkin_path: "/console/personal",
      checkin_trigger_selector: "button.checkin" }))).toBeNull();
  });
});

describe("check in all", () => {
  const provider = (
    id: string,
    accounts: Array<[string, Partial<Maintenance>]>,
    enabled = true,
    base_url = "https://agentrouter.org",
  ) =>
    ({
      id,
      enabled,
      base_url,
      accounts: accounts.map(([account_id, over]) => ({
        account_id,
        maintenance: maintenance(over),
      })),
    }) as unknown as Provider;

  const view = (account_id: string, phase: string) =>
    ({ provider_id: "router", account_id, phase }) as unknown as CheckinView;

  it("starts each idle account exactly once, across providers", () => {
    const targets = checkinAllTargets(
      [provider("router", [["a", {}], ["b", {}]])],
      [view("a", "idle"), view("b", "failed")],
    );
    expect(targets).toEqual([
      { providerId: "router", accountId: "a" },
      { providerId: "router", accountId: "b" },
    ]);
  });

  it("never replaces a running or paused session", () => {
    const targets = checkinAllTargets(
      [provider("router", [["a", {}], ["b", {}], ["c", {}]])],
      [view("a", "running"), view("b", "waiting_for_user")],
    );
    expect(targets).toEqual([{ providerId: "router", accountId: "c" }]);
  });

  it("skips disabled declarations and accounts with no browser executor", () => {
    const targets = checkinAllTargets(
      [
        provider("router", [["a", {}], ["off", { checkin_enabled: false }]]),
        provider("disabled", [["b", {}]], false),
        provider("http-only", [["c", {}]], true, "https://other.example"),
        provider(
          "generic",
          [["d", { checkin_path: "/console/personal", checkin_trigger_selector: "button" }]],
          true,
          "https://other.example",
        ),
      ],
      [],
    );
    expect(targets).toEqual([
      { providerId: "router", accountId: "a" },
      { providerId: "generic", accountId: "d" },
    ]);
  });
});
