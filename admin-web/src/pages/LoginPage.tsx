// 登录页：真实调用 /api/auth/login，失败展示后端中文错误。
import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { useAuth } from "../context/AuthContext";
import { ApiError } from "../lib/types";
import { Button, Input } from "../components/ui";

export function LoginPage() {
  const { login } = useAuth();
  const navigate = useNavigate();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setFieldErrors({});
    setLoading(true);
    try {
      await login(username, password);
      navigate("/", { replace: true });
    } catch (err) {
      if (err instanceof ApiError) {
        if (err.status === 422 && err.code === "validation") {
          // 字段级错误（如果有 errors 则逐字段展示，否则展示 message）
          setError(err.message);
        } else {
          setError(err.message);
        }
      } else {
        setError("网络异常，请稍后重试");
      }
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="flex min-h-screen items-center justify-center bg-slate-50 p-4">
      <div className="w-full max-w-md rounded-2xl border border-slate-200 bg-white p-8 shadow-sm">
        <div className="mb-8 text-center">
          <h1 className="text-2xl font-bold text-slate-900">Drission Cloud</h1>
          <p className="mt-1 text-sm text-slate-500">云端 Master 自动化调度管理平台</p>
        </div>
        <form onSubmit={handleSubmit} className="space-y-4">
          <Input
            label="用户名"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoComplete="username"
            required
            error={fieldErrors.username}
          />
          <Input
            label="密码"
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="current-password"
            required
            error={fieldErrors.password}
          />
          {error && (
            <div role="alert" className="rounded-lg bg-red-50 px-3 py-2.5 text-sm text-red-700">
              {error}
            </div>
          )}
          <Button type="submit" loading={loading} className="w-full">
            {loading ? "正在登录..." : "登 录"}
          </Button>
        </form>
      </div>
    </div>
  );
}
