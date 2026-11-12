# fuyubot-plugin-mongo

FuyuBot 的 **MongoDB 服务插件**（Rust，cdylib），服务名 `storage.mongo`，ABI v6。它给**任意**
插件提供集合级 CRUD：不认识番剧，也不认识任何业务表。

```text
业务插件 ──call_service("storage.mongo", …)──▶ 本插件 ──官方 mongodb 驱动──▶ MongoDB
```

## 安装

发布页按平台提供 zip，解压到插件目录即可（一个插件一个目录），布局是
`plugins/fuyubot.mongo/plugin.json` 加 `plugins/fuyubot.mongo/bin/fuyubot_plugin_mongo.dll`。
Windows 用 `fuyubot_plugin_mongo.dll`，Linux / Android 用 `libfuyubot_plugin_mongo.so`，
macOS 用 `libfuyubot_plugin_mongo.dylib`。装好后重启机器人即可。

## 配置

配置写在宿主 `fuyubot.toml` 的 `[plugin.fuyubot.mongo]` 段里：

```toml
[plugin.fuyubot.mongo]
uri = "mongodb://user:pass@127.0.0.1:27017/?authSource=admin"
db = "anime"
timeout_ms = 10000
```

宿主 0.1.4 传给 `start()` 的是固定 JSON（`plugins_dir` / `scripts_dir` / `spawn`），
**不会**带插件配置段，所以插件在 `start()` 里用 `host.get_config("fuyubot.mongo", "")`
自己读一次；段里的键覆盖启动参数。**只认配置，不读环境变量。**
日志里的连接串会把密码打码（`mongodb://user:***@host`）。

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `uri` | `mongodb://127.0.0.1:27017` | 标准连接串，含副本集、SRV、账号密码；`mongodb+srv://` 由驱动自己解析 DNS |
| `db` | `appdata` | 库名，调用时可用 `db` 覆盖 |
| `timeout_ms` | `10000` | server selection + connect 超时（驱动默认 30 秒会把调用线程按住） |
| `app_name` | `fuyubot` | 连接串没带 `appName` 时补上，云控制台按它区分客户端 |

`start()` 会真连一次，配错了在启动日志里就能看到，不用等第一条业务查询。**连不上不拦启动**：
预热最多等 2 秒，超时就打一行
`[mongo] 没连上 …；插件照常运行，库起来后第一次查询会自动重连`，插件照常激活；真连不上的业务
调用各自返回错误。要确认「现在到底通不通」就调 `ping`，它按 `timeout_ms` 等结果。

业务插件也可以通过服务方法 `configure`（`{uri, db, timeout_ms}`）下发参数；已经建立过连接时返回
`applied: false`，此时只切换库名。

## 方法

入参都是 JSON，出参统一 `{"ok":true, ...}`；失败是 `{"ok":false,"error":"..."}`。
`collection` 必填，`db` 可选（覆盖默认库）。

| 方法 | 入参 | 说明 |
| --- | --- | --- |
| `ping` | – | 连通性 + 集合数 |
| `find` | `collection, filter?, projection?, sort?, limit?, skip?` | 上限 10 000 条 |
| `find_one` | `collection, filter?, projection?` | |
| `insert` | `collection, docs`（对象或数组） | 返回 `inserted` 与 `ids` |
| `update` | `collection, filter, update?/patch?, upsert?, many?` | `patch` 会自动包 `$set` |
| `delete` | `collection, filter, many?` | 空过滤会被拒绝（确实要清空传 `allow_empty:true`） |
| `count` | `collection, filter?` | |
| `aggregate` | `collection, pipeline` | 大集合用这个，服务端先筛 |
| `distinct` | `collection, field, filter?` | |
| `ensure_index` | `collection, keys, unique?, sparse?, name?, ttl_seconds?` | 幂等 |
| `collections` | `db?` | 列集合名 |
| `configure` | `uri, db, timeout_ms?` | 下发连接参数，见「配置」 |

示例：

```json
{"collection":"anime","filter":{"id":400602},"projection":{"name":1,"name_cn":1}}
{"collection":"torrents","patch":{"status":"done"},"filter":{"hash":"abc"},"upsert":true}
```

## 数据约定

- **BSON 走 relaxed Extended JSON**：`ObjectId` 回来是 `{"$oid":"..."}`，日期是 `{"$date":"..."}`，
  数字和字符串保持自然形态；回写时同样的形态会被识别成对应 BSON 类型，而不是普通子文档。
- **服务是同步的**：调用方（业务插件）不需要 async 传染，但别在 TDLib 更新回调线程里跑慢查询，
  一次重查询就会占住那个线程。

## 注意

- **`find` 有上限**：一次最多回 10 000 条，要更多用 `limit` + `skip` 分页；大集合优先
  `aggregate`，让服务端先筛再回。
- **`delete` 默认拒绝空过滤**：`{}` 要显式传 `allow_empty:true`，避免一次写错参数清库。
- **`update` 的 `patch` 会包成 `$set`**：要用 `$inc`、`$push` 等操作符就把完整 update 文档
  放进 `update`，不要再走 `patch`。
- **索引要自己建**：`ensure_index` 幂等，可以每次启动都调；唯一索引建在已有重复数据的集合上
  会失败，先清理重复再建。

## 来源与致谢

- 服务接口按 FuyuBot ABI v6 编写（服务调用经宿主，提供方用 `current_plugin()` 能看到真实
  调用方）；插件只做通用通道，业务语义（该查哪些字段、怎么排序）留在
  业务侧，换个业务不用改数据库插件。
- MongoDB 驱动用官方 `mongodb` crate，BSON 类型来自 `bson` crate。

## License

MIT OR Apache-2.0
