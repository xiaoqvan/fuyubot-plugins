# 本地 vendored fork

这是 [`thomasmost/hyper-render`](https://github.com/thomasmost/hyper-render) 的一个**本地副本**，
用于 `fuyubot-plugin-html2img`。不是发布包，`Cargo.toml` 里已设 `publish = false`。

## 来源

- upstream：https://github.com/thomasmost/hyper-render
- 合并的 PR：[#1 — Bump blitz to 0.3.0-alpha.4; add DataUrlNetProvider for @font-face; expose StyleThreading](https://github.com/thomasmost/hyper-render/pull/1)
- PR head（Kigo-Digital fork）：`9edbb75440e19e2aec13de585982f5cbb5b565e8`
- 同步时间：2026-11-08

## 外部 URL（对应 upstream PR #3）

upstream [PR #3 `feat(net): add remote resource loading with blitz-net`](https://github.com/thomasmost/hyper-render/pull/3)
用 `blitz-net` + `tokio` 抓远程资源。但 `blitz-net 0.2.1` 绑定 `blitz-traits 0.2`，与 PR #1 的
`blitz 0.3.0-alpha.4` 类型不兼容（`blitz-net 0.3.0-beta.4` 又要求 `blitz-traits =0.3.0-beta.4`），
两条 PR 无法同时按原样落地。

因此本 fork 在同一个 `NetProvider` 钩子上用 `ureq` 实现了等价的外部资源加载（`src/net.rs`）：

- 支持：`data:`（原有）+ **公网 `http(s)` 的图片/字体**；
- 不支持：`file://`、远程样式表（`<link rel=stylesheet>` / `@import`）、相对路径；
- 安全限制：仅 80/443、拒绝 loopback/私网/link-local/CGNAT/组播（含 DNS 解析结果）、
  每次跳转重新校验、最多 3 次跳转、8 MiB 上限、5 s 连接 / 10 s 读取超时、
  只接受 `image/*` / `font/*` 等 MIME；
- 代理：读取 `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY`（与 reqwest/curl 一致）。

## 相对 upstream PR head 的本地改动

### 打包 / 体积

1. `Cargo.toml` 增加 `publish = false`（避免同名误发 crates.io）；
2. 删除 `benches/`、`examples/`、`tests/`、`[dev-dependencies]` 与对应 `[[bench]]` /
   `[[example]]` 段（vendored 依赖不需要跑上游测试，减少仓库体积）；
   默认特性改为只开 `png`（本插件不发 PDF，避免 krilla/pdf 依赖进入构建）；
3. 删除 `.claude/`、`.gitignore`、`CLAUDE.md` 这些开发辅助文件。

### 兼容性修复（PR head 在当前依赖组合下不完整）

4. `Cargo.toml` 增加 `glifo = "=0.1.0"` pin：`vello_cpu 0.0.8` 的依赖范围写的是
   `glifo = "0.1"`，但 glifo 0.1.1 改了 `GlyphRenderer` trait，导致 vello_cpu 0.0.8
   编译失败。锁回它验证过的 0.1.0。
5. `src/lib.rs` 的 `create_document` 显式设置 `base_url = https://hyper-render.invalid/`：
   blitz 0.3.0-alpha.4 默认 base 是 `data:text/css;...`（cannot-be-a-base），
   `data:image/...` 甚至相对 `src` 都会在 `resolve_url` 里 panic；PR #1 只加了
   NetProvider，没有覆盖这里。这不改变网络行为：非 `data:` URL 仍然被忽略。

除第 5 条外，`src/` 与 PR head 一致。

## 升级方式

```bash
# 取上游 PR/分支
gh api repos/Kigo-Digital/hyper-render/tarball/<新的 commit> > /tmp/hr.tar.gz
tar -xzf /tmp/hr.tar.gz -C /tmp/hr --strip-components=1
# 只覆盖 src/ 与 Cargo.toml 的依赖段，再按上面的清单重做打包改动
```

PR 合并进 upstream 并发版后，应改回 crates.io 依赖并删掉本目录。
