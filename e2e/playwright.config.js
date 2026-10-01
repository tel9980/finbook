const { defineConfig } = require("@playwright/test");

module.exports = defineConfig({
  testDir: "./tests",
  timeout: 60_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  // CI 偶发（浏览器调度/计时）容忍一次，避免整条流水线因单个抖动重跑
  retries: 1,
  reporter: [["list"]],
  use: {
    baseURL: process.env.E2E_BASE_URL || "http://127.0.0.1:18080",
    headless: true,
    trace: "retain-on-failure",
  },
});
