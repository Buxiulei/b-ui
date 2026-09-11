# b-ui 交接文档 v2（2026-09-11 下午）

写给**接手这个会话的下一个 agent**。读完这一份就能接着干，不需要回看聊天记录。
前一版交接（同日上午，TUN 修复线）的内容已并入本文；本文以本工作区 `Baiyi/bui-c-tun-mode-issue-df82e5` 分支、v3.6.3 为基准。

> 本文推到了公开仓库，所以**生产主机的 IP / 端口 / 面板域名全部用 SSH 别名代替**（`bwg-tizi` / `bwg-rick` / `baiyi`）。真实地址在本机长期记忆：`~/.claude/projects/-Users-woo-Desktop-b-ui/memory/{b-ui-servers,bwg-rick-config,baiyi-linux-client}.md`，以及 `~/.ssh/config`。

---

## 0. 一分钟速览

| 事项 | 状态 |
|---|---|
| bui-c TUN 打不开的 bug | ✅ 已修（v3.6.3）、已上 main、两台服务器已部署、客户端已验证（上午完成，见 §1.0） |
| 架构审计 · 读取阶段（7 个子系统 + 面板） | ✅ 完成，原始产物已落盘（§2） |
| 架构审计 · 反驳验证（78 条结论，106 个 agent） | ✅ 完成：**54 确认 / 22 部分成立 / 2 推翻**，判决已落盘 `docs/superpowers/audits/2026-09-11-verdicts.json` |
| 架构审计 · 中文汇总报告（报告→批评→修订） | ⏳ 会话结束时正在写。看 `docs/superpowers/audits/2026-09-11-architecture-audit.md` 是否存在；不存在按 §3 从 journal 恢复或重跑汇总 |
| 对抗 GFW 拓扑/协议/Rust 调研（6 路 Sonnet） | ✅ 报告已落盘 `docs/superpowers/research/2026-09-11-anti-gfw-topology-protocols.md`（**未经 Fable 审查**） |
| 静态住宅代理供应商调研（6 路 Sonnet） | ✅ 报告已落盘 `docs/superpowers/research/2026-09-11-static-residential-proxy-providers.md`（**未经 Fable 审查**，且摘要有一处内部矛盾，见 §1.4） |
| 住宅自动黑名单 | 设计已定稿；**并进 v4 一起做**（用户已拍板）；v3.6.x 草稿计划里的 update.sh D10 迁移块作废 |
| v4 架构方案（brainstorming） | 停在「探索上下文」末尾：三份输入（审计报告、GFW 调研、供应商调研）齐了就该提 2–3 个方案。**不要直接动代码** |

**下一个 agent 第一件事**：`git pull`，确认 §2 里的 3 份报告都在；缺哪份按 §3 恢复。然后按 §4 的顺序做。

---

## 1. 本会话做了什么

### 1.0 上午已上线的工作（沿用）

bui-c 开 TUN 必报「接口未就绪」的根因是 2026-03-20 commit `09b70f4` 删掉了 tun inbound 的 `interface_name`，sing-box 自动取名 `tun0`，而 v3.6.0 的就绪判定死等 `bui-tun`。修复：补回 `"interface_name": "bui-tun"`，`TUN_SCHEMA_VERSION` 7→8（`b-ui-client.sh:1358` 附近），已装客户端自动重生成配置。v3.6.3 = `a27d5b0`，两台服务器与 baiyi 客户端均已验证。

### 1.1 用户在本会话拍板的决策（不要再问一遍）

1. **子 agent 模型分工**：Fable 负责审查 / 设计方案 / 验收 / 分发任务；**Opus 执行**（写代码、部署）；**Sonnet 调研**（读码盘点、查文档、收集状态）；**Haiku 监控**。Workflow/Agent 调用按角色显式传 `model`。已写入记忆 `model-tiering-policy.md`。
2. **住宅自动黑名单并进 v4**，不按 v3.6.x 草稿单做。
3. **v4 方向**：从对抗 GFW 的拓扑/协议原理出发重新设计，追求「更聪明、更简洁、可依赖」；**核心代码尽量 Rust**，目标降延迟、提效率。
4. **Rust 范围**（用户在四选一里选的）：**控制面 + 客户端用 Rust**（面板 / 安装器 / 更新器 / 住宅出口控制器 / Linux 客户端）；**线上协议核心保留成熟实现**（REALITY / Hysteria2 等继续用 Xray / sing-box / hysteria，不自己实现协议），v2rayN 用户零影响。
5. **住宅上游要求**：真正的静态住宅 / ISP IP；限制要比 Bright Data 小——Bright Data 禁 Google 搜索（policy_20110）、支付/TikTok（policy_20050）、非白名单端口（5228 FCM / 5223 APNs），global 模式下每天几千条 403。
6. 沿用上午的 4 条：v4 允许两台服务器**重装**、迁移块整体删除；先单机做扎实、为多机留口；8 项能力全保留只砍实现冗余；**发版顺序 bwg-rick 先、用户验收后才动 bwg-tizi**。

以上全部已写进 `~/.claude/projects/-Users-woo-Desktop-b-ui/memory/bui-rollout-order.md`（v4 决策汇总）。

### 1.2 架构审计（三段式，前两段完成）

**读取（Sonnet ×7，~114 万 token，10 分钟）**：server-dataplane / update-migrations / linux-client / residential / history / datapath-efficiency / prod-state（只读 SSH 两台生产机）。面板那份用的是上午已落盘的产物。原始 JSON：`docs/superpowers/audits/2026-09-11-readers-raw-UNVERIFIED.json`。

**去重**：83 条待验证结论 → 78 条（合并 3 条跨组重复：singbox-converter / `/auth/hysteria` / 9998 端口；跳过 2 条纯描述）。清单：`docs/superpowers/audits/2026-09-11-claims.json`，其中 14 条标 `high_impact`，5 条带 `counter_claim`（读取阶段发现的三处矛盾）。

**反驳验证（Fable ×106）**：单条一人反驳；14 条高影响各派 3 个视角（静态调用图 / 运行时调用 / 历史+生产机实况）多数裁决。结果 `docs/superpowers/audits/2026-09-11-verdicts.json`：

| 判决 | 条数 | 备注 |
|---|---|---|
| CONFIRMED | 54 | 其中 v4 重装前提下可删的 45 条 |
| PARTIALLY | 22 | 结论方向对、细节有误，`correction` 字段写了真实情况 |
| REFUTED | 2 | `ctrl-C6`（「core.sh 装机已写静态 resolv.conf」）、`ctrl-C7`（「core.sh 装机已开 v3.5 端口」）——都被推翻，**数据面那边是对的**：`setup_static_dns()` 和 `configure_firewall()` 在 core.sh 里是**死代码**，静态 DNS 实际靠 update.sh 的 D 块在下一次 cron 才生效 |

**汇总报告里要盯的读取阶段错误**（验证已纠正，报告若没吸收要手改）：
- `ctrl-C18`：`npm install` 只在「有新版本」分支跑，**不是**每 6 小时都跑；
- `prod-C7`：bwg-tizi 磁盘多占的 ~3.6G 是 `/root/.vscode-server`，**不是** .bak 文件；
- `prod-C4`：update.sh 的 .bak 写入点是 8 处不是 11 处；
- `eff-C4`：10 秒流量循环是**唯一**持久化用量的地方，整体不算冗余；
- `data-C3`：`install_chinese_fonts()` 只判「部分成立」，看 verdicts 里的 correction；
- `eff-C5`：b-ui-relay 无 LimitNOFILE 判「部分成立」，看 correction 里生产机实测的 `/proc/<pid>/limits`。

**我自己核过的额外观察（未进报告，提方案时要用）**：
- 两台机 `singbox-relay.json` 的 `resi-pool` 都已是 `selector`，与仓库一致；但 bwg-tizi 直到今天 06:56 UTC 的 v3.6.3 更新才换上——**selector 是 v3.6.0 引入的，已装机器的 relay 配置没有跟着重生成，滞后了 1–2 个版本**。这是「生成器改了、既有机器不重生成」这一类问题的实例。
- 两台机 24h 内住宅 relay 几乎没见过微信流量（rick 0 条、tizi 1 条），被拒的全是 Apple / Google / FCM:5228。

### 1.3 v2rayN「微信图片加载不出来、上传中途失败」的调查状态

用户会话开头问的，中途被打断，**未收尾**。已排除「住宅上游拒掉微信」（见 1.2 末条）。剩下的主假设：本机 sing-box TUN 里那条活了 1m5s、一直报 `process DNS packet: unpack request: bad question name: dns: bad rdata / buffer size too small` 的 UDP 流，是微信的 UDP 传输被 `sniff` 误判成 DNS 后被 `hijack-dns` 劫持黑洞（仓库里 `b-ui-client.sh:1339-1342` 记录过 cloudflared QUIC 被同样误判的先例，修法是把该流在 sniff 规则之前直连）。**坐实只需一行证据**：在 v2rayN 日志里搜同一个连接号（如 `4281814682`），往上找 `inbound packet connection to <IP>:<端口>`。用户要看时再推进。

### 1.4 两份调研（Sonnet，未经审查）

- **对抗 GFW 调研**（✅ 完成，14/14 agent，~140 万 token）：6 个维度（GFW 检测手段 / 协议横评 / Rust 生态 / 拓扑模式 / 延迟与效率真相 / 客户端兼容矩阵）→ 缺口批评 + 6 条补查 → 中文报告 368 行、107 条带日期与置信度的参考文献。汇总 agent 自述的核心结论（**我没来得及审查**，下一个 agent 要核）：
  - REALITY+Vision 与 Hysteria2+Salamander+端口跳跃在**中国大陆均无被指纹化的一手证据**（Iran/Russia 的数据不能直接套用）→ 建议保持不变；不建议加国内中转层或迁到 CDN 前置（XHTTP 过 CF 有不稳定记录）。
  - 「Rust 化核心」**没有可验证的延迟收益**（RTT 由物理链路 / 拥塞控制 / 握手轮次决定，与语言无关）；唯一站得住的收益是内存可预测性。生产实测两台机内存远低于 Xray-core 已知泄漏阈值，但跑的 Xray **26.3.27 恰是被报告有 REALITY 连接泄漏的版本** → 上线前做 soak test。
  - `cfal/shoes` 是唯一同时实现 REALITY + Hysteria2 的 Rust 项目，但缺多用户/流量统计钩子、无 Salamander、生产规模负载未验证 → **不能直接替换现有内核**（与用户已选的「协议核心保留成熟实现」一致）。
  - 报告给出三档 Rust 范围的工作量估计、三个候选 v4 架构方向（保守 / 中等 / 最小改动）、以及可在 bwg-rick 上跑的一周 A/B 测试设计。
  - 核实了 b-ui 当前 Xray inbound 是纯 TCP REALITY（非 XHTTP）；4–5 份手工同步的节点集合逻辑是最值得优先修的技术债。
  - 报告自己列的 6 个未决问题（shoes 与 v2rayN/sing-box 的线上兼容、Rust 栈在 1 vCPU/1GB 下千用户实测、b-ui 当前 RSS/CPU 基线、REALITY 在大陆的一手检测报告、Hysteria2 行为指纹、sing-box/mihomo 的 XHTTP 支持进度）在报告 §9。
- **静态住宅供应商调研**（✅ 完成，14/14 agent，~140 万 token）：6 个维度（西方 ISP 代理商 / 中文市场供应商 / ToS 限制条款 / 定价 / IP 质量证据 / 与 b-ui 接入）→ 6 条补查 → 中文报告 316 行（31 家供应商总表、6 维加权评分、AUP 条款对照 Bright Data 基线、IP 来源三分类、成本估算 $3–30/月、bwg-rick 试测协议）。汇总 agent 自述结论（**未审查**）：
  - **Top 5**：Decodo（原 Smartproxy；官方 FAQ 明确放行 Google、Proxyway 实测欺诈分最低、SOCKS5 UDP 已验证）、Rayobyte（ASN 匹配率 88% 最高，无 UDP）、MarsProxies（机制/支付最好，口碑未核实）、Oxylabs（企业级，KYC 约 25% 拒绝率，对「给 1000 个下游用户转售」的用法有被拒风险）、Proxy-Seller（支付最友好、最便宜，无独立 ASN 审计）。
  - **避开**：NetNut 与 IPIDEA 全家族（922S5 / LunaProxy / PIA S5 等，Google / FBI 已证实为僵尸网络）、**CliProxy**（用户已试过；独立测试 ASN 是数据中心不是 ISP）、IPRoyal（Proxyway 实测仅 27% ASN 匹配、曾持有疑似未授权 Comcast 段）、Webshare（静态住宅 IP 被滥用库标记）。
  - ⚠️ **报告内部有矛盾要核**：IPRoyal 在「避开」名单里，但缺口问题又把 IPRoyal/Decodo 叫「强候选」并问它们是否白标 NetNut。下一个 agent 审查时先把这条理清。
  - 未决（子任务搜索预算耗尽）：多数供应商的 SOCKS5 UDP ASSOCIATE 未实测、OpenAI/Anthropic/Gemini 实际可达性未实测、IPRoyal/Decodo 是否白标 NetNut、CliProxy 与 IPIDEA 的关系、SOAX 能否解锁 5228/5223 端口。都只能靠买 1 个 IP 在 bwg-rick 上按报告 §8 试。

两份都要**先过 Fable 审查**再用：核对引用日期、把 anecdotal 和 measured 分开、剔掉营销话术。

---

## 2. 产物文件索引

| 路径 | 内容 | 可信度 |
|---|---|---|
| `HANDOVER.md` | 本文 | — |
| `docs/superpowers/audits/2026-09-11-web-panel-findings-UNVERIFIED.md` | 面板子系统读取产物（18 条待验证结论，已并入 claims） | 读取级 |
| `docs/superpowers/audits/2026-09-11-readers-raw-UNVERIFIED.json` | 其余 7 个子系统的结构化读取产物 | 读取级 |
| `docs/superpowers/audits/2026-09-11-claims.json` | 去重后的 78 条待验证结论（含 counter_claim / high_impact） | — |
| `docs/superpowers/audits/2026-09-11-verdicts.json` | **106 个反驳 agent 的判决**（3 视角的含 lenses[]） | **已验证** |
| `docs/superpowers/audits/2026-09-11-architecture-audit.md` | 中文汇总报告（8 节：拓扑图 / 每节点跳数 / 组件判定表 / 反复横跳 / 生产实况 / 千人评估 / 简化路线图 / 待决问题） | 若存在：Fable 写，仍需人核对 §1.2 列出的纠正 |
| `docs/superpowers/research/2026-09-11-anti-gfw-topology-protocols.md` | 对抗 GFW 调研报告（10 节，含参考文献） | Sonnet，未审查 |
| `docs/superpowers/research/2026-09-11-static-residential-proxy-providers.md` | 住宅供应商调研报告（10 节，含短名单与验证计划） | Sonnet，未审查；若不存在按 §3 恢复 |
| `docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md` | 黑名单设计（已定稿，v4 内实现） | 已批准 |
| `docs/superpowers/plans/drafts/2026-09-11-residential-auto-blacklist/` | 黑名单实施计划草稿（v3.6.x 版，**已作废**，仅供 v4 实现时参考探测/判定逻辑） | 草稿 |
| `~/.claude/projects/-Users-woo-Desktop-b-ui/memory/` | 长期记忆：服务器、发版顺序、v4 决策、模型分工、客户端主机 | — |

---

## 3. 工作流状态与恢复方法

Workflow 的 `resumeFromRunId` **只能同会话续跑**，新会话不能复用缓存；但每个 agent 的返回值都在 journal 里，可以直接抽。会话目录：`/Users/woo/.claude/projects/-Users-woo-Desktop-b-ui/3fdfa1bb-9856-4df7-a787-8fd9626a8acd/`

| 工作流 | Run ID | 状态（会话结束时） | journal |
|---|---|---|---|
| 审计·读取 | `wf_07b89f6f-e45` | 完成，已落盘 | `subagents/workflows/wf_07b89f6f-e45/journal.jsonl` |
| 审计·验证+汇总 | `wf_5c92ae58-07c` | 106 验证完成；`synthesize:report` → `critic:completeness` → `synthesize:revise` 进行中 | `subagents/workflows/wf_5c92ae58-07c/journal.jsonl` |
| 对抗 GFW 调研 | `wf_6dc202ce-f83` | ✅ 完成（14/14），报告已落盘 | `subagents/workflows/wf_6dc202ce-f83/journal.jsonl` |
| 住宅供应商调研 | `wf_35806c08-baf` | ✅ 完成（14/14），报告已落盘 | `subagents/workflows/wf_35806c08-baf/journal.jsonl` |

脚本都在 `/Users/woo/.claude/projects/-Users-woo-Desktop-b-ui--claude-worktrees-bui-c-tun-mode-issue-df82e5/3fdfa1bb-9856-4df7-a787-8fd9626a8acd/workflows/scripts/`：`architecture-audit-read-wf_07b89f6f-e45.js`、`architecture-audit-verify-synthesize-wf_5c92ae58-07c.js`、`anti-gfw-research-wf_6dc202ce-f83.js`、`static-residential-proxy-providers-wf_35806c08-baf.js`。

**从 journal 抽某个 agent 的返回值**：
```bash
J=<journal.jsonl>; python3 -c "
import json,sys; L={}
for l in open('$J'):
    e=json.loads(l)
    if e['type']=='started': L[e['agentId']]=e.get('label')
    elif e['type']=='result': print(L.get(e['agentId']), '=>', json.dumps(e['result'],ensure_ascii=False)[:300])"
```

**报告缺失时怎么办**：
- 审计报告缺失 → 用 `architecture-audit-verify-synthesize-*.js` 里 `phase('Synthesize')` 之后那三段 prompt（synth / critic / revise），把 `verdictsJson` 换成读取 `docs/superpowers/audits/2026-09-11-verdicts.json`，起一个只含汇总的小工作流（Fable，`effort: 'high'`）。不要重跑验证。
- 供应商报告缺失 → journal 里 6 个 `research:*` + `followup:*` 的结果都在，把它们喂给 `static-residential-proxy-providers-*.js` 的 synth prompt 即可（Sonnet）。
- 对抗 GFW 报告同理（但它已经落盘）。

---

## 4. 建议的下一步（有序）

1. **审查审计报告**（Fable）：逐条对照 `verdicts.json`——REFUTED 的两条不能以事实出现；§1.2 的 6 处纠正要吸收；补上 §1.2 末尾两条观察（relay 配置滞后、微信不走住宅）。
2. **审查两份调研**（Fable）：只保留 measured/documented 的结论进方案；协议横评里注意 Hysteria2 的 UDP QoS 风险与 REALITY 的现状；Rust 生态里确认 REALITY / Hysteria2 没有生产级 Rust 实现（这正是用户选「协议核心保留成熟实现」的理由）。
3. **先给用户看**：拓扑图 + 组件判定表 + 可删行数（用户明确要「从网络拓扑原理入手」），再谈方案。
4. **brainstorming 提 2–3 个 v4 方案**（superpowers:brainstorming，已在「探索上下文」末尾）。每个方案要写清：协议集（TCP/443 一条、UDP/QUIC 一条）、拓扑（几跳、几个进程、住宅出口是否还要本地 socks 那一跳）、Rust 覆盖面（控制面 + 客户端）、黑名单如何作为住宅模块的一部分、对 v2rayN / sing-box 客户端的兼容、主要风险、工作量。逐节呈现、逐节确认。
5. 方案获批 → 写 `docs/superpowers/specs/2026-09-1x-v4-architecture-design.md` → 用户 review → `superpowers:writing-plans`。
6. 供应商：让用户从短名单挑一家买 1 个 IP，按报告 §8 的验证计划在 **bwg-rick** 上试（面板加 `socks5://…`，跑 ippure / Google / AI 站 / 推送端口 / 24h 稳定性）。
7. 实施阶段严格：**bwg-rick 先跑通并交用户验收，通过后才动 bwg-tizi**；执行用 Opus，调研用 Sonnet，监控用 Haiku。

---

## 5. 生产环境与访问方式（别名）

| 别名 | 身份 | 说明 |
|---|---|---|
| `bwg-tizi` | **生产**（不可随意动） | 住宅 global 模式；v3.6.3；运行自 2026-05-09，有历史堆积（.bak、手工改动残留、`hysteria-residential.service.d/99-debug.conf` 调试覆盖使日志 19 倍于 rick） |
| `bwg-rick` | 测试机 | 住宅 global；v3.6.3；2026-06-03 重装过，干净 |
| `baiyi` | Linux 客户端 | 免密 sudo，bui-c 3.6.3（手工装的，`SCRIPT_VERSION` 用 sed 打进去），sing-box 1.13.19，TUN 开着 |

**SSH 坑**：从这台 Mac **直连 bwg-tizi 会在 banner 前被掐**（Mac 出口走 bwg-tizi 自己的住宅链路，上游封了非白名单端口）。**必须跳板**：`ssh -o BatchMode=yes -o ConnectTimeout=25 -J bwg-rick bwg-tizi '<cmd>'`。注意本机 Bash 工具的 shell 是 **zsh**，未加引号的变量不分词，`-J bwg-rick bwg-tizi` 别塞在一个变量里。

**跑 update.sh 的坑**：重启 hysteria 时 SSH 会被踢（退出码仍 0）。用 detached：`nohup env B_UI_NO_JITTER=1 bash /opt/b-ui/update.sh -y > /root/upd.log 2>&1 < /dev/null &`，然后轮询，最后复查 6 个服务 active。

**已知噪音**：bwg-tizi relay 每 24h 约 3.1 万条 `403 Forbidden` / `rejected`，目标是 `api.smoot.apple.com`、`gateway.icloud.com`、`www.google.com`、`init.push.apple.com`、大量 `<ip>:5228`——这就是黑名单要解决的。

---

## 6. 仓库状态

- 工作区：`/Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5`（git worktree）；主检出 `/Users/woo/Desktop/b-ui` 停在 v3.6.2 的 main，`git pull` 即可快进。
- 本次推送：分支 `Baiyi/bui-c-tun-mode-issue-df82e5` 与 `main` 同步到同一提交（纯文档，无代码改动，无需发版）。
- 推送前做了脱敏：本文与 `docs/superpowers/plans/drafts/2026-09-11-residential-auto-blacklist/section-C.md` 里的生产 IP/端口替换为别名。

---

## 7. 必须知道的坑与约定

- 项目、UI、commit 全中文；`fix(scope):` / `feat(scope):` / `bump: vX.Y.Z`；`version.json` 是版本唯一真源。
- 客户端 TUN 模板改动**必须** bump `TUN_SCHEMA_VERSION`；服务端 sing-box 订阅模板和客户端 TUN 模板是两个独立模板。
- 节点集合逻辑目前在 4 处重复（三个订阅生成器 + `app.js genUri`），改端口/标签/obfs 要同时改。
- 凭据绝不能上命令行（`ps` 泄漏），curl 代理凭据走 `-K -`。
- sing-box 配置要同时兼容 1.13 和 1.14，不用 `rule_set`。真实二进制（macOS arm64）在 `/private/tmp/claude-501/-Users-woo-Desktop-b-ui--claude-worktrees-bui-c-tun-mode-issue-df82e5/df6c5c3f-03e2-4ccb-b279-60c80612f9cf/scratchpad/sbbin/{1.12.0,1.13.0,1.14.0}/sing-box`，随时可能被清理。
- 本地验证：`bash -n install.sh server/*.sh b-ui-client.sh && node --check web/server.js && node --check web/app.js`。
- 别用裸 `git stash`（stash 栈与其他会话共享）。
- MCP：`gitnexus`、`plugin:github:github` 本会话连不上；GitHub 用 `gh` CLI。
- Workflow 监视：我用零 token 的 `Monitor` 轮询 journal 计数；grep `rate_limit` 会被面板源码里的 `RATE_LIMIT` 常量误中，**判断是否真限流看 `"result": null` 的数量**。

---

*本文由 Claude Fable 5.1 于 2026-09-11 15:30 前后写成（用户额度将尽时交接）。*
