//! Search forms must share the latest state after a partial results update.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bikesnest_infrastructure::Db;
use bikesnest_test_support::{db_test, pool, test_config};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn fragment(query: &str) -> String {
    let app = bikesnest_web::app_router(
        std::sync::Arc::new(test_config()),
        Db::from_pool(pool().await),
    )
    .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/search?{query}"))
                .header("HX-Request", "true")
                .header("HX-Request-Type", "partial")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

fn mirror<'a>(body: &'a str, id: &str) -> &'a str {
    let start = body
        .find(&format!("id=\"{id}\""))
        .expect("state mirror present");
    let block = &body[start..];
    let block = &block[..block.find("</span>").unwrap()];
    assert!(block.contains("hx-swap-oob=\"true\""));
    block
}

#[db_test]
async fn partial_results_refresh_filters_and_destination_in_other_forms(_tx: &mut TestTx) {
    let body = fragment(
        "lat=-25.43&lon=-49.27&cost=free&type=rack&security=cctv&radius=2000&sort=distance",
    )
    .await;
    let sort = mirror(&body, "search-sort-state");
    for field in ["cost", "type", "security", "radius", "lat", "lon"] {
        assert!(
            sort.contains(&format!("name=\"{field}\"")),
            "sort retains {field}"
        );
    }
    assert!(sort.contains("value=\"free\""));
    assert!(
        !sort.contains("name=\"sort\""),
        "sort remains a visible control"
    );
    for id in ["search-query-state", "search-filter-state"] {
        assert!(mirror(&body, id).contains("value=\"distance\""));
    }
    assert!(mirror(&body, "search-browse-state").contains("value=\"free\""));
}

#[db_test]
async fn partial_results_remove_cleared_filters_and_preserve_browse_bounds(_tx: &mut TestTx) {
    let body = fragment("bbox=-49.29,-25.45,-49.25,-25.40").await;
    for id in [
        "search-sort-state",
        "search-query-state",
        "search-browse-state",
    ] {
        let state = mirror(&body, id);
        assert!(
            !state.contains("name=\"cost\""),
            "cleared cost stays cleared"
        );
        assert!(
            !state.contains("name=\"cursor\""),
            "new search resets pagination"
        );
    }
    for id in [
        "search-sort-state",
        "search-filter-state",
        "search-browse-state",
    ] {
        assert!(mirror(&body, id).contains("value=\"-49.29,-25.45,-49.25,-25.40\""));
    }
    assert!(!mirror(&body, "search-query-state").contains("name=\"bbox\""));
}
