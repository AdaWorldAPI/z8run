import { pluginsApi } from "@/api/plugins";
import { NODE_DEFINITIONS, type NodeDefinition } from "@/lib/nodeDefinitions";
import { create } from "zustand";

interface PluginState {
  /** Plugin nodes registered on the server. */
  definitions: NodeDefinition[];
  loaded: boolean;
  /** Fetches the plugin list once; later calls are no-ops. */
  load: () => Promise<void>;
}

/** The request in flight, shared by concurrent callers. */
let pending: Promise<void> | null = null;

export const usePluginStore = create<PluginState>((set, get) => ({
  definitions: [],
  loaded: false,
  load: () => {
    if (get().loaded) return Promise.resolve();
    pending ??= pluginsApi
      .list()
      .then((definitions) => set({ definitions, loaded: true }))
      // Plugins are optional: the built-in palette still works.
      .catch(() => set({ loaded: true }))
      .finally(() => {
        pending = null;
      });
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
