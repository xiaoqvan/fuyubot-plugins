# fuyubot-plugin-sqlite

FuyuBot 的 **SQLite 服务插件**（Rust，cdylib），服务名 `fuyubot.sqlite`，ABI v6。它自己不订阅
命令、不发消息：启动时把进程级服务注册到宿主，其它插件在 `start()` 里
`call_service("fuyubot.sqlite", …)` 就能建表、读写数据。所有方法 **JSON 进、JSON 出**，值永远
走参数绑定，SQL 文本里只有占位符；SQLite 以 bundled 方式编入，运行时不依赖系统库。

## 安装

发布页按平台提供 zip，解压到机器人的插件目录即可（一个插件一个目录），布局是
`plugins/fuyubot.sqlite/plugin.json` 加 `plugins/fuyubot.sqlite/bin/fuyubot_plugin_sqlite.dll`。

Windows 用 `fuyubot_plugin_sqlite.dll`，Linux / Android 用 `libfuyubot_plugin_sqlite.so`，
macOS 用 `libfuyubot_plugin_sqlite.dylib`。重启机器人后日志里会出现
`sqlite 服务已就绪：mode=per_plugin dir=<项目>/data/sqlite`；数据目录无需手建，首次打开库时创建。

## 配置

配置段是 `[plugin.fuyubot.sqlite]`（插件名点分 → TOML 嵌套表），全部可选：

```toml
[plugin.fuyubot.sqlite]
mode = "per_plugin"       # per_plugin = 每个调用方插件一个库（默认）；shared = 共用一个库
dir = "data/sqlite"       # 数据库目录，相对路径以「项目根」（plugins/ 的上一级）为基准
file = "fuyubot.sqlite3"  # shared 模式的库文件名；per_plugin 模式用 <plugin>.sqlite3
table_prefix = ""         # 两种模式都会叠加的额外表前缀（可空）
journal_mode = "wal"      # wal / delete / truncate / persist
busy_timeout_ms = 3000    # 拿不到锁时最多等多久
max_rows = 1000           # 单次 query 最多回多少行（硬上限，请求只能往小里调）
max_sql_bytes = 65536     # 单条 SQL 的字节上限
max_batch = 100           # 一次 batch / migrate 最多多少条
max_result_bytes = 8388608
read_only = false         # true = 以只读方式打开，所有写操作返回 read_only
```

配置写错（未知 `mode`、非法 `table_prefix`、越界数字）会让插件 start 失败，宿主日志给出具体
原因，不会静默退回默认值。`read_only = true` 要求库文件已存在：本插件不会为只读连接新建文件。

## 两种隔离模式

| | `per_plugin`（默认） | `shared` |
| --- | --- | --- |
| 库文件 | `<dir>/<plugin>.sqlite3` | `<dir>/<file>` |
| 表名 | 调用方自己写（模板默认 `kv` / `log`…） | 模板自动加 `<plugin>_` 前缀 |
| 跨插件查询 | 做不到（不同文件） | 可以，但建议只查自己的前缀 |
| 适合 | 各自的业务数据分开 | 需要联表 / 单库备份 |

`shared` 模式下原始 SQL 不会被改写：调用方仍要自己把前缀带上（`migrate` 与 `tables` 会替你
处理模板表）。两种模式的隔离都是**协作式**的，见「注意」。

## 服务方法

服务名 `fuyubot.sqlite`。成功 `{"ok": true, "...": "..."}`，失败
`{"ok": false, "error": {"code": "denied_sql", "message": "…"}}`。

**`plugin` 可以省**：ABI v6 起宿主在服务调用期间压「调用者栈」，服务用 `current_plugin()`
问得到真实调用方（插件伪造不了）——省了 `plugin` 就自动用调用者自己的名字。显式写了就必须与
调用者一致，否则返回 `caller_mismatch`；宿主自己调（没有调用者）时仍然要显式给 `plugin`。
插件名会校验能否安全地变成文件名 / 表前缀。

### `info`

入参 `{"plugin": "my.plugin"}`，返回当前模式、目录、限额、SQLite 版本、模板列表；带 `plugin`
时顺带给出该插件会用的库路径与表前缀。

### `migrate` / `templates`

`templates` 列出内置模板（名字、说明、默认表名、建表 SQL）。`migrate` 幂等应用模板
（`CREATE TABLE/INDEX IF NOT EXISTS`），整批包在一个事务里：

```json
{"plugin": "my.plugin", "templates": ["kv", "log"]}
{"plugin": "my.plugin", "tables": [{"template": "kv", "table": "settings"}]}
```

返回实际建出来的表名：`per_plugin` 模式就是 `kv`；`shared` 模式是 `my_plugin_kv`
（插件名里的非 `[A-Za-z0-9_]` 变成 `_`）。

| 名字 | 表结构 | 用途 |
| --- | --- | --- |
| `kv` | `key` PK / `value` TEXT / `updated_at` | 键值设置 |
| `log` | `id` / `level` / `message` / `created_at` + 时间索引 | 追加日志 |
| `counter` | `name` PK / `value` INT / `updated_at` | 自增序号、统计 |
| `document` | `key` PK / `doc` TEXT / `updated_at` | JSON 文档 |
| `queue` | `id` / `payload` / `status` / 时间列 + `(status,id)` 索引 | 待处理任务 |

模板 SQL 里的 `{table}` 是加引号的完整表名，`{name}` 是不加引号的完整表名（只用在索引名里），
两个占位符都只由服务渲染；调用方给的 `table` 必须先过标识符校验。

### `execute`

执行**一条**写语句（`INSERT` / `UPDATE` / `DELETE` / DDL 都行），成功返回
`{"ok": true, "changes": 1, "last_insert_rowid": 7, "database": "…"}`：

```json
{"plugin": "my.plugin", "sql": "INSERT INTO kv (key, value) VALUES (?1, ?2)", "params": ["greeting", "你好"]}
```

一次只允许一条语句，多条写入请用 `batch`（否则返回 `multiple_statements`）。`INSERT ... RETURNING`
不受支持：`execute` 会报「返回了结果」。需要新行的 id 用 `last_insert_rowid`，要别的返回值写入后
再查一次。

### `query`

只接受**只读语句**（`SELECT` / `WITH ... SELECT` / `EXPLAIN`），由 `sqlite3_stmt_readonly`
判定；写语句返回 `not_readonly`。

```json
{"plugin": "my.plugin", "sql": "SELECT key, value FROM kv WHERE key = ?1", "params": ["greeting"], "limit": 100}
```

```json
{"ok": true, "columns": ["key", "value"], "rows": [["greeting", "你好"]], "row_count": 1, "truncated": false}
```

`rows` 是**按列顺序的数组**（重名列不会互相覆盖）。`limit` 只能往小里调，上限是配置的
`max_rows`；触到行数或字节限额时会截断并置 `truncated: true`。

### `batch`

一组写语句，**整体一个 `IMMEDIATE` 事务**，任一失败全部回滚。

```json
{"plugin": "my.plugin", "statements": [
  {"sql": "INSERT INTO kv (key, value) VALUES (?1, ?2)", "params": ["a", "1"]},
  {"sql": "UPDATE counter SET value = value + 1 WHERE name = ?1", "params": ["hits"]}
]}
```

### `tables`

列出当前库里的表与列（`shared` 模式只报该插件前缀下的表）：`{"plugin": "my.plugin"}`。

## 参数与类型

`params` 有两种形状，**值永远不会被拼进 SQL**：

- **数组** → 位置参数，对应 `?` / `?1` / `?2`；个数必须与语句需要的参数个数**完全相等**
  （少给会被 SQLite 当成 `NULL`，所以服务直接报 `param_count`）。
- **对象** → 命名参数，对应 `:name` / `@name` / `$name`，键可带前缀也可不带。

| JSON | SQLite |
| --- | --- |
| `null` | NULL |
| `true` / `false` | 1 / 0 |
| 整数 | INTEGER（i64） |
| 浮点 | REAL（拒绝 NaN / inf） |
| 字符串 | TEXT |
| `[0..255, …]` | BLOB |
| `{"$blob": "hex"}` | BLOB |
| `{"$json": …}` | 序列化成 JSON 文本的 TEXT |

查询结果反过来：BLOB 变成 `{"$blob":"0102ff"}`，非有限浮点变成 `{"$real":"nan|inf|-inf"}`，
非 UTF-8 的 TEXT 也按 BLOB 回传（不丢字节）。

## 安全设计

- **参数绑定**：所有值走 `sqlite3_bind_*`。用 `x'); DROP TABLE kv; --` 或 `' OR '1'='1` 当值
  写入，表还在，查询也不会被绕开。
- **单语句 + 黑名单**：扫描器先把字符串 / 引号标识符 / 注释「抹成空格」，再按顶层 `;` 切，
  `SELECT ';'` 不会误切，`SELECT 1; DROP TABLE x` 会被拒；`ATTACH` / `DETACH`（读写任意文件）、
  `PRAGMA`、`VACUUM`（`VACUUM INTO` 写文件）、`BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT` /
  `RELEASE`（连接共享，要原子写请用 `batch`）一律 `denied_sql`。
- **危险函数**：`load_extension(` / `readfile(` / `writefile(` 一律拒，引号写法
  （`"load_extension"(…)`）也认；bundled SQLite 本身没开扩展加载，这是第二道防线。
- **连接加固**：`SQLITE_DBCONFIG_DEFENSIVE=ON`、`TRUSTED_SCHEMA=OFF`、关闭「双引号字符串字面量」
  兼容（`DQS_DML` / `DQS_DDL`）、`foreign_keys=ON`、`busy_timeout`。
- **标识符 / 路径 / 限额**：表名列名只允许 `[A-Za-z_][A-Za-z0-9_]*`（≤64 字节，不许 `sqlite_`
  前缀）；插件名不许带 `/`、`\`、`..`，库文件跑不出数据目录；SQL 字节数、参数个数、返回行数、
  返回字节数、batch 条数都有上限。

这些防的是**数据里的注入**，不是恶意插件——插件都是宿主进程里的原生代码，本就能绕过任何边界。

## 最小 Rust 调用示例

```rust
use fuyubot_abi::HostApi;

fn bootstrap(host: &HostApi) -> Result<(), String> {
    let call = |method: &str, input: &str| {
        host.call_service("fuyubot.sqlite", method, input)
            .map_err(|error| format!("{method} 失败：{error}"))
    };
    // 建表（幂等）
    call("migrate", r#"{"plugin":"my.plugin","templates":["kv"]}"#)?;
    // 参数化写入：值只走 params
    call("execute", r#"{"plugin":"my.plugin","sql":"INSERT INTO kv (key, value) VALUES (?1, ?2)","params":["greeting","你好"]}"#)?;
    // 参数化查询
    let reply = call("query", r#"{"plugin":"my.plugin","sql":"SELECT value FROM kv WHERE key = ?1","params":["greeting"]}"#)?;
    let value: serde_json::Value = serde_json::from_str(&reply)
        .map_err(|error| format!("服务返回的不是 JSON：{error}"))?;
    assert_eq!(value["rows"][0][0], "你好");
    Ok(())
}
```

在自己的 `soft_inject()` 里声明 `"fuyubot.sqlite"`，宿主会保证服务在被调用前已加载。

> 示例里保留 `plugin` 是为了让「读写的是谁的库」一眼可见；ABI v6 起它**可以省**，
> 服务会自动用调用者自己的名字。

## 注意

- **不是安全边界**：服务能做到「不把值拼进 SQL」「不让调用方改连接参数」「不越出数据目录」，
  但同进程插件可以直接 `dlopen`、调任意 C 函数。隔离靠约定，不靠权限。
- **`plugin` 写自己的名字**：ABI v6 起服务能向宿主核实调用者，冒用别人的名字会直接
  `caller_mismatch`（v5 时代只能靠自律）。仍然别把插件名做成用户可控的输入。
- **同一库串行执行**：每个库一个连接，同一时刻只有一条语句在跑（避免事务串台）。单条慢查询会
  挡住同库其它调用，`busy_timeout` 只管锁等待；长任务请分页查、小批 batch 写。
- **模板只管「表存在」**：schema 演进（加字段、改类型）要自己写 `ALTER TABLE`；`migrate` 幂等，
  可以每次启动都调。
- **停止时关闭连接**：插件停止会丢弃管理器，Windows 上库文件不会被锁住，可直接备份 / 复制。

## 来源与致谢

- 代码来源：本仓库自行实现，只提供通用 SQL 通道，不含业务语义。
- 服务接口按 FuyuBot ABI v6 编写，依赖运行时仓库提供的 `fuyubot-abi` crate（`define_plugin!`、
  `HostApi::call_service` 由它提供）；SQLite 本体来自 rusqlite 的 `bundled` 特性（随构建编入的
  sqlite3 C 源码），所以单文件分发即可。

## License

MIT OR Apache-2.0
