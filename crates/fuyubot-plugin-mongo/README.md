# fuyubot-plugin-mongo

FuyuBot 的 MongoDB 服务插件。服务名 `storage.mongo`，只提供集合级读写，不认识任何业务表。别的插件通过 `call_service("storage.mongo", ...)` 调用。

```text
业务插件 ──call_service("storage.mongo", …)──▶ 本插件 ──▶ MongoDB
```

## 安装

```text
plugins/
└── fuyubot.mongo/
    ├── plugin.json
    └── bin/fuyubot_plugin_mongo.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`。装好后重启机器人。

## 配置

配置写在 `fuyubot.toml` 的 `[plugin.fuyubot.mongo]` 段：

```toml
[plugin.fuyubot.mongo]
uri = "mongodb://user:pass@127.0.0.1:27017/?authSource=admin"
db = "anime"
timeout_ms = 10000
```

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `uri` | `mongodb://127.0.0.1:27017` | 连接串，支持副本集 / SRV / 账号密码 |
| `db` | `appdata` | 默认库名，调用时可以覆盖 |
| `timeout_ms` | `10000` | 选服务器和建连的超时 |
| `app_name` | `fuyubot` | 云控制台里显示的客户端名 |

只认配置，不读环境变量；日志里的连接串会把密码打码。

启动时会试连一次，连不上不拦启动，只打一行日志，插件照常激活，库起来后第一次查询会自动重连。真要确认通不通，调 `ping`。

## 方法

入参都是 JSON，成功返回 `{"ok": true, ...}`，失败返回 `{"ok": false, "error": "..."}`。`collection` 必填，`db` 可选。

| 方法 | 入参 | 说明 |
| --- | --- | --- |
| `ping` | – | 连通性和集合数 |
| `find` | `collection, filter?, projection?, sort?, limit?, skip?` | 一次最多 10000 条 |
| `find_one` | `collection, filter?, projection?` | |
| `insert` | `collection, docs` | 对象或数组，返回 `inserted` 和 `ids` |
| `update` | `collection, filter, update?/patch?, upsert?, many?` | `patch` 会自动包 `$set` |
| `delete` | `collection, filter, many?` | 空过滤默认拒绝 |
| `count` | `collection, filter?` | |
| `aggregate` | `collection, pipeline` | 大集合用这个，让服务端先筛 |
| `distinct` | `collection, field, filter?` | |
| `ensure_index` | `collection, keys, unique?, sparse?, name?, ttl_seconds?` | 幂等 |
| `collections` | `db?` | 列出集合名 |
| `configure` | `uri, db, timeout_ms?` | 下发连接参数 |

示例：

```json
{"collection":"anime","filter":{"id":400602},"projection":{"name":1,"name_cn":1}}
{"collection":"torrents","patch":{"status":"done"},"filter":{"hash":"abc"},"upsert":true}
```

## 注意

- BSON 走 relaxed Extended JSON：`ObjectId` 是 `{"$oid":"..."}`，日期是 `{"$date":"..."}`。
- `find` 一次最多 10000 条，要更多用 `limit` + `skip` 分页。
- `delete` 传空过滤会被拒绝，确实要清空请加 `allow_empty: true`。
- 要用 `$inc`、`$push` 这类操作符就把完整 update 文档放进 `update`，不要走 `patch`。
- 索引要自己建，`ensure_index` 可以每次启动都调。
- 服务是同步的，别在 TDLib 更新回调线程里跑慢查询。

## 来源与许可

服务接口按 FuyuBot ABI v6 编写，MongoDB 驱动用官方 `mongodb` crate，BSON 类型来自 `bson` crate。

MIT OR Apache-2.0
