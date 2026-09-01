import { useState } from "react";
import { useParams, Link } from "react-router-dom";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import {
  getPublisher,
  updatePublisher,
  addPublisherAlias,
  listPublisherEditions,
} from "../lib/api";
import type {
  PublisherDetailResponse,
  PublisherEditionsResponse,
} from "../lib/types";
import {
  Button,
  Card,
  Dialog,
  EmptyRow,
  ErrorBox,
  Input,
  Select,
  SkeletonCard,
  SkeletonTable,
  StatusBadge,
  Table,
  Td,
  Badge,
} from "../components/ui";
import {
  Building2,
  Globe,
  Tag,
  BookOpen,
  Edit2,
  ChevronLeft,
  ChevronRight,
  ArrowLeft,
} from "lucide-react";

export function PublisherDetailPage() {
  const { id } = useParams<{ id: string }>();
  const { user } = useAuth();
  const toast = useToast();
  const canWrite = can(user?.role, "manage_account");

  const [page, setPage] = useState(1);
  const [statusFilter, setStatusFilter] = useState<string>("");

  // 出版社详情
  const {
    data: detailData,
    loading: detailLoading,
    error: detailError,
    reload: reloadDetail,
  } = useApi<PublisherDetailResponse>(() => getPublisher(id!), [id]);

  // 出版社专属书目
  const {
    data: editionsData,
    loading: editionsLoading,
    error: editionsError,
    reload: reloadEditions,
  } = useApi<PublisherEditionsResponse>(
    () =>
      listPublisherEditions(id!, {
        status: statusFilter || undefined,
        limit: 20,
        offset: (page - 1) * 20,
      }),
    [id, page, statusFilter]
  );

  // 编辑信息 Modal
  const [editOpen, setEditOpen] = useState(false);
  const [editName, setEditName] = useState("");
  const [editCountry, setEditCountry] = useState("");
  const [editWebsite, setEditWebsite] = useState("");
  const [editDesc, setEditDesc] = useState("");
  const [savingEdit, setSavingEdit] = useState(false);

  // 添加别名 Modal
  const [aliasOpen, setAliasOpen] = useState(false);
  const [newAlias, setNewAlias] = useState("");
  const [addingAlias, setAddingAlias] = useState(false);

  const publisher = detailData?.publisher;
  const aliases = detailData?.aliases ?? [];

  const handleOpenEdit = () => {
    if (!publisher) return;
    setEditName(publisher.name);
    setEditCountry(publisher.country || "");
    setEditWebsite(publisher.website || "");
    setEditDesc(publisher.description || "");
    setEditOpen(true);
  };

  const handleSaveEdit = async () => {
    if (!editName.trim()) {
      toast.error("出版社名称不能为空");
      return;
    }
    setSavingEdit(true);
    try {
      await updatePublisher(id!, {
        name: editName.trim(),
        country: editCountry.trim() || undefined,
        website: editWebsite.trim() || undefined,
        description: editDesc.trim() || undefined,
      });
      toast.success("出版社信息已更新");
      setEditOpen(false);
      reloadDetail();
    } catch (e: any) {
      toast.error(e?.message || "更新失败");
    } finally {
      setSavingEdit(false);
    }
  };

  const handleAddAlias = async () => {
    if (!newAlias.trim()) {
      toast.error("别名不能为空");
      return;
    }
    setAddingAlias(true);
    try {
      await addPublisherAlias(id!, newAlias.trim());
      toast.success(`别名「${newAlias.trim()}」已添加，并已自动关联匹配图书`);
      setAliasOpen(false);
      setNewAlias("");
      reloadDetail();
      reloadEditions();
    } catch (e: any) {
      toast.error(e?.message || "添加别名失败");
    } finally {
      setAddingAlias(false);
    }
  };

  const editions = editionsData?.items ?? [];
  const totalEditions = editionsData?.total ?? 0;
  const totalPages = Math.max(1, Math.ceil(totalEditions / 20));

  if (detailLoading && !detailData) {
    return (
      <div className="space-y-6">
        <SkeletonCard />
        <SkeletonCard />
      </div>
    );
  }

  if (detailError || !publisher) {
    return (
      <div className="space-y-4">
        <Link to="/publishers" className="inline-flex items-center text-xs text-slate-500 hover:text-slate-800">
          <ArrowLeft className="mr-1 h-3.5 w-3.5" />
          返回出版社列表
        </Link>
        <ErrorBox message={detailError || "出版社不存在"} onRetry={reloadDetail} />
      </div>
    );
  }

  const rate =
    publisher.editions_count > 0
      ? ((publisher.acquired_count / publisher.editions_count) * 100).toFixed(1)
      : "0.0";

  return (
    <div className="space-y-6">
      {/* 顶部返回与操作 */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="space-y-1">
          <Link
            to="/publishers"
            className="inline-flex items-center text-xs font-medium text-slate-500 hover:text-blue-600 transition-colors"
          >
            <ArrowLeft className="mr-1 h-3.5 w-3.5" />
            返回出版社列表
          </Link>
          <h1 className="text-xl font-bold text-slate-900 flex items-center gap-2">
            <Building2 className="h-6 w-6 text-blue-600" />
            <span>{publisher.name}</span>
            {publisher.country && <Badge variant="neutral">{publisher.country}</Badge>}
          </h1>
        </div>
        <div className="flex items-center gap-2">
          {canWrite && (
            <Button variant="secondary" size="sm" onClick={() => setAliasOpen(true)}>
              <Tag className="mr-1.5 h-4 w-4 text-purple-600" />
              添加别名
            </Button>
          )}
          {canWrite && (
            <Button variant="secondary" size="sm" onClick={handleOpenEdit}>
              <Edit2 className="mr-1.5 h-4 w-4" />
              编辑资料
            </Button>
          )}
        </div>
      </div>

      {/* 出版社信息与统计概览 */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <Card className="p-4 md:col-span-2">
          <h3 className="text-xs font-semibold text-slate-700 mb-2">出版社简介与主档资料</h3>
          <p className="text-xs text-slate-600 leading-relaxed min-h-[48px]">
            {publisher.description || "暂无简介信息，可通过「编辑资料」补充官方背景或机构介绍。"}
          </p>
          {publisher.website && (
            <div className="mt-3 pt-3 border-t border-slate-100 flex items-center gap-1.5 text-xs text-blue-600">
              <Globe className="h-3.5 w-3.5" />
              <a href={publisher.website} target="_blank" rel="noreferrer" className="hover:underline truncate">
                {publisher.website}
              </a>
            </div>
          )}
        </Card>

        <Card className="p-4">
          <div className="text-xs text-slate-500 font-medium">拥有书目统计</div>
          <div className="mt-2 text-2xl font-bold text-slate-900">
            {publisher.editions_count.toLocaleString()}
            <span className="text-xs font-normal text-slate-400 ml-1">个版本</span>
          </div>
          <div className="mt-2 text-[11px] text-slate-400">
            涵盖 <strong className="text-slate-700">{publisher.works_count.toLocaleString()}</strong> 部规范作品
          </div>
        </Card>

        <Card className="p-4">
          <div className="text-xs text-slate-500 font-medium">可用文件与归档率</div>
          <div className="mt-2 text-2xl font-bold text-green-600">
            {publisher.acquired_count.toLocaleString()}
            <span className="text-xs font-normal text-slate-400 ml-1">个版本有文件</span>
          </div>
          <div className="mt-2 flex items-center gap-2">
            <div className="flex-1 bg-slate-100 rounded-full h-1.5 overflow-hidden">
              <div
                className="bg-green-600 h-1.5 rounded-full"
                style={{ width: `${Math.min(100, Number(rate))}%` }}
              />
            </div>
            <span className="text-xs font-mono font-semibold text-slate-600">{rate}%</span>
          </div>
        </Card>
      </div>

      {/* 别名标签区域 */}
      <Card className="p-4">
        <div className="flex items-center justify-between mb-2">
          <div className="text-xs font-semibold text-slate-800 flex items-center gap-1.5">
            <Tag className="h-3.5 w-3.5 text-purple-600" />
            <span>已映射别名 ({aliases.length} 个)</span>
          </div>
          <span className="text-[11px] text-slate-400">
            导入原始数据时，若命中以下任意别名，将自动归一为此出版社
          </span>
        </div>
        <div className="flex flex-wrap gap-1.5">
          <Badge variant="info" className="font-medium">
            主名称：{publisher.name}
          </Badge>
          {aliases.map((al) => (
            <Badge key={al.id} variant="neutral" className="text-slate-600">
              {al.alias_name}
            </Badge>
          ))}
          {aliases.length === 0 && (
            <span className="text-xs text-slate-400 italic">暂无别名，可点击右上角添加常见缩写或历史译名</span>
          )}
        </div>
      </Card>

      {/* 专属书库列表 */}
      <Card>
        <div className="border-b border-slate-100 p-4 flex flex-wrap items-center justify-between gap-3">
          <div>
            <h3 className="text-sm font-semibold text-slate-900 flex items-center gap-1.5">
              <BookOpen className="h-4 w-4 text-blue-600" />
              <span>旗下出版书目列表 ({totalEditions.toLocaleString()} 本)</span>
            </h3>
            <p className="text-xs text-slate-500 mt-0.5">
              这里全部是已拥有书目；可筛选当前文件状态和主动补文件任务
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Select
              value={statusFilter}
              onChange={(e) => {
                setStatusFilter(e.target.value);
                setPage(1);
              }}
              className="text-xs w-36"
            >
              <option value="">全部状态</option>
              <option value="总库已拥有">仅书目 / 无主动任务</option>
              <option value="已下载">已下载</option>
              <option value="待下载">待下载</option>
              <option value="排队中">排队中</option>
              <option value="下载中">下载中</option>
              <option value="暂时失败">暂时失败</option>
            </Select>
            <Button variant="secondary" size="sm" onClick={reloadEditions}>
              刷新
            </Button>
          </div>
        </div>

        {editionsError && <ErrorBox message={editionsError} onRetry={reloadEditions} />}

        <Table
          headers={["书名 / 版本", "作者", "出版年份", "ISBN / 标识符", "来源格式", "可用文件", "文件/任务状态", "操作"]}
          empty={!editionsLoading && editions.length === 0 ? <EmptyRow colSpan={8} text="该出版社下暂无匹配书目" /> : undefined}
        >
          {editionsLoading && !editionsData ? (
            <SkeletonTable columns={8} rows={8} />
          ) : (
            editions.map((ed) => (
              <tr key={ed.id} className="hover:bg-slate-50/80 transition-colors">
                <Td className="font-medium text-slate-900 max-w-[280px]">
                  <Link
                    to={`/library/editions/${ed.id}`}
                    className="text-blue-600 hover:underline line-clamp-1"
                    title={ed.title}
                  >
                    {ed.title}
                  </Link>
                </Td>
                <Td className="text-xs text-slate-600 max-w-[160px] truncate" title={(ed.authors || []).join(", ")}>
                  {(ed.authors || []).join(", ") || "-"}
                </Td>
                <Td className="text-xs text-slate-500">{ed.publish_year || "-"}</Td>
                <Td className="text-xs font-mono text-slate-500">
                  {(ed.identifiers || [])[0] || "-"}
                </Td>
                <Td className="text-xs text-slate-500">
                  <div className="flex flex-wrap gap-1">
                    {(ed.source_formats || []).map((f) => (
                      <span key={f} className="rounded bg-slate-100 px-1.5 py-0.5 text-[10px] font-mono uppercase text-slate-600">
                        {f}
                      </span>
                    ))}
                  </div>
                </Td>
                <Td className="text-xs text-slate-500">
                  <div className="flex flex-wrap gap-1">
                    {(ed.holding_formats || []).map((f) => (
                      <span key={f} className="rounded bg-green-50 border border-green-200 px-1.5 py-0.5 text-[10px] font-mono uppercase font-bold text-green-700">
                        {f}
                      </span>
                    ))}
                    {(ed.holding_formats || []).length === 0 && <span className="text-slate-300">-</span>}
                  </div>
                </Td>
                <Td>
                  <StatusBadge status={ed.acquisition_status} />
                </Td>
                <Td>
                  <Link to={`/library/editions/${ed.id}`}>
                    <Button size="sm" variant="ghost">
                      详情
                    </Button>
                  </Link>
                </Td>
              </tr>
            ))
          )}
        </Table>

        {editionsData && editionsData.total > 0 && (
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
              第 {page} / {totalPages} 页 · 共 {editionsData.total.toLocaleString()} 本书目
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

      {/* 编辑资料 Dialog */}
      <Dialog
        open={editOpen}
        title="编辑出版社资料"
        onClose={() => setEditOpen(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEditOpen(false)}>
              取消
            </Button>
            <Button onClick={handleSaveEdit} loading={savingEdit}>
              保存修改
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
              value={editName}
              onChange={(e) => setEditName(e.target.value)}
            />
          </div>
          <div>
            <label className="block font-medium text-slate-700 mb-1">
              国家/地区
            </label>
            <Input
              value={editCountry}
              onChange={(e) => setEditCountry(e.target.value)}
            />
          </div>
          <div>
            <label className="block font-medium text-slate-700 mb-1">
              官方网站
            </label>
            <Input
              value={editWebsite}
              onChange={(e) => setEditWebsite(e.target.value)}
              placeholder="https://..."
            />
          </div>
          <div>
            <label className="block font-medium text-slate-700 mb-1">
              简介描述
            </label>
            <textarea
              value={editDesc}
              onChange={(e) => setEditDesc(e.target.value)}
              rows={3}
              className="w-full rounded-md border border-slate-300 px-3 py-2 text-xs text-slate-700 outline-none focus:border-blue-500"
              placeholder="补充出版社背景介绍..."
            />
          </div>
        </div>
      </Dialog>

      {/* 添加别名 Dialog */}
      <Dialog
        open={aliasOpen}
        title="添加出版社别名"
        onClose={() => setAliasOpen(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setAliasOpen(false)}>
              取消
            </Button>
            <Button onClick={handleAddAlias} loading={addingAlias}>
              确认添加
            </Button>
          </>
        }
      >
        <div className="space-y-4 text-xs">
          <p className="text-slate-500 leading-relaxed">
            输入该出版社的常见缩写、英文名或历史别名。添加后，系统会自动将总库中所有以此名称录入的图书批量绑定到该出版社。
          </p>
          <div>
            <label className="block font-medium text-slate-700 mb-1">
              别名文本 <span className="text-red-500">*</span>
            </label>
            <Input
              value={newAlias}
              onChange={(e) => setNewAlias(e.target.value)}
              placeholder="如：清华大学出版社有限公司 / Tsinghua University Press"
            />
          </div>
        </div>
      </Dialog>
    </div>
  );
}
