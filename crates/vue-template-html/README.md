# vue-template-html

把 **Vue 3 的模板**编译成 HTML 字符串。纯 Rust：没有 Vue runtime、没有 Node.js、
没有 JavaScript 引擎、没有 DOM。面向服务端拼 HTML 与图片生成。

```rust
use vue_template_html::Template;
use serde_json::json;

let template = Template::compile(r#"<h1>{{ title }}</h1>"#)?;
let html = template.render(&json!({ "title": "你好" }))?;

assert_eq!(html, "<h1>你好</h1>");
# Ok::<(), vue_template_html::Error>(())
```

## 安装

```toml
[dependencies]
vue-template-html = { path = "../vue-template-html" }   # 与本仓库同处时的写法
serde_json = "1"
```

## 怎么用

### 编译一次，渲染多次

模板编译成 `Template`，之后每次渲染只做「求值 + 拼字符串」。`Template` 是 `Clone` 的，
可以跨线程共享；同一份模板换数据可以一直渲染。

```rust
use vue_template_html::Template;
use serde_json::json;

let template = Template::compile(
    r#"<div class="card" :class="{ featured }">
         <h1>{{ title }}</h1>
         <p v-if="user">{{ user.name }}</p>
         <p v-else>匿名访客</p>
         <ul>
           <li v-for="(item, i) in items" :key="item.id">{{ i + 1 }}. {{ item.name }}</li>
         </ul>
       </div>"#,
)?;

let data = json!({
    "title": "今日推荐",
    "featured": true,
    "user": { "name": "Alice" },
    "items": [{ "id": 1, "name": "咖啡" }, { "id": 2, "name": "可颂" }],
});

let html = template.render(&data)?;
assert!(html.contains("<h1>今日推荐</h1>"));
assert!(html.contains("1. 咖啡"));

// 换一份数据继续渲染，不用重新编译。
let empty = template.render(&json!({
    "title": "空列表", "featured": false, "user": null, "items": [],
}))?;
# Ok::<(), vue_template_html::Error>(())
```

模板只渲染一次时，可以直接用便捷函数：

```rust
let html = vue_template_html::render("<b>{{ n }}</b>", &serde_json::json!({ "n": 1 }))?;
```

### 数据来自 serde_json

渲染数据就是 `&serde_json::Value`，通常是一个对象：它的键就是模板里的变量名。
支持对象属性（`user.name`）、数组下标（`items[0].name`）、`length`（`items.length`）。

### 读取 `.vue` 文件里的 props

`Sfc::parse` 拆开 `<template>` / `<script>` / `<style>`，并读取 `defineProps` 声明的 props
（只读声明，**不执行** `<script>`）：

```rust
use vue_template_html::{Sfc, Template};

let source = std::fs::read_to_string("Help.vue")?;
let sfc = Sfc::parse(&source)?;

for prop in sfc.props()? {
    println!("{}: {:?} optional={} default={:?}",
        prop.name, prop.kind, prop.optional, prop.default);
}

// 按 props 的类型 / 默认值生成占位数据，先跑起来，再换成业务数据
let data = sfc.sample_data();
let html = Template::compile(sfc.template())?.render(&data)?;
```

认得的 `defineProps` 写法：内联类型字面量、同文件的 `interface Props` / `type Props`、
`withDefaults(..., { … })` 字面量默认值、运行时对象写法
（`defineProps({ title: String, count: { type: Number, default: 0 } })`）、数组简写。

### 选项

```rust
use vue_template_html::{CompileOptions, RenderOptions, UndefinedBehavior, Template};

let template = Template::compile_with_options(source, CompileOptions { strict: true })?;

let html = template.render_with_options(
    &data,
    &RenderOptions {
        undefined: UndefinedBehavior::Error, // 未定义变量报错，而不是当 null
        max_iterations: 1_000,               // 单个 v-for 的迭代上限
        max_depth: 64,                       // 嵌套深度上限
        strict: false,
        sanitizer: None,
        trusted_html: true,
    },
)?;
```

| 选项 | 默认 | 说明 |
| --- | --- | --- |
| `CompileOptions::strict` | `false` | 未知指令、属性里的 `{{ }}` 报错，而不是静默丢弃 |
| `RenderOptions::undefined` | `Lenient` | 未定义变量当 `null`（输出空串）；`Error` 则报错 |
| `RenderOptions::max_iterations` | `10_000` | 单个 `v-for` 的迭代上限，超出报 `Limit` |
| `RenderOptions::max_depth` | `128` | 元素 / 条件 / 循环的嵌套上限 |
| `RenderOptions::strict` | `false` | 渲染期也把未定义变量当错误 |
| `RenderOptions::sanitizer` | `None` | `v-html` 的清洗接口（`Sanitizer` trait） |
| `RenderOptions::trusted_html` | `true` | `v-html` 是否按可信 HTML 原样输出；`false` 则转义 |

渲染不可信模板前，把 `max_iterations` / `max_depth` 调小。也可以链式构造：

```rust
let options = RenderOptions::new().strict_undefined().max_iterations(1_000);
let html = template.render_with_options(&data, &options)?;
```

### 错误

所有错误都是 `vue_template_html::Error`，带 `kind`（`Parse` / `Expression` / `Render` /
`Limit` / `Sanitize` / `Unsupported`）、消息、字节偏移与行列号：

```text
表达式错误（第 1 行第 8 列）：插值表达式：表达式在 `+` 之后缺少操作数
```

## 能写什么

| 能力 | 写法 |
| --- | --- |
| 文本插值 | `{{ title }}`、`{{ user.name }}`、`{{ items[0].name }}`、`{{ i + 1 }}` |
| 条件渲染 | `v-if` / `v-else-if` / `v-else`，支持嵌套与 `<template>` 分支 |
| 循环 | `v-for="item in items"`、`(item, i)`、`(value, key)`、`(value, key, i)`、`v-for="i in 10"`；`in` / `of` 都认 |
| 原始 HTML | `v-html="html"` |
| 文本 | `v-text="text"`（总是转义） |
| 属性绑定 | `:src="url"`、`v-bind:width="w"`；布尔属性、`class` / `style` 特殊合并 |
| 显隐 | `v-show="flag"`（只加 / 不加 `display: none`，元素保留） |
| 跳过编译 | `v-pre`（整棵子树里的 `{{ }}`、指令原样输出） |
| 逻辑容器 | `<template v-if>` / `<template v-for>`（自身不输出标签） |
| 动态 class | 字符串 / 数组 / 对象，与静态 `class` 按顺序合并 |
| 动态 style | 字符串 / 数组 / 对象，camelCase → kebab-case |
| SVG | 标签名与属性名保留大小写（`viewBox`、`preserveAspectRatio`、`xlink:href`） |
| 原始文本 | `<script>` / `<style>` 内容不转义；`<textarea>` 支持插值 |

### 表达式

表达式由 Rust 自己解析、自己求值，**不执行任何 JavaScript**。能写的：

- 字面量：字符串、数字、`true` / `false` / `null` / `undefined`
- 变量、对象属性、数组下标、括号、`.length`
- 数组字面量 `['a', cls]`、对象字面量 `{ active: on, 'font-size': size + 'px' }`（支持简写）
- 一元 `!` `-` `+`；算术 `+ - * / %`；比较 `=== !== == != > < >= <=`；逻辑 `&& ||`；三元 `? :`
- 作用域链：`v-for` 的局部变量遮蔽外层同名变量，循环结束自动恢复
- `&&` / `||` 返回操作数本身并短路（`name || '匿名'` 可用）

## 刻意这么设计

这些行为是有意为之，不是待修的缺口：

- **没有 JS 运行时。** 表达式自己解析求值，不把字符串交给任何 JS 引擎——所以没有函数调用、
  没有 `Math` / `Date` 之类全局对象。需要在数据里先算好再传进来。
- **严格相等按结构比较。** JSON 里没有引用身份，数组 / 对象用 `===` 比的是内容；
  Vue 比的是引用。
- **`class` 不去重、`style` 不补 `px`。** 与 Vue 3 一致；要单位请自己写 `size + 'px'`。
- **空白保留。** 不做 Vue 的 `whitespace: 'condense'`，模板里的换行与缩进会留在输出里。
  对 HTML 渲染通常无影响，但字符串对比会多出空白。
- **属性顺序固定。** 静态属性在前、动态绑定其后、`class` / `style` 最后。与 HTML 语义无关。
- **`v-show` 只管自己写过的那份 `display`。** 不会主动抹掉静态 `style="display:none"`。
- **自闭合 `<div/>` 输出成 `<div></div>`。** HTML 里 non-void 元素不能自闭合。
- **孤立的 `v-else` 直接报错。** Vue 只给警告并按普通元素渲染，这里当作模板错误。
- **对象遍历顺序**默认按 `serde_json` 的键序；开 crate 的 `preserve_order` 特性后与 JS 的插入顺序一致。
- **`v-html` 默认可信。** 按 Vue 语义原样输出；内容不可信就挂 `Sanitizer` 或把
  `trusted_html` 设为 `false`。引擎不内置白名单——清洗策略差异太大，内置反而容易做错。

## 这些写法不能用

- `v-on` / `@event`、`v-model`、`v-slot` / `#slot`、`v-memo`（属性会被丢弃）
- 组件、`<component>`、`<teleport>`、`<suspense>`、`<keep-alive>`、过渡动画
- 自定义指令、`v-bind="object"` 对象展开、动态参数 `:[name]`、修饰符语义（`.prop` 之类只剥离不生效）
- `v-for` 遍历字符串 / 迭代器、别名解构 `({ id } in list)`
- 表达式里的函数调用、方法调用、`new`、赋值、箭头函数、模板字符串、正则、可选链、
  `??`、位运算、自增自减
- `v-html` 的内置清洗（请用 `Sanitizer`）
- 完整 HTML 实体表（内置常用命名实体 + 全部数字实体；生僻命名实体请写数字实体）

## 和图片生成配合

引擎只输出 HTML 字符串，接哪个渲染器都行。例如配合
[hyper-render](https://github.com/thomasmost/hyper-render)：

```rust
let html = template.render(&data)?;
let png = hyper_render::render_to_png(
    &html,
    hyper_render::Config::new().width(800).height(600).scale(2.0),
)?;
```

## 来源与致谢

- **语义参照**：[Vue 3](https://github.com/vuejs/core)（`compiler-core` / `runtime-core`，对照 3.5 的行为）。
  这里只有对模板语义的对照实现，代码是纯 Rust 重写，不包含 Vue 源码，也不在运行时依赖它。
- **解析 / 求值 / 序列化**：本仓库自行实现（手写 scanner + 递归下降 + 解释执行），
  没有引入第三方模板引擎，也没有借用任何 JS 引擎。
- **数据层**：只依赖 `serde_json`。

## License

MIT OR Apache-2.0。
