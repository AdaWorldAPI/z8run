import { UserInfoSchema } from "@/lib/schemas";
import { assertAuthResponse, assertSessionResponse } from "@/lib/validation";
import ky from "ky";

const authApi = ky.create({
  prefixUrl: "/auth",
  timeout: 10000,
  // Send/receive the HttpOnly session cookie (SEC-009).
  credentials: "include",
});

export type { AuthResponse, UserInfo } from "@/lib/schemas";

export const authService = {
  register: (email: string, username: string, password: string) =>
    authApi
      .post("register", { json: { email, username, password } })
      .json<unknown>()
      .then(assertAuthResponse),

  login: (email: string, password: string) =>
    authApi
      .post("login", { json: { email, password } })
      .json<unknown>()
      .then(assertAuthResponse),

  // Auth is carried by the session cookie; no token argument needed.
  me: () =>
    authApi
      .get("me")
      .json<unknown>()
      .then((v) => UserInfoSchema.parse(v)),

  // Session probe used on load: always 200 with { user } (null if not signed
  // in), so the browser console stays clean.
  session: () =>
    authApi.get("session").json<unknown>().then(assertSessionResponse),

  logout: () => authApi.post("logout").json<{ status: string }>(),
};
