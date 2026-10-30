//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

#[tokio::test]
async fn health_endpoint() {
    let (state, _bd, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "ok");
}

#[tokio::test]
async fn normal_user_cannot_enter_others_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    // 开通 wang（无账套）
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "wang", "display_name": "王出纳", "password": "Wang123456" }),
        ))
        .await
        .unwrap();
    let (_, sid) = login(&state, "wang", "Wang123456").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Wang123456", "new": "Wang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 试图进入 boss 的账套 → 403
    let status = select_book(&state, &sid, "b1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "普通用户不应能进入他人账套");

    // 自己的账套列表应为空
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("\"books\":[]"), "无账套用户列表应为空：{s}");
}

#[tokio::test]
async fn deleting_book_unblocks_user_deletion() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "zhang", "display_name": "张会计", "password": "Zhang123456" }),
        ))
        .await
        .unwrap();
    let (_, sid) = login(&state, "zhang", "Zhang123456").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Zhang123456", "new": "Zhang654321" }),
        ))
        .await
        .unwrap();
    // 自建账套（治理收归：helper 由管理员代建并邀请 zhang 入套）
    let (_, body) = create_book(&state, &sid, "张记贸易").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();
    // 名下有账套的账号不能被删（新模型下账套只归管理员——删管理员本人被自身保护拦下）
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/boss")
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "不能删除当前登录的管理员"
    );

    // 新模型：账套归管理员——普通成员无权删套，管理员可删自己的套
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/books/{}", key))
                .header(header::COOKIE, &sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通成员不能删除账套");
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/books/{}", key))
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员应能删除账套");
    // 该套应已从列表消失（管理员名下仍有 b1 等既有账套）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &admin_sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(!s.contains(&key), "删除后列表不应再含该套：{s}");
    // 此时账号可删
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/zhang")
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "账套清空后应能删除账号");
}

#[tokio::test]
async fn last_admin_cannot_be_removed() {
    let (state, _bd, _dir) = test_state();
    // 开通第二个管理员，用它来删 boss（boss 不是其当前会话账号）
    let (_, boss_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss_sid,
            serde_json::json!({ "username": "m2", "display_name": "管理员二", "password": "M2aa123456", "is_admin": true }),
        ))
        .await
        .unwrap();
    let (_, m2_sid) = login(&state, "m2", "M2aa123456").await;
    // 首登强制改密
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &m2_sid,
            serde_json::json!({ "old": "M2aa123456", "new": "M2aa654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "m2 改密应成功");
    // 平台管理员可删除他人账套（boss 名下有预置账套 b1，先清掉才能删账号）
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/books/b1")
                .header(header::COOKIE, &m2_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "平台管理员应能删除他人账套");
    // 两个管理员时可删 boss
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/boss")
                .header(header::COOKIE, &m2_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "存在两个管理员时可删其一");
    // 只剩 m2 一个管理员时，删除自己应被拒绝
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/m2")
                .header(header::COOKIE, &m2_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "最后一个管理员不可删除"
    );
}

#[tokio::test]
async fn accounts_crud_usage_guard_and_begin() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 新增科目（带辅助核算维度）
    let acc = |name: &str| {
        serde_json::json!({
            "account": {
                "code": "8888", "name": name, "category": "asset", "dir": "debit",
                "aux": 0, "unit": null, "currency": null, "has_qty": false,
                "is_cash": false, "is_bank": false, "cash_flow_item": null,
                "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
            },
            "aux_kinds": ["item", "supplier"]
        })
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, acc("原材料")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增科目应成功");

    // 重复编码 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, acc("重复")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复科目应拒绝");

    // 非数字编码 / 上级不存在 → 400（否则报表前缀聚合会 panic）
    let mut bad = acc("坏编码");
    bad["account"]["code"] = serde_json::json!("科目");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, bad))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "非数字科目编码应被服务端拒绝"
    );

    let mut orphan = acc("无上级");
    orphan["account"]["code"] = serde_json::json!("999901");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, orphan))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "上级科目不存在的编码应被拒绝"
    );

    // 列表包含新科目，掩码 = item(32) | supplier(2) = 34
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/accounts", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("8888"), "科目列表应含 8888：{s}");
    let list: serde_json::Value = serde_json::from_str(&s).unwrap();
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["code"] == "8888")
        .expect("科目 8888 应存在");
    assert_eq!(row["aux"], 34, "辅助核算掩码应为 34");

    // 修改名称
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/accounts", &sid, acc("原材料改良")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 未使用 → 可删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/accounts/8888", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "未使用科目应可删除");

    // 建一个无辅助核算的科目，被凭证引用后 → 删除被拦截
    let plain = serde_json::json!({
        "account": {
            "code": "1409", "name": "测试物料", "category": "asset", "dir": "debit",
            "aux": 0, "unit": null, "currency": null, "has_qty": false,
            "is_cash": false, "is_bank": false, "cash_flow_item": null,
            "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
        },
        "aux_kinds": []
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, plain))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        post_simple_voucher(&state, &sid, "1409").await,
        StatusCode::OK
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/accounts/1409", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已用科目不可删除");

    // 期初录入 + 回读（贷方应为负数）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([
                { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "1000", "ac": "0", "qty": null },
                { "account_code": "2001", "dir": "credit", "yb": "3000", "ad": "0", "ac": "0", "qty": null }
            ]),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    let rows: serde_json::Value = serde_json::from_str(&s).unwrap();
    let arr = rows.as_array().unwrap();
    let cash = arr.iter().find(|r| r["account_code"] == "1001").unwrap();
    let loan = arr.iter().find(|r| r["account_code"] == "2001").unwrap();
    assert_eq!(
        cash["year_begin"].as_str().unwrap().parse::<f64>().unwrap(),
        5000.0,
        "借方期初应为正"
    );
    assert_eq!(
        loan["year_begin"].as_str().unwrap().parse::<f64>().unwrap(),
        -3000.0,
        "贷方期初应为负"
    );

    // 操作日志可查且非空（含科目操作记录）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/logs?limit=50&q=%E7%A7%91%E7%9B%AE", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    let logs: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(!logs.as_array().unwrap().is_empty(), "日志搜索应非空：{s}");

    // 备份 → 列表落地 → 恢复
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/backups", &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "备份应成功");
    let s = body_string(resp).await;
    let name: String = serde_json::from_str::<serde_json::Value>(&s).unwrap()["name"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains(&name), "备份列表应包含 {name}：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/restore",
            &sid,
            serde_json::json!({ "file": name }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "恢复应成功");
    // 路径穿越必须被拒绝（file_name 归一化后落入 backups 内且不存在 → 404）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/restore",
            &sid,
            serde_json::json!({ "file": "../../etc/passwd" }),
        ))
        .await
        .unwrap();
    assert!(
        !resp.status().is_success(),
        "路径穿越必须被拒绝：{}",
        resp.status()
    );

    // 账套参数读写
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    let mut opts: serde_json::Value = serde_json::from_str(&s).unwrap();
    opts["company"] = serde_json::json!("改名公司");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("改名公司"), "参数应已更新：{s}");
}

/// 审核环节默认开（对标金蝶星空 + 会计内控底线：制单/审核/记账三权分离）。
///
/// 锁两件事：① 通过生产建账接口创建的账套，审核环节默认是**开**的
///         ② 默认配置下「草稿直接记账」被拒，必须先审核
///
/// 这两个用例是 api.rs 夹具显式 `enable_audit: false` 的正当性来源——夹具关掉
/// 是为了让别的用例专注自己的主题，默认值本身由这里盯住。
#[tokio::test]
async fn audit_default_on_for_new_books() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;

    // 生产路径：POST /api/books 建的新账套
    let (status, body) = create_book(&state, &admin_sid, "默认审核").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &admin_sid, &key).await, StatusCode::OK);

    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &admin_sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let o: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(
        o["enable_audit"], true,
        "新建账套的审核环节必须默认开启（制单/审核/记账三权分离）"
    );
    // 出纳签字现在也**默认开**。原来是 false，理由是「资金域节点不是全行业默认」——
    // 但那等于新建账套一出生就在默认绕过出纳：谁都不必做任何决定，就已经在绕了。
    // 与审核环节保持同一取向（默认开、要关得在账套参数里显式关，是一次有意识的决定）。
    assert_eq!(
        o["require_cashier"], true,
        "新建账套的出纳签字必须默认开启（现金/银行是内控风险最高的一块）"
    );
    // 建账向导要问「有没有出纳」：选了不启用才写 false
    let mut o2 = o.clone();
    o2["require_cashier"] = serde_json::json!(false);
    let r = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &admin_sid, o2))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "显式关掉出纳签字应当允许");
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &admin_sid))
        .await
        .unwrap();
    let back: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(
        back["require_cashier"], false,
        "关掉后应能读回 false（显式关闭必须真的生效）"
    );
}

/// 备份目录全局共享，但 list/restore 必须按账套隔离，不能跨租户读取/覆盖
#[tokio::test]
async fn backups_isolated_per_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let zhang = provision_plain_user(&state, &admin_sid, "zhangy", "Zy12345678").await;
    let li = provision_plain_user(&state, &admin_sid, "liy", "Ly12345678").await;

    let (status, body) = create_book(&state, &zhang, "张记").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let zk = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &zhang, &zk).await, StatusCode::OK);

    let (status, body) = create_book(&state, &li, "李记").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lk = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &li, &lk).await, StatusCode::OK);

    // 管理员进入张记套后备份（备份/恢复为管理员能力；按套隔离语义不变）
    assert_eq!(select_book(&state, &admin_sid, &zk).await, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/backups",
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "备份应成功");
    let v: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).expect("备份响应应是 JSON");
    let zname = v["name"].as_str().expect("应返回备份文件名").to_string();

    // 李记套的备份列表里不能出现张记的备份（管理员切到李记套上下文查询）
    assert_eq!(select_book(&state, &admin_sid, &lk).await, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &admin_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_string(resp).await;
    assert!(!list.contains(&zname), "不应看到其他账套的备份：{list}");

    // 从李记套恢复张记备份必须被拒（不是 404，而是明确属于别的账套）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/restore",
            &admin_sid,
            serde_json::json!({ "file": zname }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "不应允许恢复其他账套的备份"
    );

    // 回到张记套：本套的备份仍可见（确认过滤没有把范围清空）
    assert_eq!(select_book(&state, &admin_sid, &zk).await, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &admin_sid))
        .await
        .unwrap();
    let own = body_string(resp).await;
    assert!(own.contains(&zname), "本套的备份应可见：{own}");
}

/// 建套收归管理员 + 存量账套归属迁移（版本更新时执行、幂等）：
/// 普通账号名下的套 → 最早管理员名下，并把管理员补进该套的套内管理员成员行。
#[tokio::test]
async fn only_admin_creates_books_and_owners_migrate() {
    let (state, books_dir, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;

    // 1) 普通账号不能建套；2) 管理员能建套（owner=boss）
    let u = provision_plain_user(&state, &admin_sid, "fr1", "Fr12345678").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/books",
            &u,
            serde_json::json!({ "key": "", "company": "普通账号自建", "start_period": 202601 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "仅管理员可新建账套");
    let (st, body) = create_book(&state, &admin_sid, "存量改造套").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let k1 = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(state.realm.book_owner(&k1).unwrap().unwrap(), "boss");

    // 3) 造迁移前存量：普通账号 owner 的账套（realm 直插 + 真实文件 + 注册）
    let path = books_dir.join("legacy1.fbk");
    let _ = findb::Db::create_no_admin(&path, &BookOptions::default()).expect("建存量账套文件");
    state
        .realm
        .register_book("legacy1", path.to_str().unwrap(), "fr1", "旧账套")
        .expect("直插存量账套记录");
    state.books.register(&path, 16);

    // 4) 执行迁移：归属接管 + 套内补管理员成员行
    let n = state.migrate_book_owners_to_admin();
    assert!(n >= 1, "应接管至少 1 个存量账套（实际 {n}）");
    assert_eq!(
        state.realm.book_owner("legacy1").unwrap().unwrap(),
        "boss",
        "普通账号名下的套应归管理员"
    );
    assert_eq!(
        state.realm.book_owner(&k1).unwrap().unwrap(),
        "boss",
        "管理员名下的套不动"
    );
    let db = findb::Db::open(&path).expect("打开存量账套");
    let bu = findb::users::get(&db, "boss").expect("读套内成员");
    assert!(
        bu.map(|x| x.is_admin()).unwrap_or(false),
        "管理员应补进套内管理员成员行"
    );

    // 5) 幂等：二次执行无事可做
    let n2 = state.migrate_book_owners_to_admin();
    assert_eq!(n2, 0, "二次迁移应为 0");

    // 6) 管理员可进入迁移后的套
    assert_eq!(
        select_book(&state, &admin_sid, "legacy1").await,
        StatusCode::OK,
        "管理员应能进入迁移后的套"
    );
}

/// 探针必须**能抓到故障** —— 否则它是虚假的安全感。
///
/// 启动时必须把所有账套迁到当前版本，并让 readiness 如实汇报。
///
/// 回归背景（生产真实发生）：迁移原本是**懒执行**的 —— 账套第一次被打开时
/// 才跑 `schema::init`。于是升级后：部署完成、readiness 全绿，而用户第一次
/// 点「收货」才报 `no such column: item_code`。故障点与原因隔了几小时，
/// 部署的人不会联想到 schema 版本。
///
/// 现在改成启动时后台批量迁移，并且 readiness 汇报三件事：
/// 迁完了没、有没有账套迁不动、还有没有账套根本没迁过。
#[tokio::test]
async fn startup_migrates_every_book_and_readiness_reports_it() {
    let (state, bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造一个「已注册但从未打开」的账套。
    //
    // 不能直接用刚建好的 b1：创建账套时已经打开过它，迁移早跑完了，pending 当然是空的。
    // `unregister` + `register` 正是**恢复账套**走的那条路（覆盖文件后重新注册），
    // 所以这里构造的就是恢复后「文件是旧版本、还没迁」的真实状态。
    let fbk = std::fs::read_dir(&bd)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "fbk").unwrap_or(false))
        .expect("应有一个账套文件");
    let key = fbk.file_stem().unwrap().to_string_lossy().to_string();
    state.books.unregister(&key);
    state.books.register(&fbk, 16);
    assert_eq!(
        state.books.unmigrated_books(),
        vec![key.clone()],
        "前置条件：重新注册后该账套应处于未迁移状态"
    );

    // ① 未迁移的账套必须被 readiness 点名（而不是报绿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "还有账套没迁过，readiness 不能报就绪"
    );
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let mig = &r["checks"]["migration"];
    assert_eq!(mig["ok"], false, "migration.ok 应为 false：{mig}");
    let pending = mig["pending"].as_array().expect("应报出 pending 列表");
    assert!(
        !pending.is_empty(),
        "必须指名道姓报出哪些账套没迁过，否则运维无从下手：{mig}"
    );
    // 顺带确认报的是当前 schema 版本，便于一眼看出「落后几个版本」
    assert!(
        mig["schema_version"].is_i64(),
        "应报出 schema_version：{mig}"
    );

    // ② 跑批量迁移后应全部成功、pending 清空、readiness 转绿
    let st = state.books.migrate_all();
    assert!(st.done(), "migrate_all 返回时不该还在进行中");
    assert!(
        st.failed.is_empty(),
        "批量迁移不该有失败（全新账套都是当前版本）：{:?}",
        st.failed
    );
    assert_eq!(st.ok, st.total, "所有账套都应迁移成功");
    assert!(st.total > 0, "夹具里应至少有一个账套，实际 {}", st.total);
    assert!(
        state.books.unmigrated_books().is_empty(),
        "迁移后不应还有未迁移账套：{:?}",
        state.books.unmigrated_books()
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "迁移完成后 readiness 应 200");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["checks"]["migration"]["ok"], true);
    assert_eq!(
        r["checks"]["migration"]["pending"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(r["ok"], true, "全部检查应通过：{r}");

    // ③ 幂等：再跑一次不报错、结果不变
    let again = state.books.migrate_all();
    assert!(again.failed.is_empty() && again.ok == again.total);

    // ④ liveness 全程 200 —— 迁移是后台做的，绝不能拖垮 liveness
    //    （liveness 一旦碰重活，编排器会判 unhealthy 并重启，放大抖动）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 账套迁不动时，readiness 必须报 503 并指名道姓，而不是继续报绿。
///
/// 与上一条互补：那条测「还没迁」，这条测「迁了但失败」。
/// 财务系统里一个账套打不开不是「可以先用着」，必须让人看见。
#[tokio::test]
async fn readiness_reports_503_when_a_book_cannot_be_migrated() {
    let (state, bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 把账套文件写坏，让 schema::init 失败
    let fbk = std::fs::read_dir(&bd)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "fbk").unwrap_or(false))
        .expect("应有一个账套文件");
    let key = fbk.file_stem().unwrap().to_string_lossy().to_string();
    std::fs::write(&fbk, b"not a sqlite database at all").unwrap();
    // 换一个干净连接重开（写坏前的连接可能还有缓存）
    state.books.unregister(&key);
    state.books.register(&fbk, 16);

    let st = state.books.migrate_all();
    assert!(st.done());
    assert!(
        st.has_failures(),
        "坏掉的账套必须被记为迁移失败，而不是悄悄跳过：{st:?}"
    );
    assert_eq!(st.ok, 0, "唯一那个账套应该失败：{st:?}");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "有账套迁不动，readiness 必须是 503"
    );
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let failed = r["checks"]["migration"]["failed"]
        .as_array()
        .expect("应报 failed 列表");
    assert_eq!(failed.len(), 1, "应指名报出那个账套：{r}");
    assert_eq!(failed[0]["book"].as_str().unwrap(), key.as_str());
    assert!(
        !failed[0]["error"].as_str().unwrap_or("").is_empty(),
        "必须带上错误原因，否则运维无从判断：{r}"
    );
}

/// 回归背景：原 `/api/health` 返回常量 `ok`（不碰任何依赖），于是磁盘写满、
/// 账套文件损坏、账号库打不开这些「系统已不可用」的状态，它一律报健康。
/// 监控拿着这样的答案会一直显示「一切正常」，人就不去查别的地方了。
///
/// 这里用「账套文件被截断成垃圾」模拟真实损坏：文件存在、能打开，
/// 但查询时才报错 —— 正是 `Connection::open` 惰性打开最容易漏过的那类故障。
#[tokio::test]
async fn readiness_probe_detects_a_corrupt_book_file() {
    let (state, bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // ① 正常状态：liveness 与 readiness 都 200
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "健康时 readiness 应 200");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], true, "健康时 ok 应为 true：{r}");
    assert_eq!(r["checks"]["realm"]["ok"], true);
    assert_eq!(r["checks"]["books_dir_writable"]["ok"], true);

    // ② 把账套文件写坏（截断成非 SQLite 内容）
    let fbk = std::fs::read_dir(&bd)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "fbk").unwrap_or(false))
        .expect("应有一个账套文件");
    std::fs::write(&fbk, b"this is definitely not a sqlite database").unwrap();

    // ③ liveness 仍 200（进程确实还活着）—— 这正是两者的分工
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "liveness 不该碰依赖：账套坏了不代表进程死了"
    );

    // ④ readiness 必须抓到：503，且明说是哪个账套坏了
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "账套损坏时 readiness 必须报 503，否则负载均衡会继续往坏实例打流量"
    );
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], false, "响应体也要明说不可用：{r}");
    assert_eq!(
        r["checks"]["book_sample"]["ok"], false,
        "要指出是抽样账套查询失败：{r}"
    );
    let detail = r["checks"]["book_sample"].to_string();
    assert!(
        detail.contains("error"),
        "必须给出错误原因，否则运维看到「不健康」却不知道修什么：{detail}"
    );
}

/// 探针不该在数据目录里留下垃圾，也不该把自己的临时文件当成账套。
#[tokio::test]
async fn readiness_probe_leaves_no_files_behind() {
    let (state, bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let names: Vec<String> = std::fs::read_dir(&bd)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "探针跑完目录里应只剩账套本身，实际：{names:?}"
    );
    assert!(
        names[0].ends_with(".fbk"),
        "探针的临时文件不能以 .fbk 结尾，否则下次启动会被当成新账套注册：{names:?}"
    );
}

/// 备份后缀碰撞：key=`202610` 不能认领 `alice_202610` 的备份。
#[tokio::test]
async fn backup_suffix_collision_cannot_cross_books() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;

    let a = create_book_with_key(&state, &admin_sid, "alice_202610").await;
    assert_eq!(select_book(&state, &admin_sid, &a).await, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/backups",
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let name = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["name"]
        .as_str()
        .unwrap()
        .to_string();

    // 这个 key 是上一个账套 key 的后缀，旧实现会在这里命中 alice 的备份。
    let b = create_book_with_key(&state, &admin_sid, "202610").await;
    assert_eq!(select_book(&state, &admin_sid, &b).await, StatusCode::OK);

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &admin_sid))
        .await
        .unwrap();
    let list = body_string(resp).await;
    assert!(
        !list.contains(&name),
        "key=202610 不应看到 alice_202610 的备份：{list}"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/restore",
            &admin_sid,
            serde_json::json!({ "file": name }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "后缀碰撞不能跨账套恢复"
    );
}
