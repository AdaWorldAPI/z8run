/**
 * Schemas for the data the app receives from the server (R-09).
 *
 * Each response type is derived from its schema (`z.infer`), so the checks
 * and the types can't drift apart. Objects are "loose": fields the server
 * adds later pass through untouched instead of being stripped.
 */

import { z } from "zod";

// ── Auth ─────────────────────────────────────────────────

export const UserInfoSchema = z.looseObject({
  id: z.string(),
  email: z.string(),
  username: z.string(),
  roles: z.array(z.string()),
});
export type UserInfo = z.infer<typeof UserInfoSchema>;

/** Login/register body. The session token is NOT here: it lives only in
 *  the HttpOnly cookie (A-09). */
export const AuthResponseSchema = z.looseObject({ user: UserInfoSchema });
export type AuthResponse = z.infer<typeof AuthResponseSchema>;

/** Session probe: always 200, `user` is null when signed out. */
export const SessionResponseSchema = z.looseObject({
  user: UserInfoSchema.nullable(),
});
export type SessionResponse = z.infer<typeof SessionResponseSchema>;

// ── Flows ────────────────────────────────────────────────

export const FlowSummarySchema = z.looseObject({
  id: z.string().min(1),
  name: z.string(),
  description: z.string(),
  /** Derived from the last execution when the flow has run, else the stored status. */
  status: z.string(),
  /** ISO timestamp of the last execution, or null if it has never run. */
  last_run_at: z.string().nullable(),
  nodes: z.number(),
  edges: z.number(),
  created_at: z.string(),
  updated_at: z.string(),
});
export type FlowSummary = z.infer<typeof FlowSummarySchema>;

export const FlowListResponseSchema = z.looseObject({
  flows: z.array(FlowSummarySchema),
  total: z.number(),
});
export type FlowListResponse = z.infer<typeof FlowListResponseSchema>;

/** The flow the editor loads. The canvas maps over `canvas_nodes` and
 *  `canvas_edges`, so they must be arrays when present. */
export const FlowDetailSchema = z.looseObject({
  id: z.string().min(1),
  name: z.string(),
  description: z.string(),
  version: z.string(),
  status: z.string(),
  /** True while the flow's public hooks are live (deployed snapshot exists). */
  deployed: z.boolean().optional(),
  nodes: z.array(z.unknown()),
  edges: z.array(z.unknown()),
  canvas_nodes: z.array(z.unknown()).nullish(),
  canvas_edges: z.array(z.unknown()).nullish(),
  viewport: z.object({ x: z.number(), y: z.number(), zoom: z.number() }),
  config: z.unknown(),
  created_at: z.string(),
  updated_at: z.string(),
});
export type FlowDetail = z.infer<typeof FlowDetailSchema>;

/** Create/import response: the app navigates to its id. */
export const CreateFlowResponseSchema = z.looseObject({
  id: z.string().min(1),
  name: z.string(),
  description: z.string().optional(),
  status: z.string().optional(),
  created_at: z.string().optional(),
});
export type CreateFlowResponse = z.infer<typeof CreateFlowResponseSchema>;

/** Start/deploy response. `node_map` links engine events (by core node id)
 *  to canvas nodes. */
export const StartFlowResponseSchema = z.looseObject({
  flow_id: z.string(),
  trace_id: z.string().optional(),
  status: z.string(),
  node_map: z.record(z.string(), z.string()),
  routes: z
    .array(z.object({ method: z.string(), path: z.string() }))
    .optional(),
});
export type StartFlowResponse = z.infer<typeof StartFlowResponseSchema>;

// ── Engine events (WebSocket) ────────────────────────────

/** An engine event. Unknown event types are kept (the log shows them
 *  generically); known fields must have the right type. */
export const EngineEventSchema = z.looseObject({
  type: z.string().min(1),
  flow_id: z.string().optional(),
  trace_id: z.string().optional(),
  node_id: z.string().optional(),
  from_node: z.string().optional(),
  to_node: z.string().optional(),
  message_id: z.string().optional(),
  duration_us: z.number().optional(),
  duration_ms: z.number().optional(),
  error: z.string().optional(),
  /** Payload preview for message_sent events */
  payload: z.unknown().optional(),
  /** Output preview for node_completed events */
  output: z.unknown().optional(),
  /** Streaming chunk for stream_chunk events */
  chunk: z.string().optional(),
  /** Whether streaming is complete for stream_chunk events */
  done: z.boolean().optional(),
});
export type EngineEvent = z.infer<typeof EngineEventSchema>;

// ── Plugins ──────────────────────────────────────────────

const PORT_TYPES = [
  "any",
  "string",
  "number",
  "boolean",
  "object",
  "array",
  "binary",
] as const;

export const PluginPortSchema = z.looseObject({
  id: z.string(),
  name: z.string(),
  // Types the editor doesn't know are shown as `any`.
  type: z.enum(PORT_TYPES).catch("any"),
  required: z.boolean().default(false),
});

/** One entry of GET /plugins, shaped like an editor node definition. */
export const PluginEntrySchema = z.looseObject({
  type: z.string().min(1),
  label: z.string().optional(),
  description: z.string().optional(),
  icon: z.string().optional(),
  inputs: z.array(PluginPortSchema).default([]),
  outputs: z.array(PluginPortSchema).default([]),
  defaultConfig: z.record(z.string(), z.unknown()).default({}),
});
export type PluginEntry = z.infer<typeof PluginEntrySchema>;

export const PluginListSchema = z.looseObject({
  plugins: z.array(z.unknown()),
});
