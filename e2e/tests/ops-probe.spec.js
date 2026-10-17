const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 运维探针：liveness 与 readiness 的分工
//
// 这一条守的是**监控能不能抓到故障**，不是接口好不好看：
//   /api/health       = liveness，故意不碰任何依赖，恒返回 ok
//   /api/health/ready = readiness，真去查账号库 / 账套库 / 数据目录
//
// 回归背景：原探针只有 /api/health（返回常量 ok），磁盘写满、账套损坏、
// 账号库被锁这些「系统已不可用」的状态它一律报健康。监控拿着这样的答案
// 会一直显示「一切正常」，人就不去查别的地方了 —— 虚假的安全感比没监控更糟。
//
// 为什么要分两个：liveness 一旦碰 DB，数据库变慢就把容器判 unhealthy，
// 编排器随即重启应用，于是「数据库一慢就重启，重启又让数据库更慢」。

test("readiness 探针在真实服务上报告每项依赖都可用", async ({ page }) => {
  await newBook(page, `E2E探针${Date.now()}`);

  const live = await page.request.get("/api/health");
  expect(live.ok(), "liveness 应 200").toBeTruthy();
  expect((await live.text()).trim()).toBe("ok");

  const ready = await page.request.get("/api/health/ready");
  expect(ready.status(), "健康时 readiness 应 200").toBe(200);
  const r = await ready.json();
  expect(r.ok, `readiness 应报 ok：${JSON.stringify(r)}`).toBe(true);
  // 每项依赖都要单独给出结论 —— 只回一个笼统的 ok，运维不知道该查哪
  expect(r.checks.realm.ok, "要查账号库").toBe(true);
  expect(r.checks.books_dir_writable.ok, "要查数据目录可写").toBe(true);
  expect(typeof r.checks.books_count, "要报账套数量").toBe("number");
  // 建了账套之后抽样检查才有意义
  expect(r.checks.books_count, "刚建完账套目录应有 1 个账套").toBeGreaterThan(0);
  expect(r.checks.book_sample.ok, "要抽样打开一个账套做只读查询").toBe(true);
});

test("readiness 免登录（监控没有浏览器会话）", async ({ request }) => {
  // `request` 是独立的 APIRequestContext，**不带浏览器 cookie**。
  // 必须用它而不是 `page.request`：后者带着登录态，命中了鉴权也照样 200，
  // 那种写法测的是「登录用户能不能访问」，不是「免登录通不通」。
  const live = await request.get("/api/health");
  expect(live.status(), "liveness 不该要求登录").toBe(200);

  const ready = await request.get("/api/health/ready");
  expect(ready.status(), "readiness 不该要求登录").toBe(200);
  const r = await ready.json();
  expect(r.ok, `免登录时也应给出真实结论：${JSON.stringify(r)}`).toBe(true);
});
