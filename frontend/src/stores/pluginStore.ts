import { pluginsApi } from "@/api/plugins";
import { NODE_DEFINITIONS, type NodeDefinition } from "@/lib/nodeDefinitions";
import { create } from "zustand";

export type PluginLoadStatus = "idle" | "loading" | "loaded" | "error";

/** Waits before each automatic retry of a failed plugin list request. */
export const RETRY_DELAYS_MS = [500, 2000];

interface PluginState {
  /** Plugin nodes registered on the server. */
  definitions: NodeDefinition[];
  status: PluginLoadStatus;
  /**
   * Fetches the plugin list, retrying a failed request a couple of times.
   * Resolves once loaded or failed. After a success later calls are no-ops;
   * after a failure the next call tries again.
   */
  load: () => Promise<void>;
}

/** The request in flight, shared by concurrent callers. */
let pending: Promise<void> | null = null;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function fetchWithRetry(): Promise<NodeDefinition[]> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await pluginsApi.list();
    } catch (err) {
      const delay = RETRY_DELAYS_MS[attempt];
      if (delay === undefined) throw err;
      await sleep(delay);
    }
  }
}

export const usePluginStore = create<PluginState>((set, get) => ({
  definitions: [],
  status: "idle",
  load: () => {
    if (get().status === "loaded") return Promise.resolve();
    pending ??= (async () => {
      set({ status: "loading" });
      try {
        set({ definitions: await fetchWithRetry(), status: "loaded" });
      } catch {
        // Plugins are optional: the built-in palette still works, and the
        // palette offers a retry.
        set({ status: "error" });
      } finally {
        pending = null;
      }
    })();
    return pending;
  },
}));

/** Finds a node definition among built-in and plugin nodes. */
export function findNodeDefinition(type: string): NodeDefinition | undefined {
  return (
    NODE_DEFINITIONS.find((d) => d.type === type) ??
    usePluginStore.getState().definitions.find((d) => d.type === type)
  );
}
