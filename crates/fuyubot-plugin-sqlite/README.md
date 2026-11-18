# fuyubot-plugin-sqlite

FuyuBot 的 SQLite 服务插件。服务名 `fuyubot.sqlite`，给别的插件提供建表和读写能力。所有方法 JSON 进、JSON 出，值走参数绑定，SQL 里只有占位符。SQLite 以 bundled 方式编入，运行时不依赖系统库。

## 安装

```text
plugins/
└── fuyubot.sqlite/
    ├── plugin.json
    └── bin/fuyubot_plugin_sqlite.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`。启动后日志里会出现 `sqlite 服务已就绪：mode=per_plugin dir=…`，数据目录首次打开库时自动创建。

## 配置

配置段 `[plugin.fuyubot.sqlite]`，全部可选：

```toml
[plugin.fuyubot.sqlite]
mode = "per_plugin"       # per_plugin = 每个插件一个库；shared = 共用一个库
dir = "data/sqlite"       # 相对项目根（plugins/ 的上一级）
file = "fuyubot.sqlite3"  # shared 模式的库文件名
table_prefix = ""
journal_mode = "wal"
busy_timeout_ms = 3000
max_rows = 1000
max_sql_bytes = 65536
max_batch = 100
max_result_bytes = 8388608
read_only = false
```

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `mode` | `per_plugin` | 每个插件一个库，还是共用一个库 |
| `dir` | `data/sqlite` | 库文件放哪，相对项目根 |
| `file` | `fuyubot.sqlite3` | `shared` 模式下的库文件名 |
| `table_prefix` | 空 | 再加一层表前缀 |
| `journal_mode` | `wal` | 日志模式 |
| `busy_timeout_ms` | `3000` | 等锁最多多久 |
| `max_rows` | `1000` | 一次查询最多回多少行 |
| `max_sql_bytes` | `65536` | 单条 SQL 的字节上限 |
| `max_batch` | `100` | 一次批量最多多少条 |
| `max_result_bytes` | `8388608` | 一次返回的字节上限 |
| `read_only` | `false` | 只读打开，写操作直接报错 |

配置写错会让插件启动失败，日志里会给出具体原因。`read_only = true` 要求库文件已经存在，本插件不会为只读连接新建文件。

## 两种模式

| | `per_plugin`（默认） | `shared` |
| --- | --- | --- |
| 库文件 | `<dir>/<plugin>.sqlite3` | `<dir>/<file>` |
| 表名 | 调用方自己写 | 模板自动加 `<plugin>_` 前缀 |
| 跨插件查询 | 做不到 | 可以，但建议只查自己的前缀 |
| 适合 | 各业务数据分开 | 需要联表或单库备份 |

`shared` 模式下原始 SQL 不会被改写，调用方仍要自己带前缀；`migrate` 和 `tables` 会处理模板表。

## 方法

服务名 `fuyubot.sqlite`。成功返回 `{"ok": true, ...}`，失败返回 `{"ok": false, "error": {"code": "...", "message": "..."}}`。

`plugin` 可以省：服务会用宿主记录的真实调用方名字；显式写了就必须和调用方一致，否则返回 `caller_mismatch`。

| 方法 | 说明 |
| --- | --- |
| `info` | 当前模式、目录、限额、SQLite 版本和模板列表 |
| `templates` | 内置模板的名字、说明、默认表名和建表 SQL |
| `migrate` | 幂等应用模板，整批包在一个事务里 |
| `execute` | 执行一条写语句，返回 `changes` 和 `last_insert_rowid` |
| `query` | 只读查询，返回 `columns` 和 `rows` |
| `batch` | 一组写语句，整体一个事务，任一失败全部回滚 |
| `tables` | 列出当前库的表和列 |

内置模板有 `kv`、`log`、`counter`、`document`、`queue`，按名字直接选。

示例：

```json
{"plugin":"my.plugin","templates":["kv","log"]}
{"plugin":"my.plugin","sql":"INSERT INTO kv (key, value) VALUES (?1, ?2)","params":["greeting","你好"]}
{"plugin":"my.plugin","sql":"SELECT key, value FROM kv WHERE key = ?1","params":["greeting"],"limit":100}
```

`query` 返回 `{"ok":true,"columns":["key","value"],"rows":[["greeting","你好"]],"row_count":1,"truncated":false}`。`rows` 是按列顺序的数组。

## 参数与类型

`params` 可以是数组（位置参数，对应 `?` / `?1`）或对象（命名参数，对应 `:name` / `@name` / `$name`）。个数必须和语句需要的一致，值永远不会拼进 SQL。

| JSON | SQLite |
| --- | --- |
| `null` | NULL |
| `true` / `false` | 1 / 0 |
| 整数 | INTEGER |
| 浮点 | REAL |
| 字符串 | TEXT |
| `[0..255, …]` | BLOB |
| `{"$blob": "hex"}` | BLOB |
| `{"$json": …}` | JSON 文本 |

查询结果里 BLOB 变成 `{"$blob":"..."}`，非有限浮点变成 `{"$real":"nan|inf|-inf"}`。

## 安全

- 所有值走参数绑定，注入字符串只会被当成普通值。
- 一次只允许一条语句，多条写入用 `batch`。
- `ATTACH`、`DETACH`、`PRAGMA`、`VACUUM`、事务语句和 `load_extension` / `readfile` / `writefile` 一律拒绝。
- 表名列名只允许 `[A-Za-z_][A-Za-z0-9_]*`，插件名不许带路径分隔符，库文件跑不出数据目录。
- 这些防的是数据里的注入，不是恶意插件。同进程插件可以直接 dlopen，隔离靠约定。

## 注意

- 每个库一个连接，同一时刻只跑一条语句。单条慢查询会挡住同库其它调用，长任务请分页查、小批写。
- `migrate` 只管「表存在」，加字段、改类型要自己写 `ALTER TABLE`。
- `execute` 不支持 `INSERT ... RETURNING`，需要新行 id 用 `last_insert_rowid`。
- 插件停止时会关闭连接，Windows 上库文件不会被锁住。

## 来源与许可

服务接口按 FuyuBot ABI v6 编写，SQLite 来自 rusqlite 的 `bundled` 特性。

MIT OR Apache-2.0
