# OpenSearch 接入与重建指南

## 架构约束

- PostgreSQL 是唯一事实源，OpenSearch 只保存可删除、可重建的书目搜索投影。
- 书目、作者、标识符、馆藏和获取状态在数据库事务内写入 `catalog_outbox`。
- Master 后台批量消费 Outbox；OpenSearch 整批确认成功后才把事件标记为 `已同步`。
- OpenSearch 不可用时，关键词检索自动回退 PostgreSQL；导入和调度不受影响。
- 空关键词列表继续使用 PostgreSQL 键集分页，避免搜索引擎承担不必要的全库浏览。

## 首次部署

OpenSearch 至少预留 2 GiB 内存，生产建议 4 GiB 以上。Linux 宿主机先设置：

```bash
sudo sysctl -w vm.max_map_count=262144
```

编辑 `deploy/.env`：

```dotenv
OPENSEARCH_ENABLED=1
OPENSEARCH_VERSION=3.8.0
OPENSEARCH_URL=http://opensearch:9200
OPENSEARCH_INDEX=catalog-editions-v1
OPENSEARCH_JAVA_OPTS=-Xms1g -Xmx1g
```

然后启动并确认健康：

```bash
docker compose -f deploy/docker-compose.yml up -d opensearch master
docker compose -f deploy/docker-compose.yml ps
docker compose -f deploy/docker-compose.yml logs -f opensearch master
```

Compose 内置节点没有发布 `9200`，且关闭了安全插件，只允许同一 Docker 网络中的 Master
访问。若使用托管或跨主机 OpenSearch，必须把 `OPENSEARCH_URL` 改为 HTTPS，并通过
`OPENSEARCH_USERNAME`、`OPENSEARCH_PASSWORD` 注入凭据；不要把密码写进 TOML 或仓库。

## 已有数据首次全量建索引

历史版本曾把 Outbox 直接标记为完成，因此升级后必须执行一次全量重建：

```bash
./deploy/manage.sh reindex-catalog 500
```

这个命令只删除并重建 OpenSearch 投影，不修改 PostgreSQL 书目。两百多万条数据建议在业务
低峰运行。重建期间普通检索仍可回退 PostgreSQL，后台增量事件会保留并在重建后再次覆盖为
最新事实。

## 验证

```sql
SELECT status, count(*)
FROM catalog_outbox
GROUP BY status;
```

正常情况下 `待同步` 会逐步下降到接近 0。查看同步日志：

```bash
docker logs -f drission-master
```

搜索接口不暴露 OpenSearch 地址或 DSL。可在管理端分别测试书名、作者、出版社、ISBN/DOI，
并观察 Master 日志中是否出现“OpenSearch 检索失败，回退 PostgreSQL”。

## 回滚

设置 `OPENSEARCH_ENABLED=0` 并重启 Master 即可完全恢复 PostgreSQL 检索。OpenSearch 数据卷
是可重建副本，确认不再需要后再由运维人员单独删除，禁止在普通升级脚本中自动清理。
