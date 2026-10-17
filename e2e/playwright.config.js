const { defineConfig } = require("@playwright/test");

module.exports = defineConfig({
  testDir: "./tests",
  timeout: 60_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  // 串行跑（workers: 1）。三个理由，缺一不可：
  // ① 账套上限是**每账号 10 个**，E2E 全部共用 admin 一个账号。并行时各 worker
  //    互相抢额度，撞上限后 `POST /api/books` 全 400，所有用例都卡在「等不到主界面」，
  //    1 个真实失败被放大成 30 个（实测 31 failed）。
  // ② `TEST_WORKER_INDEX` 是**全局递增计数器**，不是稳定的并行槽位——每个文件开新
  //    worker 就涨一次（实测跑到 8+），靠它做账套名前缀清理根本清不到别人的书。
  //    串行时索引恒为 0，helpers.js 的前缀清理才真正有效。
  // ③ 全套件共用一个 admin 账号和一个数据目录，并行本就存在状态互相踩踏。
  // 代价是墙钟时间（约 +30%），换来的是失败信号可信——这正是本套件最需要的。
  workers: 1,
  // CI 偶发（浏览器调度/计时）容忍一次，避免整条流水线因单个抖动重跑
  retries: 1,
  reporter: [["list"]],
  use: {
    baseURL: process.env.E2E_BASE_URL || "http://127.0.0.1:18080",
    headless: true,
    trace: "retain-on-failure",
  },
});
