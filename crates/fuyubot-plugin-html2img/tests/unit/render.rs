use super::*;

fn request(html: &str) -> Request {
    Request {
        html: html.to_owned(),
        width: 320,
        height: 200,
        scale: 1.0,
        auto_height: false,
        dark: false,
        transparent: false,
        padding: 20,
        font_family: "sans-serif".to_owned(),
    }
}

#[test]
fn fragment_gets_wrapped_with_base_style() {
    let html = document(&request("<p>hi</p>"));
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<style data-fuyubot=\"base\">"));
    assert!(html.contains("<p>hi</p>"));
}

#[test]
fn document_gets_style_injected_after_head() {
    let html = document(&request(
        "<!DOCTYPE html><html><head><title>t</title></head><body>x</body></html>",
    ));
    let style = html.find("<style data-fuyubot").unwrap();
    let title = html.find("<title>").unwrap();
    assert!(style < title, "基础样式必须排在用户样式前面");
    assert!(!html.contains("<body><!DOCTYPE"));
}

#[test]
fn document_without_head_injects_before_body() {
    let html = document(&request("<html><body>x</body></html>"));
    let style = html.find("<style data-fuyubot").unwrap();
    let body = html.find("<body").unwrap();
    assert!(style < body);
}

#[test]
fn header_is_not_mistaken_for_head() {
    // `<header>` 不能被当成 `<head>`：样式应插在 body 之前，而不是页头里面。
    let html = document(&request("<html><body><header>标题</header>x</body></html>"));
    let style = html.find("<style data-fuyubot").unwrap();
    let header = html.find("<header>").unwrap();
    assert!(
        style < header,
        "基础样式必须在 <header> 之前（实际 {style} vs {header}）"
    );
    assert!(style < html.find("<body").unwrap());
}

#[test]
fn dark_and_transparent_change_background() {
    let mut dark = request("<p>x</p>");
    dark.dark = true;
    assert!(base_style(&dark).contains("background:#0f1115"));

    let mut transparent = request("<p>x</p>");
    transparent.transparent = true;
    assert!(base_style(&transparent).contains("background:transparent"));
}

#[test]
fn concurrent_renders_are_safe() {
    // 服务方法可能被多个插件同时调用；Stylo 的全局线程池在并发
    // Parallel 遍历时会 panic，所以 render() 固定用 StyleThreading::Sequential。
    let handles: Vec<_> = (0..4)
        .map(|index| {
            std::thread::spawn(move || {
                let mut request = request(&format!("<p>并发渲染 {index}</p>"));
                request.width = 160;
                request.height = 80;
                render(&request).map(|rendered| rendered.png.len())
            })
        })
        .collect();
    for handle in handles {
        let length = handle.join().expect("线程 panic").expect("渲染失败");
        assert!(length > 0);
    }
}

#[test]
fn images_with_data_and_relative_urls_do_not_panic() {
    // hyper-render 的 blitz 默认 base_url 是 `data:text/css;...`（cannot-be-a-base），
    // 任何 `data:` / 相对 URL 都会在 resolve_url 里 panic。vendored fork 设了 http base。
    let pixel = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    let html = format!(r#"<img src="data:image/png;base64,{pixel}"><img src="missing.png">"#);
    let rendered = render(&request(&html)).expect("data:/相对图片不应 panic");
    assert_eq!((rendered.width, rendered.height), (320, 200));
}

#[test]
fn renders_png_with_requested_size() {
    let rendered = render(&request("<h1>HTML → 图片</h1>")).expect("渲染应成功");
    assert!(rendered.png.starts_with(&[0x89, b'P', b'N', b'G']));
    assert_eq!((rendered.width, rendered.height), (320, 200));
    assert_eq!(
        png_dimensions(&rendered.png),
        Some((rendered.width, rendered.height))
    );
}

#[test]
fn scale_doubles_pixels() {
    let mut request = request("<p>x</p>");
    request.scale = 2.0;
    let rendered = render(&request).expect("渲染应成功");
    assert_eq!((rendered.width, rendered.height), (640, 400));
}

#[test]
fn scale_keeps_css_layout() {
    // 回归：hyper-render 的 viewport 会把 width 当物理像素（layout 宽度 = width/scale），
    // 不在 CSS 里固定宽度的话，scale = 2 会把每行字数砍半、内容高度明显超过 2 倍。
    let html = "<p>插件端到端渲染测试。这是一段更长的中文文本，用来观察换行是否正常。</p>";
    let mut one = request(html);
    one.auto_height = true;
    one.height = 100;
    let mut two = one.clone();
    two.scale = 2.0;

    let small = render(&one).expect("渲染应成功");
    let large = render(&two).expect("渲染应成功");
    assert_eq!(large.width, small.width * 2);
    let expected = small.height as i32 * 2;
    assert!(
        (large.height as i32 - expected).abs() <= 4,
        "scale 后布局应保持一致：{} vs {}（2×{}）",
        large.height,
        expected,
        small.height
    );
}

#[test]
fn auto_height_grows_with_content() {
    let mut request = request("<div style=\"height:400px\">x</div>");
    request.auto_height = true;
    request.height = 100;
    let rendered = render(&request).expect("渲染应成功");
    assert!(
        rendered.height > 100,
        "auto_height 应撑高：{}",
        rendered.height
    );
}

#[test]
fn jpeg_starts_with_soi_and_ends_with_eoi() {
    let rendered = render(&request("<p style=\"font-size:32px\">你好</p>")).expect("渲染应成功");
    let jpeg = to_jpeg(&rendered.png, 80).expect("转码应成功");
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG 应以 SOI 开头");
    assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "JPEG 应以 EOI 结尾");
    assert!(
        jpeg.len() < rendered.png.len(),
        "同一张图 JPEG 应该比 PNG 小：{} vs {}",
        jpeg.len(),
        rendered.png.len()
    );
}
