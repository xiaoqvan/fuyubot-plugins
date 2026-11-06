# Mentioned Username 规则

mentioned_username 是弱证据，但必须结合上下文，不要只看用户名本身。

---

## 普通 @用户名

例如 “你好 @abc”：username_ad = false，ad_detected = false。

---

## 商业/交易上下文

例如：“出售游戏账号 @abc”“兼职赚钱 @abc”“购买联系 @abc”“代理项目 @abc”“有微信支付宝来赚钱 @abc” → username_ad = true，并参与广告判定。这是“获利/交易语义 + 具体入口”的组合升级，不要求再出现价格或 URL。

---

## 成人上下文

例如：“激情爆操 @abc”“成人服务 @abc”“约炮 @abc” → username_ad = true，因为该用户名承担了成人引流入口的作用。

---

## 非常重要：@用户名不需要本身包含广告关键词

例如 “激情爆操 @wpdta”：“激情爆操” = 成人服务/成人行为语义，“@wpdta” = 主动指向具体 Telegram 用户，两者组合即成人语义 + 具体用户入口，属于明显成人引流。必须：ad_detected = true，r18 = true，confidence >= 0.90。

---

## 关键原则

不要判断 “@abc 是不是广告账号？”，而应该判断 “@abc 在当前消息中是否被用于广告/交易/成人引流？”

若 type=user/channel 且 name/bio/PersonalChannel 展示出商业推广、交易、博彩、成人服务等明确语义，也可将 username_ad 置为 true。
