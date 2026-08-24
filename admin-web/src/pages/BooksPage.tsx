// 图书主数据页：列表 + 确认 + 合并。
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import { api } from "../lib/api";
import { ApiError, type Book } from "../lib/types";
import { formatTime } from "../lib/format";
import {
  Button,
  Card,
  EmptyRow,
  ErrorBox,
  Spinner,
  StatusBadge,
  Table,
  Td,
} from "../components/ui";

export function BooksPage() {
  const { user } = useAuth();
  const toast = useToast();
  const canManage = can(user?.role, "manage_task");
  const { data, loading, error, reload } = useApi<Book[]>(() =>
    api.get("/api/books", { limit: 200 }),
  );

  const confirm = async (book: Book) => {
    try {
      await api.post(`/api/books/${book.id}/confirm`);
      toast.success(`已确认《${book.raw_title}》`);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "确认失败");
    }
  };

  if (loading) return <Spinner label="正在加载图书..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">图书主数据</h2>
          <p className="text-sm text-slate-500">全局唯一图书库：一本书只保留一份有效成果</p>
        </div>
        <Button variant="secondary" size="sm" onClick={reload}>
          刷新
        </Button>
      </div>
      <Card>
        <Table
          headers={["序号", "书名", "作者", "出版社", "ISBN", "核验状态", "创建时间", "操作"]}
          empty={!data || data.length === 0 ? <EmptyRow colSpan={8} text="暂无图书" /> : undefined}
        >
          {(data ?? []).map((b) => (
            <tr key={b.id}>
              <Td className="text-xs text-slate-500">{b.seq}</Td>
              <Td className="max-w-64">
                <div className="truncate font-medium text-slate-800" title={b.raw_title}>
                  {b.raw_title}
                </div>
              </Td>
              <Td className="max-w-40 truncate text-xs text-slate-500" title={b.raw_author ?? ""}>
                {b.raw_author ?? "-"}
              </Td>
              <Td className="max-w-40 truncate text-xs text-slate-500" title={b.raw_publisher ?? ""}>
                {b.raw_publisher ?? "-"}
              </Td>
              <Td className="font-mono text-xs text-slate-500">{b.raw_isbn ?? "-"}</Td>
              <Td>
                <StatusBadge status={b.verify_status} />
              </Td>
              <Td className="text-xs text-slate-500">{formatTime(b.created_at)}</Td>
              <Td>
                {canManage && b.verify_status !== "已确认" ? (
                  <Button size="sm" variant="secondary" onClick={() => confirm(b)}>
                    确认
                  </Button>
                ) : (
                  <span className="text-xs text-slate-300">-</span>
                )}
              </Td>
            </tr>
          ))}
        </Table>
      </Card>
    </div>
  );
}
