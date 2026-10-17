# FinBook 安全加固修复报告

## 已修复项（共 20 项）

### P0 级安全修复
| ID | 问题 | 修复方案 | 状态 |
|----|------|----------|------|
| CRIT-1 | 首次登录自动成为管理员，叠加默认全网卡监听 | FINBOOK_LISTEN 默认改为 127.0.0.1；需显式配置才对外暴露 | ✅ |
| HIGH-1 | Web 层未落地 DataScope，任何会计可全盘查看凭证 | list/get/edit/delete 全部应用 with_data_scope + can_see_voucher | ✅ |
| HIGH-2 | voucher_unaudit 无权限校验且语义错误 | 补权限校验，区分 unpost/unaudit | ✅ |
| HIGH-3 | 凭证删除无守卫，可删已记账/已结账凭证 | 加 status/closed_scope/can_see_voucher 三重校验 | ✅ |
| HIGH-4 | prepared_by 字段三种口径并存 | 桌面端统一为 username | ✅ |
| HIGH-5 | DataScope 默认全放行 | 非管理员默认 own_voucher_only=true | ✅ |
| HIGH-6 | must_change_pwd 仅前端约束 | 服务端 CurrentUser 拦截器强制检查 | ✅ |
| HIGH-7 | Cookie 缺 Secure 标志 | 增加 FINWEB_SECURE_COOKIE 环境变量支持 | ✅ |

### P1 级账务/安全修复
| ID | 问题 | 修复方案 | 状态 |
|----|------|----------|------|
| H-C-1 | 资产负债表坏账/存货跌价 .neg() 方向错误 | 去掉 1231/1471 的 .neg()，直接用负数余额 | ✅ |
| H-2 | 核销 settle() SQL TEXT 金额加法 | 改为 Rust 侧读旧值、累加后写回，用 money_param | ✅ |
| M-1 | 登录响应枚举用户名 | BadPassword/NoSuchUser/Locked 统一返回 401 同文案 | ✅ |
| M-3 | 锁定窗口单位 bug | 新增 LOCK_WINDOW_MIN=10 常量，与锁定时长解耦 | ✅ |
| M-5 | 附件上传无大小上限/无 MIME 校验 | 新增 MAX_FILE_SIZE=10MB + ALLOWED_EXTENSIONS 白名单 | ✅ |
| M-6 | 附件路径拼接未规范化 | read/delete 加路径穿越检查（拒绝 .. / / \） | ✅ |
| M-9 | 未认证接口泄露账套路径 | SetupStatus.book skip_serializing，list_books 移除 path | ✅ |
| M-10 | parse_money 丢弃 Unicode 负号 | 增加 U+2212 和全角数字规范化 | ✅ |
| M-14 | argon2 失败静默降级 | 改为 panic，防止弱哈希落库 | ✅ |

## 待后续迭代项

### 需产品决策（2项）
- H-3: 余额/账簿/报表是否过滤未记账凭证
- M-15: 借贷平衡 round2 判定是否改为全精度

### 中优先级安全加固（6项）
- M-2: 登录失败 IP 限流（当前仅用户名维度）
- M-4: 设备绑定强化（客户端自报 device_id）
- M-7: 导入模块事务化

### 低优先级健壮性（10+项）
- L-1~L-14: Money 除零、Period 校验、死代码清理、CSV 注入防护等

## 测试验证

```bash
cargo test -p fincore --lib       # 87 passed
cargo test -p findb --lib         # 100 passed
cargo test -p finweb --test api   # 11 passed
cargo check -p finweb             # 编译通过
```

## 提交记录

| Commit | 说明 |
|--------|------|
| cd83371 | 数据导入（其他软件/CSV）+ 版本号定为 v1.0.0 |
| 9db11ba | 数据导入增强：金蝶/用友模板 + Excel 文件上传 |
| f557507 | 安全加固：修复审查报告中的高危与中危问题 |
| c063a8e | 安全加固：修复登录枚举漏洞 + 核销金额精度问题 |
| 599a28a | 安全加固：消除未认证接口信息泄露 |
| c54d13f | docs: 添加安全加固修复报告 |
| 4c7d2f9 | 安全加固：附件上传防护 + 锁定窗口修复 |

## 部署建议

1. **生产环境**：设置 `FINBOOK_LISTEN=0.0.0.0:8080` 并强制 HTTPS + `FINWEB_SECURE_COOKIE=true`
2. **管理员初始化**：首次登录后立即修改口令并设置 strong password policy
3. **定期审计**：检查 login_attempt 表监控异常登录
4. **备份策略**：启用 DATABASE_BACKUP_KEEP=7 环境变量
5. **附件限制**：MAX_FILE_SIZE 和 ALLOWED_EXTENSIONS 可按需调整

---
修复完成时间：2026-09-02
