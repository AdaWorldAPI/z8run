/**
 * Tiny, dependency-free runtime validators for the MOST critical API responses.
 *
 * The app otherwise trusts `ky(...).json<T>()` casts blindly. If the backend
 * shape changes or data is corrupted, that surfaces late and confusingly. These
 * hand-written type-guards run at the fetch boundary so a malformed response
 * fails fast with a clear, logged error instead of crashing deep in the UI.
 *
 * They cover the contracts that drive navigation, rendering and execution
 * (R-09): auth, flow detail and list, create/import (whose id we navigate
 * to), start (whose node map links engine events to the canvas) and engine
 * events from the WebSocket. Trivial responses are left typed-only.
 */

import type { AuthResponse } from "@/api/auth";
import type { EngineEvent } from "@/hooks/useEngineSocket";
import type {
  CreateFlowResponse,
  FlowDetail,
  FlowListResponse,
} from "@/types/flow";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Validate a login/register response before it is trusted and stored.
 * Requires a `user` object carrying the expected fields. The session token is
 * NOT part of the body: it lives only in the HttpOnly cookie (A-09).
 * Throws a descriptive Error on any mismatch.
 */
export function assertAuthResponse(value: unknown): AuthResponse {
  if (!isRecord(value)) {
    throw new Error("Invalid auth response: expected an object");
  }
  const user = value.user;
  if (!isRecord(user)) {
    throw new Error("Invalid auth response: missing 'user' object");
  }
  for (const field of ["id", "email", "username"] as const) {
    if (typeof user[field] !== "string") {
      throw new Error(`Invalid auth response: user.${field} must be a string`);
    }
  }
  if (!Array.isArray(user.roles)) {
    throw new Error("Invalid auth response: user.roles must be an array");
  }
  return value as unknown as AuthResponse;
}

/**
 * Validate the flow-detail response the editor loads onto the canvas.
 * The canvas rendering casts `canvas_nodes` / `canvas_edges` to arrays and maps
 * over them, so a non-array here would crash mid-render. Validate the shape
 * (fields are optional in the payload, but when present must be arrays) and
 * throw a descriptive Error otherwise.
 */
export function assertFlowDetail(value: unknown): FlowDetail {
  if (!isRecord(value)) {
    throw new Error("Invalid flow response: expected an object");
  }
  if (typeof value.id !== "string") {
    throw new Error("Invalid flow response: missing or non-string 'id'");
  }
  if (value.canvas_nodes != null && !Array.isArray(value.canvas_nodes)) {
    throw new Error("Invalid flow response: 'canvas_nodes' must be an array");
  }
  if (value.canvas_edges != null && !Array.isArray(value.canvas_edges)) {
    throw new Error("Invalid flow response: 'canvas_edges' must be an array");
  }
  return value as unknown as FlowDetail;
}

const isString = (v: unknown): v is string => typeof v === "string";
const isNumber = (v: unknown): v is number =>
  typeof v === "number" && Number.isFinite(v);

/**
 * Validate the flow list. Every entry is rendered and linked by id, so each
 * must carry the fields the list page reads.
 */
export function assertFlowList(value: unknown): FlowListResponse {
  if (!isRecord(value) || !Array.isArray(value.flows)) {
    throw new Error("Invalid flow list: expected { flows: [...] }");
  }
  value.flows.forEach((flow, i) => {
    if (!isRecord(flow)) {
      throw new Error(`Invalid flow list: entry ${i} is not an object`);
    }
    for (const field of ["id", "name", "status", "updated_at"] as const) {
      if (!isString(flow[field])) {
        throw new Error(
          `Invalid flow list: flows[${i}].${field} must be a string`,
        );
      }
    }
    for (const field of ["nodes", "edges"] as const) {
      if (!isNumber(flow[field])) {
        throw new Error(
          `Invalid flow list: flows[${i}].${field} must be a number`,
        );
      }
    }
    if (flow.last_run_at != null && !isString(flow.last_run_at)) {
      throw new Error(
        `Invalid flow list: flows[${i}].last_run_at must be a string`,
      );
    }
  });
  return value as unknown as FlowListResponse;
}

/** Validate a create/import response: the app navigates to its id. */
export function assertCreatedFlow(value: unknown): CreateFlowResponse {
  if (!isRecord(value) || !isString(value.id) || !value.id) {
    throw new Error("Invalid create response: missing 'id'");
  }
  return value as unknown as CreateFlowResponse;
}

export interface StartFlowResponse {
  flow_id: string;
  trace_id?: string;
  status: string;
  node_map: Record<string, string>;
  routes?: { method: string; path: string }[];
}

/**
 * Validate a start/deploy response. `node_map` links engine events (by core
 * node id) to canvas nodes, so it must map strings to strings.
 */
export function assertStartResponse(value: unknown): StartFlowResponse {
  if (!isRecord(value) || !isString(value.status) || !isString(value.flow_id)) {
    throw new Error("Invalid start response: missing 'flow_id' or 'status'");
  }
  const map = value.node_map;
  if (!isRecord(map) || !Object.values(map).every(isString)) {
    throw new Error("Invalid start response: 'node_map' must map ids to ids");
  }
  if (
    value.routes != null &&
    !(
      Array.isArray(value.routes) &&
      value.routes.every(
        (r) => isRecord(r) && isString(r.method) && isString(r.path),
      )
    )
  ) {
    throw new Error("Invalid start response: 'routes' is malformed");
  }
  return value as unknown as StartFlowResponse;
}

const EVENT_STRING_FIELDS = [
  "flow_id",
  "trace_id",
  "node_id",
  "from_node",
  "to_node",
  "message_id",
  "error",
  "chunk",
] as const;
const EVENT_NUMBER_FIELDS = ["duration_us", "duration_ms"] as const;

/**
 * Parse an engine event from a WebSocket message. Returns `null` for
 * anything that isn't a well-formed event, so a broken message is dropped
 * instead of reaching the stores. Unknown event types are kept: the log
 * shows them generically.
 */
export function parseEngineEvent(raw: unknown): EngineEvent | null {
  if (!isString(raw)) return null;
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!isRecord(value) || !isString(value.type) || !value.type) return null;
  for (const field of EVENT_STRING_FIELDS) {
    if (value[field] != null && !isString(value[field])) return null;
  }
  for (const field of EVENT_NUMBER_FIELDS) {
    if (value[field] != null && !isNumber(value[field])) return null;
  }
  if (value.done != null && typeof value.done !== "boolean") return null;
  return value as unknown as EngineEvent;
}
