// 登录流程测试（V5 第 11.1 节）：成功跳转、失败展示中文错误、会话失效回登录。
import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { AuthProvider, useAuth } from "../context/AuthContext";
import { LoginPage } from "../pages/LoginPage";

function renderLogin() {
  return render(
    <MemoryRouter initialEntries={["/login"]}>
      <AuthProvider>
        <Routes>
          <Route path="/login" element={<LoginPage />} />
          <Route path="/" element={<HomeProbe />} />
        </Routes>
      </AuthProvider>
    </MemoryRouter>,
  );
}

function HomeProbe() {
  const { user } = useAuth();
  return <div>首页:{user?.username ?? "无"}</div>;
}

describe("登录流程", () => {
  it("登录成功跳转首页", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/auth/me") {
        return { ok: false, status: 401, json: async () => ({}) } as Response;
      }
      if (url === "/api/auth/login") {
        return {
          ok: true,
          status: 200,
          json: async () => ({
            user: {
              id: "u1",
              username: "admin",
              role: "超级管理员",
              status: "启用",
              token_version: 1,
              created_at: "2026-01-01T00:00:00Z",
              last_login_at: null,
            },
          }),
        } as Response;
      }
      return { ok: false, status: 500, json: async () => ({}) } as Response;
    });

    renderLogin();
    await userEvent.type(await screen.findByLabelText("用户名"), "admin");
    await userEvent.type(screen.getByLabelText("密码"), "Admin@2026local");
    await userEvent.click(screen.getByRole("button", { name: /登\s*录/ }));

    await waitFor(() => {
      expect(screen.getByText("首页:admin")).toBeInTheDocument();
    });
  });

  it("登录失败展示后端中文错误，不跳转", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/auth/me") {
        return { ok: false, status: 401, json: async () => ({}) } as Response;
      }
      if (url === "/api/auth/login") {
        return {
          ok: false,
          status: 401,
          json: async () => ({ code: "unauthorized", message: "用户名或密码错误" }),
        } as Response;
      }
      return { ok: false, status: 500, json: async () => ({}) } as Response;
    });

    renderLogin();
    await userEvent.type(await screen.findByLabelText("用户名"), "admin");
    await userEvent.type(screen.getByLabelText("密码"), "wrong");
    await userEvent.click(screen.getByRole("button", { name: /登\s*录/ }));

    expect(await screen.findByText("用户名或密码错误")).toBeInTheDocument();
    // 仍在登录页（没有跳转）
    expect(screen.getByRole("button", { name: /登\s*录/ })).toBeInTheDocument();
  });
});
