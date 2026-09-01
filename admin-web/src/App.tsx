// 应用路由：登录页与受保护的管理后台（8 个权威入口 + 兼容旧版重定向）。
import { BrowserRouter, Navigate, Outlet, Route, Routes, useLocation } from "react-router-dom";
import { AuthProvider, useAuth } from "./context/AuthContext";
import { ToastProvider } from "./context/ToastContext";
import { AppLayout } from "./components/layout";
import { Spinner } from "./components/ui";

// 权威页面
import { OverviewPage } from "./features/overview/OverviewPage";
import { CatalogSearchPage } from "./pages/CatalogSearchPage";
import { CatalogDetailPage } from "./pages/CatalogDetailPage";
import { PublishersPage } from "./pages/PublishersPage";
import { PublisherDetailPage } from "./pages/PublisherDetailPage";
import { CatalogAcquisitionsPage } from "./pages/CatalogAcquisitionsPage";
import { CatalogImportsPage } from "./pages/CatalogImportsPage";
import { InventoryScansPage } from "./features/catalog/InventoryScansPage";
import { InventoryReviewPage } from "./features/catalog/InventoryReviewPage";
import { AccountCenterPage } from "./features/accounts/AccountCenterPage";
import {
  RegistrationQueuePage,
  RegistrationGroupDetailPage,
} from "./features/accounts/RegistrationQueuePage";

// 容器布局与子页面
import { OperationsLayout } from "./features/operations/OperationsLayout";
import { WorkersPage } from "./pages/WorkersPage";
import { WorkerDetailPage } from "./pages/WorkerDetailPage";
import { ProxiesPage } from "./pages/ProxiesPage";
import { SessionsPage } from "./pages/SessionsPage";

import { AttentionLayout } from "./features/attention/AttentionLayout";
import { ManualActionsPage } from "./pages/ManualActionsPage";
import { CatalogQualityPage } from "./pages/CatalogQualityPage";
import { AlertsPage } from "./pages/AlertsPage";

import { SystemLayout } from "./features/system/SystemLayout";
import { LogsPage } from "./pages/LogsPage";
import { SettingsPage } from "./pages/SettingsPage";

import { LoginPage } from "./pages/LoginPage";
import { RedirectWithId, RedirectWithQuery } from "./legacy/redirects";

/** 路由守卫：未登录跳登录页；已登录访问登录页跳首页。 */
function Guard() {
  const { user, loading } = useAuth();
  const location = useLocation();
  if (loading) return <Spinner label="正在恢复会话..." />;
  if (!user) return <Navigate to="/login" state={{ from: location }} replace />;
  return <AppLayout />;
}

function GuestGuard() {
  const { user, loading } = useAuth();
  if (loading) return <Spinner label="正在恢复会话..." />;
  if (user) return <Navigate to="/" replace />;
  return <Outlet />;
}

export function App() {
  return (
    <AuthProvider>
      <ToastProvider>
        <BrowserRouter future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
          <Routes>
            <Route element={<GuestGuard />}>
              <Route path="/login" element={<LoginPage />} />
            </Route>

            <Route element={<Guard />}>
              {/* 1. 图书馆分组 */}
              <Route index element={<OverviewPage />} />
              <Route path="/library" element={<CatalogSearchPage />} />
              <Route path="/library/editions/:id" element={<CatalogDetailPage />} />
              <Route path="/publishers" element={<PublishersPage />} />
              <Route path="/publishers/:id" element={<PublisherDetailPage />} />
              <Route path="/acquisitions" element={<CatalogAcquisitionsPage />} />
              <Route path="/imports" element={<CatalogImportsPage />} />
              <Route path="/inventory-scans" element={<InventoryScansPage />} />
              <Route path="/inventory-reviews" element={<InventoryReviewPage />} />

              {/* 2. 运行分组 */}
              <Route path="/accounts" element={<AccountCenterPage />} />
              <Route path="/accounts/registrations" element={<RegistrationQueuePage />} />
              <Route path="/accounts/registrations/:id" element={<RegistrationGroupDetailPage />} />

              <Route path="/operations" element={<OperationsLayout />}>
                <Route index element={<Navigate to="workers" replace />} />
                <Route path="workers" element={<WorkersPage />} />
                <Route path="workers/:id" element={<WorkerDetailPage />} />
                <Route path="inventory-scans" element={<InventoryScansPage />} />
                <Route path="proxies" element={<ProxiesPage />} />
                <Route path="sessions" element={<SessionsPage />} />
              </Route>

              <Route path="/attention" element={<AttentionLayout />}>
                <Route index element={<Navigate to="manual" replace />} />
                <Route path="manual" element={<ManualActionsPage />} />
                <Route path="inventory-reviews" element={<InventoryReviewPage />} />
                <Route path="quality" element={<CatalogQualityPage />} />
                <Route path="alerts" element={<AlertsPage />} />
              </Route>

              {/* 3. 管理分组 */}
              <Route path="/system" element={<SystemLayout />}>
                <Route index element={<Navigate to="logs" replace />} />
                <Route path="logs" element={<LogsPage />} />
                <Route path="settings" element={<SettingsPage />} />
              </Route>

              {/* 旧版路由全兼容重定向 (Legacy Redirects) */}
              <Route path="/inventory-scans" element={<Navigate to="/operations/inventory-scans" replace />} />
              <Route path="/inventory-reviews" element={<Navigate to="/attention/inventory-reviews" replace />} />
              <Route path="/catalog/overview" element={<Navigate to="/" replace />} />
              <Route path="/catalog/search" element={<RedirectWithQuery to="/library" />} />
              <Route path="/catalog/editions/:id" element={<RedirectWithId to="/library/editions" />} />
              <Route path="/catalog/acquisitions" element={<RedirectWithQuery to="/acquisitions" />} />
              <Route path="/catalog/imports" element={<RedirectWithQuery to="/imports" />} />
              <Route path="/catalog/quality" element={<RedirectWithQuery to="/attention/quality" />} />
              <Route path="/books" element={<Navigate to="/library" replace />} />
              <Route path="/tasks" element={<Navigate to="/acquisitions" replace />} />
              <Route path="/batches" element={<Navigate to="/imports?tab=legacy" replace />} />
              <Route path="/batches/:id" element={<Navigate to="/imports?tab=legacy" replace />} />
              <Route
                path="/account-registration-batches"
                element={<Navigate to="/accounts/registrations" replace />}
              />
              <Route
                path="/account-registration-batches/:id"
                element={<RedirectWithId to="/accounts/registrations" />}
              />
              <Route path="/manual-actions" element={<Navigate to="/attention/manual" replace />} />
              <Route path="/workers" element={<Navigate to="/operations/workers" replace />} />
              <Route path="/workers/:id" element={<RedirectWithId to="/operations/workers" />} />
              <Route path="/proxies" element={<Navigate to="/operations/proxies" replace />} />
              <Route path="/sessions" element={<Navigate to="/operations/sessions" replace />} />
              <Route path="/alerts" element={<Navigate to="/attention/alerts" replace />} />
              <Route path="/logs" element={<Navigate to="/system/logs" replace />} />
              <Route path="/settings" element={<Navigate to="/system/settings" replace />} />

              <Route path="*" element={<Navigate to="/" replace />} />
            </Route>
          </Routes>
        </BrowserRouter>
      </ToastProvider>
    </AuthProvider>
  );
}
