import { useState } from "react";
import { Link } from "react-router-dom";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import {
  listPublishers,
  createPublisher,
  mergePublishers,
  syncPublishersFromEditions,
} from "../lib/api";
import type { PublisherListResponse } from "../lib/types";
import {
  Button,
  Card,
  CardHeader,
  Dialog,
  EmptyRow,
  ErrorBox,
  Input,
  Select,
  SkeletonTable,
  Table,
  Td,
  Badge,
} from "../components/ui";
import {
  Building2,
  Plus,
  Search,
  GitMerge,
  RefreshCw,
  ChevronLeft,
  ChevronRight,
  BookOpen,
} from "lucide-react";

export function PublishersPage() {
  const { user } = useAuth();
  const toast = useToast();
  const canWrite = can(user?.role, "manage_account");
  const isSuperAdmin = user?.role === "超级管理员";

  const [page, setPage] = useState(1);
  const [query, setQuery] = useState("");
  const [searchKw, setSearchKw] = useState("");
  const [sortBy, setSortBy] = useState("editions");

  const { data, loading, error, reload } = useApi<PublisherListResponse>(
    () =>
      listPublishers({
        query: searchKw || undefined,
        sort_by: sortBy,
        limit: 20,
        offset: (page - 1) * 20,
      }),
    [page, searchKw, sortBy]
  );

  // 新增出版社 Modal
  const [createOpen, setCreateOpen] = useState(false);
  const [newName, setNewName] = useState("");
  const [newCountry, setNewCountry] = useState("");
  const [creating, setCreating] = useState(false);

  // 合并出版社 Modal
  const [mergeOpen, setMergeOpen] = useState(false);
  const [sourceId, setSourceId] = useState("");
  const [targetId, setTargetId] = useState("");
  const [merging, setMerging] = useState(false);

  // 一键同步初始化
  const [syncing, setSyncing] = useState(false);

  const handleSearch = (e: React.FormEvent) => {
    e.preventDefault();
    setPage(1);
    setSearchKw(query.trim());
  };

  const handleCreate = async () => {
    if (!newName.trim()) {
      toast.error("出版社名称不能为空");
      return;
    }
    setCreating(true);
    try {
      await createPublisher({
        name: newName.trim(),
        country: newCountry.trim() || undefined,
      });
      toast.success(`出版社「${newName.trim()}」创建成功`);
      setCreateOpen(false);
      setNewName("");
      setNewCountry("");
      reload();
    } catch (e: any) {
      toast.error(e?.message || "创建出版社失败");
    } finally {
      setCreating(false);
    }
  };

  const handleMerge = async () => {
    if (!sourceId.trim() || !targetId.trim()) {
      toast.error("请同时指定源出版社和目标出版社编号");
      return;
    }
    if (sourceId.trim() === targetId.trim()) {
      toast.error("源出版社与目标出版社不能相同");
      return;
    }
    setMerging(true);
    try {
      const res = await mergePublishers(sourceId.trim(), targetId.trim());
      toast.success(res.message || "出版社合并成功");
      setMergeOpen(false);
      setSourceId("");
      setTargetId("");
      reload();
    } catch (e: any) {
      toast.error(e?.message || "合并失败");
    } finally {
      setMerging(false);
    }
  };

  const handleSyncFromEditions = async () => {
    setSyncing(true);
    try {
      const res = await syncPublishersFromEditions();
      toast.success(res.message || "同步完成");
      reload();
    } catch (e: any) {
      toast.error(e?.message || "同步失败");
    } finally {
      setSyncing(false);
    }
  };

  const items = data?.items ?? [];
  const total = data?.total ?? 0;
  const totalPages = Math.max(1, Math.ceil(total / 20));

  return (
    <div className="space-y-6">
      {/* 顶部标题与操作栏 */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-slate-900 flex items-center gap-2">
            <Building2 className="h-6 w-6 text-blue-600" />
            <span>出版社管理</span>
          </h1>
          <p className="text-xs text-slate-500">
            全库规范出版社主档、别名消歧归一与专属书库导航
          </p>
        </div>
        <div className="flex items-center gap-2">
          {isSuperAdmin && (
            <Button
              variant="secondary"
              size="sm"
              loading={syncing}
              onClick={handleSyncFromEditions}
            >
              <RefreshCw className="mr-1.5 h-4 w-4 text-blue-600" />
              从总库初始化主档
            </Button>
          )}
          {canWrite && (
            <Button variant="secondary" size="sm" onClick={() => setMergeOpen(true)}>
              <GitMerge className="mr-1.5 h-4 w-4 text-purple-600" />
              合并出版社
            </Button>
          )}
          {canWrite && (
            <Button size="sm" onClick={() => setCreateOpen(true)}>
              <Plus className="mr-1.5 h-4 w-4" />
              新增出版社
            </Button>
          )}
        </div>
      </div>

      {error && <ErrorBox message={error} onRetry={reload} />}

      {/* 筛选与搜索 */}
      <Card className="p-4">
        <form onSubmit={handleSearch} className="flex flex-wrap items-center gap-3">
          <div className="relative flex-1 min-w-[240px]">
            <Search className="absolute left-3 top-2.5 h-4 w-4 text-slate-400" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索出版社名称或简介..."
              className="pl-9 text-xs"
            />
          </div>
          <div className="w-48">
            <Select
              value={sortBy}
              onChange={(e) => {
                setSortBy(e.target.value);
                setPage(1);
              }}
              className="text-xs"
            >
              <option value="editions">按图书版本量降序</option>
              <option value="acquired">按已下载馆藏降序</option>
              <option value="holdings">按馆藏文件总数降序</option>
              <option value="name">按名称字母排序</option>
            </Select>
          </div>
          <Button type="submit" size="sm" variant="secondary">
            搜索
          </Button>
          {searchKw && (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              onClick={() => {
                setQuery("");
                setSearchKw("");
                setPage(1);
              }}
            >
              清除搜索
            </Button>
          )}
        </form>
      </Card>

      {/* 出版社列表表格 */}
      <Card>
        <CardHeader
          title={`全部出版社 (${total.toLocaleString()} 家)`}
          description="点击出版社进入专属书目库，可查看该社所有收录图书及馆藏状态"
        />
        <Table
          headers={["出版社名称", "国家/地区", "总作品数", "收录版本数", "已下载馆藏", "馆藏覆盖率", "操作"]}
          empty={!loading && items.length === 0 ? <EmptyRow colSpan={7} text="暂无出版社数据" /> : undefined}
        >
          {loading && !data ? (
            <SkeletonTable columns={7} rows={8} />
          ) : (
            items.map((pub) => {
              const rate = pub.editions_count > 0 ? ((pub.acquired_count / pub.editions_count) * 100).toFixed(1) : "0.0";
              return (
                <tr key={pub.id} className="hover:bg-slate-50/80 transition-colors">
                  <Td className="font-semibold text-slate-900">
                    <Link
                      to={`/publishers/${pub.id}`}
                      className="text-blue-600 hover:text-blue-800 hover:underline inline-flex items-center gap-1.5"
                    >
                      <span>{pub.name}</span>
                    </Link>
                  </Td>
                  <Td className="text-xs text-slate-500">
                    {pub.country ? <Badge variant="neutral">{pub.country}</Badge> : "-"}
                  </Td>
                  <Td className="text-xs text-slate-600 font-medium">
                    {pub.works_count.toLocaleString()}
                  </Td>
                  <Td className="text-xs text-slate-900 font-bold">
                    {pub.editions_count.toLocaleString()}
                  </Td>
                  <Td className="text-xs text-green-600 font-semibold">
                    {pub.acquired_count.toLocaleString()}
                  </Td>
                  <Td className="text-xs text-slate-500">
                    <div className="flex items-center gap-2">
                      <div className="w-16 bg-slate-100 rounded-full h-1.5 overflow-hidden">
                        <div
                          className="bg-blue-600 h-1.5 rounded-full"
                          style={{ width: `${Math.min(100, Number(rate))}%` }}
                        />
                      </div>
                      <span className="text-[11px] font-mono">{rate}%</span>
                    </div>
                  </Td>
                  <Td>
                    <Link to={`/publishers/${pub.id}`}>
                      <Button size="sm" variant="ghost">
                        <BookOpen className="mr-1 h-3.5 w-3.5 text-blue-600" />
                        专属书库
                      </Button>
                    </Link>
                  </Td>
                </tr>
              );
            })
          )}
        </Table>

        {data && data.total > 0 && (
          <div className="border-t border-slate-100 p-4 flex items-center justify-between">
            <Button
              variant="secondary"
              size="sm"
              disabled={page <= 1}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
            >
              <ChevronLeft className="h-4 w-4" />
              上一页
            </Button>
            <span className="text-xs text-slate-500">
              第 {page} / {totalPages} 页 · 共 {data.total.toLocaleString()} 家出版社
            </span>
            <Button
              variant="secondary"
              size="sm"
              disabled={page >= totalPages}
              onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
            >
              下一页
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        )}
      </Card>

      {/* 新增出版社 Dialog */}
      <Dialog
        open={createOpen}
        title="新增规范出版社"
        onClose={() => setCreateOpen(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreateOpen(false)}>
              取消
            </Button>
            <Button onClick={handleCreate} loading={creating}>
              确认创建
            </Button>
          </>
        }
      >
        <div className="space-y-4 text-xs">
          <div>
            <label className="block font-medium text-slate-700 mb-1">
              出版社名称 <span className="text-red-500">*</span>
            </label>
            <Input
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              placeholder="如：清华大学出版社"
            />
          </div>
          <div>
            <label className="block font-medium text-slate-700 mb-1">
              国家/地区 (可选)
            </label>
            <Input
              value={newCountry}
              onChange={(e) => setNewCountry(e.target.value)}
              placeholder="如：中国 / 美国"
            />
          </div>
        </div>
      </Dialog>

      {/* 合并出版社 Dialog */}
      <Dialog
        open={mergeOpen}
        title="合并出版社（消歧归一）"
        onClose={() => setMergeOpen(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setMergeOpen(false)}>
              取消
            </Button>
            <Button onClick={handleMerge} loading={merging} variant="danger">
              确认合并
            </Button>
          </>
        }
      >
        <div className="space-y-4 text-xs">
          <p className="text-slate-500 leading-relaxed">
            合并操作将把 <strong>源出版社</strong> 及其所有别名、关联图书版本全部迁移到 <strong>目标出版社</strong> 下，并将源出版社名称作为别名沉淀给目标出版社。
          </p>
          <div>
            <label className="block font-medium text-slate-700 mb-1">
              源出版社 ID (将被合并并删除)
            </label>
            <Input
              value={sourceId}
              onChange={(e) => setSourceId(e.target.value)}
              placeholder="输入源出版社的 UUID"
            />
          </div>
          <div>
            <label className="block font-medium text-slate-700 mb-1">
              目标出版社 ID (保留并继承图书)
            </label>
            <Input
              value={targetId}
              onChange={(e) => setTargetId(e.target.value)}
              placeholder="输入目标出版社的 UUID"
            />
          </div>
        </div>
      </Dialog>
    </div>
  );
}
