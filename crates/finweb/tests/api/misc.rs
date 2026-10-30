//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

#[tokio::test]
async fn wrong_password_rejected() {
    let (state, _bd, _dir) = test_state();
    let (status, _) = login(&state, "boss", "WrongPass123!").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn setup_status_after_bootstrap() {
    let (state, _bd, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"admin_set\":true"), "引导后应有管理员：{s}");
}

#[tokio::test]
async fn owner_enters_own_book_and_sees_company() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let status = select_book(&state, &sid, "b1").await;
    assert_eq!(status, StatusCode::OK, "进入自己的账套应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"vouchers\""), "仪表盘应正常返回：{s}");
}

#[tokio::test]
async fn import_run_kingdee_template_csv() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let status = select_book(&state, &sid, "b1").await;
    assert_eq!(status, StatusCode::OK);
    // 金蝶模板期初：科目编码,科目名称,方向,期初余额,累计借方,累计贷方
    let csv = "\u{feff}科目编码,科目名称,方向,期初余额,累计借方,累计贷方\n\
               1001,库存现金,借,10000,50000,30000\n\
               100201,银行存款-工行,贷,2000,0,2000\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "begin",
                "template": "kingdee",
                "text": csv,
                "mapping": {},
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "金蝶模板导入应成功");
    let s = body_string(resp).await;
    assert!(s.contains("\"ok\":2"), "应导入 2 条：{s}");
}

#[tokio::test]
async fn import_run_excel_file_upload() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let _ = select_book(&state, &sid, "b1").await;
    // 用 findb 的 fixture：用友模板期初表（Excel）
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../findb/tests/fixtures/yonyou_begin.xlsx"
    );
    let bytes = std::fs::read(fixture).expect("读取 fixture xlsx 失败");
    let b64 = base64_encode(&bytes);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "begin",
                "template": "yonyou",
                "text": "",
                "file": b64,
                "mapping": {},
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "Excel 上传导入应成功");
    let s = body_string(resp).await;
    assert!(s.contains("\"ok\":2"), "应导入 2 条：{s}");
}

#[tokio::test]
async fn templates_and_aux_lifecycle() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 模板 CRUD
    let tpl = serde_json::json!({
        "id": 0, "name": "月度计提房租", "memo": "每月房租",
        "entries": [
            { "summary": "计提房租", "account_code": "660201", "dir": "debit", "amount": "5000", "aux": {} },
            { "summary": "计提房租", "account_code": "221101", "dir": "credit", "amount": "5000", "aux": {} }
        ],
        "freq": "monthly", "start_period": 202601, "end_period": null,
        "last_period": null, "active": true
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/templates", &sid, tpl.clone()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建模板应成功");
    let id: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();

    // 到期列表应含月度模板
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/templates/due?period=202601", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("月度计提房租"), "本期到期应包含模板：{s}");

    // 生成凭证 → 回写 last_period，本期到期列表不再包含
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/templates/{id}/generate"),
            &sid,
            serde_json::json!({ "period": 202601, "date": "2026-01-31" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "模板生成凭证应成功");
    let s = body_string(resp).await;
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&s).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert!(vid > 0, "应返回凭证 id：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/templates/due?period=202601", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(
        !s.contains("月度计提房租"),
        "生成后本期到期列表应移除该模板（last_period 已推进）：{s}"
    );

    // 修改
    let mut tpl2 = tpl.clone();
    tpl2["name"] = serde_json::json!("月度计提房租(改)");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(&format!("/api/templates/{id}"), &sid, tpl2))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/templates/{id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 辅助档案 CRUD
    let ent = serde_json::json!({
        "id": 0, "kind": "customer", "code": "C001", "name": "客户一",
        "parent_code": null, "disabled": false, "props": {}, "memo": ""
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/aux", &sid, ent))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建档案应成功");
    let aid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/aux?kind=customer", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("C001"), "客户档案应存在：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/aux/{aid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 工作流预置模板：一键落流程（草稿态）+ 拒绝不适用单据。
///
/// 关键契约：模板描述的是行业惯例而非本企业授权制度，所以**落的是草稿不是已发布**——
/// 擅自发布等于替企业做了内控决策，且有单据在流转时改流程会让在途实例的
/// current_node 指向不存在的节点（引擎报「流程可能被改」）。
#[tokio::test]
async fn workflow_template_apply_and_reject_mismatch() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 模板清单
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/workflows/templates", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let tpls: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let rows = tpls["rows"].as_array().unwrap();
    assert!(rows.len() >= 4, "预置模板不应少于 4 套：{tpls}");
    let keys: Vec<&str> = rows.iter().filter_map(|t| t["key"].as_str()).collect();
    for want in ["simple", "standard", "claim_full", "funds", "amount_tiered"] {
        assert!(keys.contains(&want), "缺少模板 {want}：{keys:?}");
    }
    // 每个模板都要说明适用单据与依赖字段，前端据此筛选
    for t in rows {
        assert!(!t["name"].as_str().unwrap_or("").is_empty());
        assert!(!t["desc"].as_str().unwrap_or("").is_empty());
        assert!(
            t["node_count"].as_i64().unwrap_or(0) >= 2,
            "模板节点过少：{t}"
        );
        assert!(!t["applicable"].as_array().unwrap().is_empty(), "{t}");
    }

    // 金额分级模板不适用报价单（报价单无 amount 字段，套上去审批时会直接报错卡死）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows/templates/apply",
            &sid,
            serde_json::json!({ "key": "amount_tiered", "biz_type": "quotation" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "不适用单据应被拒");

    // 正常应用：落草稿
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows/templates/apply",
            &sid,
            serde_json::json!({ "key": "claim_full", "biz_type": "claim" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let whole: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let f = &whole["flow"];
    assert_eq!(f["status"], "draft", "模板应用后必须是草稿，不得自动发布");
    assert_eq!(f["biz_type"], "claim");
    let nodes = f["nodes"].as_array().unwrap();
    // 部门 → 财务 → 出纳 三级
    let names: Vec<&str> = nodes
        .iter()
        .filter(|n| n["type"] == "approve")
        .map(|n| n["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(names.len(), 3, "应有三个审批节点：{names:?}");
    assert!(
        names.iter().any(|n| n.contains("出纳")),
        "缺出纳节点：{names:?}"
    );
    // 出纳节点的参与人必须是 cashier 角色
    let cash = nodes
        .iter()
        .find(|n| n["name"].as_str().unwrap_or("").contains("出纳"))
        .unwrap();
    assert_eq!(
        cash["participants"][0], "cashier",
        "出纳节点参与人应为 cashier：{cash}"
    );

    // 不存在的模板 → 404/400
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows/templates/apply",
            &sid,
            serde_json::json!({ "key": "nope", "biz_type": "claim" }),
        ))
        .await
        .unwrap();
    assert!(
        r.status() == StatusCode::BAD_REQUEST || r.status() == StatusCode::NOT_FOUND,
        "不存在的模板应被拒，实际 {}",
        r.status()
    );

    // 应用后能在流程列表里看到
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/workflows", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert!(
        list["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["biz_type"] == "claim"),
        "模板落地的流程应出现在列表里"
    );
}

/// 无密钥 / 错密钥 / 已停用一律 401，且提示要带 Authorization
#[tokio::test]
async fn v1_requires_bearer_key() {
    let (state, _bd, _dir) = test_state();

    let (st, body) = v1_get(&state, "/api/v1/accounts", "").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "不带密钥应 401：{body}");
    assert!(
        body.contains("Authorization"),
        "401 应说清缺什么，而不是只说未授权：{body}"
    );

    let (st, _) = v1_get(&state, "/api/v1/accounts", "fbk_0000000000000000").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "错密钥应 401");

    // 格式对但库里没有的 32 字节密钥
    let ghost = format!("fbk_{}", "0".repeat(64));
    let (st, _) = v1_get(&state, "/api/v1/accounts", &ghost).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "不存在的密钥应 401");

    // 停用后立即失效——「停用」若不立刻生效，泄露事故的处置窗口就不存在了
    let key = issue_ro_key(&state, "b1").await;
    let (st, _) = v1_get(&state, "/api/v1/accounts", &key).await;
    assert_eq!(st, StatusCode::OK, "刚签发的密钥应可用");

    let id = state
        .realm
        .api_key_list()
        .expect("列密钥失败")
        .into_iter()
        .find(|k| k.book_key == "b1")
        .expect("应能找到刚签的密钥")
        .id;
    state
        .realm
        .api_key_set_disabled(id, true)
        .expect("停用失败");
    let (st, body) = v1_get(&state, "/api/v1/accounts", &key).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "停用后应立即 401：{body}");
}

/// 过期密钥不能过——expire_at 不是装饰字段
#[tokio::test]
async fn v1_expired_key_rejected() {
    let (state, _bd, _dir) = test_state();
    let past = (chrono::Local::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let (_, key) = state
        .realm
        .api_key_create("已过期", "b1", "", &past, "boss")
        .expect("签发失败");
    let (st, body) = v1_get(&state, "/api/v1/accounts", &key).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "过期密钥应 401：{body}");
}

/// 库只存哈希：库里任何地方都不该出现明文密钥
#[tokio::test]
async fn v1_key_stored_as_hash_only() {
    let (state, _bd, _dir) = test_state();
    let key = issue_ro_key(&state, "b1").await;
    assert!(key.starts_with("fbk_"), "密钥应有可辨识前缀：{key}");

    let listed = state.realm.api_key_list().expect("列密钥失败");
    let row = listed.first().expect("应有一把密钥");
    // 列表接口只回前缀，不回任何可用于鉴权的完整凭据
    assert!(
        !row.prefix.contains(&key) || row.prefix.len() < key.len(),
        "列表不应暴露完整密钥：{:?}",
        row.prefix
    );
    // 直接查库：哈希不等于明文
    let dump = { state.realm.debug_dump_api_keys() };
    assert!(
        !dump.contains(&key),
        "库中出现了明文密钥（只该存哈希）：{dump}"
    );
}

/// 绑账套的密钥只能读自己那个账套；平台级密钥必须显式给 X-Book-Key
#[tokio::test]
async fn v1_key_is_scoped_to_its_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let (st, body) = create_book(&state, &admin_sid, "第二账套").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let other = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();

    // 绑 b1 的密钥请求另一个账套不存在——v1 路径里没有账套参数，
    // 目标账套只由密钥绑定决定，所以这里验证的是「换个密钥才能换账套」
    let key_b1 = issue_ro_key(&state, "b1").await;
    let (st, body) = v1_get(&state, "/api/v1/accounts", &key_b1).await;
    assert_eq!(st, StatusCode::OK);
    let company = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert!(company.is_object(), "科目表应返回对象：{body}");

    // 平台级密钥（book_key 空）：不带 X-Book-Key → 400；带了就正常
    let (_, plat) = state
        .realm
        .api_key_create("平台级", "", "", "", "boss")
        .expect("签发失败");
    let r = handlers::router(state.clone())
        .oneshot(bearer_get("/api/v1/accounts", &plat))
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::BAD_REQUEST,
        "平台级密钥不给账套应 400，而不是默默挑一个"
    );
    let msg = body_string(r).await;
    assert!(msg.contains("X-Book-Key"), "应指明用哪个头：{msg}");

    for k in [&other, "b1"] {
        let r = handlers::router(state.clone())
            .oneshot(bearer_get_for_book("/api/v1/accounts", &plat, k))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK, "X-Book-Key={k} 应可用");
    }
}

/// 只读密钥能读基础档案，但碰不到需要 fin_report 的报表
#[tokio::test]
async fn v1_readonly_scope_blocks_reports() {
    let (state, _bd, _dir) = test_state();
    let key = issue_ro_key(&state, "b1").await;

    for uri in [
        "/api/v1/accounts",
        "/api/v1/items",
        "/api/v1/me",
        "/api/v1/vouchers",
        "/api/v1/statements",
    ] {
        let (st, body) = v1_get(&state, uri, &key).await;
        assert_eq!(st, StatusCode::OK, "只读密钥应能读 {uri}：{body}");
    }

    for uri in ["/api/v1/trial-balance", "/api/v1/ledger?code=1001"] {
        let (st, body) = v1_get(&state, uri, &key).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "只读密钥不应能读 {uri}：{body}");
        assert!(
            body.contains("只读"),
            "403 应说清当前是只读范围，而不是只说权限不足：{body}"
        );
    }
}

/// 给了 fin_report 就能读报表
#[tokio::test]
async fn v1_fin_report_scope_allows_reports() {
    let (state, _bd, _dir) = test_state();
    let (_, key) = state
        .realm
        .api_key_create("报表只读", "b1", "fin_report", "", "boss")
        .expect("签发失败");

    let (st, body) = v1_get(&state, "/api/v1/trial-balance", &key).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert!(v["totals"]["begin_debit"].is_string(), "应含合计：{body}");

    let (st, body) = v1_get(&state, "/api/v1/ledger?code=1001", &key).await;
    assert_eq!(st, StatusCode::OK, "{body}");

    // 缺必填参数要给 400 并说明缺哪个，不能 500
    let (st, body) = v1_get(&state, "/api/v1/ledger", &key).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("code"), "应指明缺 code：{body}");
}

/// 非法输入返回 400 而不是 500：开放 API 的调用方是程序，报错要能自纠
#[tokio::test]
async fn v1_bad_input_is_400_not_500() {
    let (state, _bd, _dir) = test_state();
    let (_, key) = state
        .realm
        .api_key_create("报表", "b1", "fin_report", "", "boss")
        .expect("签发失败");

    for uri in [
        "/api/v1/vouchers?status=bogus",
        "/api/v1/vouchers?from=not-a-period",
        "/api/v1/vouchers?page=abc",
    ] {
        let (st, body) = v1_get(&state, uri, &key).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{uri} 应 400：{body}");
    }

    // 账套不存在 → 404（不是 401：密钥本身是有效的，是目标不存在）
    let (_, ghost_key) = state
        .realm
        .api_key_create("孤儿", "no_such_book", "", "", "boss")
        .expect("签发失败");
    let (st, body) = v1_get(&state, "/api/v1/accounts", &ghost_key).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
}

/// v1 不含写操作：任何 /api/v1/* 的 POST 都必须被拒
///
/// 这是 v1 最要紧的一条边界。开放接口一旦有写入口，「谁能改账」就由接口设计决定，
/// 而外部系统数量不可控——第一版只承诺「读得到」，不承诺「写得进」。
#[tokio::test]
async fn v1_has_no_write_endpoints() {
    let (state, _bd, _dir) = test_state();
    let key = issue_ro_key(&state, "b1").await;

    for uri in ["/api/v1/vouchers", "/api/v1/accounts", "/api/v1/items"] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let r = handlers::router(state.clone())
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header(header::AUTHORIZATION, format!("Bearer {key}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            // 405（方法不允许）或 404（无此路由）都算「没有写入口」，
            // 但绝不能是 2xx
            assert!(
                !r.status().is_success(),
                "{method} {uri} 不应成功，实际 {} —— v1 承诺只读",
                r.status()
            );
        }
    }
}

/// v1 绕开会话门禁但**不**绕开鉴权：没有密钥的 /api/v1 一律 401
///
/// 单独盯这一条：api_auth_gate 里给 /api/v1/ 加了前缀豁免（密钥没有 cookie，
/// 套 session_of 会把合法外部调用全 401）。豁免不能变成「不鉴权」。
#[tokio::test]
async fn v1_session_gate_exemption_does_not_mean_open() {
    let (state, _bd, _dir) = test_state();
    // 即便带着一个真实有效的浏览器会话，v1 也要密钥——两条鉴权是物理隔离的
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/accounts")
                .header(header::COOKIE, admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "只有 cookie 没有密钥，v1 应 401 —— 会话不能顶替密钥"
    );
}

/// 只有 health 与 openapi.json 是免鉴权的，其余都要密钥
#[tokio::test]
async fn v1_public_endpoints_limited_to_health_and_spec() {
    let (state, _bd, _dir) = test_state();
    for uri in ["/api/v1/health", "/api/v1/openapi.json"] {
        let (st, body) = v1_get(&state, uri, "").await;
        assert_eq!(st, StatusCode::OK, "{uri} 应免鉴权：{body}");
    }
    let v = serde_json::from_str::<serde_json::Value>(
        &v1_get(&state, "/api/v1/openapi.json", "").await.1,
    )
    .unwrap();
    assert_eq!(v["openapi"], serde_json::json!("3.0.3"));
    let paths = v["paths"].as_object().expect("paths 应是对象");
    assert!(paths.contains_key("/api/v1/health"));

    let key = issue_ro_key(&state, "b1").await;
    for (p, spec) in paths {
        assert!(p.starts_with("/api/v1/"), "文档路径应在 v1 下：{p}");
        // OpenAPI 规范里参数段写作 {id}，实现（axum 0.7/matchit 0.7）要写 :id。
        // 这里把 {x} 换成真值，既符合规范也顺便验证参数段路由真的接上了。
        let method = p.replace("{id}", "1");
        let public = spec["get"]["security"].as_array().is_some(); // "security": [] = 显式免鉴权
                                                                   // 判据用「带真密钥也该通」而不是「不带密钥该 401」——
                                                                   // 后者有个陷阱：路径拼错（如把 :id 写成 {id}）会掉进 spa_fallback，
                                                                   // 未登录时**也**返回 401，于是「缺密钥→401」这个检查照样通过，
                                                                   // 路由其实是死的。带密钥再打一次才能区分「鉴权生效」和「路由不存在」。
        let (st, body) = v1_get(&state, &method, &key).await;
        // 判「路由没接上」不能只看状态码：handler 自己的 404（凭证不存在）
        // 与 spa_fallback 的 404（路由不存在）状态码完全一样，只有消息不同。
        assert!(
            !(st == StatusCode::UNAUTHORIZED || body.contains("接口不存在")),
            "文档里的 {p} 带合法密钥返回 {st}「{body}」—— 路由没接上"
        );
        // 不带密钥：免鉴权的两条照常通，其余必须 401
        let (st2, body2) = v1_get(&state, &method, "").await;
        if public {
            assert_eq!(st2, StatusCode::OK, "{p} 声明免鉴权却 {st2}：{body2}");
        } else {
            assert_eq!(st2, StatusCode::UNAUTHORIZED, "{p} 无密钥应 401：{body2}");
        }
    }
}

/// 浏览器那套端点没有被 v1 的豁免顺带放开
#[tokio::test]
async fn v1_exemption_does_not_loosen_browser_api() {
    let (state, _bd, _dir) = test_state();
    // 未登录访问普通 /api/* 仍应是 401
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/accounts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "加 /api/v1/ 豁免不应放宽 /api/accounts"
    );
}

/// 签发响应带明文，列表响应不带；停用后立刻 401；删除后彻底不见
#[tokio::test]
async fn api_key_lifecycle_over_http() {
    let (state, _bd, _dir) = test_state();
    let boss_sid = login(&state, "boss", "Admin!2026").await.1;

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/api-keys",
            &boss_sid,
            serde_json::json!({ "name": "仓库对接", "book_key": "b1", "scopes": "", "expires_at": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v = serde_json::from_str::<serde_json::Value>(&body_string(r).await).unwrap();
    let secret = v["secret"].as_str().expect("签发响应应带明文").to_string();
    let id = v["id"].as_i64().expect("应带 id");
    assert!(secret.starts_with("fbk_"), "{secret}");

    // 列表里绝不能有明文
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/api-keys", &boss_sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let list_body = body_string(r).await;
    assert!(
        !list_body.contains(&secret),
        "列表泄露了明文密钥：{list_body}"
    );

    // 停用 → 立刻 401
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/api-keys/{id}"),
            &boss_sid,
            serde_json::json!({ "disabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let (st, _) = v1_get(&state, "/api/v1/accounts", &secret).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "停用应立即生效");

    // 重新启用 → 恢复
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/api-keys/{id}"),
            &boss_sid,
            serde_json::json!({ "disabled": false }),
        ))
        .await
        .unwrap();
    let (st, _) = v1_get(&state, "/api/v1/accounts", &secret).await;
    assert_eq!(st, StatusCode::OK, "启用后应恢复");

    // 删除 → 彻底不可用
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/api-keys/{id}"))
                .header(header::COOKIE, boss_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let (st, _) = v1_get(&state, "/api/v1/accounts", &secret).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "删除后应不可用");
}

/// 签发参数校验：不给名字、给不存在的账套、给未知 scope、给坏日期都要 400
#[tokio::test]
async fn api_key_create_validates_input() {
    let (state, _bd, _dir) = test_state();
    let boss_sid = login(&state, "boss", "Admin!2026").await.1;

    // 每条带上期望状态码：账套不存在是 404（对象不存在），其余是 400（参数不合法）
    for (body, want, why) in [
        (
            serde_json::json!({ "book_key": "b1" }),
            StatusCode::BAD_REQUEST,
            "空名称",
        ),
        (
            serde_json::json!({ "name": "x", "book_key": "no_such" }),
            StatusCode::NOT_FOUND,
            "账套不存在",
        ),
        (
            serde_json::json!({ "name": "x", "book_key": "b1", "scopes": "drop_table" }),
            StatusCode::BAD_REQUEST,
            "未知 scope",
        ),
        (
            serde_json::json!({ "name": "x", "book_key": "b1", "expires_at": "明年某天" }),
            StatusCode::BAD_REQUEST,
            "坏日期",
        ),
    ] {
        let r = handlers::router(state.clone())
            .oneshot(authed_post("/api/api-keys", &boss_sid, body))
            .await
            .unwrap();
        let st = r.status();
        let got = body_string(r).await;
        assert_eq!(st, want, "{why}：{got}");
    }

    // 合法 scope 能签发
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/api-keys",
            &boss_sid,
            serde_json::json!({ "name": "报表对接", "book_key": "b1", "scopes": "fin_report" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "{:?}", body_string(r).await);
}

/// 审批流覆盖到采购订单：发布流程后，「确认」不再立即改状态，而是先走节点链。
///
/// 这是新增单据类型最容易漏的一环——引擎支持了 `intercept`，但业务入口
/// 没插闸门，结果配了流程却永远不生效、且不报任何错。所以这里从 HTTP 层
/// 走一遍：建订单 → 发流程 → 首次确认返回 pending 且状态**不变** → 批到终态
/// 才真的确认。
#[tokio::test]
async fn workflow_gates_purchase_order_confirm() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let po_id = make_po(&state, &sid).await;

    apply_tpl(&state, &sid, "order_standard", "purchase_order").await;

    // 第一次「确认」：只推进流程，不改订单状态
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v["pending"].is_string(),
        "配了流程后首次确认应返回下一节点：{v}"
    );
    // 关键：订单状态**不能**已经被确认
    let s = po_status_of(&state, "b1", po_id).await;
    assert_ne!(s, "Confirmed", "未批完就确认 = 审批流形同虚设（当前 {s}）");

    // 第二次确认：走到终态，订单才真的被确认
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let body2 = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "{body2}");
    assert_eq!(
        po_status_of(&state, "b1", po_id).await,
        "Confirmed",
        "批到终态后应真的确认"
    );
}

/// 没有已发布流程时，行为必须与从前完全一致（向后兼容）
#[tokio::test]
async fn no_flow_means_order_confirms_immediately() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let po_id = make_po(&state, &sid).await;
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v.get("pending").is_none(),
        "没有已发布流程时不该返回 pending：{v}"
    );
    assert_eq!(po_status_of(&state, "b1", po_id).await, "Confirmed");
}

/// 条件字段校验：把依赖 amount 的模板套到生产订单上要被拒（它没有金额）
#[tokio::test]
async fn template_requiring_amount_rejected_on_production_order() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 订单分级模板本来就不适用于生产订单（biz_types 不含它）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows/templates/apply",
            &sid,
            serde_json::json!({ "key": "order_tiered", "biz_type": "production_order" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let body = body_string(r).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("不适用"), "应说明不适用：{body}");

    // 模板清单里生产订单可用的是数量模板，且它声明的依赖字段该是 planned_qty
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/workflows/templates", &sid))
        .await
        .unwrap();
    let tpls: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let prod_tpls: Vec<&serde_json::Value> = tpls["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| {
            t["applicable"]
                .as_array()
                .map(|a| a.iter().any(|x| x["biz_type"] == "production_order"))
                .unwrap_or(false)
        })
        .collect();
    assert!(!prod_tpls.is_empty(), "至少应有一套模板适用于生产订单");
    for t in prod_tpls {
        let need: Vec<&str> = t["requires_fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str())
            .collect();
        assert!(
            !need.contains(&"amount"),
            "【{}】声明了 amount，但生产订单没有金额字段——配上去审批即卡死：{t}",
            t["name"]
        );
    }
}

/// 条件字段清单端点：字段集按单据不同，且非法单据要被拒
///
/// 前端以前写死一句「amount/qty/customer_code 等」。用户在生产订单上照着写
/// `amount > 5000` 会一路保存成功、直到单据提交审批才报「条件字段不存在」——
/// 流程建了、实例起了，单据卡死。所以清单必须按单据取，不能写死。
#[tokio::test]
async fn cond_fields_endpoint_is_per_biz_type() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let get = |biz: &str| {
        let state = state.clone();
        let sid = sid.clone();
        let biz = biz.to_string();
        async move {
            let r = handlers::router(state)
                .oneshot(authed_get(
                    &format!("/api/workflows/cond-fields?biz_type={biz}"),
                    &sid,
                ))
                .await
                .unwrap();
            let st = r.status();
            let body = body_string(r).await;
            (st, body)
        }
    };

    let (st, body) = get("claim").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let claim_fields: Vec<String> = v["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    assert!(
        claim_fields.contains(&"amount".to_string()),
        "{claim_fields:?}"
    );

    let (st, body) = get("production_order").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let prod_fields: Vec<String> = v["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    assert!(
        prod_fields.contains(&"planned_qty".to_string()),
        "{prod_fields:?}"
    );
    assert!(
        !prod_fields.contains(&"amount".to_string()),
        "生产订单没有金额，清单里不该出现 amount：{prod_fields:?}"
    );
    assert!(
        claim_fields != prod_fields,
        "两张单据的字段集不该相同，否则说明清单写死了"
    );

    // 采购/销售订单走金额口径（价税合计）
    for biz in ["purchase_order", "sales_order"] {
        let (st, body) = get(biz).await;
        assert_eq!(st, StatusCode::OK, "{biz}：{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let f: Vec<String> = v["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        assert!(f.contains(&"amount".to_string()), "{biz} 缺 amount：{f:?}");
    }

    // 非法单据类型 400 并列出可选值（别让用户面对空清单猜）
    let (st, body) = get("nope").await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("生产订单"), "应列出可选单据：{body}");
}

/// 设了标准成本方法但没录单价 → 差异表点名提示，不静默给 0
#[tokio::test]
async fn cost_variance_flags_missing_standard_cost() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 只设方法，不给标准成本单价
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "standard", "standard_cost": "0" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "standard", "standard_cost": "0" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("9").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
    }

    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/cost/material-variance?period=2026-01",
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert!(
        v["rows"].as_array().map(|a| a.is_empty()).unwrap_or(false),
        "没有标准单价的存货不该进差异表"
    );
    let warns: Vec<&str> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert!(
        warns
            .iter()
            .any(|w| w.contains("140301") && w.contains("标准成本单价")),
        "必须点名是哪个存货：{warns:?}"
    );
}

/// 到岸成本全链：建采购单→入库→录入运费→过账→存货金额被抬高、凭证出
///
/// 这是到岸成本模块存在的理由：不摊运费，存货估值偏低、销售成本低估，
/// **而且「采购价格差异」会算歪** —— 货价便宜、运费贵，差异表却显示「节约」。
#[tokio::test]
async fn landed_cost_end_to_end() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };

    // 存货 100 件 × 8 元 → 结存价值 800
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("8").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
    }
    // 造一张已收货的采购订单
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-03", "status": "Confirmed",
                "supplier_code": "S001", "supplier_name": "供应商甲",
                "lines": [{
                    "item_code": "140301", "item_name": "材料甲",
                    "qty_ordered": "100", "unit_price": "8", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "建采购订单应成功：{b}");
    let po_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .expect("应返回采购订单 id");
    // 记一笔到货（让 fetch_lines_from_po 有已收货行）
    {
        let db = state.db_for("b1").unwrap();
        db.conn()
            .execute(
                "INSERT INTO po_receipt(po_id, period, date, qty, memo) VALUES(?1,202601,'2026-01-05',100,'')",
                [po_id],
            )
            .unwrap();
    }

    // 从采购订单取行：只列已收货的
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/landed-cost/from-po?po_id={po_id}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let from_po: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let rows = from_po["rows"].as_array().expect("rows 应是数组");
    assert_eq!(rows.len(), 1, "已收货 100 件应能取到 1 行：{from_po}");
    assert_eq!(rows[0]["item_code"], serde_json::json!("140301"));
    assert_eq!(num(&rows[0]["qty"]), 100.0, "取到的是已收货量");

    // 保存草稿
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/landed-cost",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-08",
                "supplier_code": "S001", "supplier_name": "供应商甲",
                "basis": "qty", "freight": "100",
                "dr_account": "140301", "cr_account": "220201",
                "lines": [{
                    "po_id": po_id, "item_code": "140301", "item_name": "材料甲",
                    "qty": "100", "amount": "800", "manual_amount": "0"
                }]
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "保存到岸成本单应成功：{b}");
    let lcv_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        num(&serde_json::from_str::<serde_json::Value>(&b).unwrap()["row"]["total_charges"]),
        100.0
    );

    // 过账前存货金额没变
    {
        let db = state.db_for("b1").unwrap();
        let st_ = findb::business::stock_state(
            &db,
            "140301",
            fincore::Period::new(2026, 1).unwrap(),
            fincore::engine::costing::CostMethod::MovingAverage,
        )
        .unwrap();
        assert_eq!(
            st_.amount,
            fincore::Money::parse("800").unwrap(),
            "过账前不该变"
        );
    }

    // 过账
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/landed-cost/{lcv_id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "过账应成功：{b}");
    let posted: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(num(&posted["total_to_stock"]), 100.0);
    assert_eq!(num(&posted["total_to_expense"]), 0.0, "货还在，全部进存货");
    assert!(posted["voucher_id"].as_i64().unwrap_or(0) > 0, "应生成凭证");

    // 存货金额被抬高
    {
        let db = state.db_for("b1").unwrap();
        let st_ = findb::business::stock_state(
            &db,
            "140301",
            fincore::Period::new(2026, 1).unwrap(),
            fincore::engine::costing::CostMethod::MovingAverage,
        )
        .unwrap();
        assert_eq!(
            st_.amount,
            fincore::Money::parse("900").unwrap(),
            "结存金额 800 + 运费 100 = 900"
        );
        assert_eq!(st_.qty, fincore::Money::parse("100").unwrap(), "不该动数量");
    }

    // 重复过账被拒
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/landed-cost/{lcv_id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert!(r.status().is_client_error(), "重复过账必须被拒");
}

/// 到岸成本要计入采购价格差异，否则「节约」是假的
///
/// 回归背景：到岸成本以 `kind=adjust`（qty=0、只有金额）落库，而差异表原来只把
/// `qty > 0` 的入库行算进成本口径 —— 运费被完全排除。货价便宜 5%、运费贵 20% 时
/// 差异表显示「节约」，实际成本反而更高，且数字看着完全正常。
#[tokio::test]
async fn landed_cost_shows_up_in_purchase_variance() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };

    // 标准成本 7；货价 6（便宜 1 元）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "standard", "standard_cost": "7" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("6").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        // 500 到岸成本，memo 必须带「到岸成本」才能被差异表识别
        findb::business::stock_adjust(
            &db,
            fincore::Period::new(2026, 1).unwrap(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 8).unwrap(),
            "140301",
            "",
            fincore::Money::parse("500").unwrap(),
            "到岸成本 LC2026010001 分摊",
        )
        .unwrap();
    }
    findb::business::stock_summary(
        &state.db_for("b1").unwrap(),
        fincore::Period::new(2026, 1).unwrap(),
        fincore::engine::costing::CostMethod::Standard,
    )
    .unwrap();

    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/cost/material-variance?period=2026-01",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let row = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item"] == "140301")
        .expect("应有 140301 差异行");
    assert_eq!(num(&row["landed_cost"]), 500.0, "到岸成本要单列出来");
    assert_eq!(num(&row["in_amount"]), 1100.0, "600 货款 + 500 运费");
    assert_eq!(
        num(&row["price_variance"]),
        400.0,
        "含运费后是超支 4 元 × 100 = 400；不含运费会误报成「节约 100」"
    );
}

/// 货已出清时过账：整笔转费用，存货里一分钱都不能留
#[tokio::test]
async fn landed_cost_on_sold_goods_goes_to_expense() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };
    let p = fincore::Period::new(2026, 1).unwrap();
    let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let db = state.db_for("b1").unwrap();
    let q = fincore::Money::parse("10").unwrap();
    let pr = fincore::Money::parse("8").unwrap();
    for (kind, qty, price, amount) in [
        (
            findb::business::StockKind::Purchase,
            q,
            pr,
            (q * pr).round2(),
        ),
        (
            findb::business::StockKind::Sale,
            q.negated(),
            fincore::Money::ZERO,
            fincore::Money::ZERO,
        ),
    ] {
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: p,
                biz_date: d,
                kind,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty,
                price,
                amount,
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
    }
    // 到岸成本 60，货已卖光 → 必须整笔转费用
    let mut lc = findb::landedcost::LandedCost {
        period: p.ymm(),
        date: "2026-01-08".to_string(),
        supplier_code: "S001".to_string(),
        basis: "qty".to_string(),
        freight: fincore::Money::parse("60").unwrap(),
        dr_account: "140301".to_string(),
        cr_account: "220201".to_string(),
        expense_account: "660201".to_string(),
        ..Default::default()
    };
    let lines = vec![findb::landedcost::LcLine {
        po_id: 1,
        item_code: "140301".into(),
        item_name: "材料甲".into(),
        qty: fincore::Money::parse("10").unwrap(),
        amount: fincore::Money::parse("80").unwrap(),
        ..Default::default()
    }];
    let id = findb::landedcost::save(&db, &mut lc, &lines, "boss").unwrap();
    let vid = findb::landedcost::post(&db, id, "boss").unwrap();
    let after = findb::landedcost::get(&db, id).unwrap().unwrap();
    assert_eq!(
        after.total_to_stock(),
        fincore::Money::ZERO,
        "货已出清不得入存货"
    );
    assert_eq!(
        after.total_to_expense(),
        fincore::Money::parse("60").unwrap()
    );
    // 凭证：借费用 / 贷应付，没有借存货那条
    let v = findb::vouchers::get(&db, vid).unwrap().unwrap();
    assert!(v.balanced());
    assert_eq!(v.entries[0].account_code, "660201", "借方应是费用科目");
    // 结存必须是 0 / 0（幽灵存货检查）
    let st = findb::business::stock_state(
        &db,
        "140301",
        p,
        fincore::engine::costing::CostMethod::MovingAverage,
    )
    .unwrap();
    assert!(
        st.qty.is_zero() && st.amount.is_zero(),
        "不得留下幽灵存货：数量 {} 金额 {}",
        st.qty,
        st.amount
    );
    let _ = (sid, num);
}

/// ATP 的日期口径：未排期的在途**不能**用来承诺交期
///
/// 回归背景：仓里没有工作中心产能数据（`work_center` 是自由文本、`routing.std_hours`
/// 只参与算成本），所以系统编不出真实完工日 —— 只有计划员自己排的 `plan_end` 才
/// 有依据。把有排期和没排期的在途合成一个「在途 500」拿去承诺客户，就是编日期。
#[tokio::test]
async fn atp_endpoint_splits_dated_incoming_and_honours_date_param() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };

    // 现货 100
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("1").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140501".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
    }
    // 两张产单：300 排到 1/20 完工，200 没排期
    {
        let db = state.db_for("b1").unwrap();
        for (qty, plan_end) in [("300", "2026-01-20"), ("200", "")] {
            let per = fincore::Period::new(2026, 1).unwrap();
            let mut o = findb::scm::ProductionOrder {
                id: 0,
                // `prod_save` 不自动取号（handler 才取），直调引擎时必须自己给，
                // 否则撞 UNIQUE 约束、报的是 SQLite 原始错误而不是可读的提示
                no: findb::scm::prod_next_no(&db, per).unwrap(),
                period: fincore::Period::new(2026, 1).unwrap(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
                item_code: "140501".into(),
                item_name: "成品甲".into(),
                planned_qty: fincore::Money::parse(qty).unwrap(),
                completed_qty: fincore::Money::ZERO,
                status: findb::scm::ProdStatus::InProgress,
                work_center: String::new(),
                so_id: 0,
                prepared_by: "boss".into(),
                memo: String::new(),
                order_kind: "inhouse".into(),
                supplier_code: String::new(),
                supplier_name: String::new(),
                plan_start: if plan_end.is_empty() {
                    String::new()
                } else {
                    "2026-01-10".into()
                },
                plan_end: plan_end.into(),
            };
            let id = findb::scm::prod_save(&db, &mut o).unwrap();
            if !plan_end.is_empty() {
                findb::scm::prod_schedule(
                    &db,
                    &[(id, "2026-01-10".to_string(), plan_end.to_string())],
                )
                .unwrap();
            }
        }
    }

    // 不带 date：总量口径，在途分成已排期/未排期
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(num(&v["on_hand"]), 100.0);
    assert_eq!(num(&v["incoming"]), 500.0);
    assert_eq!(num(&v["incoming_dated"]), 300.0, "排了期的才算有交期");
    assert_eq!(num(&v["incoming_undated"]), 200.0, "没排期的要单列");
    assert_eq!(v["earliest_ready"], serde_json::json!("2026-01-20"));

    // date=2026-01-15：1/20 完工的还不算数，只认现货
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501&date=2026-01-15", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(num(&v["incoming"]), 0.0, "1/20 完工的量在 1/15 不可承诺");
    assert_eq!(num(&v["atp"]), 100.0, "1/15 只能承诺现货 100");
    assert_eq!(v["as_of"], serde_json::json!("2026-01-15"));
    // 未排期的部分不能被这个口径吞掉，界面要能同时说「总量」和「有保证的量」
    assert_eq!(num(&v["incoming_undated"]), 200.0);

    // date=2026-01-20：300 算进来；未排期的 200 仍然不算
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501&date=2026-01-20", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(num(&v["incoming"]), 300.0);
    assert_eq!(num(&v["atp"]), 400.0, "现货 100 + 有日期的在途 300");
    assert!(
        v["date_formula"]
            .as_str()
            .unwrap_or("")
            .contains("未排期的在途不计入"),
        "响应要带日期口径说明：{v}"
    );

    // 日期格式错误要明确报错
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501&date=2026/01/20", &sid))
        .await
        .unwrap();
    assert!(r.status().is_client_error(), "日期格式不对必须被拒");
}

/// 翻完每一页，去重后的凭证号集合 == 全部凭证号，且不重不漏。
#[tokio::test]
async fn voucher_page_walks_every_row_exactly_once() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 23).await;

    let mut seen: Vec<i64> = Vec::new();
    for page in 1..=5 {
        let p = get_voucher_page(
            &state,
            &sid,
            &format!("period=202601&page={page}&page_size=5"),
        )
        .await;
        assert_eq!(p["total"], 23);
        for it in p["items"].as_array().unwrap() {
            seen.push(it["id"].as_i64().unwrap());
        }
    }
    let unique: std::collections::HashSet<i64> = seen.iter().copied().collect();
    assert_eq!(seen.len(), 23, "应恰好取到 23 张，实际 {}", seen.len());
    assert_eq!(
        unique.len(),
        23,
        "不应有重复：重复 {}/{}",
        seen.len(),
        unique.len()
    );
    // 越界页必须是空数组，而不是报错或回绕到第一页
    let p = get_voucher_page(&state, &sid, "period=202601&page=99&page_size=5").await;
    assert_eq!(p["items"].as_array().unwrap().len(), 0, "越界页应为空数组");
}

/// 合计必须是**全量**的，不能是本页的。
///
/// 会计核对时看的是「本期间借方合计 = 贷方合计」这个总账数字。
/// 按页汇总会让人以为账不平（翻到第 3 页合计突然变了）。
#[tokio::test]
async fn voucher_page_totals_cover_whole_set_not_the_page() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 9).await; // 每张借 100 / 贷 100

    let p1 = get_voucher_page(&state, &sid, "period=202601&page=1&page_size=4").await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 4);
    assert_eq!(p1["total"], 9);
    assert_eq!(
        money_num(p1["debit_total"].as_str().unwrap()),
        900.0,
        "第 1 页也要显示全量合计 900"
    );
    assert_eq!(money_num(p1["credit_total"].as_str().unwrap()), 900.0);

    let p3 = get_voucher_page(&state, &sid, "period=202601&page=3&page_size=4").await;
    assert_eq!(p3["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        money_num(p3["debit_total"].as_str().unwrap()),
        900.0,
        "合计不应随页码变化"
    );

    // 过滤条件收窄时合计跟着变：证明合计与列表同源，不是写死的
    let nohit = get_voucher_page(&state, &sid, "period=202601&q=根本不存在的摘要").await;
    assert_eq!(nohit["total"], 0);
    assert_eq!(money_num(nohit["debit_total"].as_str().unwrap()), 0.0);
}

/// 暂估必须封顶在实收金额内 —— 端到端走 HTTP。
///
/// 回归背景：`po_estimate` 只有普通索引（非 UNIQUE），`est_amount` 全手填，
/// `po_estimate_add` 此前零校验。同一 PO 同一物料可反复登记暂估，每登一次
/// 就出一张「借 存货 / 贷 应付」凭证，应付与存货双双虚增，全程无报错。
/// 销售侧 `prod_from_so` 早有封顶，采购侧此前完全裸奔。
#[tokio::test]
async fn estimate_is_capped_by_receipt_over_http() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Confirmed", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "100", "unit_price": "10", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单应成功");
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // ① 没收货就登暂估：额度 0，必须被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "500" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "没收货就暂估应被拒");
    let msg = body_string(resp).await;
    assert!(msg.contains("超出可暂估额度"), "错误信息要说清原因：{msg}");

    // ② 收 100（税率 0，含税 1000），登满 1000 可以
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "date": "2026-01-08", "qty": "100" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购收货应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "1000" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "额度内暂估应成功");
    let est_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // ③ 再登一次（哪怕金额只有 1 元）必须被拒 —— 额度已用满，
    //    这正是原先能把应付凭空做虚的那条路
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "额度用满后重复登记暂估必须被拒（重复入账 = 应付虚增）"
    );

    // ④ 冲回后额度释放：发票到票、暂估冲回之后应能重新暂估全额
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/estimate/{est_id}/settle"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "冲回应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "1000" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "冲回后额度应释放，能重新暂估全额：{}",
        body_string(resp).await
    );
}

/// 最近价带出：采购订单保存自动沉淀价格历史，按日期倒序返回最新价。
#[tokio::test]
async fn price_history_suggest() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 初始无历史
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/price-history?item=RM77", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 0);

    // 建单（9 元）→ 自动沉淀
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-15", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "RM77", "qty_ordered": "3", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/price-history?item=RM77", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "保存订单应沉淀价格历史");
    assert_eq!(rows[0]["price"], "9");
    assert_eq!(rows[0]["supplier"], "S01");

    // 第二天 10 元 → 最新在前
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-16", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "RM77", "qty_ordered": "5", "unit_price": "10", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/price-history?item=RM77", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["price"], "10", "按日期倒序，最新价在前");

    // 缺 item → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/price-history", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Ctrl+K 搜档案：编码/名称命中 → kind=aux 且带 aux_kind 跳转字段
#[tokio::test]
async fn quick_search_aux() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建一条客户档案（名字带少见字，避免撞上预置档案）
    let ent = serde_json::json!({
        "id": 0, "kind": "customer", "code": "C077", "name": "快搜氧化客户",
        "parent_code": null, "disabled": false, "props": {}, "memo": ""
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/aux", &sid, ent))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建档案应成功");

    // 按名称搜到
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/quick-search?q=%E5%BF%AB%E6%90%9C%E6%B0%A7%E5%8C%96",
            &sid,
        )) // 快搜氧化
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "aux" && x["view"] == "aux" && x["aux_kind"] == "customer"),
        "应搜到档案并带跳转字段：{r}"
    );

    // 按编码搜到
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/quick-search?q=C077", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "aux"),
        "编码应搜到档案：{r}"
    );
}

/// 单据下推与追溯（对标金蝶源单→目标单）：审批 → 下推PO → 双向链 → 到货流水并入 → 拆单。
#[tokio::test]
async fn doc_push_chain() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建请购（draft）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/req",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-08",
                "item_code": "140301", "item_name": "原料", "qty": "30",
                "status": "draft", "requester": "张三", "memo": "下推链造数"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建请购");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/req?period=202601", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let req = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["memo"] == "下推链造数")
        .expect("请购应存在");
    let req_id = req["id"].as_i64().unwrap();
    assert_eq!(req["status"], "draft");

    // 未审批 → 下推拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/req/{req_id}/push-po"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未审批不能下推");

    // 审批（无工作流 → 默认流直接通过）→ 下推成功
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/req/{req_id}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审批请购");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/req/{req_id}/push-po"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下推应成功");
    let body = body_string(resp).await;
    let push: serde_json::Value = serde_json::from_str(&body).unwrap();
    let po_id = push["po_id"].as_i64().unwrap();
    assert!(
        push["po_no"].as_str().unwrap().starts_with("CG"),
        "订单号 CG 前缀：{body}"
    );

    // 请购 → ordered；链双向
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/req?period=202601", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let req = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"].as_i64() == Some(req_id))
        .unwrap();
    assert_eq!(req["status"], "ordered", "下推后请购应为已下推");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=po&id={po_id}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let links: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        links["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "req" && n["dir"] == "up" && n["id"].as_i64() == Some(req_id)),
        "PO 应见上游请购：{links}"
    );

    // 0 价防呆：未补价到货 → 400（0 价入库会污染移动平均）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-10", "qty": "30", "memo": "到货" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "0 价订单到货应被防呆拦截"
    );
    // 补价 + 补供应商（save_po 按 id 更新）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "id": po_id, "period": 202601, "date": "2026-01-08",
                "supplier_code": "S01", "supplier_name": "供应商甲",
                "status": "Draft", "memo": "源：请购",
                "lines": [{ "item_code": "140301", "qty_ordered": "30", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "补价应成功");
    // 到货执行 → 采购入库库存流水 + 流水并入链（下游 receipt）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-10", "qty": "30", "memo": "到货" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货登记（补价后）");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=po&id={po_id}"),
            &sid,
        ))
        .await
        .unwrap();
    let links: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        links["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "receipt" && n["dir"] == "down"),
        "PO 链应含到货流水：{links}"
    );

    // 拆单：再次下推 → 请购链两个下游 PO
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/req/{req_id}/push-po"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "拆单再下推");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=req&id={req_id}"),
            &sid,
        ))
        .await
        .unwrap();
    let links: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pos: Vec<_> = links["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "po" && n["dir"] == "down")
        .collect();
    assert_eq!(pos.len(), 2, "请购应见两张下游订单：{links}");

    // kind 非法 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/doc-links?kind=bogus&id=1", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Web 导入预检：报告缺失科目 → 带映射导入成功。
#[tokio::test]
async fn web_import_analyze_and_map() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let _ = select_book(&state, &sid, "b1").await;

    // 预检：9999 不在科目表中，2001 存在
    let csv = "9999,借,100\n2001,贷,100\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/analyze",
            &sid,
            serde_json::json!({ "kind": "begin", "template": "generic", "text": csv }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入预检应成功");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let missing = v["missing"].as_array().unwrap();
    assert_eq!(missing.len(), 1, "只应报告缺失科目 9999：{v}");
    assert_eq!(missing[0]["code"], serde_json::json!("9999"));
    assert_eq!(missing[0]["count"], serde_json::json!(1));

    // 映射 9999→1001 后导入：两行都应写入
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "begin", "template": "generic", "text": csv,
                "mapping": { "9999": "1001" },
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "带映射的导入应成功");
    let s = body_string(resp).await;
    assert!(s.contains("\"ok\":2"), "应导入 2 行：{s}");
    assert!(s.contains("\"skipped\":0"), "不应跳过：{s}");
}

/// 期初整批导入必须原子：第 2 行金额非法时，第 1 行也不能落地。
///
/// 修复前 `balances::upsert_begin` 是裸 `conn.execute`，每行各成一次隐式提交，
/// 第 k 行失败会让前 k-1 行留在库里，客户端既无法判断落地了哪些行也无法安全重试。
#[tokio::test]
async fn begin_import_is_all_or_nothing() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([
                { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                // 第 2 行金额非法 → 整批必须回滚（含第 1 行）
                { "account_code": "2001", "dir": "debit", "yb": "不是数字", "ad": "0", "ac": "0", "qty": null },
                { "account_code": "2202", "dir": "credit", "yb": "3000", "ad": "0", "ac": "0", "qty": null }
            ]),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "第 2 行非法应 400：{b}");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        rows.as_array().unwrap().is_empty(),
        "整批应回滚，一条都不能留下：{rows}"
    );

    // 修正后同一批（去掉坏行）应正常整批落地
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([
                { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                { "account_code": "2001", "dir": "credit", "yb": "3000", "ad": "0", "ac": "0", "qty": null }
            ]),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "合法批次应成功：{b}");
    assert!(b.contains("\"count\":2"), "应写入 2 条：{b}");
}

/// 往来期初合计：按金额格式（2 位小数）输出，且非 ar/ap 的 kind 不并进应付合计。
#[tokio::test]
async fn arap_opening_totals_money_and_no_misbucket() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let db = state.db_for("b1").expect("打开 b1");
    let mk = |kind: &str, doc: &str, party: &str, amt: &str| findb::settle::ArapOpening {
        id: 0,
        kind: kind.to_string(),
        party_code: party.to_string(),
        party_name: party.to_string(),
        doc_no: doc.to_string(),
        doc_date: "2025-12-31".to_string(),
        amount: fincore::Money::parse(amt).unwrap(),
        memo: String::new(),
        created_by: "boss".to_string(),
    };
    findb::settle::arap_opening_insert(&db, &mk("ar", "XSQ-1", "Q01", "5000"), "boss").unwrap();
    findb::settle::arap_opening_insert(&db, &mk("ap", "CGQ-1", "S01", "3000.5"), "boss").unwrap();
    // 脏数据：kind 写成大写 AR（导入笔误），绝不能被并进 total_ap
    findb::settle::arap_opening_insert(&db, &mk("AR", "XSQ-2", "Q02", "7000"), "boss").unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/arap-opening", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "往来期初列表：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 3, "三行都应返回：{b}");
    // 金额口径：2 位小数 + 千分位（fmt_qty 会印成 "5,000"）
    assert_eq!(r["total_ar"], "5,000.00", "应收合计按金额格式：{b}");
    assert_eq!(r["total_ap"], "3,000.50", "应付合计按金额格式：{b}");
    // 大写 AR 那 7000 既不进 ar 也不进 ap
    assert_eq!(r["unknown_kind"], 1, "未知 kind 应单独计数：{b}");
    assert_eq!(
        money_num(r["total_ar"].as_str().unwrap()) + money_num(r["total_ap"].as_str().unwrap()),
        8000.5,
        "合计不应把 7000 混进来：{b}"
    );
}

/// ③ 期初写不存在的科目必须被拒 —— 修前钱从所有报表里静默消失。
///
/// 实测：写 `999999` 返回 `200 {"count":1,"ok":true}`，随后
///   · 试算平衡报表**没有这一行**，总额照样报「平衡」
///   · 资产负债表查不到这 1,000,000
/// 凭证录入那条路上有「非末级科目不能记账」「核算数量科目必须填数量」「借贷不平衡」
/// 一整套校验，唯独期初这条路原来是裸的。会计把科目编码打错一位就踩到。
#[tokio::test]
async fn begin_rejects_nonexistent_account() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 一批里混一个不存在的科目：整批必须被拒，且真实那行也不能落地
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!({
                "rows": [
                    { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                    { "account_code": "999999", "dir": "debit", "yb": "1000000", "ad": "0", "ac": "0", "qty": null }
                ],
                "delete_ids": []
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "不存在科目应 400：{b}");
    assert!(
        b.contains("999999"),
        "报错必须点名是哪个科目，否则会计不知道改哪一位：{b}"
    );

    // 拒绝时不能留半截数据
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        rows.as_array().unwrap().is_empty(),
        "校验失败必须整批不落地：{rows}"
    );

    // 一次列出**全部**缺失科目，不是遇到第一个就停。
    //
    // 为什么单独断言这一点：DB 层（balances::ensure_begin_leaf）也会拦不存在的
    // 科目，但它是在**逐行 upsert 的过程中**报错，报的只是当前那一行。Web 这道
    // 预检的价值就是先把整批扫一遍，把 999999、888888 一次性告诉会计 ——
    // 否则改一行、存一次、再报下一个，几十行的期初要来回十几趟。
    //
    // 没有这条断言的话，把 Web 预检整段删掉，上面那两条断言照样过
    // （DB 层兜住了），变异测试就失去了意义。实测过。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!({
                "rows": [
                    { "account_code": "1001",    "dir": "debit",  "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                    { "account_code": "999999",  "dir": "debit",  "yb": "1000", "ad": "0", "ac": "0", "qty": null },
                    { "account_code": "888888",  "dir": "credit", "yb": "2000", "ad": "0", "ac": "0", "qty": null }
                ],
                "delete_ids": []
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "两个坏科目应 400：{b}");
    assert!(b.contains("999999"), "必须点名 999999：{b}");
    assert!(
        b.contains("888888"),
        "必须一次列出全部缺失科目，不能只报第一个：{b}"
    );

    // 对照：真实科目照常能存（别把校验写太死）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([{ "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null }]),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "真实科目应能存：{b}");
}

/// ④ 界面上的「移除」必须真删库里的行。
///
/// 实测：移除 140501 → 保存 → DB 纹丝不动 → 试算平衡从「1,544,100 vs 1,544,100 平衡」
/// 变成「1,544,100 vs 2,544,099 不平衡」，而界面上那行已经不见了。会计改个错字
/// 重新保存，期初金额就翻倍。
///
/// 根因：后端 `upsert_begin_on` 是 `ON CONFLICT DO UPDATE`，**没有任何删除语义**；
/// 前端把行从草稿删掉后只提交剩下的行，于是被删的那行永远留在库里。
#[tokio::test]
async fn begin_delete_ids_actually_remove_rows() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 先存 3 条
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([
                { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                { "account_code": "2001", "dir": "credit", "yb": "3000", "ad": "0", "ac": "0", "qty": null },
                { "account_code": "220201", "dir": "credit", "yb": "2000", "ad": "0", "ac": "0", "qty": null }
            ]),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "先存 3 条");

    // 读回 id
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let id_of = |code: &str| -> i64 {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["account_code"] == code)
            .unwrap_or_else(|| panic!("应有 {code}：{rows}"))["id"]
            .as_i64()
            .unwrap()
    };
    let (id1001, id2001, _) = (id_of("1001"), id_of("2001"), id_of("220201"));

    // 界面上移除 2001（3,000 贷方），另两行原样重发
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!({
                "rows": [
                    { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                    { "account_code": "220201", "dir": "credit", "yb": "2000", "ad": "0", "ac": "0", "qty": null }
                ],
                "delete_ids": [id2001]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "带删除的保存应成功：{b}");
    assert!(b.contains("\"deleted\":1"), "响应应报告删了几条：{b}");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let codes: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["account_code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes.len(),
        2,
        "移除后应只剩 2 行，实际 {codes:?} —— 被移除的行还留在库里"
    );
    assert!(!codes.contains(&"2001"), "2001 必须消失，实际 {codes:?}");

    // 只 upsert 一条、**不带** delete_ids：另一条不能被误删
    // （这条钉住"按 id 精确删"而不是"整表替换"——后者会删掉别人维护的行）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!({
                "rows": [
                    { "account_code": "1001", "dir": "debit", "yb": "6000", "ad": "0", "ac": "0", "qty": null }
                ],
                "delete_ids": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "只改一行应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2, "不带 delete_ids 时未提及的行必须保留：{rows}");
    // Money 序列化成字符串（"6000.00"），不是 JSON number —— 前端 renderBegin 里
    // 也是按 String(r.year_begin) 处理的
    let yb1001: f64 = arr
        .iter()
        .find(|r| r["account_code"] == "1001")
        .expect("1001 行应在")["year_begin"]
        .as_str()
        .unwrap_or_else(|| panic!("year_begin 应是字符串金额：{rows}"))
        .parse()
        .unwrap();
    assert_eq!(
        yb1001, 6000.0,
        "重发的行应是覆盖而非新增（原来翻倍就是这么来的）：{rows}"
    );
    let _ = id1001;
}

/// ⑧ 重复点「结转损益」必须说清是"已经结过了"，而不是"损益发生额为零"。
///
/// 实测：第一次结转生成 658,000 的凭证，第二次点，提示是
///   「导入数据有误：本期没有需要结转的损益类科目（损益发生额均为零）」
/// 而账上明明有 658,000 的损益额。两处都误导：
///   · "导入数据有误" 是加在 `From<FinError>` 上的万能前缀，任何引擎错误都会被说成
///     导入错误（用户根本没在导入）
///   · "损益发生额均为零" 把「已结转过」说成「本期没做业务」
#[tokio::test]
async fn carry_forward_twice_explains_it_was_already_carried() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 本期发生一笔费用：借 6401 1000 / 贷 1001 1000
    post_voucher(&state, &sid, 1, serde_json::json!([
        { "line": 1, "account_code": "6401", "summary": "费用", "debit": "1000", "credit": "0" },
        { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "1000" }
    ]))
    .await;

    // 第一次结转：成功，生成 1,000 的结转凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/carry-forward",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "第一次结转应成功：{b}");

    // 第二次：应被拒（账结法幂等），但提示必须说清原因
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/carry-forward",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "重复结转应被拒：{b}");
    assert!(
        b.contains("已结转过"),
        "提示必须说明「已结转过」，否则用户以为账做错了：{b}"
    );
    assert!(
        !b.contains("导入数据有误"),
        "结转不是导入操作，不该带「导入数据有误」前缀：{b}"
    );
    assert!(
        !b.contains("损益发生额均为零"),
        "本期有 1,000 费用，说「损益发生额均为零」是错的：{b}"
    );
}
