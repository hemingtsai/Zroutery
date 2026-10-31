import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import {
  applyTierOrder,
  applyZenJevPreset,
  api,
  emptyProvider,
  isZenJevProvider,
  modelName,
  ms,
  ZEN_JEV_BASE_URL,
  type AppConfig,
  type ModelEntry,
} from "./api";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const invokeMock = vi.mocked(tauriInvoke);

describe("config document revision binding", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("keeps an older cloned config bound to its own revision after a newer reply", async () => {
    const older = { providers: [] } as unknown as AppConfig;
    const newer = { providers: [] } as unknown as AppConfig;
    invokeMock
      .mockResolvedValueOnce({ config: older, config_revision: "revision-old" })
      .mockResolvedValueOnce({ config: newer, config_revision: "revision-new" })
      .mockResolvedValueOnce({ config: newer, config_revision: "revision-new" });

    const oldSnapshot = await api.snapshot();
    const oldClone = structuredClone(oldSnapshot.config);
    await api.snapshot();
    await api.saveConfig(oldClone);

    expect(oldClone.__config_revision).toBe("revision-old");
    expect(invokeMock).toHaveBeenLastCalledWith("save_config", {
      config: { providers: [] },
      expectedRevision: "revision-old",
    });
  });

  it("strips the bridge marker from wire config and leaves legacy saves unguarded", async () => {
    invokeMock.mockResolvedValue({
      config: { providers: [] },
      config_revision: "saved-revision",
    });
    const marked = {
      providers: [],
      __config_revision: "document-revision",
    } as unknown as AppConfig;
    await api.saveConfig(marked);
    expect(invokeMock).toHaveBeenLastCalledWith("save_config", {
      config: { providers: [] },
      expectedRevision: "document-revision",
    });
    expect(
      (invokeMock.mock.calls.at(-1)?.[1] as { config: AppConfig }).config,
    ).not.toHaveProperty("__config_revision");

    await api.saveConfig({ providers: [] } as unknown as AppConfig);
    expect(invokeMock).toHaveBeenLastCalledWith("save_config", {
      config: { providers: [] },
      expectedRevision: undefined,
    });
  });
});

describe("modelName display contract", () => {
  it("exports the shared readable-name function used by health and model rows", () => {
    expect(modelName({ display_name: "  Readable model  ", upstream_model: "technical-id" }))
      .toBe("Readable model");
  });

  it("keeps the exact upstream name when no readable override is present", () => {
    expect(modelName({ display_name: "  ", upstream_model: "model-v4.1-long-suffix" }))
      .toBe("model-v4.1-long-suffix");
  });
});

describe("Jev endpoint preset", () => {
  function config(): AppConfig {
    return { providers: [], models: [], classifier: { candidates: [], enabled: false },
      server: { bypass_proxy: true } } as unknown as AppConfig;
  }

  it("uses the current official endpoint, native identity and a bounded proxy-safe timeout", () => {
    const original = config();
    const next = applyZenJevPreset(original)!;
    expect(next.providers[0].base_url).toBe(ZEN_JEV_BASE_URL);
    expect(next.providers[0].client_profile).toBe("native");
    expect(next.providers[0].extra_headers["x-opencode-client"]).toBe("zroutery");
    expect(next.providers[0].timeout_secs).toBe(15);
    expect(next.providers[0].key_ref).toBe("");
    expect(next.providers[0].bypass_proxy).toBeNull();
    expect(next.server.bypass_proxy).toBe(true);
    expect(original.providers).toHaveLength(0);
    expect(applyZenJevPreset(next)).toBeNull();
  });

  it("recognizes and migrates only the legacy endpoint without duplicate candidates or other edits", () => {
    const initial = applyZenJevPreset(config())!;
    initial.providers[0].base_url = "https://opencode.ai/zen/";
    initial.providers[0].bypass_proxy = false;
    initial.providers[0].timeout_secs = 21;
    initial.models[0].aliases = ["operator-jev"];
    initial.classifier.candidates[0].model = "operator-jev";
    expect(isZenJevProvider(initial.providers[0])).toBe(true);
    const next = applyZenJevPreset(initial)!;
    expect(next.providers).toHaveLength(1);
    expect(next.providers[0]).toEqual({ ...initial.providers[0], base_url: ZEN_JEV_BASE_URL });
    expect(next.models).toEqual(initial.models);
    expect(next.classifier).toEqual(initial.classifier);
    expect(initial.providers[0].base_url).toBe("https://opencode.ai/zen/");
    expect(applyZenJevPreset(next)).toBeNull();
  });

  it("leaves custom hosts and decision paths alone", () => {
    const host = emptyProvider("custom", "Custom", "decision", "https://opencode.ai.example/zen");
    const path = emptyProvider("path", "Path", "decision", "https://opencode.ai/zen", { decision: { path: "/v1/custom", threshold: 0.7 } });
    expect(isZenJevProvider(host)).toBe(false);
    expect(isZenJevProvider(path)).toBe(false);
    const initial = config();
    initial.providers = [host, path];
    const next = applyZenJevPreset(initial)!;
    expect(next.providers.slice(0, 2)).toEqual(initial.providers);
  });
});

describe("applyTierOrder", () => {
  function entry(
    provider_id: string,
    upstream_model: string,
    tier: ModelEntry["tier"],
    priority: number,
    enabled = true,
  ): ModelEntry {
    return {
      provider_id,
      upstream_model,
      tier,
      priority,
      enabled,
      weight: 1,
      capabilities: {
        vision: false,
        tools: false,
        thinking: false,
        structured_output: false,
        audio: false,
        video: false,
        files: false,
      },
      display_name: null,
      aliases: [],
      max_output_tokens: null,
      context_window: null,
      effort_levels: [],
      pricing: null,
    };
  }
  const order = (...pairs: [string, string][]) =>
    pairs.map(([provider_id, upstream_model]) => ({ provider_id, upstream_model }));

  it("renumbers the dragged order into spaced priorities", () => {
    const models = [
      entry("a", "m1", "standard", 40),
      entry("a", "m2", "standard", 30),
      entry("a", "m3", "standard", 20),
    ];
    const next = applyTierOrder(models, "standard", order(["a", "m3"], ["a", "m1"], ["a", "m2"]))!;
    expect(next.map((m) => m.priority)).toEqual([20, 30, 10]);
    // The input is untouched: the caller's snapshot stays as it was.
    expect(models.map((m) => m.priority)).toEqual([40, 30, 20]);
  });

  it("touches only the named tier and leaves disabled members where they were", () => {
    const models = [
      entry("a", "m1", "standard", 40),
      entry("a", "off", "standard", 35, false),
      entry("a", "fast", "fast", 20),
    ];
    const next = applyTierOrder(models, "standard", order(["a", "m1"]))!;
    expect(next[0].priority).toBe(10);
    expect(next[1].priority).toBe(35);
    expect(next[2].priority).toBe(20);
  });

  it("refuses an order that names a model the tier does not have, or names one twice", () => {
    const models = [entry("a", "m1", "standard", 40), entry("a", "m2", "reasoning", 20)];
    expect(applyTierOrder(models, "standard", order(["a", "m2"]))).toBeNull();
    expect(applyTierOrder(models, "standard", order(["a", "m1"], ["a", "m1"]))).toBeNull();
  });
});

describe("ms duration tiers", () => {
  it("keeps raw milliseconds under a second", () => {
    expect(ms(850)).toBe("850 ms");
    expect(ms(999.4)).toBe("999 ms");
  });

  it("reads seconds with two decimals under a minute", () => {
    expect(ms(1000)).toBe("1.00 s");
    expect(ms(12_340)).toBe("12.34 s");
  });

  it("switches to minutes at a minute and pads the seconds", () => {
    expect(ms(60_000)).toBe("1m 00s");
    expect(ms(267_522)).toBe("4m 28s");
    expect(ms(599_000)).toBe("9m 59s");
  });

  it("reads hours without seconds, defensively for stalled upstreams", () => {
    expect(ms(3_600_000)).toBe("1h 00m");
    expect(ms(3_723_000)).toBe("1h 02m");
  });

  it("renders the dash for unknowns", () => {
    expect(ms(null)).toBe("—");
    expect(ms(undefined)).toBe("—");
  });
});
