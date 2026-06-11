<div align="center">

# CC Switch（本地网关增强版）

</div>

> [!IMPORTANT]
> **本项目修改自 [farion1231/cc-switch](https://github.com/farion1231/cc-switch)**，并非原版。
> 感谢原作者 [@farion1231](https://github.com/farion1231) 的优秀工作，原项目官网：[ccswitch.io](https://ccswitch.io)。
> 本仓库在原版基础上做了大量裁剪与增强（见下方修改清单），目标是把 cc-switch 打造成一个更纯粹、更强的**本地桌面 LLM 网关**。如需原版功能与官方支持，请前往上游仓库。

一个用于管理 / 切换 Claude Code、Claude Desktop、Codex、Gemini CLI、OpenCode、OpenClaw、Hermes 供应商配置的桌面应用（Tauri 2 + React + Rust），内置本地代理网关，支持多 Key 池、通道级故障转移与会话亲和。

---

## 相对上游的主要修改

### ✂️ 精简

- **预设供应商列表精简**：七个应用的预设只保留**官方**与**自定义**入口（OpenCode 额外保留 OMO / OMO Slim 功能模板），聚焦自有供应商配置
- **移除 GitHub Copilot 接入**（含 Copilot 模型映射、请求优化器、配额展示）
- **移除 Codex OAuth 账号代理**（accounts 模式及配额页脚）
- **移除应用内自更新**（tauri-plugin-updater）与"关于"页的 CC Switch 版本检测区块，"关于"页聚焦本地 CLI 工具环境检查
- 移除便携模式检测等不再使用的代码路径

### 🔑 新增：供应商 Key 池

- 每个供应商可配置**多个 API Key** 组成池（独立 SQLite 表，不再受单一 `settingsConfig` 限制）
- Key 级管理界面：启用/停用、优先级、权重、复制、设为配置 Key、**清除冷却**、重置健康状态
- **添加供应商时可一次粘贴多个 Key**（空格 / 逗号 / 换行分隔），创建后自动组成 Key 池
- 直连配置使用最高优先级可用 Key，代理路由共享同一个池自动调度

### 🔀 增强：通道级故障转移与会话亲和

- 重试 / 熔断 / 冷却从"供应商级"细化到 **`provider:key` 通道级**，单 Key 失效不会拖垮整个供应商（指数退避冷却）
- **会话亲和**：同一会话记住上次成功的通道并优先复用，直到其失效或过期
- 代理转发链路重构：响应处理、错误分类与映射、流式处理统一收敛

### 🎨 界面调整

- "添加供应商"页面重排：预设选择器改为**卡片式布局**（图标 + 名称 + 说明），统一供应商入口同步统一样式
- Key 池入口融合进 API Key 输入区，展示池状态摘要
- 系统环境变量冲突检测横幅（ANTHROPIC_* 等变量会覆盖托管配置时提醒）

---

## 开发与构建

```bash
# 安装依赖
pnpm install

# 开发模式
pnpm dev

# 构建发布版
pnpm build        # 即 pnpm tauri build
```

> [!WARNING]
> 如果不走 tauri CLI、直接用 cargo 编译发布二进制，**必须**带上 `custom-protocol` feature，
> 否则二进制仍指向开发服务器 `http://localhost:3000`，启动后会白屏：
>
> ```bash
> cd src-tauri
> cargo build --release --features tauri/custom-protocol
> ```

技术栈：Tauri 2 / React 18 / TypeScript / Rust / SQLite。

- 前端类型检查：`pnpm typecheck`
- 单元测试：`pnpm test:unit`（前端）、`cargo test`（src-tauri）

## 数据与配置

- 应用数据库：`~/.cc-switch/cc-switch.db`（供应商、Key 池、代理状态、用量统计等）
- 接管 Claude Code 时改写 `~/.claude/settings.json`，退出 / 关闭代理时自动还原备份

## License

本项目沿用上游的 [MIT License](LICENSE)（Copyright (c) 2025 Jason Young）。
所有修改部分同样以 MIT 协议发布。

## 致谢

- 上游项目：[farion1231/cc-switch](https://github.com/farion1231/cc-switch) —— 本仓库的全部基础能力（多应用配置切换、托盘、代理接管框架、用量统计等）均来自上游
- [Tauri](https://tauri.app/)
