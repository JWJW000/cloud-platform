//! OpenSearch 搜索投影真实 PostgreSQL + 模拟 Bulk API 集成测试。

mod support;

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{head, post, put};
use axum::Router;
use master_server::catalog::ingestion::{execute_import, StartImportRequest};
use master_server::config::OpenSearchConfig;
use master_server::opensearch::{process_outbox_events, OpenSearchClient};

#[tokio::test]
async fn existing_index_receives_additive_mapping_updates() {
    let received = Arc::new(Mutex::new(String::new()));
    let app = Router::new()
        .route("/catalog-editions-v1", head(|| async { StatusCode::OK }))
        .route(
            "/catalog-editions-v1/_mapping",
            put(
                |State(received): State<Arc<Mutex<String>>>, body: Bytes| async move {
                    *received.lock().unwrap() = String::from_utf8(body.to_vec()).unwrap();
                    axum::Json(serde_json::json!({"acknowledged": true}))
                },
            ),
        )
        .with_state(received.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = OpenSearchClient::new(OpenSearchConfig {
        enabled: true,
        url: format!("http://{address}"),
        ..Default::default()
    })
    .unwrap();
    client.ensure_index().await.unwrap();

    let body = received.lock().unwrap().clone();
    assert!(body.contains("publisher_exact"));
    assert!(body.contains("publisher_id"));
}

#[tokio::test]
async fn outbox_is_acknowledged_only_after_bulk_api_success() {
    let db = require_db!();
    let request = StartImportRequest {
        source_name: "opensearch_test".to_string(),
        source_type: Some("csv".to_string()),
        file_name: "opensearch.csv".to_string(),
        sheet_name: None,
        text_content: Some(
            "title,author,publisher,isbn,format\nOpenSearch实战,测试作者,测试出版社,9787111111111,pdf\n"
                .to_string(),
        ),
        server_manifest: None,
    };
    execute_import(&db.pool, &request).await.unwrap();

    let received = Arc::new(Mutex::new(String::new()));
    let app = Router::new()
        .route(
            "/catalog-editions-v1/_bulk",
            post(
                |State(received): State<Arc<Mutex<String>>>, body: Bytes| async move {
                    *received.lock().unwrap() = String::from_utf8(body.to_vec()).unwrap();
                    axum::Json(serde_json::json!({"errors": false, "items": []}))
                },
            ),
        )
        .with_state(received.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = OpenSearchClient::new(OpenSearchConfig {
        enabled: true,
        url: format!("http://{address}"),
        ..Default::default()
    })
    .unwrap();
    let processed = process_outbox_events(&db.pool, &client, 100).await.unwrap();
    assert!(processed >= 1);

    let body = received.lock().unwrap().clone();
    assert!(body.contains("OpenSearch实战"));
    assert!(body.ends_with('\n'), "Bulk NDJSON 必须以换行结束");
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catalog_outbox WHERE status = '待同步'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(pending, 0);

    db.teardown().await;
}

#[tokio::test]
async fn one_rejected_bulk_item_does_not_block_successful_items() {
    let db = require_db!();
    execute_import(
        &db.pool,
        &StartImportRequest {
            source_name: "opensearch_partial_test".to_string(),
            source_type: Some("csv".to_string()),
            file_name: "opensearch_partial.csv".to_string(),
            sheet_name: None,
            text_content: Some(
                "title,author,publisher,isbn,format\n投影成功项,作者甲,出版社甲,9787111111111,pdf\n投影失败项,作者乙,出版社乙,9787222222222,pdf\n"
                    .to_string(),
            ),
            server_manifest: None,
        },
    )
    .await
    .unwrap();

    let app = Router::new().route(
        "/catalog-editions-v1/_bulk",
        post(|body: Bytes| async move {
            let lines: Vec<&str> = std::str::from_utf8(&body).unwrap().lines().collect();
            let mut items = Vec::new();
            for (index, pair) in lines.chunks(2).enumerate() {
                let metadata: serde_json::Value = serde_json::from_str(pair[0]).unwrap();
                let id = metadata["index"]["_id"].as_str().unwrap();
                if index == 0 {
                    items.push(serde_json::json!({"index": {"_id": id, "status": 201}}));
                } else {
                    items.push(serde_json::json!({
                        "index": {
                            "_id": id,
                            "status": 400,
                            "error": {"reason": "测试拒绝"}
                        }
                    }));
                }
            }
            axum::Json(serde_json::json!({"errors": true, "items": items}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = OpenSearchClient::new(OpenSearchConfig {
        enabled: true,
        url: format!("http://{address}"),
        ..Default::default()
    })
    .unwrap();

    let processed = process_outbox_events(&db.pool, &client, 100).await.unwrap();
    assert!(processed > 0, "成功项目对应的事件应被确认");
    let (synced, pending): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = '已同步'), \
                count(*) FILTER (WHERE status = '待同步') FROM catalog_outbox",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(synced > 0);
    assert!(pending > 0, "失败项目对应的事件必须保留重试");

    db.teardown().await;
}
