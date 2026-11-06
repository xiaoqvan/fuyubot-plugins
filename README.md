# fuyubot-plugins

个人的 [FuyuBot](https://github.com/CatMoeCircle/Fuyu-Bot) 插件集合：**一个仓库，三个插件**。
每个插件是独立的 cdylib 动态库，按[插件分发格式 v1](https://catmoecircle.github.io/FuyuBot-docs/docs/plugin/bundle/)
各自打成一个 bundle zip 发布——一个仓库多插件，索引里各占一条、Release 里各挂各的资产。

| 插件 | id | 说明 |
| --- | --- | --- |
| [fuyubot-plugin-sqlite](crates/fuyubot-plugin-sqlite/) | `fuyubot.sqlite` | SQLite 存储服务：参数化查询、表结构模板、事务批量写入，按插件隔离库文件 |
| [fuyubot-plugin-mongo](crates/fuyubot-plugin-mongo/) | `fuyubot.mongo` | MongoDB 存储服务（`storage.mongo`）：集合级 CRUD、聚合、索引管理 |
| [fuyubot-plugin-adblock](crates/fuyubot-plugin-adblock/) | `fuyubot.adblock` | 广告拦截：关键词 / 文件 / 贴纸 / AI 审核，命中即删除、禁言或封禁；**硬依赖 `storage.mongo`** |

## 仓库布局

```text
fuyubot-plugins/
├── Cargo.toml            # workspace：三个插件 crate 的依赖与 profile 只写在这一处
├── crates/
│   ├── fuyubot-plugin-sqlite/    # 每个 crate 根部有自己的一份 plugin.json
│   ├── fuyubot-plugin-mongo/
│   └── fuyubot-plugin-adblock/
├── scripts/package.py    # 统一打包：cargo 产物 → bundle zip（--plugin 选目标）
└── .github/workflows/release.yml
```

## 依赖布局（重要）

`fuyubot-abi` / `fuyubot-simplify` 只存在于运行时仓库，其中 `fuyubot-simplify` 是
`publish = false`——所以本仓库用 **path 依赖**，要求与 FuyuBot checkout **并排**：

```text
<父目录>/
├── FuyuBot/            # 运行时仓库
└── fuyubot-plugins/    # 本仓库
```

换布局就改根 `Cargo.toml` 的 `[workspace.dependencies]` 里那两行（或改成 git 依赖）。

## 构建

```bash
cargo build --workspace          # 三个插件一起编
cargo test  --workspace          # 单测（含纯函数与 service 外壳）
```

⚠️ Windows 上 `ureq` → rustls 的 ring 要编 C 代码，MSVC 工具链必须和 vcvars 对上，
否则报 `cl : Command line error D8050`——仓库根上的 **`build.cmd`** 一键把环境对齐
（`build.cmd` = `cargo build --workspace`，`build.cmd test --workspace` 跑测试）；
原理与手工配方见 [adblock README](crates/fuyubot-plugin-adblock/README.md)「Windows 上的两个坑」。

## 打包发布

一个插件一个 zip（`<id>-<版本>-<平台>.zip`），平台口径与 `fuyubot-loader` 的
`current_platform()` 一致：

```bash
cargo build --release
python scripts/package.py --plugin fuyubot-plugin-sqlite --platform windows-x86_64 --version 0.1.0
python scripts/package.py --plugin fuyubot-plugin-mongo   --platform windows-x86_64 --version 0.1.0
python scripts/package.py --plugin fuyubot-plugin-adblock --platform windows-x86_64 --version 0.1.0
```

打 tag（`v*`）后 CI 自动全平台编译、打包并挂到 Release。商店索引照契约 §7 各插件
各写一条，`assets` 的 url 指向同一个 Release 里的不同 zip 即可。

## 安装

TUI 插件商店直接装，或手动把解压出来的目录放进机器人的插件目录：

```text
plugins/
├── fuyubot.sqlite/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_sqlite.dll
├── fuyubot.mongo/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_mongo.dll
└── fuyubot.adblock/
    ├── plugin.json
    └── bin/fuyubot_plugin_adblock.dll
```

`fuyubot.adblock` 的 `plugin.json` 声明了 `depends: ["storage.mongo"]`，宿主会等
`fuyubot.mongo` 先激活（mongo 插件缺席时 adblock 停在 Pending，不会静默失效）。

## 许可

MIT OR Apache-2.0。
