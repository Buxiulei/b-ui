# 住宅分流默认关键字表扩充（v3.6.1，R11）

- 日期：2026-09-11
- 状态：已批准（主理人：Claude / OpenAI / Gemini 全部相关站点走住宅；加谷歌支付域名；加 ippure 等住宅 IP 检测站）
- 文件：`server/residential-helper.sh`（`DEFAULT_DOMAINS` 唯一真源）、`docs/residential-proxy-guide.md`、相关测试

## 1. 实测依据（2026-09-11 于 bwg-tizi，经 Bright Data ISP zone HTTP 44445 逐域 HEAD 探测）

| 结论 | 域名 |
|---|---|
| 放行（出口 168.158.161.12） | api.123169.xyz、myip.ipip.net、cf.999831.xyz（ippure.com 网页后端）；openai.com 全家（api/auth/auth0/platform/cdn/help/pay）、chatgpt.com（ab/sora/operator）、sora.com、cdn.oaistatic.com、files.oaiusercontent.com；anthropic.com 全家、claude.ai、claude.com、claudeusercontent.com；gemini.google.com、aistudio.google.com、notebooklm.google.com、jules.google.com、labs.google、antigravity.google、idx.google.com、ai.google.dev、deepmind.google、makersuite.google.com、bard.google.com、clients6.google.com（alkalimakersuite-pa/waa-pa/signaler-pa/payments-pa）、accounts/myaccount/apis/ogs/play.google.com、pay/payments/wallet/one.google.com、*.googleapis.com（generativelanguage/cloudcode-pa/oauth2/firebase/aiplatform/fonts）、gstatic、googleusercontent；grok.com、x.ai、api.x.ai、copilot.microsoft.com、github.com、api.githubcopilot.com、cursor.com、api2.cursor.sh、perplexity.ai、mistral.ai、huggingface.co、replicate.com、api.together.xyz、groq.com、statsig.com、featuregates.org；ippure.com、my.ippure.com、ipquery.io、ipinfo.io、ip-api.com、ping0.cc、ip.sb、browserleaks.com、whoer.net、ipleak.net、scamalytics.com、ipqualityscore.com、ip2location.com、iplocation.net、whatismyipaddress.com、ipdata.co、ipapi.co、ipregistry.co、ip.skk.moe、ping.pe |
| **Bright Data 拒绝** | www.google.com（policy_20110，搜索）；checkout/js/api/m.stripe.com、www.paypal.com（policy_20050，支付处理商）；tiktok.com（policy_20050） |
| 无法解析（apex 无记录，非拒绝） | oaiusercontent.com、statsig.anthropic.com |

## 2. 决策

- 关键字用 sing-box `domain_keyword` / Clash `DOMAIN-KEYWORD` 子串语义，所以**不写裸 `google`**（会把被封的 www.google.com 也送进住宅），改按子域枚举。
- **Stripe / PayPal 不进表**：Bright Data 按策略拒绝支付处理商，进表反而让 ChatGPT Plus / Claude Pro 的付款页打不开；它们继续从 VPS 直出（现状）。谷歌支付域（pay/payments/wallet/one.google + payments-pa.clients6）放行，进表。
- `oai` 这种过宽子串换成 `oaistatic` + `oaiusercontent`；`x.ai` 用 `api.x.ai`（裸 `x.ai` 子串会误伤大量域名）。
- `tiktok` 保留在默认表（非 AI，但既有用户依赖）；Bright Data 用户会被它拒，指南注明可在面板删除。
- 老用户：`domains: null` 的服务器自动跟随新默认；自定义过的（如 tizi 的 30 条）需在面板「恢复默认」。

## 3. 新默认表（DEFAULT_DOMAINS，按组）

```
# OpenAI
openai chatgpt oaistatic oaiusercontent sora.com
# Anthropic
anthropic claude
# Google AI + 账号 + 支付（不含裸 google：www.google.com 被 Bright Data 封）
gemini.google aistudio generativelanguage makersuite notebooklm jules.google labs.google antigravity idx.google ai.google.dev deepmind cloudcode
googleapis gstatic googleusercontent clients6.google
accounts.google myaccount.google apis.google ogs.google
pay.google payments.google wallet.google one.google
# 其它 AI
grok api.x.ai githubcopilot cursor perplexity mistral cohere huggingface replicate together groq statsig featuregates
# 住宅 IP 检测站
ippure ipquery ipinfo ip-api ping0 ip.sb browserleaks whoer ipleak scamalytics ipqualityscore ip2location iplocation whatismyipaddress ipdata ipapi ipregistry ip.skk.moe ping.pe
# ippure.com 网页的后端域（2026-09-11 抓包：签名 API 在 api.123169.xyz，辅助 myip.ipip.net / cf.999831.xyz；经 Bright Data 放行）
123169 ipip.net 999831
# 既有
tiktok
```

## 4. 验收

1. `residential-helper.sh domains` 输出与上表一致（去重排序后计数一致）；`bash -n`。
2. 生成的中继与三个订阅生成器的关键字规则包含全部条目；三版 sing-box `check` 通过；Clash YAML 合法。
3. 既有测试中断言 31 条默认的地方改为新计数；`update.sh migrate_residential_keywords` 的 legacy-10/25 对比表不动（仍然只把这两张老表翻成 null）。
4. 指南：新增"默认表分组说明 + Bright Data 拒绝的三类（搜索/支付处理商/TikTok）"。
5. 主理人线上：rick（domains=null）更新后自动生效；tizi 面板「恢复默认」；全局模式关闭后 v2rayN 住宅节点测速正常，gemini.google.com / chatgpt.com / claude.ai 出口为住宅 IP。
