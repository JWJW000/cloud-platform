// 应用路由：登录页与受保护的管理后台。
import { BrowserRouter, Navigate, Outlet, Route, Routes, useLocation } from "react-router-dom";
import { AuthProvider, useAuth } from "./context/AuthContext";
import { ToastProvider } from "./context/ToastContext";
import { AppLayout } from "./components/layout";
import { CatalogOverviewPage } from "./pages/CatalogOverviewPage";
import { CatalogSearchPage } from "./pages/CatalogSearchPage";
import { CatalogDetailPage } from "./pages/CatalogDetailPage";
import { CatalogAcquisitionsPage } from "./pages/CatalogAcquisitionsPage";
import { CatalogImportsPage } from "./pages/CatalogImportsPage";
import { CatalogQualityPage } from "./pages/CatalogQualityPage";
import { LoginPage } from "./pages/LoginPage";
import { DashboardPage } from "./pages/DashboardPage";
import { WorkersPage } from "./pages/WorkersPage";
import { WorkerDetailPage } from "./pages/WorkerDetailPage";
import { BatchesPage } from "./pages/BatchesPage";
import { BatchDetailPage } from "./pages/BatchDetailPage";
import { AccountRegistrationBatchesPage } from "./pages/AccountRegistrationBatchesPage";
import { AccountRegistrationBatchDetailPage } from "./pages/AccountRegistrationBatchDetailPage";
import { ManualActionsPage } from "./pages/ManualActionsPage";
import { BooksPage } from "./pages/BooksPage";
import { TasksPage } from "./pages/TasksPage";
import { AccountsPage } from "./pages/AccountsPage";
import { ProxiesPage } from "./pages/ProxiesPage";
import { SessionsPage } from "./pages/SessionsPage";
import { AlertsPage } from "./pages/AlertsPage";
import { LogsPage } from "./pages/LogsPage";
import { SettingsPage } from "./pages/SettingsPage";
import { Spinner } from "./components/ui";

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
        <BrowserRouter>
          <Routes>
            <Route element={<GuestGuard />}>
              <Route path="/login" element={<LoginPage />} />
            </Route>
            <Route element={<Guard />}>
              <Route index element={<DashboardPage />} />
              <Route path="/catalog/overview" element={<CatalogOverviewPage />} />
              <Route path="/catalog/search" element={<CatalogSearchPage />} />
              <Route path="/catalog/editions/:id" element={<CatalogDetailPage />} />
              <Route path="/catalog/acquisitions" element={<CatalogAcquisitionsPage />} />
              <Route path="/catalog/imports" element={<CatalogImportsPage />} />
              <Route path="/catalog/quality" element={<CatalogQualityPage />} />
              <Route path="/workers" element={<WorkersPage />} />
              <Route path="/workers/:id" element={<WorkerDetailPage />} />
              <Route path="/batches" element={<BatchesPage />} />
              <Route path="/batches/:id" element={<BatchDetailPage />} />
              <Route
                path="/account-registration-batches"
                element={<AccountRegistrationBatchesPage />}
              />
              <Route
                path="/account-registration-batches/:id"
                element={<AccountRegistrationBatchDetailPage />}
              />
              <Route path="/manual-actions" element={<ManualActionsPage />} />
              <Route path="/books" element={<BooksPage />} />
              <Route path="/tasks" element={<TasksPage />} />
              <Route path="/accounts" element={<AccountsPage />} />
              <Route path="/proxies" element={<ProxiesPage />} />
              <Route path="/sessions" element={<SessionsPage />} />
              <Route path="/alerts" element={<AlertsPage />} />
              <Route path="/logs" element={<LogsPage />} />
              <Route path="/settings" element={<SettingsPage />} />
              <Route path="*" element={<Navigate to="/" replace />} />
            </Route>
          </Routes>
        </BrowserRouter>
      </ToastProvider>
    </AuthProvider>
  );
}
