import { describe, expect, it } from "vitest";
import {
  assertCreatedFlow,
  assertFlowList,
  assertStartResponse,
  parseEngineEvent,
} from "./validation";

const flow = {
  id: "01a0b6e5",
  name: "Demo",
  description: "",
  status: "completed",
  last_run_at: null,
  nodes: 4,
  edges: 2,
  created_at: "2026-09-19T00:00:00Z",
  updated_at: "2026-09-19T00:00:00Z",
};

describe("flow list", () => {
  it("accepts the backend shape", () => {
    expect(assertFlowList({ flows: [flow], total: 1 }).flows[0]?.id).toBe(
      "01a0b6e5",
    );
  });

  it.each([
    ["not an object", null],
    ["flows not an array", { flows: {} }],
    ["entry without id", { flows: [{ ...flow, id: undefined }] }],
    ["nodes as text", { flows: [{ ...flow, nodes: "4" }] }],
    ["bad last_run_at", { flows: [{ ...flow, last_run_at: 5 }] }],
  ])("rejects %s", (_, value) => {
    expect(() => assertFlowList(value)).toThrow(/Invalid flow list/);
  });
});

describe("create / import", () => {
  it("needs a non-empty id to navigate to", () => {
    expect(assertCreatedFlow({ id: "x", name: "n" }).id).toBe("x");
    expect(() => assertCreatedFlow({ id: "" })).toThrow();
    expect(() => assertCreatedFlow({ name: "n" })).toThrow();
  });
});

describe("start", () => {
  const ok = {
    flow_id: "f",
    status: "deployed",
    node_map: { t: "4c21a861" },
    routes: [{ method: "POST", path: "/hook/f/echo" }],
  };

  it("accepts deployed and running responses", () => {
    expect(assertStartResponse(ok).node_map.t).toBe("4c21a861");
    expect(
      assertStartResponse({
        ...ok,
        status: "running",
        trace_id: "tr",
        routes: undefined,
      }).status,
    ).toBe("running");
  });

  it("rejects a node map that doesn't map ids to ids", () => {
    expect(() => assertStartResponse({ ...ok, node_map: { t: 1 } })).toThrow(
      /node_map/,
    );
    expect(() => assertStartResponse({ ...ok, node_map: [] })).toThrow();
    expect(() =>
      assertStartResponse({ ...ok, routes: [{ method: 1 }] }),
    ).toThrow(/routes/);
  });
});

describe("engine events", () => {
  it("parses well-formed events, known or not", () => {
    const e = parseEngineEvent(
      JSON.stringify({ type: "flow_stopped", flow_id: "f", duration_ms: 12 }),
    );
    expect(e?.type).toBe("flow_stopped");
    expect(
      parseEngineEvent(JSON.stringify({ type: "future_event" }))?.type,
    ).toBe("future_event");
  });

  it.each([
    ["not JSON", "{oops"],
    ["not a string", 42],
    ["an array", "[1]"],
    ["no type", JSON.stringify({ flow_id: "f" })],
    ["numeric node_id", JSON.stringify({ type: "node_started", node_id: 7 })],
    [
      "text duration",
      JSON.stringify({ type: "flow_completed", duration_ms: "5" }),
    ],
    ["non-boolean done", JSON.stringify({ type: "stream_chunk", done: "yes" })],
  ])("drops %s", (_, raw) => {
    expect(parseEngineEvent(raw)).toBeNull();
  });
});
