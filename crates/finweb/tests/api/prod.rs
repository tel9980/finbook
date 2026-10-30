//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

/// P1：生产订单变更/取消——变更留痕、状态守卫、未知订单 404。
#[tokio::test]
async fn prod_change_cancel_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建单（已下达）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "10", "date": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下达生产订单");
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 变更：数量 + 计划日期 + 备注 → 变更历史至少 4 条
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/prod/{pid}"),
            &sid,
            serde_json::json!({ "qty": "12", "plan_start": "2026-01-10", "plan_end": "2026-01-20", "memo": "改期" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "变更订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod/{pid}/changes"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let fields: Vec<String> = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["field"].as_str().unwrap().to_string())
        .collect();
    for f in ["计划数量", "计划开工", "计划完工", "备注"] {
        assert!(
            fields.contains(&f.to_string()),
            "变更历史应含 {f}：{fields:?}"
        );
    }

    // 非法数量 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/prod/{pid}"),
            &sid,
            serde_json::json!({ "qty": "0" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "数量 0 应 400");

    // 取消 → 200；取消后不可再变更/取消
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/cancel"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/prod/{pid}"),
            &sid,
            serde_json::json!({ "qty": "13" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已取消不可变更");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/cancel"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复取消应 400");

    // 已开工订单：变更/取消均 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "5", "date": "" }),
        ))
        .await
        .unwrap();
    let pid2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid2}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/prod/{pid2}"),
            &sid,
            serde_json::json!({ "qty": "6" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已开工不可变更");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid2}/cancel"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已开工不可取消");

    // 未知订单 → 404
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/prod/999999",
            &sid,
            serde_json::json!({ "qty": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "未知订单变更应 404");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/999999/cancel",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "未知订单取消应 404");
}

/// P1：工序质检——检验点门槛、合格/部分/全不合格、报废扣减计划量、状态守卫。
#[tokio::test]
async fn prod_qc_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 工艺路线含检验点
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/routing/140501",
            &sid,
            serde_json::json!([{ "seq": 1, "op_code": "OP1", "op_name": "车", "work_center": "WC1", "std_hours": "1", "rate": "10", "qc_required": true }]),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存工艺路线");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/routing/140501", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ops"][0]["qc_required"], true, "检验点应回显：{r}");

    // 订单 10 → 开工
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "10" }),
        ))
        .await
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");

    // 完工被工序检验门槛拦截
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/complete"),
            &sid,
            serde_json::json!({ "qty": "10", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "无检验记录应拦完工");

    // 非法检验：不合格 > 检验 / 不合格无处置
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/qc"),
            &sid,
            serde_json::json!({ "qty_insp": "10", "qty_fail": "11", "disposition": "scrap", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "不合格超检验量应 400"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/qc"),
            &sid,
            serde_json::json!({ "qty_insp": "10", "qty_fail": "2", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不合格无处置应 400");

    // 正式检验：10 检 2 报废 → 计划量 8 + 变更留痕
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/qc"),
            &sid,
            serde_json::json!({ "qty_insp": "10", "qty_fail": "2", "disposition": "scrap", "date": d15, "memo": "首检" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "录检验单：{b}");
    let qc: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(qc["result"], "partial", "部分合格：{qc}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let o = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(pid))
        .unwrap();
    assert_eq!(o["planned_qty"], "8", "报废扣减计划量：{o}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod/{pid}/changes"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["field"] == "计划数量（报废扣减）"),
        "报废应留痕：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod/{pid}/qc"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1, "检验记录 1 条");

    // 完工 8 → 200；完工后再检验 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/complete"),
            &sid,
            serde_json::json!({ "qty": "8", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "有检验记录后可完工");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/qc"),
            &sid,
            serde_json::json!({ "qty_insp": "1", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "完工后不可再检验");
}

/// 生产订单的「下达」是审批动作：草稿必须先过流程才能下达
#[tokio::test]
async fn workflow_gates_production_order_release() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建一张生产订单（老入口创建即「已下达」，改成草稿以走新路径）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "F01", "qty": "300", "work_center": "W01" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "建生产订单应成功：{v}");
    let pid = v["id"].as_i64().expect("应返回 id");
    state
        .db_for("b1")
        .unwrap()
        .conn()
        .execute(
            "UPDATE production_order SET status='draft' WHERE id=?1",
            rusqlite::params![pid],
        )
        .unwrap();

    // 先验：草稿状态下不能直接开工（开工要求已下达）——否则审批就是个摆设
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ne!(
        r.status(),
        StatusCode::OK,
        "草稿不该能直接开工 —— 那是绕过审批的口子"
    );

    apply_tpl(&state, &sid, "prod_two_stage", "production_order").await;

    // 下达：第一次只推进流程，状态仍是草稿
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/release"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["pending"].is_string(), "应返回下一节点：{v}");
    assert_eq!(
        prod_status_of(&state, "b1", pid).await,
        "draft",
        "未批完就下达 = 审批流形同虚设"
    );

    // 批到终态才真的下达（计划员 → 厂长 → 生产负责人 三段）
    for _ in 0..5 {
        let _ = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/prod/{pid}/release"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        if prod_status_of(&state, "b1", pid).await == "released" {
            break;
        }
    }
    assert_eq!(
        prod_status_of(&state, "b1", pid).await,
        "released",
        "批到终态后应真的下达"
    );
}

/// 排期接口必须拒掉自相矛盾的日期
///
/// 回归背景：`prod_schedule` 原来是**裸写** —— 连日期格式都不查，于是「完工日早于
/// 开工日」能直接进库。而 ATP 的日期承诺要拿 `plan_end` 当依据：排期错了，
/// 承诺日期就是错的，而且没有任何提示。
#[tokio::test]
async fn prod_schedule_rejects_end_before_start() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "10" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "建产单应成功：{b}");
    let id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/schedule",
            &sid,
            // 字段名是 start / end（`SchedItem`），不是 plan_start / plan_end
            serde_json::json!({ "items": [{ "id": id, "start": "2026-01-20", "end": "2026-01-10" }] }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert!(st.is_client_error(), "完工早于开工必须被拒（拿到 {st}）");
    assert!(b.contains("早于开工日"), "要说清原因：{b}");

    // 顺带钉住一个刚被校验暴露的入口缺陷：`SchedItem` 的 start/end 带
    // `#[serde(default)]`，所以**字段名写错就会静默变成空串**。以前这会把
    // 空日期直接写进库（排期看起来成功、实际什么都没排）；现在必须明确报错。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/schedule",
            &sid,
            serde_json::json!({ "items": [{ "id": id, "plan_start": "2026-01-10", "plan_end": "2026-01-20" }] }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert!(
        st.is_client_error(),
        "字段名写错导致日期为空，必须被拒（拿到 {st}）"
    );
    assert!(b.contains("格式"), "要说清是日期格式问题：{b}");
}
