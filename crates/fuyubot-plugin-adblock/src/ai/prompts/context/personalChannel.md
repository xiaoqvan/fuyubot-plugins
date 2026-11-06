# PersonalChannel 规则

仅根据 PersonalChannel.name、PersonalChannel.description 判断。**主页挂载频道名称本身就是广告/引流载体。** 不得要求 description、价格、URL 或完整闭环才判广告。

---

## 普通个人频道（不是广告）

我的动漫频道、我的日常频道、朋友的频道、技术分享、游戏开黑。

---

## 强广告证据（可直接判定 Channel_ad = true）

频道名/描述出现以下语义时，即使 description 为空也可直接判定：

* 成人/色情/福利/吃瓜/深夜/私密资源/稀缺资源/内部资源
* 付费内容 / 会员专享 / 限时资源 / 解锁 / 车 / 吃瓜群
* 赌 / 博彩 / 彩票 / 开奖 / 上分 / 下分
* 代购 / 出售 / 招代理 / 收徒 / 带做 / 跑分 / 洗钱
* 赚钱 / 日赚 / 项目 / 兼职 / 招募（配合引流语境）
* 含 🈲🔞💰🎰💸 等暗示性 emoji 且与上述语义同现

| Channel name | 判定 |
| --- | --- |
| 我的动漫频道 / 游戏开黑 | false |
| 限时私密资源🈲 | **true**，成人/付费资源引流，confidence >= 0.90 |
| 深夜福利社 | **true**，confidence >= 0.90 |
| 吃瓜资源共享 | **true**，confidence >= 0.88 |
| 日赚项目交流 | **true**，confidence >= 0.90 |
| 代购/出售/接单 | **true** |

---

## message + PersonalChannel 组合引流（极重要）

message 若把用户导向「主页 / 频道 / 挂载 / 主页有 / 去主页 / 看主页」，且 PersonalChannel 名称含成人/付费/资源/福利/私密/限时等语义，则构成明确引流，**必须**：ad_detected = true，Channel_ad = true，r18 = 若频道含成人语义则 true，confidence >= 0.90。

### 反例（错误放行，禁止再犯）

message：“宝宝主页有好看的”，PersonalChannel.name：“限时私密资源🈲” —— 这不是“模糊描述”，而是引流话术（主页有好看的）+ 成人/付费资源频道名，必须判定为广告引流。

同类话术：“主页有好康”“主页自取”“去主页看”“资料在主页”“更多在主页频道”“宝宝主页有……”

---

## 字段隔离

Channel_ad 只反映 PersonalChannel 字段，不得把 profile.name / message 的内容直接复制到 Channel_ad；但**组合判定**时允许 message 引流话术与频道名语义共同升级 ad_detected 与 confidence。
