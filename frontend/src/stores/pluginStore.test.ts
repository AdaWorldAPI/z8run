import { beforeEach, describe, expect, it, vi } from "vitest";

const list = vi.fn();
vi.mock("@/api/plugins", () => ({ pluginsApi: { list: () => list() } }));

const { usePluginStore, findNodeDefinition, RETRY_DELAYS_MS } = await import(
  "./pluginStore"
);

const echo = {
  type: "echo",
  label: "echo",
  category: "plugin" as const,
  icon: "Layers",
  description: "",
  inputs: [],
  outputs: [],
  defaultConfig: {},
};

describe("plugin store", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    list.mockReset();
    usePluginStore.setState({ definitions: [], status: "idle" });
  });

  it("recovers from a transient failure by retrying on its own", async () => {
    list.mockRejectedValueOnce(new Error("503")).mockResolvedValueOnce([echo]);

    const loading = usePluginStore.getState().load();
    expect(usePluginStore.getState().status).toBe("loading");
    await vi.advanceTimersByTimeAsync(RETRY_DELAYS_MS[0] ?? 0);
    await loading;

    expect(list).toHaveBeenCalledTimes(2);
    expect(usePluginStore.getState().status).toBe("loaded");
    expect(findNodeDefinition("echo")?.category).toBe("plugin");
  });

  it("reports an error after the retries, and a later load tries again", async () => {
    list.mockRejectedValue(new Error("down"));
    const first = usePluginStore.getState().load();
    await vi.runAllTimersAsync();
    await first;
    expect(list).toHaveBeenCalledTimes(1 + RETRY_DELAYS_MS.length);
    expect(usePluginStore.getState().status).toBe("error");

    // Not stuck: the palette's Retry (another load) fetches again.
    list.mockReset();
    list.mockResolvedValueOnce([echo]);
    await usePluginStore.getState().load();
    expect(usePluginStore.getState().status).toBe("loaded");
    expect(usePluginStore.getState().definitions).toEqual([echo]);
  });

  it("shares one request between concurrent callers and stops once loaded", async () => {
    list.mockResolvedValue([echo]);
    await Promise.all([
      usePluginStore.getState().load(),
      usePluginStore.getState().load(),
    ]);
    await usePluginStore.getState().load();
    expect(list).toHaveBeenCalledTimes(1);
  });
});
