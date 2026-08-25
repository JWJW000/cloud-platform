import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ToastProvider } from "../context/ToastContext";
import * as api from "../lib/api";
import { MailProviderStatus } from "../features/accounts/MailProviderStatus";
import { MailCodeSettings } from "../features/system/MailCodeSettings";

describe("Outlook Provider 前端脱敏与状态", () => {
  beforeEach(() => vi.restoreAllMocks());

  it("账号中心只显示脱敏状态、版本和 Worker 应用数", async () => {
    vi.spyOn(api, "getMailProviderStatus").mockResolvedValue({
      provider_type: "outlook_http",
      version: 7,
      is_active: true,
      has_api_key: true,
      health: "Worker 已全部应用",
      workers_applied: 3,
      workers_online: 3,
    });
    render(<MemoryRouter future={{ v7_startTransition: true, v7_relativeSplatPath: true }}><MailProviderStatus /></MemoryRouter>);
    await waitFor(() => expect(screen.getByText(/版本 v7/)).toBeInTheDocument());
    expect(screen.getByText(/Worker 应用 3\/3/)).toBeInTheDocument();
    expect(screen.queryByText(/https:\/\//)).not.toBeInTheDocument();
  });

  it("设置读取响应不含 API Key，密钥输入始终为空且为 password", async () => {
    vi.spyOn(api, "getMailProviderConfig").mockResolvedValue({
      provider_type: "outlook_http",
      endpoint: "https://mail.example.com/api/external/emails",
      has_api_key: true,
      poll_interval_secs: 5,
      timeout_secs: 60,
      allowed_hosts: ["mail.example.com"],
      allowed_senders: ["no-reply@example.com"],
      version: 7,
      is_active: true,
      updated_by: "root",
      updated_at: "2026-08-25T00:00:00Z",
    });
    render(
      <ToastProvider>
        <MailCodeSettings canManage />
      </ToastProvider>,
    );
    const input = await screen.findByPlaceholderText(/如需修改请输入新密钥/);
    expect(input).toHaveAttribute("type", "password");
    expect(input).toHaveValue("");
    expect(screen.getByDisplayValue("no-reply@example.com")).toBeInTheDocument();
  });
});
