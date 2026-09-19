import type { NodeDefinition } from "@/lib/nodeDefinitions";
import { NODE_ICON_MAP } from "@/lib/nodeDefinitions";
import {
  type PluginEntry,
  PluginEntrySchema,
  PluginListSchema,
} from "@/lib/schemas";
import { api } from "./client";

/** Converts a validated GET /plugins entry into a palette node definition. */
function toDefinition(entry: PluginEntry): NodeDefinition {
  const icon = entry.icon ?? "";
  return {
    type: entry.type,
    label: entry.label ?? entry.type,
    category: "plugin",
    icon: icon in NODE_ICON_MAP ? icon : "Layers",
    description: entry.description ?? "Plugin",
    inputs: entry.inputs,
    outputs: entry.outputs,
    defaultConfig: entry.defaultConfig,
  };
}

export const pluginsApi = {
  /**
   * WASM plugins registered on the server, as palette node definitions.
   * Each entry is validated on its own: a malformed one is skipped rather
   * than hiding every plugin.
   */
  list: async (): Promise<NodeDefinition[]> => {
    const body = PluginListSchema.parse(
      await api.get("plugins").json<unknown>(),
    );
    return body.plugins.flatMap((raw) => {
      const entry = PluginEntrySchema.safeParse(raw);
      return entry.success ? [toDefinition(entry.data)] : [];
    });
  },
};
