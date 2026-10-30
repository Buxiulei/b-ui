# v3 golden fixtures
生成命令: scripts/gen-v3-fixtures.sh
v3 commit: 0b5b559（web/server.js 最后改动）
生成时间: 2026-09-11T14:39:13Z

## 2026-10-30 的 v4.2 改造（REALITY 443 + self-steal）

这些样本已按 v4.2 的渲染口径就地更新，再跑 `scripts/gen-v3-fixtures.sh` 会退回到 v3 形态：

- 两个 REALITY 节点端口 `10001/10002` → 同端口 `443`；
- UUID 改为 `routed_uuid(base, 0/1)`（第 6/7 字节 `0000`/`0001`，直连 / 住宅）；
- REALITY `serverName` / URI 的 `sni=`：`www.bing.com` → `example.com`（self-steal 后面板域名）。

其余字段（HY2 端口与跳跃、分流规则、TUN/DNS 段、计费注释头）与 v3 golden 逐字相同。
重新生成后请照上面三条再变换一次，否则 `golden_subscription` 会红。
