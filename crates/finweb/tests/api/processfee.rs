//! 加工费规则 API：按面积计价 + 最低收费 + 加急费 + 规则校验

use super::*;

#[tokio::test]
async fn price_rule_quote_area_floor_and_rush() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 氧化厂典型规则：18 元/㎡，起步 30 元，加急 20 元 + 10%
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "氧化-普通",
                "unit": "㎡",
                "unit_price": "18",
                "min_charge": "30",
                "rush_fee": "20",
                "rush_rate": "10",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建规则");

    // 12.5 ㎡ × 18 = 225，不触起步价
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?qty=12.5", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["base"].as_str().unwrap()), 225.00);
    assert_eq!(money_num(v["quote"]["floor_extra"].as_str().unwrap()), 0.00);
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 225.00);

    // 0.5 ㎡ × 18 = 9 → 补到起步价 30
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?qty=0.5", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["base"].as_str().unwrap()), 9.00);
    assert_eq!(
        money_num(v["quote"]["floor_extra"].as_str().unwrap()),
        21.00
    );
    assert_eq!(
        v["quote"]["total"].as_str().unwrap(),
        "30.00",
        "最低收费兜底"
    );

    // 加急：225 + 22.5(10%) + 20 = 267.5
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?qty=12.5&rush=1", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["rush_extra"].as_str().unwrap()), 42.50);
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 267.50);

    // 数量为 0 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?qty=0", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 加急率超过 100% → 400（不是静默按 100 算）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({ "name": "乱填", "unit": "件", "unit_price": "1", "rush_rate": "120" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("0-100"));

    // 规则列表带出刚建的那条
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["rules"].as_array().unwrap().len(), 1);
    assert_eq!(v["rules"][0]["name"].as_str().unwrap(), "氧化-普通");
}

/// 折算系数 + 单价矩阵 + 按口径报价（v49）
///
/// 背景是用户的原话：氧化加工有按件/条/只算的，有按米长、按米重算的，
/// 还有按平方算的 —— 一个物料同时要挂在四五个口径上。
#[tokio::test]
async fn calc_units_drive_matrix_and_conversion() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 300 只折合 7.02 ㎡；1 米型材折合 1.85 kg
    for body in [
        serde_json::json!({ "item_code": "MB01", "calc_unit": "㎡", "factor": "0.0234", "scale": 2 }),
        serde_json::json!({ "item_code": "MB01", "calc_unit": "kg", "factor": "1.85", "scale": 2 }),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post("/api/calc-units", &sid, body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建折算系数");
    }

    // 口径列出来了
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/calc-units?item=MB01", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["units"].as_array().unwrap().len(), 2);

    // 在矩阵里给「MB01 按㎡」填价：12 元/㎡，起步 30
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/price-matrix",
            &sid,
            serde_json::json!({ "item_code": "MB01", "calc_unit": "㎡", "unit_price": "12", "min_charge": "30" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "矩阵改价");

    // 矩阵：一行两列，系数与单价都在
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-matrix", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mx: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = mx["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["item_code"] == "MB01")
        .expect("矩阵里要有 MB01 这一行");
    let area = row["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["unit"] == "㎡")
        .unwrap();
    assert_eq!(money_num(area["unit_price"].as_str().unwrap()), 12.0);
    assert_eq!(money_num(area["factor"].as_str().unwrap()), 0.0234);
    let mass = row["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["unit"] == "kg")
        .unwrap();
    assert_eq!(money_num(mass["factor"].as_str().unwrap()), 1.85);
    assert_eq!(
        mass["rule_id"].as_i64().unwrap(),
        0,
        "没定价的格子 rule_id=0"
    );

    // 300 只 → 7.02 ㎡ × 12 = 84.24（不触起步价）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=300&calc_unit=%E3%8E%A1",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["conv"]["calc_qty"].as_str().unwrap()), 7.02);
    assert_eq!(money_num(v["conv"]["factor"].as_str().unwrap()), 0.0234);
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 84.24);

    // 按 kg 报没定价 → 400，并且说清这一格是空的（不是「没有规则」这种含糊话）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=300&calc_unit=kg",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("还是空的"));

    // 没定价的口径不能拿别的口径的价格顶上（12 元/㎡ 乘 300 kg 是几百倍错账）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=300&calc_unit=kg",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 老接口（直接给计价数量）照旧能用
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&qty=7.02",
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 84.24);

    // 删掉系数后，按㎡ 报价要说「还没设折算系数」
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/calc-units?item=MB01", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let area_id = v["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["calc_unit"] == "㎡")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/calc-units/{area_id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删系数");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=300&calc_unit=%E3%8E%A1",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("折算系数"));
}

/// 提前录下一版价：生效日没到之前不能顶掉现在的价，日期写错要当场拦
#[tokio::test]
async fn matrix_cell_carries_effective_date() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/calc-units",
            &sid,
            serde_json::json!({ "item_code": "MB01", "calc_unit": "㎡", "factor": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 现在的价 10，3 月起的新价 12（提前录）
    for (price, from) in [("10", "2020-01-01"), ("12", "2999-03-01")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_put(
                "/api/price-matrix",
                &sid,
                serde_json::json!({
                    "item_code": "MB01", "calc_unit": "㎡",
                    "unit_price": price, "effective_from": from,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "改价 {price}");
    }

    // 矩阵里显示的是「现在生效的那条」，未来价单独标出来
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-matrix", &sid))
        .await
        .unwrap();
    let mx: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cell = &mx["rows"].as_array().unwrap()[0]["cells"]
        .as_array()
        .unwrap()[0];
    assert_eq!(money_num(cell["unit_price"].as_str().unwrap()), 10.0);

    // 报价也用现在这条
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=1&calc_unit=%E3%8E%A1",
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 10.0);
    assert!(!v["quote"]["future"].as_bool().unwrap());
    assert_eq!(v["quote"]["effective_from"].as_str().unwrap(), "2020-01-01");

    // 日期写错（2026-3-1）→ 400，理由说的是格式
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/price-matrix",
            &sid,
            serde_json::json!({
                "item_code": "MB01", "calc_unit": "㎡",
                "unit_price": "13", "effective_from": "2026-3-1",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("2026-03-01"));
}

/// 批量导入：客户发来的价目表整张导进去（表头跳过、同格覆盖、坏行只跳自己）
#[tokio::test]
async fn price_import_overwrites_same_cell_and_reports_bad_rows() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 一、先导一张「带表头」的价表：表头要自动跳过，两行数据都要进来
    let csv = "物料编码,客户编码,计价口径,单价,起步价,加急固定费,加急率%,生效日期,名称,备注\n\
               MB01,C01,㎡,12.00,30.00,20.00,10,,,\n\
               MB02,,只,0.45,,,,,,\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules/import",
            &sid,
            serde_json::json!({ "text": csv }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入单价");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["created"], 2, "两行新价：{v}");
    assert_eq!(v["updated"], 0);
    assert_eq!(v["skipped"], 0, "表头不该被当成坏行：{v}");

    // 二、再导一遍，MB01 那格改成 13.5，外加一行坏数据
    let csv = "物料编码,客户编码,计价口径,单价\n\
               MB01,C01,㎡,13.5\n\
               MB02,,只,abc\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules/import",
            &sid,
            serde_json::json!({ "text": csv }),
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["created"], 0, "同一格要覆盖不要新增：{v}");
    assert_eq!(v["updated"], 1);
    assert_eq!(v["skipped"], 1);
    let warns = v["warnings"].as_array().unwrap();
    assert!(
        warns[0].as_str().unwrap().contains("第 3 行"),
        "警告要带行号：{warns:?}"
    );

    // 三、矩阵里看到的必须是新价，而且只有两条（覆盖没变成三条）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-matrix?customer=C01", &sid))
        .await
        .unwrap();
    let mx: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = mx["rows"].as_array().unwrap();
    assert!(
        rows.iter().any(|r| r["item_code"] == "MB01"
            && r["cells"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["unit"] == "㎡" && c["unit_price"] == "13.50")),
        "矩阵要显示导入后的新价：{mx}"
    );

    // 四、折算系数也能批量导，导完「填只数自动折算」就通了
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/calc-units/import",
            &sid,
            serde_json::json!({ "text": "物料编码,计价口径,系数,保留位数\nMB01,㎡,0.0234,2\n" }),
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        (v["created"].as_i64(), v["skipped"].as_i64()),
        (Some(1), Some(0))
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&customer=C01&stock_qty=300&calc_unit=%E3%8E%A1",
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["conv"]["calc_qty"].as_str().unwrap(), "7.02");
    // 7.02 ㎡ × 13.5 = 94.77（起步 30 已过）
    assert_eq!(v["quote"]["total"].as_str().unwrap(), "94.77");

    // 五、模板下载：带 BOM，列头和解析器同一份
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-import/template?kind=units", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.starts_with('\u{feff}'), "CSV 要带 BOM，Excel 才不乱码");
    assert!(body.contains("物料编码,计价口径,系数,保留位数"));
}

/// 按规格算单价：客户报「2.5 元一米」「80 元一个平方」，工件报规格 —— 中间那步不该让人按计算器
#[tokio::test]
async fn spec_turns_a_rate_into_a_piece_price() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 一、批量算价器：一个费率 + 一串规格 → 每件多少钱
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-specs/calc",
            &sid,
            serde_json::json!({
                "rate": "2.5",
                "unit": "米",
                "specs": ["1200*80", "920*60", "坏行"],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["dims"], "1200×80");
    assert_eq!(rows[0]["factor"].as_str().unwrap(), "1.2000", "折合量 4 位");
    assert_eq!(
        money_num(rows[0]["price"].as_str().unwrap()),
        3.00,
        "1.2 米 × 2.5"
    );
    assert_eq!(
        money_num(rows[1]["price"].as_str().unwrap()),
        2.30,
        "0.92 米 × 2.5"
    );
    assert!(
        rows[2]["note"].as_str().unwrap().contains("看不懂"),
        "坏规格要给话说，不能算成 0 元：{:?}",
        rows[2]
    );

    // 二、平方：1250×250×3 的板 0.3125 ㎡ × 80 = 25.00
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-specs/calc",
            &sid,
            serde_json::json!({ "rate": "80", "unit": "㎡", "specs": ["1250*250*3"] }),
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["rows"][0]["price"].as_str().unwrap()), 25.00);

    // 三、没录折算系数，填规格也能报价（300 块板 = 93.75 ㎡）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({ "name": "按平方", "item_code": "MB01", "unit": "㎡", "unit_price": "80" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=300&calc_unit=%E3%8E%A1&spec=1250*250*3",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["conv"]["basis"], "spec", "没系数时按规格算");
    assert_eq!(v["conv"]["dims"], "1250×250×3");
    assert_eq!(
        v["conv"]["factor"].as_str().unwrap(),
        "0.3125",
        "折合量要留 4 位 —— 显示成 0.31 会和金额对不上"
    );
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 7500.00);

    // 四、连规格都没有 → 报错说清楚下一步做什么
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=1&calc_unit=%E3%8E%A1&spec=1250",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp).await;
    assert!(body.contains("看不懂"), "实际：{body}");
}

/// 工艺档：同一个工件、同一个客户、同一种口径，换个表面处理就是另一个价
///
/// 规则要守住的是「不许串档」：没选档位的报价绝不能抓到硬质氧化的价 ——
/// 客户拿普通氧化的单子来对账，一句「系统报的」就说不清了。
#[tokio::test]
async fn tier_prices_stay_in_their_own_tier() {
    let hard = "%E7%A1%AC%E8%B4%A8%E6%B0%A7%E5%8C%96"; // 硬质氧化
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 普通氧化：不分档的通用价 12 元/㎡
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({ "name": "氧化-随便哪档", "item_code": "MB01", "unit": "㎡", "unit_price": "12" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 硬质氧化单独一档 18 元/㎡
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "氧化-硬质", "item_code": "MB01", "unit": "㎡",
                "unit_price": "18", "process": "硬质氧化",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建硬质档");

    // 一、选了档位就报档位价：10 ㎡ × 18 = 180
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/price-rules/quote?item=MB01&qty=10&process={hard}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 180.0);

    // 二、不选档位走通用价：10 ㎡ × 12 = 120（不能串成 180）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?item=MB01&qty=10", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 120.0);

    // 三、只录了档位价、没录通用价的物料，不选档位必须报错而不是「随便借一个档」
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "导电-只这一档", "item_code": "MB02", "unit": "㎡",
                "unit_price": "25", "process": "导电氧化",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?item=MB02&qty=1", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "没选档位不许抓档位价"
    );

    // 四、档位价优先于通用价，且矩阵里「借来的通用价」要标出来
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/price-matrix?process={hard}"),
            &sid,
        ))
        .await
        .unwrap();
    let mx: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(mx["process"], "硬质氧化");
    let cell_of = |code: &str| {
        mx["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["item_code"] == code)
            .and_then(|r| {
                r["cells"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["unit"] == "㎡")
            })
            .cloned()
            .unwrap()
    };
    let hard_cell = cell_of("MB01");
    assert_eq!(money_num(hard_cell["unit_price"].as_str().unwrap()), 18.0);
    assert_eq!(
        hard_cell["fallback"].as_bool(),
        Some(false),
        "本档自己的价不该标成借用"
    );

    // 五、矩阵改价也要按档落库：同一物料同一口径，两档各自一条价
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/price-matrix",
            &sid,
            serde_json::json!({
                "item_code": "MB03", "calc_unit": "㎡", "unit_price": "30",
                "process": "硬质氧化",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "按档改矩阵");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/price-matrix",
            &sid,
            serde_json::json!({ "item_code": "MB03", "calc_unit": "㎡", "unit_price": "20" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "再改通用价");

    let count = |mx: &serde_json::Value| {
        mx["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["item_code"] == "MB03")
            .count()
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/price-matrix?process={hard}"),
            &sid,
        ))
        .await
        .unwrap();
    let mx: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(count(&mx), 1, "同物料只该有一行：{mx}");
    assert_eq!(
        money_num(cell_of_relaxed(&mx, "MB03")["unit_price"].as_str().unwrap()),
        30.0,
        "硬质档看到的是自己那 30"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-matrix", &sid))
        .await
        .unwrap();
    let mx: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        money_num(cell_of_relaxed(&mx, "MB03")["unit_price"].as_str().unwrap()),
        20.0,
        "不分档看到的是通用价 20"
    );
}

/// 从矩阵里取某物料 ㎡ 那一格（不关心行序）
fn cell_of_relaxed(mx: &serde_json::Value, code: &str) -> serde_json::Value {
    mx["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["item_code"] == code)
        .and_then(|r| {
            r["cells"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["unit"] == "㎡")
        })
        .cloned()
        .expect("矩阵里要有这一行")
}

#[tokio::test]
async fn spec_bands_pick_the_unit_by_size() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 客户原话：「200cm 以下 0.2 元/件，200cm 以上 1.5 元/米」
    for (name, unit, price, from, to) in [
        ("MB03 短件按件", "件", "0.2", "", "200"),
        ("MB03 长料按米", "米", "1.5", "200", ""),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/price-rules",
                &sid,
                serde_json::json!({
                    "name": name,
                    "item_code": "MB03",
                    "unit": unit,
                    "unit_price": price,
                    "band_dim": "max",
                    "band_from": from,
                    "band_to": to,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建分档价 {name}");
    }

    // 1200mm = 120cm ≤ 200 → 按件，300 件 × 0.2 = 60
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB03&spec=1200*80&stock_qty=300",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["conv"]["calc_unit"], "件", "短件自动按件，不用人先选口径");
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 60.00);

    // 2500mm = 250cm > 200 → 按米，10 件 × 2.5 米 × 1.5 = 37.50
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB03&spec=2500*100&stock_qty=10",
            &sid,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["conv"]["calc_unit"], "米");
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 37.50);

    // 分档价不进矩阵，但要能在 /api/price-bands 里看全
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-bands", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["rows"].as_array().unwrap().len(), 2);
    assert_eq!(v["rows"][0]["band_to"], "200", "按区间从低到高排");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-matrix", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["item_code"] != "MB03"),
        "分档价不该在矩阵里冒充通价"
    );

    // 区间填反了当场拒绝，别让一条永远用不上的价躺在表里
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "填反了", "unit": "件", "unit_price": "1",
                "band_dim": "max", "band_from": "300", "band_to": "200",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("永远用不上"));
}

#[tokio::test]
async fn qty_tiers_follow_the_calc_quantity() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 「500 件以下 0.35、以上 0.25」：只填数量区间，不填规格档
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "小批量", "item_code": "MB07", "unit": "件",
                "unit_price": "0.35", "qty_to": "500",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建小批量价");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "大批量", "item_code": "MB07", "unit": "件",
                "unit_price": "0.25", "qty_from": "500",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建大批量价");

    // 300 件走小批量：300 × 0.35
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?item=MB07&qty=300", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["quote"]["rule_name"].as_str().unwrap(), "小批量");
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 105.00);

    // 800 件走大批量：800 × 0.25
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?item=MB07&qty=800", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["quote"]["rule_name"].as_str().unwrap(), "大批量");
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 200.00);

    // 正好 500 件算下面那一档（下限不含），不会两天都命中
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?item=MB07&qty=500", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["quote"]["rule_name"].as_str().unwrap(), "小批量");

    // 分段表里看得到这两条，矩阵里看不到
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-bands", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "两条数量档都要列出来");
    assert_eq!(rows[0]["qty_to"].as_str().unwrap(), "500");
    assert_eq!(rows[1]["qty_from"].as_str().unwrap(), "500");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-matrix", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["item_code"] != "MB07"),
        "数量档价不该在矩阵里冒充通价"
    );

    // 上下限填反 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "填反了", "unit": "件", "unit_price": "1",
                "qty_from": "500", "qty_to": "200",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("永远用不上"));
}

#[tokio::test]
async fn price_history_records_who_changed_what() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建一条 0.2 元/件的价
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "MB11 按件",
                "item_code": "MB11",
                "unit": "件",
                "unit_price": "0.2",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let id = v["id"].as_i64().unwrap();

    // 改成 0.15（同一个 id，走 UPDATE）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "id": id,
                "name": "MB11 按件",
                "item_code": "MB11",
                "unit": "件",
                "unit_price": "0.15",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/history?item=MB11", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "新增一条、改价一条：{v}");
    assert_eq!(rows[0]["action"], "改价");
    assert_eq!(rows[0]["old_price"], "0.20");
    assert_eq!(rows[0]["new_price"], "0.15");
    assert_eq!(rows[0]["who"], "boss", "改了价要能看出是谁改的");
    assert_eq!(rows[1]["action"], "新增");
    assert_eq!(rows[1]["new_price"], "0.20");
}

#[tokio::test]
async fn price_adjust_previews_then_writes_a_new_version() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 一条 12 元/㎡ 的通用价
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "MB21 按㎡",
                "item_code": "MB21",
                "unit": "㎡",
                "unit_price": "12",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 预览：涨 3% → 12.00 变 12.36，只算不落库
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/adjust?item_code=MB21&mode=pct&value=3&effective_from=2027-01-01",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "只有这一条该调：{v}");
    assert_eq!(rows[0]["old_price"], "12.00");
    // 算出来的价按 Money 的 4 位小数走（0.2808 这类每件价必须留住），比数值不比字面
    assert!((money_num(rows[0]["new_price"].as_str().unwrap()) - 12.36).abs() < 1e-9);
    assert_eq!(rows[0]["action"], "新建");
    assert_eq!(rows[0]["effective_from"], "2027-01-01");
    // 预览不写库
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["rules"].as_array().unwrap().len(), 1, "预览不该落库：{v}");

    // 执行
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules/adjust",
            &sid,
            serde_json::json!({
                "item_code": "MB21",
                "mode": "pct",
                "value": "3",
                "effective_from": "2027-01-01",
                "memo": "2027 年度涨价",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["created"].as_u64(), Some(1));
    assert_eq!(v["updated"].as_u64(), Some(0));

    // 价目表多了一版，老价还在
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let list = v["rules"].as_array().unwrap();
    assert_eq!(list.len(), 2, "老价 + 新一版：{v}");
    let nu = list
        .iter()
        .find(|r| r["effective_from"] == "2027-01-01")
        .expect("新一版");
    assert_eq!(nu["unit_price"], "12.36");
    assert_eq!(nu["memo"], "2027 年度涨价");

    // 同一天再执行一次是覆盖，不会涨两次
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules/adjust",
            &sid,
            serde_json::json!({
                "item_code": "MB21",
                "mode": "pct",
                "value": "3",
                "effective_from": "2027-01-01",
            }),
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["updated"].as_u64(),
        Some(1),
        "同一天重跑要覆盖，不是再加一条：{v}"
    );
    let again = money_num(v["rows"][0]["new_price"].as_str().unwrap());
    assert!(
        (again - 12.36).abs() < 1e-9,
        "底价仍是 12.00，不该变成 12.73：{v}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["rules"].as_array().unwrap().len(),
        2,
        "覆盖后仍是两条：{v}"
    );

    // 荒诞参数要说人话
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/adjust?mode=pct&value=0", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("等于没调"));
}

#[tokio::test]
async fn price_export_downloads_xlsx_and_round_trips() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // MB31：老价 10 + 现价 12 + 停用 99 —— 只该导出「现价 12」这一条
    for (name, price, from, disabled) in [
        ("MB31 老价", "10", "2020-01-01", false),
        ("MB31 现价", "12", "2025-01-01", false),
        ("MB31 停用", "99", "", true),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/price-rules",
                &sid,
                serde_json::json!({
                    "name": name, "item_code": "MB31", "unit": "㎡",
                    "unit_price": price, "effective_from": from, "disabled": disabled
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
    // MB32：加急率走 exact_param 落库（可能是三位小数），导出不能四舍五入丢尾数
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "MB32 加急率", "item_code": "MB32", "unit": "只",
                "unit_price": "0.45", "rush_rate": "7.125"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // MB31 的折算系数：1 件 = 0.0234 ㎡ —— 导出要在表里写清这个换算
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/calc-units",
            &sid,
            serde_json::json!({ "item_code": "MB31", "calc_unit": "㎡", "factor": "0.0234", "scale": 4 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建折算系数");

    // 导出
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/export", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("spreadsheetml"), "content-type 应为 xlsx：{ct}");
    let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.len() > 1000, "导出内容不该是空：{} 字节", bytes.len());
    assert_eq!(&bytes[..2], b"PK", "xlsx 是 zip，头两字节固定 PK");

    // 读回校验：表头与导入模板一致，数据只留当前生效价
    let rows = findb::imports::read_xlsx_bytes(&bytes).unwrap();
    assert_eq!(rows.len(), 3, "表头 + 2 行当前价：{rows:?}");
    assert_eq!(rows[0].len(), 18, "18 列：16 列单价 + 库存单位 + 折算系数");
    assert_eq!(rows[0][0], "物料编码");
    assert_eq!(rows[0][3], "单价");
    assert_eq!(rows[0][16], "库存单位");
    assert_eq!(rows[0][17], "折算系数");
    let mb31 = rows.iter().find(|r| r[0] == "MB31").expect("MB31 现价");
    assert!(
        (money_num(&mb31[3]) - 12.0).abs() < 1e-9,
        "只留现价：{mb31:?}"
    );
    assert_eq!(mb31[7], "2025-01-01", "生效日期要原样保留");
    assert_eq!(mb31[16], "件", "库存单位：没设单位档案时按件兜底");
    assert_eq!(mb31[17], "0.0234", "折算系数满精度，不丢尾数");
    let mb32 = rows.iter().find(|r| r[0] == "MB32").expect("MB32");
    assert!(
        (money_num(&mb32[6]) - 7.125).abs() < 1e-9,
        "加急率的三位小数不能丢：{}",
        mb32[6]
    );
    assert_eq!(mb32[17], "", "没录系数的口径：折算系数留空");
}
