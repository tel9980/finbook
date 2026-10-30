//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

/// P2：导出计划任务——CRUD + 立即执行写文件 + 校验。
#[tokio::test]
async fn export_schedule_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/export/schedules",
            &sid,
            serde_json::json!({ "kind": "vouchers", "period_mode": "current", "at_time": "00:00", "enabled": true, "memo": "测试" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增计划任务");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/schedules", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1, "列表 1 条：{r}");

    // 立即执行：写文件到 books_dir/exports/
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/export/schedules/{id}/run"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "立即执行：{b}");
    let path = serde_json::from_str::<serde_json::Value>(&b).unwrap()["path"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(path.contains("exports"), "路径应位于 exports：{path}");
    assert!(std::fs::metadata(&path).is_ok(), "导出文件应存在：{path}");

    // 校验：未知类型 / 非法时刻
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/export/schedules",
            &sid,
            serde_json::json!({ "kind": "nope", "at_time": "08:00" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知类型 400");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/export/schedules",
            &sid,
            serde_json::json!({ "kind": "trial", "at_time": "25:00" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非法时刻 400");

    // 删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/export/schedules/{id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除");
}
