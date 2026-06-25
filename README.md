<div align="center">

# CC Switch（本地网关增强版）

</div>

> [!IMPORTANT]
> **本项目修改自 [farion1231/cc-switch](https://github.com/farion1231/cc-switch)**，并非原版。
> 感谢原作者 [@farion1231](https://github.com/farion1231) 的优秀工作，原项目官网：[ccswitch.io](https://ccswitch.io)。
> 本仓库在原版基础上做了大量裁剪与增强（见下方修改清单），目标是把 cc-switch 打造成一个更纯粹、更强的**本地桌面 LLM 网关**。如需原版功能与官方支持，请前往上游仓库。

一个用于管理 / 切换 Claude Code、Codex、Gemini CLI、OpenCode 供应商配置的桌面应用（Tauri 2 + React + Rust），内置本地代理网关，支持多 Key 池、通道级故障转移与会话亲和。

---

## 相对上游的主要修改

### ✂️ 精简

- **应用精简到四个**：移除 Claude Desktop / OpenClaw / Hermes 三个应用，聚焦 Claude Code / Codex / Gemini / OpenCode 四个核心 CLI
- **预设供应商列表精简**：四个应用的预设只保留**官方**与**自定义**入口（OpenCode 额外保留 OMO / OMO Slim 功能模板），聚焦自有供应商配置
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
- **限流感知调度**：尊重上游 `Retry-After` 精确冷却；瞬时 429 宽限多重试、不轻易下场，配额型 429 自动归入长冷却轨道；全部 Key 冷却时 503 响应附带 `Retry-After` 退避提示
- **请求头治理**：RFC 7230 hop-by-hop 头（含 `Connection` 点名字段）双向剥离；Anthropic 专属头不泄漏给非 Anthropic 上游；每个供应商可配置**自定义请求头规则**（覆盖 / 追加 / 删除，支持从 CSV 头中精确摘除单个 token，认证头受保护）
- **anyrouter 渠道适配**：自动注入 `context-1m` beta 与 adaptive thinking（Claude API），Codex API 遇 `invalid_responses_request` 自动剥除加密推理内容后重试（按渠道名或两个官方域名识别）
- **会话亲和**：同一会话记住上次成功的通道并优先复用，直到其失效或过期
- 代理转发链路重构：响应处理、错误分类与映射、流式处理统一收敛

### 🧭 新增：路由层模型映射

- **按客户端的模型映射引擎**：Claude / Codex / Gemini 各自维护一组「请求模型 → 目标上游模型」规则，支持**精确 / 前缀 / 后缀 / 关键词 / 正则**五种匹配，自上而下首条命中生效
- 命中后目标模型即为最终上游模型，**覆盖供应商的 catalog / 环境变量映射**；未配置规则时不影响现有行为
- 内置**匹配测试器**：输入请求模型名即时预览命中的规则与最终目标

### 📊 用量查询（下沉到 Key 级）

- 用量查询配置从供应商级**下沉到每个 Key**：每个 Key 独立配置查询脚本 / baseUrl / 凭证；供应商卡片展示该供应商所有 Key 用量的**聚合求和**
- **sub2api 中转自动探测**：添加 Key 时自动探测 `{{baseUrl}}/v1/usage`，命中 sub2api 结构即零配置自动启用用量查询（sub2api 无固定域名，靠实际探测识别，非 sub2api 静默跳过）
- **官方订阅自动启用**：新建 Claude / Codex / Gemini 官方供应商时自动启用订阅额度查询，无需手动配置；供应商卡片不再保留手动用量配置按钮（统一走 Key 级配置 + 自动探测）
- **计费精度修正**：上游 `prompt_tokens` 中的缓存命中 token 不再被重复计入 `input_tokens`（OpenAI / Responses → Anthropic 转换时扣除 cache 部分，并对已扣减的上游不二次扣减），同时记录每条请求实际用于计价的模型名

### 🛡️ 新增：隐私过滤（实验性）

- **进程内正则脱敏**：代理转发前，对请求体文本字段（Claude / OpenAI / Responses / Gemini 四种格式的 `system` / `messages` / `instructions` / `contents` 等，含 `tool_result` 嵌套）中的敏感信息打码，命中替换为 `[邮箱] [电话] [身份证] [银行卡] [IP] [密钥]`；全程本机完成，**不依赖任何外部服务 / 子进程 / 二进制**
- 覆盖邮箱 / 手机号（中国大陆）/ 身份证 / 银行卡（Luhn 校验）/ IP / API 密钥（常见前缀 + JWT + 私钥 + 高熵兜底）；各类可单独开关，设置页「代理 → 隐私过滤」含实时测试框
- **best-effort，永不阻断请求**：默认关闭；开启后命中即就地脱敏，检测失败也按原文放行
- ⚠️ **效果有限（实验性）**：纯正则对上下文语义无感知，存在**漏检与误报**（高熵兜底尤其可能误伤代码 / 哈希 / base64），难以可靠覆盖自然语言里的敏感信息。要做到更准的检测，后续大概率需要引入**本地小模型（轻量 NER / SLM）** 做语义级识别——当前正则方案仅作初步、轻量的兜底

### 🎨 界面调整

- **路由页重构**：以客户端（Claude / Codex / Gemini）为顶层维度，每个客户端下聚合「应用接管 / 模型映射 / 故障转移」；服务运行状态、启停、统计与故障转移总开关统一收敛到顶部服务区
- **通用页精简**：主页面显示应用改为 2×2 网格，左右两栏配置重新平衡
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
