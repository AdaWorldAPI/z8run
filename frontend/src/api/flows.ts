import {
  assertCreatedFlow,
  assertFlowDetail,
  assertFlowList,
  assertStartResponse,
} from "@/lib/validation";
import type { CreateFlowRequest } from "@/types/flow";
import { api } from "./client";

export interface SaveFlowRequest {
  name?: string;
  description?: string;
  canvas_nodes: unknown[];
  canvas_edges: unknown[];
  viewport: { x: number; y: number; zoom: number };
}

export const flowsApi = {
  list: () => api.get("flows").json<unknown>().then(assertFlowList),

  // Validate at the boundary: the editor casts canvas_nodes/canvas_edges to
  // arrays and maps over them, so a malformed shape must fail fast here.
  get: (id: string) =>
    api.get(`flows/${id}`).json<unknown>().then(assertFlowDetail),

  create: (data: CreateFlowRequest) =>
    api.post("flows", { json: data }).json<unknown>().then(assertCreatedFlow),

  update: (id: string, data: SaveFlowRequest) =>
    api
      .put(`flows/${id}`, { json: data })
      .json<{ id: string; updated_at: string }>(),

  delete: (id: string) => api.delete(`flows/${id}`).json<{ deleted: string }>(),

  start: (id: string) =>
    api.post(`flows/${id}/start`).json<unknown>().then(assertStartResponse),

  stop: (id: string) =>
    api.post(`flows/${id}/stop`).json<{ flow_id: string; status: string }>(),

  export: (id: string) =>
    api
      .get(`flows/${id}/export`)
      .json<{ z8run_version: string; export_format: number; flow: unknown }>(),

  import: (data: unknown) =>
    api
      .post("flows/import", { json: data })
      .json<unknown>()
      .then(assertCreatedFlow),
};
