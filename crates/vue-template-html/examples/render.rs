//! 把 Vue 模板渲染成 HTML，并打印出来。
//!
//! 运行：
//!
//! ```bash
//! cargo run --example render
//! ```

use serde_json::json;
use vue_template_html::Template;

fn main() {
    let source = r#"
<div class="card" :class="{ featured: featured }" :style="{ padding: gap + 'px' }">
  <h1>{{ title }}</h1>
  <p v-if="user">{{ user.name }} · {{ user.role }}</p>
  <p v-else>匿名访客</p>

  <ul>
    <li v-for="(item, index) in items" :key="item.id">
      {{ index + 1 }}. {{ item.name }}<span v-if="item.hot"> 🔥</span>
    </li>
  </ul>

  <div v-show="items.length">共 {{ items.length }} 项</div>
  <div v-html="note"></div>
</div>
"#;

    let template = Template::compile(source).expect("模板应能编译");

    let data = json!({
        "title": "今日推荐",
        "featured": true,
        "gap": 16,
        "user": { "name": "Alice", "role": "admin" },
        "items": [
            { "id": 1, "name": "咖啡", "hot": true },
            { "id": 2, "name": "可颂", "hot": false },
        ],
        "note": "<em>图片由 hyper-render 生成</em>",
    });

    let html = template.render(&data).expect("渲染应成功");
    println!("{html}");

    // 同一个模板可以换一份数据再渲染：编译只发生一次。
    let other = json!({
        "title": "空列表",
        "featured": false,
        "gap": 8,
        "user": null,
        "items": [],
        "note": "",
    });
    println!(
        "\n---- 第二份数据 ----\n{}",
        template.render(&other).unwrap()
    );
}
