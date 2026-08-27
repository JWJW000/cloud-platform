//! OpenSearch 搜索投影真实 PostgreSQL + 模拟 Bulk API 集成测试。

mod support;

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::Router;
use master_server::catalog::ingestion::{execute_import, StartImportRequest};
use master_server::config::OpenSearchConfig;
use master_server::opensearch::{process_outbox_events, OpenSearchClient};

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
