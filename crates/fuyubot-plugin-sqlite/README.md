# fuyubot-plugin-sqlite

FuyuBot 的 **SQLite 服务插件**（Rust，cdylib）。

它自己不订阅命令、不发消息：启动时把一个进程级服务 `fuyubot.sqlite` 注册到宿主，
其它插件在自己 `start()` 里 `call_service("fuyubot.sqlite", …)` 就能建表、读写数据。
所有方法都是 **JSON 进、JSON 出**，值永远走参数绑定。

```text
  call_service("fuyubot.sqlite", "query", {"sql": "SELECT …", "params": [...]})
        │
        ▼
  fuyubot.sqlite  ──▶  SQLite（bundled，无系统库依赖）
```

- 插件 id / 服务名：`fuyubot.sqlite`
- ABI：v5
- 产物：`fuyubot_plugin_sqlite.dll` / `libfuyubot_plugin_sqlite.so` / `.dylib`
- 默认数据目录：`<项目根>/data/sqlite/`（`per_plugin` 模式下一个插件一个 `.sqlite3`）

## 安装

发布页每个平台一个 zip，解压到机器人的插件目录即可（一个插件一个目录）：

```text
plugins/
└── fuyubot.sqlite/
    ├── plugin.json
    └── bin/fuyubot_plugin_sqlite.dll
```

Windows 用 `fuyubot_plugin_sqlite.dll`，Linux / Android 用 `libfuyubot_plugin_sqlite.so`，
macOS 用 `libfuyubot_plugin_sqlite.dylib`。装好后重启机器人，日志里会出现：

```text
sqlite 服务已就绪：mode=per_plugin dir=<项目>/data/sqlite
```

也可以从源码装：

```bash
cargo build --release
fuyubot plugin install target/release/fuyubot_plugin_sqlite.dll
```

## 配置

配置段是 `[plugin.fuyubot.sqlite]`（插件名点分 → TOML 嵌套表）。全部可选：

```toml
[plugin.fuyubot.sqlite]
# per_plugin = 每个调用方插件一个库文件（默认）
# shared     = 共用一个库文件，模板建的表自动加 <plugin>_ 前缀
mode = "per_plugin"

# 数据库目录。相对路径以「项目根」为基准（plugins/ 的上一级）；
# 默认 <项目根>/data/sqlite
dir = "data/sqlite"

# shared 模式的库文件名；per_plugin 模式用 <plugin>.sqlite3
file = "fuyubot.sqlite3"

# 两种模式都会叠加的额外表前缀（可空）
table_prefix = ""

journal_mode = "wal"      # wal / delete / truncate / persist
busy_timeout_ms = 3000    # 拿不到锁时最多等多久
max_rows = 1000           # 单次 query 最多回多少行（硬上限，请求只能往小里调）
max_sql_bytes = 65536     # 单条 SQL 的字节上限
max_batch = 100           # 一次 batch / migrate 最多多少条
max_result_bytes = 8388608
read_only = false         # true = 以只读方式打开，所有写操作报 read_only
```

配置写错（未知的 `mode`、非法 `table_prefix`、越界的数字）会让插件 **start 失败**，
并在宿主日志里打出具体原因——不会静默退回默认值。

## 协议

服务名：`fuyubot.sqlite`。所有方法都是 **JSON 进、JSON 出**。

成功：

```json
{"ok": true, "...": "..."}
```

失败：

```json
{"ok": false, "error": {"code": "denied_sql", "message": "…"}}
```

**每次调用都要带 `plugin`**：ABI 的 `ServiceMethodFn` 只有 `(context, input_json)`，
宿主不传「调用者是谁」。所以插件名由调用方自己声明（服务会校验它能不能安全地
变成文件名 / 表前缀）。同进程的原生插件本来就能绕过任何边界，这里的隔离是**协作式**的。

### `info`

看当前模式、目录、限额、SQLite 版本、模板列表；带 `plugin` 时顺带给出该插件会用的库路径与表前缀。

```json
{"plugin": "my.plugin"}
```

### `migrate` / `templates`

`templates` 列出内置模板（名字、说明、默认表名、建表 SQL）。`migrate` 幂等应用模板
（`CREATE TABLE/INDEX IF NOT EXISTS`），整批包在一个事务里。

```json
{"plugin": "my.plugin", "templates": ["kv", "log"]}
```

也可以给自定义表名：

```json
{"plugin": "my.plugin", "tables": [{"template": "kv", "table": "settings"}]}
```

返回实际建出来的表名（`per_plugin` 模式就是 `kv`；`shared` 模式是
`my_plugin_kv`——插件名里的非 `[A-Za-z0-9_]` 会变成 `_`）。

内置模板：

| 名字 | 表结构 | 用途 |
| --- | --- | --- |
| `kv` | `key` PK / `value` TEXT / `updated_at` | 键值设置 |
| `log` | `id` / `level` / `message` / `created_at` + 时间索引 | 追加日志 |
| `counter` | `name` PK / `value` INT / `updated_at` | 自增序号、统计 |
| `document` | `key` PK / `doc` TEXT / `updated_at` | JSON 文档 |
| `queue` | `id` / `payload` / `status` / 时间列 + `(status,id)` 索引 | 待处理任务 |

模板 SQL 里的 `{table}` 会被替换成加引号的完整表名，`{name}` 是不加引号的完整表名
（只用在索引名里）。两个值都只由服务渲染，调用方给的 `table` 必须先过标识符校验。

### `execute`

执行**一条**写语句（`INSERT` / `UPDATE` / `DELETE` / DDL 都行）。

```json
{"plugin": "my.plugin", "sql": "INSERT INTO kv (key, value) VALUES (?1, ?2)", "params": ["greeting", "你好"]}
```

```json
{"ok": true, "changes": 1, "last_insert_rowid": 7, "database": "…"}
```

- 一次只允许一条语句；多条写入请用 `batch`（否则返回 `multiple_statements`）。
- 不允许 `ATTACH` / `DETACH` / `PRAGMA` / `VACUUM` / 事务控制语句。
- `INSERT ... RETURNING` 不支持：`execute` 会报「返回了结果」，`query` 只收只读语句。
  需要新行的 id 用返回里的 `last_insert_rowid`，需要其它返回值就再查一次。

### `query`

只接受**只读语句**（`SELECT` / `WITH ... SELECT` / `EXPLAIN`），由
`sqlite3_stmt_readonly` 判定；写语句返回 `not_readonly`。

```json
{"plugin": "my.plugin", "sql": "SELECT key, value FROM kv WHERE key = ?1", "params": ["greeting"], "limit": 100}
```

```json
{
  "ok": true,
  "columns": ["key", "value"],
  "rows": [["greeting", "你好"]],
  "row_count": 1,
  "truncated": false
}
```

`rows` 是**按列顺序的数组**（重名列不会互相覆盖）。`limit` 只能往小里调，
上限是配置的 `max_rows`。

### `batch`

一组写语句，**整体一个 `IMMEDIATE` 事务**，任一失败全部回滚。

```json
{"plugin": "my.plugin", "statements": [
  {"sql": "INSERT INTO kv (key, value) VALUES (?1, ?2)", "params": ["a", "1"]},
  {"sql": "UPDATE counter SET value = value + 1 WHERE name = ?1", "params": ["hits"]}
]}
```

### `tables`

列出当前库里的表与列（`shared` 模式只报该插件前缀下的表）。

```json
{"plugin": "my.plugin"}
```

## 参数与类型

`params` 有两种形状，**值永远不会被拼进 SQL**：

- **数组** → 位置参数，对应 `?` / `?1` / `?2`。个数必须与语句需要的参数个数**完全相等**
  （少给会被 SQLite 当成 `NULL`，所以服务直接报 `param_count`）。
- **对象** → 命名参数，对应 `:name` / `@name` / `$name`；键可以带前缀，也可以不带。

JSON 类型映射：

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

查询结果反过来：BLOB 变成 `{"$blob":"0102ff"}`，非有限浮点变成
`{"$real":"nan|inf|-inf"}`，非 UTF-8 的 TEXT 也按 BLOB 回传（不丢字节）。

## 防注入做了什么

1. **参数绑定**：所有值走 `sqlite3_bind_*`；SQL 文本里只有占位符。测试里用
   `x'); DROP TABLE kv; --` 和 `' OR '1'='1` 当值写入，表还在、查询也不会被绕开。
2. **单语句**：先用一个小扫描器把字符串 / 引号标识符 / 注释「抹成空格」，
   再按顶层 `;` 切；`SELECT ';'` 不会被误切，`SELECT 1; DROP TABLE x` 会被拒。
3. **危险语句黑名单**：`ATTACH`（读写任意文件）、`DETACH`、`PRAGMA`、
   `VACUUM`（`VACUUM INTO` 写文件）、`BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT` /
   `RELEASE`（连接是共享的，谁开事务谁负责收尾——要原子写用 `batch`）。
4. **危险函数**：`load_extension(` / `readfile(` / `writefile(` 一律拒；
   引号写法（`"load_extension"(…)`）也认。bundled SQLite 本身没有开启扩展加载，
   这是第二道防线。
5. **只读判定**：`query` 用 SQLite 自己的 `sqlite3_stmt_readonly` 判断，
   不靠「首关键字是不是 SELECT」这种启发式。
6. **连接加固**：`SQLITE_DBCONFIG_DEFENSIVE=ON`、`TRUSTED_SCHEMA=OFF`、
   关闭「双引号字符串字面量」兼容（`DQS_DML` / `DQS_DDL`）、`foreign_keys=ON`、
   `busy_timeout`；扩展加载从未启用。
7. **标识符与路径校验**：表名 / 列名只允许 `[A-Za-z_][A-Za-z0-9_]*`（≤64 字节，
   不许 `sqlite_` 前缀）；插件名不许带 `/`、`\`、`..`，防止库文件跑出数据目录。
8. **限额**：SQL 字节数、参数个数、返回行数、返回字节数、batch 条数都有上限，
   避免一次调用把宿主拖死。

## 两种隔离模式

| | `per_plugin`（默认） | `shared` |
| --- | --- | --- |
| 库文件 | `<dir>/<plugin>.sqlite3` | `<dir>/<file>` |
| 表名 | 调用方自己写（模板默认 `kv` / `log`…） | 模板自动加 `<plugin>_` 前缀 |
| 跨插件查询 | 做不到（不同文件） | 可以，但建议只查自己的前缀 |
| 适合 | 各自的业务数据分开 | 需要联表 / 单库备份 |

`shared` 模式下原始 SQL 不会被改写：调用方仍要自己把前缀带上（模板与 `tables`
会替你做）。隔离是**协作式**的——见下一节。

## 边界与注意

- **不是安全边界**：插件都是宿主进程里的原生代码，能直接 `dlopen`、能调任意 C 函数。
  服务能做的是「不把值拼进 SQL」「不让调用方改连接参数」「不越出数据目录」，
  防的是**数据里的注入**，不是防恶意插件。
- **ABI 没有调用者身份**：所以 `plugin` 必须显式传；服务无法验证你是不是那个插件。
  冒用名字会读写到对方的库——这是 ABI 的现状，不是本插件能补的。
- **串行执行**：每个库一个连接 + 一个 `Mutex`，同一时刻只有一条语句在跑。
  这是为了不让事务串台；单条慢查询会挡住同库的其它调用（`busy_timeout` 只管锁等待）。
- **`read_only = true`**：库文件必须已经存在；`migrate` / `execute` / `batch` 会失败。
- **不自动做 schema 演进**：模板只负责「表应该存在」，改字段要自己写 `ALTER TABLE`。
- **插件卸载前会关连接**（`stop()` 里丢弃管理器），Windows 上库文件不会被锁住。

## 最小 Rust 调用示例

```rust
use fuyubot_abi::{HostApi, Plugin};

struct MyPlugin {
    host: HostApi,
}

impl Plugin for MyPlugin {
    fn init(host: HostApi) -> Result<Self, String> {
        Ok(Self { host })
    }

    fn start(&mut self, _config: &str) -> Result<(), String> {
        // 建表（幂等）
        self.host
            .call_service(
                "fuyubot.sqlite",
                "migrate",
                r#"{"plugin":"my.plugin","templates":["kv"]}"#,
            )
            .map_err(|error| format!("建表失败：{error}"))?;

        // 参数化写入：值只走 params
        self.host
            .call_service(
                "fuyubot.sqlite",
                "execute",
                r#"{"plugin":"my.plugin","sql":"INSERT INTO kv (key, value) VALUES (?1, ?2)","params":["greeting","你好"]}"#,
            )
            .map_err(|error| format!("写入失败：{error}"))?;

        // 参数化查询
        let reply = self
            .host
            .call_service(
                "fuyubot.sqlite",
                "query",
                r#"{"plugin":"my.plugin","sql":"SELECT value FROM kv WHERE key = ?1","params":["greeting"]}"#,
            )
            .map_err(|error| format!("查询失败：{error}"))?;
        let value: serde_json::Value = serde_json::from_str(&reply)
            .map_err(|error| format!("服务返回的不是 JSON：{error}"))?;
        assert_eq!(value["rows"][0][0], "你好");
        Ok(())
    }

    fn soft_inject() -> Vec<String> {
        vec!["fuyubot.sqlite".to_owned()]
    }
}

fuyubot_abi::define_plugin!(MyPlugin, "my.plugin");
```

## 构建与发布

依赖只有一个 FuyuBot crate：`fuyubot-abi`，从 **crates.io** 取（`fuyubot-abi = "0.1.2"`，
ABI v5 就在这个版本里）。CI 的 runner 上没有运行时源码树，所以这里不能用 path 依赖。

```bash
cargo build --release
cargo test  --offline          # 33 passed
cargo fmt --all --check        # 0 diff
```

单个平台打 bundle 包：

```bash
python scripts/package.py --plugin fuyubot-plugin-sqlite --platform windows-x86_64 --version 0.1.0
# → dist/fuyubot.sqlite-0.1.0-windows-x86_64.zip
```

推 `v*` tag 时 `.github/workflows/release.yml` 会全平台编一遍并挂 Release：

| 平台 | runner |
| --- | --- |
| `windows-x86_64` | `windows-latest` |
| `windows-aarch64` | `windows-11-vs2026-arm` |
| `linux-x86_64` | `ubuntu-latest` |
| `linux-aarch64` | `ubuntu-24.04-arm` |
| `macos-aarch64` | `macos-14` |
| `android-aarch64` / `android-x86_64` | `ubuntu-latest` + NDK r27c + `cargo-ndk` |

Android 那条单独一个 job：bundled SQLite 是 C 源码，得靠 `cargo ndk` 把 NDK 的
clang 塞进 `CC` / `AR`，交叉编译才会走到正确的 sysroot。

## 测试

```bash
cargo test --offline
```

覆盖配置解析、SQL 扫描器（单语句 / 黑名单 / 危险函数 / 标识符）、参数绑定与类型往返、
限额、两种隔离模式、事务回滚。

> ⚠️ 那些用例**故意不用系统 `TEMP`**：临时目录一律放 `target/test-tmp`
> （`std::env::temp_dir()` 在 MSYS / 受限环境里会指到写不了的地方，一批测试会莫名全红）。
