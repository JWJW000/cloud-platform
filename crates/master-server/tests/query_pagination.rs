mod support;
use master_server::catalog::search::{search_catalog, CatalogSearchParams};
use uuid::Uuid;

#[tokio::test]
async fn catalog_pages_preserve_order_filters_and_ownership() {
    let db = require_db!();
    let work = Uuid::new_v4();
    sqlx::query("INSERT INTO works (id, preferred_title, normalized_title) VALUES ($1, 'pagination book', 'pagination book')").bind(work).execute(&db.pool).await.unwrap();
    for i in 1..=7u128 {
        sqlx::query("INSERT INTO editions (id, work_id, edition_title, language, publisher, updated_at, owned_at) VALUES ($1,$2,'pagination book',$3,$4,'2026-01-01',CASE WHEN $5 THEN now() ELSE NULL END)")
            .bind(Uuid::from_u128(i)).bind(work).bind(if i % 2 == 0 {"en"} else {"zh"})
            .bind(if i % 2 == 0 {"Publisher A"} else {"Publisher B"}).bind(i != 7)
            .execute(&db.pool).await.unwrap();
    }
    let first = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            limit: Some(2),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|x| x.id.as_u128())
            .collect::<Vec<_>>(),
        vec![6, 5]
    );
    let second = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            limit: Some(2),
            cursor: first.next_cursor.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        second
            .items
            .iter()
            .map(|x| x.id.as_u128())
            .collect::<Vec<_>>(),
        vec![4, 3]
    );
    let back = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            limit: Some(2),
            cursor: second.previous_cursor.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        back.items
            .iter()
            .map(|x| x.id.as_u128())
            .collect::<Vec<_>>(),
        vec![6, 5]
    );
    assert!(back.previous_cursor.is_none());
    let last = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            limit: Some(2),
            cursor: second.next_cursor.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(last.items.len(), 2);
    assert!(last.next_cursor.is_none());
    assert!(!last.total_is_exact, "末页条数不能冒充全部结果总数");
    let filtered = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            query: Some("pagination".into()),
            language: Some("en".into()),
            work_type: Some("整书".into()),
            resolution_status: Some("已确认".into()),
            publisher: Some("Publisher A".into()),
            acquisition_status: Some("总库已拥有".into()),
            limit: Some(200),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(filtered.limit, 100);
    assert_eq!(filtered.items.len(), 3);
    assert!(filtered.total_is_exact);
    assert_eq!(filtered.total, 3);
    assert!(filtered.items.iter().all(|x| x.language == "en"));
    let missing = search_catalog(
        &db.pool,
        &CatalogSearchParams {
            publisher: Some("' OR true --".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(missing.items.is_empty());
    db.teardown().await;
}
