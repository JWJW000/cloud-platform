//! 图书馆总库与索引设计方案 V1 验收集成测试。

mod support;

use master_server::catalog::acquisition::{
    claim_acquisition_task, report_acquisition_task, retry_acquisition_target,
    AcquisitionReportRequest, WorkerClaimRequest,
};
use master_server::catalog::ingestion::{
    execute_import, preview_import, ImportManifestRequest, StartImportRequest,
};
use master_server::catalog::search::{
    get_catalog_edition_detail, search_catalog, CatalogSearchParams,
};
use master_server::catalog::storage::{commit_library_file, CommitLibraryFileRequest};
use master_server::scheduler::catalog_bridge::{
    materialize_next_target, next_target_priority, success_in_tx,
};
use master_server::scheduler::FileEvidence;
use master_server::store::catalog_v1::{get_catalog_stats, list_quarantined_records};
use uuid::Uuid;

#[tokio::test]
async fn 总库导入_去重消歧_章节关联_幂等落库验收() {
    let db = require_db!();

    // 1. 预检测试
    let csv_content = "\
title,author,publisher,isbn,doi,format,md5,filesize,id
算法导论,Thomas Cormen,机械工业出版社,978-7-111-40701-0,,epub,d41d8cd98f00b204e9800998ecf8427e,1024000,cn-001
算法导论（英文版）,Cormen,MIT Press,9780262033848,,pdf,098f6bcd4621d373cade4e832627b4f6,2048000,en-001
计算机网络：自顶向下方法,Kurose,机械工业出版社,9787111599715,,pdf,ad0234829205b9033196ba818f7a872b,3072000,cn-002
,无名氏,无名社,123456,,txt,,0,bad-001
";
    let preview_req = ImportManifestRequest {
        source_name: "cn_test".to_string(),
        source_type: Some("csv".to_string()),
        file_name: "cn_books_01.csv".to_string(),
        sheet_name: None,
        content: None,
        text_content: Some(csv_content.to_string()),
        server_manifest: None,
    };

    let preview = preview_import(&db.pool, &preview_req).await.unwrap();
    assert_eq!(preview.total_rows, 4);
    assert_eq!(preview.source_name, "cn_test");
    assert!(!preview.is_duplicate_file);

    // 2. 执行导入
    let start_req = StartImportRequest {
        source_name: "cn_test".to_string(),
        source_type: Some("csv".to_string()),
        file_name: "cn_books_01.csv".to_string(),
        sheet_name: None,
        text_content: Some(csv_content.to_string()),
        server_manifest: None,
    };

    let import_res = execute_import(&db.pool, &start_req).await.unwrap();
    assert_eq!(import_res.total_rows, 4);
    assert_eq!(import_res.imported_count, 3);
    assert_eq!(import_res.quarantined_count, 1, "空书名记录必须被隔离");

    // 3. 重复导入完全相同内容：必须幂等，不增加新作品或任务
    let dup_res = execute_import(&db.pool, &start_req).await.unwrap();
    assert_eq!(
        dup_res.duplicate_count, 3,
        "同一文件重复导入必须命中幂等去重"
    );

    // 4. 统计核对
    let stats = get_catalog_stats(&db.pool).await.unwrap();
    assert_eq!(stats.total_sources, 1);
    assert_eq!(stats.total_source_records, 3);
    assert_eq!(stats.total_editions, 3);
    assert_eq!(stats.total_quarantined, 1);

    // 5. 检索验证与分面统计
    let search_res = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            query: Some("算法导论".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(search_res.items.len(), 2);
    let first = &search_res.items[0];
    assert!(first.title.contains("算法导论"));

    // 5.1 键集游标必须能稳定前进和返回，不依赖深 offset。
    let cursor_page_1 = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            limit: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(cursor_page_1.items.len(), 1);
    let cursor_page_2 = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            limit: Some(1),
            cursor: cursor_page_1.next_cursor.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(cursor_page_2.items.len(), 1);
    assert_ne!(cursor_page_2.items[0].id, cursor_page_1.items[0].id);
    assert!(cursor_page_2.previous_cursor.is_some());
    let returned_page = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            limit: Some(1),
            cursor: cursor_page_2.previous_cursor,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(returned_page.items[0].id, cursor_page_1.items[0].id);

    // 6. 详情下钻验证
    let detail = get_catalog_edition_detail(&db.pool, first.id)
        .await
        .unwrap();
    assert_eq!(detail.edition.id, first.id);
    assert!(!detail.source_records.is_empty(), "来源记录必须可追溯");
    assert!(!detail.source_assets.is_empty(), "来源候选文件必须存在");
    assert!(
        detail.acquisition_target.is_some(),
        "必须自动生成全局获取目标"
    );

    // 7. 隔离区查询
    let quarantined = list_quarantined_records(&db.pool, Some(false), 10, 0)
        .await
        .unwrap();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(quarantined[0].error_reason, "书名为空或无效");

    // 8. 搜索 Outbox 必须等待 OpenSearch 确认后才能完成；数据库测试只验证事件可靠落库。
    let outbox_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catalog_outbox WHERE status = '待同步'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(outbox_count >= 3);
}

#[tokio::test]
async fn 全局获取池_并发领取_租约与证据入库闭环() {
    let db = require_db!();

    // 1. 导入一本书
    let csv_content = "title,author,publisher,isbn,format,md5,filesize\n分布式系统概念与设计,Coulouris,机械工业出版社,9787111400000,pdf,11111111111111111111111111111111,5000000\n";
    let start_req = StartImportRequest {
        source_name: "acq_test".to_string(),
        source_type: Some("csv".to_string()),
        file_name: "acq_test.csv".to_string(),
        sheet_name: None,
        text_content: Some(csv_content.to_string()),
        server_manifest: None,
    };
    execute_import(&db.pool, &start_req).await.unwrap();

    // 2. Worker 节点领取任务
    let node_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let claim_req = WorkerClaimRequest {
        node_id,
        session_id,
        slot_index: 0,
        supported_formats: vec!["pdf".to_string()],
    };

    let assignment = claim_acquisition_task(&db.pool, &claim_req, 300)
        .await
        .unwrap()
        .expect("必须成功领取获取任务");

    assert_eq!(assignment.title, "分布式系统概念与设计");
    assert_eq!(assignment.format, "pdf");

    // 3. 并发第二次领取：租约有效，不能重复领取同一任务
    let second_claim = claim_acquisition_task(&db.pool, &claim_req, 300)
        .await
        .unwrap();
    assert!(second_claim.is_none(), "处于租约期内的任务不得被再次领取");

    // 4. 模拟任务执行中汇报失败并退避
    let fail_report = AcquisitionReportRequest {
        target_id: assignment.target_id,
        execution_id: assignment.execution_id,
        stage: "下载中".to_string(),
        result: Some("失败".to_string()),
        error_code: Some("NETWORK_TIMEOUT".to_string()),
        error_message: Some("连接超时".to_string()),
    };
    report_acquisition_task(&db.pool, &fail_report)
        .await
        .unwrap();

    // 5. 管理员手动重置任务
    retry_acquisition_target(&db.pool, assignment.target_id)
        .await
        .unwrap();

    // 6. 提交已下载馆藏文件证据（SHA-256 校验）
    let sha256_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let commit_req = CommitLibraryFileRequest {
        edition_id: assignment.edition_id,
        storage_backend: "NAS".to_string(),
        object_key: "books/9787111400000.pdf".to_string(),
        format: "pdf".to_string(),
        actual_size_bytes: 5242880,
        sha256: sha256_hash.to_string(),
        md5: Some("11111111111111111111111111111111".to_string()),
        source_asset_id: assignment.source_asset_id,
    };

    let commit_res = commit_library_file(&db.pool, &commit_req).await.unwrap();
    assert!(commit_res.is_new_file);
    assert!(commit_res.meets_strategy);

    // 7. 验证目标状态收敛为「已下载」
    let detail = get_catalog_edition_detail(&db.pool, assignment.edition_id)
        .await
        .unwrap();
    let target = detail.acquisition_target.unwrap();
    assert_eq!(
        target.status, "已下载",
        "馆藏文件校验通过后获取目标必须置为已下载"
    );
    assert_eq!(target.satisfied_holding_id, Some(commit_res.holding_id));
    assert_eq!(detail.holdings.len(), 1);
    assert_eq!(detail.holdings[0].1.sha256, sha256_hash);
}

#[tokio::test]
async fn 总库目标_可物化为现有worker任务并双向同步状态() {
    let db = require_db!();
    let csv_content = "title,author,publisher,isbn,format,filesize\n兼容调度测试书,测试作者,测试出版社,9787111999999,pdf,5000000\n";
    execute_import(
        &db.pool,
        &StartImportRequest {
            source_name: "worker_bridge_test".to_string(),
            source_type: Some("csv".to_string()),
            file_name: "worker_bridge_test.csv".to_string(),
            sheet_name: None,
            text_content: Some(csv_content.to_string()),
            server_manifest: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        next_target_priority(&db.pool).await.unwrap(),
        Some(0),
        "未物化的总库目标必须对统一工作选择器可见"
    );

    let mut tx = db.pool.begin().await.unwrap();
    assert!(materialize_next_target(&mut tx).await.unwrap());
    tx.commit().await.unwrap();

    let (target_id, task_id, batch_status): (Uuid, Uuid, String) = sqlx::query_as(
        "SELECT at.id, bt.id, db.status FROM acquisition_targets at \
         JOIN book_tasks bt ON bt.id = at.id \
         JOIN batch_books bb ON bb.book_id = bt.book_id \
         JOIN download_batches db ON db.id = bb.batch_id \
         WHERE at.edition_id = (SELECT id FROM editions WHERE edition_title = $1 LIMIT 1)",
    )
    .bind("兼容调度测试书")
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(target_id, task_id, "镜像任务必须与总库目标共用编号");
    assert_eq!(batch_status, "执行中");

    let execution_id = Uuid::new_v4();
    sqlx::query(
        "UPDATE book_tasks SET status = '已分配', attempts = 1, \
             lease_execution_id = $2, lease_expires_at = now() + interval '5 minutes' \
         WHERE id = $1",
    )
    .bind(task_id)
    .bind(execution_id)
    .execute(&db.pool)
    .await
    .unwrap();
    let mirrored: (String, i32, Option<Uuid>) = sqlx::query_as(
        "SELECT status, attempts, lease_execution_id FROM acquisition_targets WHERE id = $1",
    )
    .bind(target_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(mirrored.0, "已领取");
    assert_eq!(mirrored.1, 1);
    assert_eq!(mirrored.2, Some(execution_id));

    retry_acquisition_target(&db.pool, target_id).await.unwrap();
    let reset: (String, String, i32) = sqlx::query_as(
        "SELECT at.status, bt.status, bt.attempts FROM acquisition_targets at \
         JOIN book_tasks bt ON bt.id = at.id WHERE at.id = $1",
    )
    .bind(target_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(reset, ("待下载".to_string(), "待处理".to_string(), 0));

    let node_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO worker_nodes \
             (id, name, hostname, os, status, node_token_hash, registration_status) \
         VALUES ($1, $2, $2, 'Linux', '在线', $3, '已批准')",
    )
    .bind(node_id)
    .bind(format!("bridge-node-{node_id}"))
    .bind("test-token-hash")
    .execute(&db.pool)
    .await
    .unwrap();

    let sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("UPDATE book_tasks SET status = '已完成' WHERE id = $1")
        .bind(task_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    success_in_tx(
        &mut tx,
        target_id,
        Uuid::new_v4(),
        Some(node_id),
        &FileEvidence {
            nas_relative_path: "文件/兼容调度测试书.pdf".to_string(),
            file_name: "兼容调度测试书.pdf".to_string(),
            size_bytes: 5_000_000,
            sha256: sha256.to_string(),
            format: "pdf".to_string(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let evidence: (String, i64, i64) = sqlx::query_as(
        "SELECT at.status, \
                (SELECT count(*) FROM holdings h WHERE h.edition_id = at.edition_id), \
                (SELECT count(*) FROM library_file_locations lfl \
                 JOIN holdings h ON h.library_file_id = lfl.library_file_id \
                 WHERE h.edition_id = at.edition_id AND lfl.verify_status = '有效') \
         FROM acquisition_targets at WHERE at.id = $1",
    )
    .bind(target_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(evidence, ("已下载".to_string(), 1, 1));
}
