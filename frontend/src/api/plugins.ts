import type { NodeDefinition } from "@/lib/nodeDefinitions";
import { NODE_ICON_MAP } from "@/lib/nodeDefinitions";
import type { PortDefinition, PortType } from "@/types/flow";
import { api } from "./client";

const PORT_TYPES: PortType[] = [
  "any",
  "string",
  "number",
  "boolean",
  "object",
  "array",
  "binary",
];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function toPorts(value: unknown): PortDefinition[] {
  if (!Array.isArray(value)) return [];
  return value.filter(isRecord).map((p) => ({
    id: String(p.id ?? p.name ?? ""),
    name: String(p.name ?? p.id ?? ""),
    type: PORT_TYPES.includes(p.type as PortType)
      ? (p.type as PortType)
      : "any",
    required: p.required === true,
  }));
}

/** Converts one entry of GET /plugins into a palette node definition. */
function toDefinition(value: unknown): NodeDefinition | null {
  if (!isRecord(value) || typeof value.type !== "string" || !value.type) {
    return null;
  }
  const icon = typeof value.icon === "string" ? value.icon : "";
  return {
    type: value.type,
    label: typeof value.label === "string" ? value.label : value.type,
    category: "plugin",
    icon: icon in NODE_ICON_MAP ? icon : "Layers",
    description:
      typeof value.description === "string" ? value.description : "Plugin",
    inputs: toPorts(value.inputs),
    outputs: toPorts(value.outputs),
    defaultConfig: isRecord(value.defaultConfig) ? value.defaultConfig : {},
  };
}

export const pluginsApi = {
  /** WASM plugins registered on the server, as palette node definitions. */
  list: async (): Promise<NodeDefinition[]> => {
    const body = await api.get("plugins").json<unknown>();
    const plugins =
      isRecord(body) && Array.isArray(body.plugins) ? body.plugins : [];
    return plugins
      .map(toDefinition)
      .filter((d): d is NodeDefinition => d !== null);
  },
};
