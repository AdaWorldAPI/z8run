/**
 * Runtime validation of server responses at the fetch/WebSocket boundary
 * (R-09). A malformed response fails fast here with a clear error instead of
 * crashing deep in the UI. The checks and the resulting types both come from
 * the schemas in `./schemas`, so they can't drift apart.
 */

import { z } from "zod";
import {
  type AuthResponse,
  AuthResponseSchema,
  type CreateFlowResponse,
  CreateFlowResponseSchema,
  type EngineEvent,
  EngineEventSchema,
  type FlowDetail,
  FlowDetailSchema,
  type FlowListResponse,
  FlowListResponseSchema,
  type SessionResponse,
  SessionResponseSchema,
  type StartFlowResponse,
  StartFlowResponseSchema,
} from "./schemas";

/** Parses `value` with `schema`, throwing `Invalid <what>: <details>`. */
function parseOrThrow<T extends z.ZodType>(
  schema: T,
  value: unknown,
  what: string,
): z.infer<T> {
  const result = schema.safeParse(value);
  if (!result.success) {
    throw new Error(`Invalid ${what}: ${z.prettifyError(result.error)}`);
  }
  return result.data;
}

/** Login/register response, before it is trusted and stored. */
export const assertAuthResponse = (value: unknown): AuthResponse =>
  parseOrThrow(AuthResponseSchema, value, "auth response");

/** Session probe response. */
export const assertSessionResponse = (value: unknown): SessionResponse =>
  parseOrThrow(SessionResponseSchema, value, "session response");

/** The flow the editor loads onto the canvas. */
export const assertFlowDetail = (value: unknown): FlowDetail =>
  parseOrThrow(FlowDetailSchema, value, "flow response");

/** The flow list: every entry is rendered and linked by id. */
export const assertFlowList = (value: unknown): FlowListResponse =>
  parseOrThrow(FlowListResponseSchema, value, "flow list");

/** Create/import response: the app navigates to its id. */
export const assertCreatedFlow = (value: unknown): CreateFlowResponse =>
  parseOrThrow(CreateFlowResponseSchema, value, "create response");

/** Start/deploy response: its node map links engine events to the canvas. */
export const assertStartResponse = (value: unknown): StartFlowResponse =>
  parseOrThrow(StartFlowResponseSchema, value, "start response");

/**
 * Parses an engine event from a WebSocket message. Returns `null` for
 * anything that isn't a well-formed event, so a broken message is dropped
 * instead of reaching the stores.
 */
export function parseEngineEvent(raw: unknown): EngineEvent | null {
  if (typeof raw !== "string") return null;
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  const result = EngineEventSchema.safeParse(value);
  return result.success ? result.data : null;
}
