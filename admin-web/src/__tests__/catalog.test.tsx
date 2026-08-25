import { describe, it, expect, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { CatalogOverviewPage } from "../pages/CatalogOverviewPage";
import { CatalogSearchPage } from "../pages/CatalogSearchPage";
import * as api from "../lib/api";
import { ToastProvider } from "../context/ToastContext";

describe("图书馆总库页面渲染与交互测试", () => {
  it("总览页面正常渲染核心指标卡片", async () => {
    vi.spyOn(api, "getCatalogStats").mockResolvedValue({
      total_sources: 5,
      total_source_records: 18908445,
      total_works: 12000000,
      total_editions: 14000000,
      total_chapters: 1584965,
      total_holdings: 800000,
      total_library_files: 750000,
      total_library_bytes: 107374182400,
      acquired_targets: 800000,
      pending_targets: 13200000,
      downloading_targets: 120,
      failed_targets: 15,
      needs_confirm_targets: 30,
      total_quarantined: 12,
      missing_isbn_count: 500000,
      missing_author_count: 20000,
      ambiguous_works_count: 140,
    });
    vi.spyOn(api, "listCatalogImportRuns").mockResolvedValue([]);

    render(
      <ToastProvider>
        <MemoryRouter future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
          <CatalogOverviewPage />
        </MemoryRouter>
      </ToastProvider>
    );

    await waitFor(() => {
      expect(screen.getByText("图书馆总库与索引总览")).toBeDefined();
      expect(screen.getByText("18,908,445")).toBeDefined();
    });
  });

  it("检索页面正常展示检索结果列表", async () => {
    vi.spyOn(api, "searchCatalog").mockResolvedValue({
      items: [
        {
          id: "ed-1",
          work_id: "wk-1",
          work_type: "整书",
          title: "算法导论（第3版）",
          authors: ["Thomas Cormen"],
          publisher: "机械工业出版社",
          publish_year: 2013,
          language: "zh",
          identifiers: ["9787111407010"],
          source_formats: ["epub", "pdf"],
          holding_formats: ["pdf"],
          acquisition_status: "已下载",
          resolution_status: "已确认",
          updated_at: new Date().toISOString(),
        },
      ],
      total: 1,
      limit: 20,
      offset: 0,
      status_facets: [],
      language_facets: [],
      format_facets: [],
    });

    render(
      <ToastProvider>
        <MemoryRouter future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
          <CatalogSearchPage />
        </MemoryRouter>
      </ToastProvider>
    );

    await waitFor(() => {
      expect(screen.getByText("算法导论（第3版）")).toBeDefined();
      expect(screen.getByText("Thomas Cormen")).toBeDefined();
    });
  });
});
