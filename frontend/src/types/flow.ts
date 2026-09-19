export type PortType =
  | "any"
  | "string"
  | "number"
  | "boolean"
  | "object"
  | "array"
  | "binary";

export type NodeCategory =
  | "input"
  | "process"
  | "output"
  | "logic"
  | "data"
  | "ai"
  | "communication"
  | "security"
  | "plugin";

export type NodeStatus = "idle" | "running" | "success" | "error" | "disabled";

export type FlowStatus =
  | "idle"
  | "running"
  | "paused"
  | "completed"
  | "error"
  | "stopped";

export interface PortDefinition {
  id: string;
  name: string;
  type: PortType;
  required?: boolean;
}

export interface Z8NodeData {
  [key: string]: unknown;
  label: string;
  type: string;
  category: NodeCategory;
  icon: string;
  config: Record<string, unknown>;
  status: NodeStatus;
  lastOutput?: unknown;
  inputs: PortDefinition[];
  outputs: PortDefinition[];
}

// Server response types are derived from their runtime schemas.
export type {
  CreateFlowResponse,
  FlowDetail,
  FlowListResponse,
  FlowSummary,
} from "@/lib/schemas";

export interface CreateFlowRequest {
  name: string;
  description?: string;
}

/** Port type color mapping */
export const PORT_COLORS: Record<PortType, string> = {
  any: "#94A3B8",
  string: "#22C55E",
  number: "#3B82F6",
  boolean: "#F59E0B",
  object: "#8B5CF6",
  array: "#EC4899",
  binary: "#EF4444",
};

/** Node category color mapping */
export const CATEGORY_COLORS: Record<NodeCategory, string> = {
  input: "#22C55E",
  process: "#3B82F6",
  output: "#F59E0B",
  logic: "#8B5CF6",
  data: "#06B6D4",
  ai: "#EC4899",
  communication: "#F97316",
  security: "#EF4444",
  plugin: "#A3A3A3",
};
