//! A-Bogus：抖音网页端每个数据接口都带的 `a_bogus` 签名。
//!
//! 逐行移植自 [Evil0ctal/Douyin_TikTok_Download_API](https://github.com/Evil0ctal/Douyin_TikTok_Download_API)
//! （Apache-2.0）的 `dtk/signing/native/abogus.py`，与 Python 版**逐字节对拍**
//! （crate 根 `sign-vectors.json` 的 `abogus` / `flow` 用例，测试在本文件的
//! `mod tests`）。
//!
//! ## 算法从哪来
//!
//! 算法逆向自抖音自己的 `bdms.js` v1.0.1.19-fix.01（2026-09）。`a_bogus` 不出自
//! 产 `x-secsdk-web-signature` 的 `secsdk`，也不出自产 `msToken` 的 `webmssdk`，
//! 而是出自 `bdms.js`——页面把它装到 `XMLHttpRequest` / `fetch` / `URL` 上，给
//! `/aweme/v1/` 一类路径的请求签名。页面加载的任何文件里都没有 "a_bogus" 字样：
//! 生成器（`fn150`，只经 `fn103` 从三个请求装饰器到达）活在字节码 VM 里，
//! 归属是结构推出来的，不是猜字符串常量位置猜的。
//!
//! ## 载荷长什么样
//!
//! ```text
//! base64_s4( 4B 头 ‖ RC4( 8B 版本块 ‖ 噪声展开(50 标量 ‖ 几何串 ‖ 尾串 ‖ 校验和) ) )
//! ```
//!
//! - **头**：明文 `(3, 82)`（[`HEADER_MAGIC`]），经 [`mask_pair`] 混入两个噪声字节；
//! - **版本块**：SDK 版本 [`SDK_VERSION`]，两对各四字节，第二对的两个噪声字节
//!   是环境报告（[`probe_noise`] / [`tripwire_noise`]）；
//! - **五十标量**：毫秒时钟 / `ink`（时钟减 1ms 的活性检查）/ 双周期计数 /
//!   `page_id` / `aid` / 三路摘要各三个字节（[`DIGEST_CHAINS`]）……按
//!   [`FIELD_ORDER`] 固定置换排布；
//! - **校验和**：版本块 + 五十标量的逐字节 XOR。它是格式的内部冗余，也是
//!   [`structure_error`] 能分辨「真签名」与「像签名的东西」的原因——校验和覆盖的
//!   字段本身就在载荷里，能通过说明整个布局（版本块、置换、噪声展开、密码）都
//!   达成了一致，凑是凑不出来的。
//!
//! ## 两个坑（照搬源码 docstring 的分量）
//!
//! 1. **像 RC4 但不是 RC4**：S-box 逆序初始化、密钥调度用乘法。教科书 RC4
//!    暴力搜那个一字节密钥一无所获——大概这就是目的（[`rc4`]）。
//! 2. **随机数是装饰，但不是全装饰**：`Math.random` 不进校验和、不影响任何
//!    可解码字段，同一毫秒两次签名大部分字节不同而含义相同。但噪声分布不能是
//!    均匀的：[`HEADER_NOISE_BANDS`]（浏览器家族频段）、[`probe_noise`]（可达
//!    集）、[`tripwire_noise`]（绊线健康位）这三个字节是**环境报告**，拿着签名
//!    的人能读出来——均匀随机反而是破绽。
//!
//! ## 随机数缝
//!
//! [`ABogus::new`] 收 `&mut dyn FnMut() -> f64`（Python 侧只消费 `rng.random()`），
//! 生产传 xorshift 之类的实现即可；测试用 `sign-vectors.json` 里录好的浮点流
//! 按序重放（`int(rng.random() * N)` = `(rng() * N as f64) as u32`，截断语义相同）。

use super::sm3::sm3_hash;

// --------------------------------------------------------------------------
// 常量（与 abogus.py 一字不改地对应）
// --------------------------------------------------------------------------

/// `bdms.js` 随身带的五套字母表里这里够得着的两套：`s4` 编最终签名，`s3` 编
/// 进第三路摘要的 User-Agent。
pub const ALPHABETS: [(&str, &str); 2] = [
    (
        "s3",
        "ckdp1h4ZKsUB80/Mfvw36XIgR25+WQAlEi7NLboqYTOPuzmFjJnryx9HVGDaStCe",
    ),
    (
        "s4",
        "Dkdpgh2ZmsQB80/MfvV36XI1R45-WUAlEixNLwoqYTOPuzKFjJnry79HbGcaStCe",
    ),
];

/// 哈希前往 query / body 上追加的盐。是「常量且秘而不宣」意义上的盐——
/// 既不长也不带密钥。
pub const SALT: &str = "dhzx";

/// 头里噪声稀释前的两个明文字节。这是本格式最接近魔数的东西，
/// 也是 [`structure_error`] 查的第一件事。
pub const HEADER_MAGIC: (u8, u8) = (3, 82);

/// `"1.0.1.19-fix.01"` 经 SDK 自己解析后的版本块：`split(".")` + JS `~~`
/// 把 `"19-fix"` 截成 19、`"01"` 截成 1，但只用前四个，实测版本块就是
/// `[1, 0, 1, 0]`。
pub const SDK_VERSION: [u8; 4] = [1, 0, 1, 0];

/// 一个字节，就是全部密钥。为什么教科书 RC4 搜不到它见模块文档。
pub const PAYLOAD_KEY: u8 = 0xD3;

/// `2024-07-24T16:00:00Z`。有一个字段从这里开始数双周期（fortnight）。
pub const FORTNIGHT_EPOCH_MS: u64 = 1_721_836_800_000;

/// 抖音网页自己的标识（页面配置 `bdms` 时给的）。它们既在签名里又在 query 里，
/// 两处不一致是最便宜的破绽。
pub const PAGE_ID: u32 = 6241;
pub const AID: u32 = 6383;

/// [`expand_noise`] 把三个载荷字节藏进四个载体字节用的掩码。
/// `0x91 | 0x42 | 0x2C == 0xFF`——三个掩码瓜分一个字节，第四个字节才装得下
/// 前三个让给噪声的那些位（且在原位），整层因此可逆。
pub const NOISE_MASKS: [u8; 3] = [0x91, 0x42, 0x2C];
pub const DATA_MASKS: [u8; 3] = [0x6E, 0xBD, 0xD3];

/// 五十个标量怎么洗进载荷。不是密码，只是固定置换；照字节码里的样子写出来，
/// 是为了拿真实签名核对的人能看到与解码器相同的顺序。
pub const FIELD_ORDER: [&str; 50] = [
    "L34", "L44", "L56", "L61", "L73", "L29", "L70", "L45", "L35", "L49", "L38", "L66", "L51",
    "L68", "L28", "L48", "L64", "L47", "L30", "L71", "L26", "L55", "L31", "L69", "L59", "L40",
    "L62", "L63", "L27", "L72", "L41", "L74", "L57", "L52", "L42", "L39", "L33", "L67", "L53",
    "L43", "L65", "L46", "L36", "L24", "L60", "L32", "L79", "L80", "L84", "L85",
];

/// 三个摘要金丝雀的 `(offset, sentinel, fallback)`——按 [`canary`] 的形参顺序
/// 位置传参（Python 就是 `canary(digest, *chain.canary)`）。正常时字段持有
/// `offset` 起第一个不等于 `sentinel` 的摘要字节；写出哨兵值本身是 SDK 在报告
/// 自己的环境探针炸了。我们永远写诚实值。
pub const CANARIES: [(usize, u8, u8); 3] = [(3, 11, 12), (4, 8, 9), (5, 12, 13)];

/// 一路哈希输入在五十标量里落下的三个字节：`slots` 是 [`FIELD_ORDER`] 里的
/// 名字，`indices` 是两个原样写入的摘要位，`canary` 是第三个槽位遵循的哨兵三元组。
pub struct DigestChain {
    pub slots: [&'static str; 3],
    pub indices: (usize, usize),
    pub canary: (usize, u8, u8),
}

/// 三个哈希输入各落在哪。生成器与离线解码器读同一张表，解码器才不会漂移。
/// 每路只绑三个字节是刻意的不对称：足以**证明**某条 query / body / UA 就是
/// 这个签名封的那条，远远不足以把哈希反推回去。
pub const DIGEST_CHAINS: [(&str, DigestChain); 3] = [
    (
        "query",
        DigestChain {
            slots: ["L48", "L49", "L51"],
            indices: (9, 18),
            canary: CANARIES[0],
        },
    ),
    (
        "body",
        DigestChain {
            slots: ["L52", "L53", "L55"],
            indices: (10, 19),
            canary: CANARIES[1],
        },
    ),
    (
        "user_agent",
        DigestChain {
            slots: ["L56", "L57", "L59"],
            indices: (11, 21),
            canary: CANARIES[2],
        },
    ),
];

/// 未被插桩的浏览器对自己的六项环境探针 + 一个反爬位集报出来的样子
/// （`fn150` 读的就是这些）。池化身份应该长这样。
pub const ENV_FLAGS: u32 = 1;
pub const DETECT_FLAGS: u32 = 14;
pub const NR_FLAGS: u32 = 0x21;
pub const NR_TAG: [u8; 4] = [0, 0, 0, 0];

/// `window.onwheelx._Ax` 在位且已锁死——SDK 装在自己身上再冻结的状态。
/// 12 意味着有人解锁过，11 意味着没了，两者都会被打分，都不该是自己的样子。
pub const TRIPWIRE_LOCKED: u8 = 3;

/// `fn149` 给每页签名计数分桶。6 = 「本页签名少于 140 次」，浏览器一生中
/// 大部分时间都待在这。
pub const CALL_BUCKET: u8 = 6;

/// 1080p 显示器上的 Chrome：1920x1080 屏幕，去掉系统任务栏剩 1032 可用高，
/// 再去掉浏览器自身 chrome 剩 947 视口高。指纹不带屏幕尺寸时用它——
/// 说得过去的常量胜过随机值，每次请求都变的几何本身就是破绽。
pub const DEFAULT_BROWSER_INFO: &str = "1920|947|1920|1032|1920|1032|1920|1080|Win32";

/// 从屏幕高里减掉的系统任务栏 / 浏览器 chrome 行数（[`browser_info_from_screen`]）。
const TASKBAR_PX: u64 = 48;
const CHROME_PX: u64 = 85;

/// `fn143` 报的浏览器家族，伪装成噪声：每家族一条 40 宽的频段，头里第二个
/// 噪声字节是 `floor(random * 40) + base`。43 个真实 Chrome 签名实测全落在
/// 0..38。
///
/// 频段盖不满 0..255：落在缝里的值是任何浏览器都发不出的，落在错家族的值
/// 与旁边的 User-Agent 自相矛盾——两者都是免费的打分点。均匀随机 6% 落缝里、
/// 78% 落错家族。
const HEADER_NOISE_BANDS: [(&str, u8); 6] = [
    ("chrome", 0),
    ("firefox", 40),
    ("safari", 81),
    ("edge", 125),
    ("huawei", 170),
    ("other", 210),
];

/// `fn145` 报 SDK 自己的绊线。`fn152` 装三个绊线（`window.onwheelx._Ax` 与两个
/// 拼错的 `pemrissions` 属性），`fn153` 冻结它们——bit 1、4、5、7 置位才是
/// *健康*态（与 `L66 == 3` 同理）。真实抓包恰好取到这 16 个值。均匀随机
/// 94% 会报出「被拆了 / 没锁」的页面。
const TRIPWIRE_SET: u8 = 0xB2;
const TRIPWIRE_FREE: u8 = 0x4D;

/// 「不是 a_bogus」的问题种类（区别于「是 a_bogus 但内容不对」）。
pub const DECODE_PROBLEMS: [&str; 3] = [
    "alphabet",
    "length not a multiple of four",
    "payload too short",
];

// --------------------------------------------------------------------------
// 浏览器几何
// --------------------------------------------------------------------------

/// 签名逐字携带的 `navigator`/`screen` 串：九个字段以 `|` 相连，顺序照 SDK
/// 自己的对象字面量。它是载荷里唯一以可读文本一路活到校验和的部分，
/// 字段顺序没有商量余地。
// 九个字段一一对应对象字面量；拆成结构体只会让调用处更难核对顺序。
#[allow(clippy::too_many_arguments)]
pub fn build_browser_info(
    inner_width: u64,
    inner_height: u64,
    outer_width: u64,
    outer_height: u64,
    avail_width: u64,
    avail_height: u64,
    screen_width: u64,
    screen_height: u64,
    platform: &str,
) -> String {
    [
        inner_width.to_string(),
        inner_height.to_string(),
        outer_width.to_string(),
        outer_height.to_string(),
        avail_width.to_string(),
        avail_height.to_string(),
        screen_width.to_string(),
        screen_height.to_string(),
        platform.to_owned(),
    ]
    .join("|")
}

/// 由一个屏幕尺寸推出自洽的九字段几何。浏览器报的四个矩形互不独立：
/// 窗口不大于工作区、工作区是屏幕减系统栏、视口是窗口减浏览器 chrome。
/// 从一个数推出来才自洽，随手拼的不会。
pub fn browser_info_from_screen(width: u64, height: u64, platform: &str) -> String {
    let avail_height = height.saturating_sub(TASKBAR_PX).max(1);
    build_browser_info(
        width,
        avail_height.saturating_sub(CHROME_PX).max(1),
        width,
        avail_height,
        width,
        avail_height,
        width,
        height,
        platform,
    )
}

// --------------------------------------------------------------------------
// 原语
// --------------------------------------------------------------------------

/// SDK 的「RC4」，不是 RC4：S-box 从**降序**恒等出发，密钥调度在教科书只做
/// 加法的地方做乘法；密钥流生成器本身没动。
///
/// 两处改动比看起来重要：它们让「拿标准 RC4 把每个单字节密钥试一遍」对抓到的
/// 载荷一无所获——在读出字节码之前这里就是这么撞的墙，这大概就是它们的目的。
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut box_ = [0u8; 256];
    for i in 0..256 {
        box_[255 - i] = i as u8;
    }
    let mut j = 0usize;
    for i in 0..256 {
        // Python：j = (j * box[i] + j + key[i % len(key)]) % 256——乘法版密钥调度。
        j = (j * box_[i] as usize + j + key[i % key.len()] as usize) % 256;
        box_.swap(i, j);
    }
    let mut out = vec![0u8; data.len()];
    let mut i = 0usize;
    let mut j = 0usize;
    for (index, &byte) in data.iter().enumerate() {
        i = (i + 1) % 256;
        j = (j + box_[i] as usize) % 256;
        box_.swap(i, j);
        out[index] = byte ^ box_[(box_[i] as usize + box_[j] as usize) % 256];
    }
    out
}

fn alphabet_table(alphabet: &str) -> &'static str {
    ALPHABETS
        .iter()
        .find(|(name, _)| *name == alphabet)
        .map(|(_, table)| *table)
        .unwrap_or_else(|| panic!("unknown alphabet: {alphabet}"))
}

/// 用 SDK 的某套字母表做 base64，字面 `=` 补位。除数字集外就是标准 base64：
/// 补位规则与六位分组都是普通的。
pub fn encode_base64(data: &[u8], alphabet: &str) -> String {
    let table = alphabet_table(alphabet).as_bytes();
    let mut out = String::new();
    let mut offset = 0;
    while offset < data.len() {
        let chunk = &data[offset..data.len().min(offset + 3)];
        // Python：int.from_bytes(chunk + b"\x00" * (3 - len(chunk)), "big")
        let mut block = 0u32;
        for &byte in chunk {
            block = (block << 8) | byte as u32;
        }
        block <<= 8 * (3 - chunk.len());
        let digits = [
            (block >> 18) & 0x3F,
            (block >> 12) & 0x3F,
            (block >> 6) & 0x3F,
            block & 0x3F,
        ];
        for &digit in &digits[..chunk.len() + 1] {
            out.push(table[digit as usize] as char);
        }
        offset += 3;
    }
    out.extend(std::iter::repeat_n('=', (4 - out.len() % 4) % 4));
    out
}

/// [`encode_base64`] 的逆。外来数字抛错（Python `ValueError`）。
pub fn decode_base64(text: &str, alphabet: &str) -> Result<Vec<u8>, String> {
    let table = alphabet_table(alphabet);
    let body = text.trim_end_matches('=');
    let mut out = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut offset = 0;
    while offset < chars.len() {
        let chunk = &chars[offset..chars.len().min(offset + 4)];
        let mut block = 0u32;
        for &char in chunk {
            let value = table
                .find(char)
                .map(|index| index as u32)
                .ok_or_else(|| format!("not a {alphabet} digit: {char:?}"))?;
            block = (block << 6) | value;
        }
        block <<= 6 * (4 - chunk.len());
        for &shift in &[16u32, 8, 0][..chunk.len() - 1] {
            out.push(((block >> shift) & 0xFF) as u8);
        }
        offset += 4;
    }
    Ok(out)
}

/// JavaScript 串转字节，SDK（`fn139`）的路子：走 **UTF-16 码元**，每单元
/// U+0100 以下出一个字节、以上大端出两个——代理对出四个字节、汉字出两个
/// （UTF-8 会是三个）。43 个真实签名实测：这个规则 43/43 复现声明的几何长度，
/// UTF-8 只有 41/43。
///
/// 只在 U+00FF 以上才与 UTF-8 分叉，所以差异一直隐形，直到一个带中文的
/// `navigator.platform` 被签了名。几何长度被校验和盖着，搞错会让整个帧错位、
/// 整个签名作废，而不是只坏一个字段。
pub fn js_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for code in text.encode_utf16() {
        if code & 0xFF00 != 0 {
            out.push((code >> 8) as u8);
        }
        out.push((code & 0xFF) as u8);
    }
    out
}

/// `value` 的 `count` 个小端字节（Python `_le_bytes`）。
pub fn le_bytes(value: u64, count: usize) -> Vec<u8> {
    (0..count)
        .map(|index| ((value >> (8 * index)) & 0xFF) as u8)
        .collect()
}

/// `offset` 起第一个不等于 `sentinel` 的摘要字节，全等于则给 `fallback`。
///
/// 哨兵值是被保留的：写出它等于 SDK 在告诉服务端「我自己的环境探针炸了」。
/// 我们从不写它，所以这里只可能在报摘要。
pub fn canary(digest: &[u8], offset: usize, sentinel: u8, fallback: u8) -> u8 {
    for &byte in digest.get(offset..).unwrap_or(&[]) {
        if byte != sentinel {
            return byte;
        }
    }
    fallback
}

/// `SM3(SM3(text + SALT))`，32 个字节。str 按 UTF-8 哈希（Python
/// `sm3_to_array` 的 str 分支）。
pub fn digest_of(text: &str) -> [u8; 32] {
    sm3_hash(&sm3_hash(format!("{text}{SALT}").as_bytes()))
}

/// 第三路：把 User-Agent 先 RC4 再 base64，哈希一次。
///
/// 与另两路不同只哈希一次。RC4 密钥由环境探针拼成，所以对给定身份是常量，
/// 而不是每次调用一换。
pub fn user_agent_digest(user_agent: &str) -> [u8; 32] {
    let key = [
        (ENV_FLAGS / 256) as u8,
        (ENV_FLAGS % 256) as u8,
        (DETECT_FLAGS % 256) as u8,
    ];
    // 码元而非 UTF-8：密码读的是不掩码的 `charCodeAt`，其后的 base64 才掩到
    // 八位，所以密钥流每个码元走一步。用 UTF-8 的话一个汉字走三步、摘要不同——
    // 实测 43/43 对 40/43。
    let sealed = rc4(&key, &js_bytes(py_strip(user_agent)));
    sm3_hash(encode_base64(&sealed, "s3").as_bytes())
}

/// `chain` 贡献的三个字节（给它自己那路的摘要）。
pub fn chain_bytes(chain: &DigestChain, digest: &[u8]) -> (u8, u8, u8) {
    let (first, second) = chain.indices;
    let (offset, sentinel, fallback) = chain.canary;
    (
        digest[first],
        digest[second],
        canary(digest, offset, sentinel, fallback),
    )
}

/// Python `str.strip()` 的空白集：Unicode 空白 + `\x1c`-`\x1f`（Rust
/// `trim` 不含后四者）。
fn py_strip(text: &str) -> &str {
    text.trim_matches(|char: char| char.is_whitespace() || matches!(char, '\x1c'..='\x1f'))
}

// --------------------------------------------------------------------------
// 噪声层
// --------------------------------------------------------------------------

/// `fn143` 会从哪条频段取数：给定 User-Agent 判家族。
///
/// 顺序要紧：Edge 与华为都自称 "Chrome"，Chrome 自称 "Safari"。SDK 先测专名
/// 再测通名，这里照做。
fn family_of(user_agent: &str) -> &'static str {
    let ua = user_agent.to_lowercase();
    for name in ["edg", "huawei", "firefox", "chrome", "safari"] {
        if ua.contains(name) {
            return if name == "edg" { "edge" } else { name };
        }
    }
    "other"
}

/// `fn143`：穿着噪声外衣的浏览器家族报告。
fn header_noise(user_agent: &str, rng: &mut dyn FnMut() -> f64) -> u8 {
    let family = family_of(user_agent);
    let base = HEADER_NOISE_BANDS
        .iter()
        .find(|(name, _)| *name == family)
        .map(|(_, base)| *base)
        .unwrap_or(210);
    ((base as u32 + draw_int(rng, 40)) & 0xFF) as u8
}

/// Python `int(rng.random() * span)`：乘完截断（非负，与 Rust `as` 同语义）。
fn draw_int(rng: &mut dyn FnMut() -> f64, span: u32) -> u32 {
    (rng() * span as f64) as u32
}

/// `fn144`：`nr()[4] & 64` 清零时走的那条道（它就是清零的）。
///
/// 110 以下原样通过，以上强制成奇数——110..240 有一半取不到。均匀字节 31%
/// 落在够不着的集合外。
fn probe_noise(rng: &mut dyn FnMut() -> f64) -> u8 {
    let value = draw_int(rng, 240);
    if value > 109 {
        (value + value % 2 + 1) as u8
    } else {
        value as u8
    }
}

/// `fn145`：「我的绊线都在位且已锁」，掺一点真噪声。
fn tripwire_noise(rng: &mut dyn FnMut() -> f64) -> u8 {
    ((draw_int(rng, 255) as u8) & TRIPWIRE_FREE) | TRIPWIRE_SET
}

/// 把两个字节摊到四个上，另一半用噪声填。
///
/// 每个输出字节一半位来自载荷、一半来自噪声字节（隔位掩码 `0xAA`/`0x55`）。
/// 还原只要载荷那半——但噪声那半同样还原得出来，SDK 正是拿三个位置里的两个
/// 报告自己（见 [`header_noise`] / [`probe_noise`] / [`tripwire_noise`]）。
/// `low` / `high` 就是调用方塞报告的口子；不塞就是 SDK 无事可报时的素抽。
fn mask_pair(
    pair: (u8, u8),
    rng: &mut dyn FnMut() -> f64,
    low: Option<u8>,
    high: Option<u8>,
) -> [u8; 4] {
    let noise = draw_int(rng, 65535);
    let low = low.unwrap_or((noise & 0xFF) as u8);
    let high = high.unwrap_or(((noise >> 8) & 0xFF) as u8);
    [
        (low & 0xAA) | (pair.0 & 0x55),
        (low & 0x55) | (pair.0 & 0xAA),
        (high & 0xAA) | (pair.1 & 0x55),
        (high & 0x55) | (pair.1 & 0xAA),
    ]
}

/// [`mask_pair`] 的逆：拿回每字节的载荷半。
fn unmask_pair(carrier: &[u8]) -> (u8, u8) {
    (
        (carrier[0] & 0x55) | (carrier[1] & 0xAA),
        (carrier[2] & 0x55) | (carrier[3] & 0xAA),
    )
}

/// 三个字节加一个噪声字节变成四个载体字节。
///
/// 每个载体保住自己字节的一部分、其余来自噪声；第四个字节精确收拢前三个
/// 让出去的那些位，且在它们原来的位上。字节内部不挪位，所以整层可逆——
/// 结果看着仍然像密文。
fn expand_noise(body: &[u8], rng: &mut dyn FnMut() -> f64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < body.len() {
        let group = &body[offset..body.len().min(offset + 3)];
        if group.len() < 3 {
            // SDK 的收尾分支。这里构建的载荷永远是 3 的倍数、走不到——
            // 照抄是为了将来字段变化时不会在这儿悄悄分叉。
            out.push(group[0]);
            if group.len() > 1 && group[1] != 0 {
                out.push(group[1]);
            }
            offset += 3;
            continue;
        }
        let noise = (draw_int(rng, 1000) & 0xFF) as u8;
        for index in 0..3 {
            out.push((noise & NOISE_MASKS[index]) | (group[index] & DATA_MASKS[index]));
        }
        out.push(
            (group[0] & NOISE_MASKS[0]) | (group[1] & NOISE_MASKS[1]) | (group[2] & NOISE_MASKS[2]),
        );
        offset += 3;
    }
    out
}

/// [`expand_noise`] 的逆。
fn collapse_noise(frame: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < frame.len() {
        let group = &frame[offset..frame.len().min(offset + 4)];
        if group.len() < 4 {
            out.extend_from_slice(group);
            offset += 4;
            continue;
        }
        for index in 0..3 {
            out.push((group[index] & DATA_MASKS[index]) | (group[3] & NOISE_MASKS[index]));
        }
        offset += 4;
    }
    out
}

/// "L24" 这类名字 → 下标 24。
fn slot_number(name: &str) -> usize {
    name[1..].parse().expect("字段名形如 L<数字>")
}

/// 字段必须落在单字节里（Python `bytes([...])` 语义：越界直接抛）。
fn field_byte(value: u32) -> u8 {
    u8::try_from(value).expect("字段值必须落在单字节里")
}

// --------------------------------------------------------------------------
// 生成器
// --------------------------------------------------------------------------

/// 为一个浏览器身份计算 `a_bogus`。
///
/// 除配置外无状态、代价极低，所以「一个身份一个」是自然的生命周期。`rng`
/// 存在是为了让测试能钉死噪声；产出的值本身怎么抽都是对的。
pub struct ABogus<'rng> {
    user_agent: String,
    browser_info: String,
    page_id: u32,
    aid: u32,
    rng: &'rng mut dyn FnMut() -> f64,
}

impl<'rng> ABogus<'rng> {
    /// `browser_info` 用 [`DEFAULT_BROWSER_INFO`]（Python 侧的默认值）；
    /// `page_id` / `aid` 固定为 [`PAGE_ID`] / [`AID`]（Python 的默认同款）。
    pub fn new(
        user_agent: &str,
        browser_info: &str,
        rng: &'rng mut dyn FnMut() -> f64,
    ) -> ABogus<'rng> {
        ABogus {
            user_agent: user_agent.to_owned(),
            browser_info: browser_info.to_owned(),
            page_id: PAGE_ID,
            aid: AID,
            rng,
        }
    }

    /// 五十个标量，名字与离线解码器打印的一致（`L<n>` → 下标 `n`）。
    fn fields(&self, query: &str, body: &str, now_ms: u64) -> [u32; 86] {
        let digests = [
            digest_of(query),
            digest_of(body),
            user_agent_digest(&self.user_agent),
        ];

        // `ink` 是 Date.now() - 1，SDK 早一次调用时种在 navigator 的原型上。
        // 它是对自己的活性检查：`ink` 不比时钟慢一毫秒的载荷，不是跑完了
        // 整个入口点的代码拼出来的。
        let ink = now_ms - 1;
        let fortnights = (now_ms - FORTNIGHT_EPOCH_MS) / (1000 * 60 * 60 * 24 * 14);
        let info_bytes = js_bytes(&self.browser_info);
        let tail_bytes = js_bytes(&format!("{},", (now_ms + 3) & 0xFF));

        let mut fields = [0u32; 86];
        fields[24] = 41;
        fields[26] = fortnights as u32;
        fields[27] = CALL_BUCKET as u32;
        // 距 SDK 初始化的毫秒数再加三。刚加载的页在这里签个很小的数；我们
        // 报三——入口点被及时走到，对一个不常开页面的进程来说是诚实答案。
        fields[28] = 3;
        fields[35] = ENV_FLAGS & 0xFF;
        fields[36] = (ENV_FLAGS / 256) & 0xFF;
        fields[38] = NR_FLAGS & 0xFF;
        fields[39] = (NR_FLAGS >> 8) & 0xFF;
        fields[66] = TRIPWIRE_LOCKED as u32;
        fields[79] = (info_bytes.len() & 0xFF) as u32;
        fields[80] = ((info_bytes.len() >> 8) & 0xFF) as u32;
        fields[84] = (tail_bytes.len() & 0xFF) as u32;
        fields[85] = ((tail_bytes.len() >> 8) & 0xFF) as u32;
        for (index, byte) in le_bytes(now_ms, 6).into_iter().enumerate() {
            fields[29 + index] = byte as u32;
        }
        for (index, byte) in le_bytes(DETECT_FLAGS as u64, 4).into_iter().enumerate() {
            fields[44 + index] = byte as u32;
        }
        for (index, &byte) in NR_TAG.iter().enumerate() {
            fields[40 + index] = byte as u32;
        }
        for (index, byte) in le_bytes(ink, 6).into_iter().enumerate() {
            fields[60 + index] = byte as u32;
        }
        for (index, byte) in le_bytes(self.page_id as u64, 4).into_iter().enumerate() {
            fields[67 + index] = byte as u32;
        }
        for (index, byte) in le_bytes(self.aid as u64, 4).into_iter().enumerate() {
            fields[71 + index] = byte as u32;
        }
        // 三个哈希输入，走解码器读的同一张表。九个槽位过去是一个一个点名的，
        // 也正是解码器可能与我们静默不一致的地方。
        for (chain, digest) in DIGEST_CHAINS.iter().zip(digests.iter()) {
            let written = chain_bytes(&chain.1, digest);
            for (slot, value) in chain.1.slots.iter().zip([written.0, written.1, written.2]) {
                fields[slot_number(slot)] = value as u32;
            }
        }
        fields
    }

    /// 一次请求的 `a_bogus` 值。
    ///
    /// `query` 是将要发送的 query（不含 `a_bogus` 自己）。`body` 是 POST 的
    /// 请求体，否则为空——它被哈希，空体签出来的就是错签名。
    ///
    /// `content_type` 只为一条规则存在：请求是 `multipart/form-data` 时 SDK
    /// 哈希前把 body 置空，因为页面那个位置的 body 是 `FormData` 不是字符串。
    /// 把部件当文本签会与平台哈希的东西不一致，所以这里同样办理。
    ///
    /// HTTP 方法刻意不参与：本版算法不哈希它（上一版哈希，遇到旧包的抓包时
    /// 值得知道）。
    ///
    /// `now_ms` 显式传入（Python 内部取 `time.time() * 1000`）；早于
    /// [`FORTNIGHT_EPOCH_MS`] 直接 panic——有一个字段从该纪元数双周期、没有
    /// 负数可放，时钟早于纪元多半是把秒当毫秒传了，签出来是安静的错。
    pub fn get_value(
        &mut self,
        query: &str,
        body: &str,
        content_type: &str,
        now_ms: u64,
    ) -> String {
        let body = if content_type.to_lowercase().contains("multipart/form-data") {
            ""
        } else {
            body
        };
        if now_ms < FORTNIGHT_EPOCH_MS {
            panic!("clock is before the a_bogus epoch: {now_ms}");
        }
        let fields = self.fields(query, body, now_ms);

        // 下面四个噪声字节里有三个是环境报告而非噪声（见模块文档）；
        // 第四个——版本块的低字节——是真·自由抽。
        let mut version = [0u8; 8];
        // Python 的求值顺序：先 pair1（1 抽），再 low=probe（1 抽）、
        // high=tripwire（1 抽），最后 _mask_pair 体内的 noise（1 抽）。
        version[..4].copy_from_slice(&mask_pair(
            (SDK_VERSION[0], SDK_VERSION[1]),
            &mut *self.rng,
            None,
            None,
        ));
        let low = probe_noise(&mut *self.rng);
        let high = tripwire_noise(&mut *self.rng);
        version[4..].copy_from_slice(&mask_pair(
            (SDK_VERSION[2], SDK_VERSION[3]),
            &mut *self.rng,
            Some(low),
            Some(high),
        ));

        // 校验和盖住版本块与五十标量，别的都不盖——不盖几何、不盖尾巴。
        // 它是格式的内部冗余，因此也是 `structure_error` 能分辨真签名的原因。
        let mut checksum = 0u8;
        for &byte in &version {
            checksum ^= byte;
        }
        for name in FIELD_ORDER {
            checksum ^= field_byte(fields[slot_number(name)]);
        }

        let mut body_bytes = Vec::with_capacity(FIELD_ORDER.len() + 96);
        for name in FIELD_ORDER {
            body_bytes.push(field_byte(fields[slot_number(name)]));
        }
        body_bytes.extend_from_slice(&js_bytes(&self.browser_info));
        body_bytes.extend_from_slice(&js_bytes(&format!("{},", (now_ms + 3) & 0xFF)));
        body_bytes.push(checksum);

        // 顺序同样是 Python 的：high 先求值（1 抽），再进 _mask_pair（1 抽）。
        let reported = header_noise(&self.user_agent, &mut *self.rng);
        let header = mask_pair(HEADER_MAGIC, &mut *self.rng, None, Some(reported));
        let frame = expand_noise(&body_bytes, &mut *self.rng);
        let mut plain = Vec::with_capacity(8 + frame.len());
        plain.extend_from_slice(&version);
        plain.extend_from_slice(&frame);
        let sealed = rc4(&[PAYLOAD_KEY], &plain);
        let mut payload = Vec::with_capacity(4 + sealed.len());
        payload.extend_from_slice(&header);
        payload.extend_from_slice(&sealed);
        encode_base64(&payload, "s4")
    }
}

// --------------------------------------------------------------------------
// 结构校验（给影子对比用）
// --------------------------------------------------------------------------

/// `problem` 是「根本不是 a_bogus」而不是「是 a_bogus 但内容不对」吗？
pub fn is_decode_problem(problem: Option<&str>) -> bool {
    problem.is_some_and(|problem| DECODE_PROBLEMS.contains(&problem))
}

/// `value` 第一个「不是良构 a_bogus」的地方，良构则 [`None`]。
///
/// 这里的每条检查都由算法定死：两个正确的实现无论噪声怎么给都会在所有检查上
/// 一致。刻意没查长度——长度随几何串走，两个都对的浏览器本来就会不同。
///
/// 最后一条最值钱：校验和盖的字段本身就在载荷里，能通过说明装配它的东西在
/// 整个布局上与我们意见一致——版本块、五十槽置换、噪声展开、密码。不是真
/// 算法凑不出一个。
pub fn structure_error(value: &str) -> Option<&'static str> {
    structure_error_with(value, "s4")
}

/// [`structure_error`]，可选字母表（Python 的 `alphabet=` 关键字参数）。
pub fn structure_error_with(value: &str, alphabet: &str) -> Option<&'static str> {
    let body = value.trim_end_matches('=');
    let table = alphabet_table(alphabet);
    if body.is_empty() || body.contains('=') || !body.chars().all(|char| table.contains(char)) {
        return Some("alphabet");
    }
    if !value.chars().count().is_multiple_of(4) {
        return Some("length not a multiple of four");
    }

    let payload = decode_base64(value, alphabet).expect("字母表检查已通过");
    // 头、版本块、装下五十标量所需的帧：4 + 8 + 68。
    if payload.len() < 80 {
        return Some("payload too short");
    }

    if unmask_pair(&payload[..4]) != HEADER_MAGIC {
        return Some("header magic");
    }

    let plain = rc4(&[PAYLOAD_KEY], &payload[4..]);
    let version = &plain[..8];
    if unmask_pair(&version[..4]) != (SDK_VERSION[0], SDK_VERSION[1]) {
        return Some("sdk version");
    }
    if unmask_pair(&version[4..8]) != (SDK_VERSION[2], SDK_VERSION[3]) {
        return Some("sdk version");
    }

    let unpacked = collapse_noise(&plain[8..]);
    if unpacked.len() < FIELD_ORDER.len() + 1 {
        return Some("frame too short");
    }
    let mut fields = [0u32; 86];
    for (index, name) in FIELD_ORDER.iter().enumerate() {
        fields[slot_number(name)] = unpacked[index] as u32;
    }

    let info_length = fields[79] | (fields[80] << 8);
    let tail_length = fields[84] | (fields[85] << 8);
    let end = FIELD_ORDER.len() as u32 + info_length + tail_length;
    if end + 1 > unpacked.len() as u32 {
        return Some("declared lengths overrun the frame");
    }
    // 反方向也得查。只查欠载曾放过一个「与自己拼接」的签名：密码是密钥流，
    // 前半照样解得开，尾巴的垃圾根本没人看。声明长度必须把载荷算平，
    // 允许噪声展开给末组补的那两个字节的出入。
    if unpacked.len() as u32 - (end + 1) > 2 {
        return Some("declared lengths leave a tail");
    }

    let mut checksum = 0u8;
    for &byte in version {
        checksum ^= byte;
    }
    for name in FIELD_ORDER {
        checksum ^= field_byte(fields[slot_number(name)]);
    }
    if unpacked[end as usize] != checksum {
        return Some("checksum");
    }
    None
}

/// [`decode`] 拆出来的可恢复字段。诊断与测试用，签名不用它。
pub struct Decoded {
    pub header_magic: (u8, u8),
    pub sdk_version: [u8; 4],
    pub now_ms: u64,
    pub ink_ms: u64,
    pub page_id: u32,
    pub aid: u32,
    pub env_flags: u8,
    pub detect_flags: u32,
    pub call_bucket: u8,
    pub tripwire: u8,
    pub browser_info: String,
    pub tail: String,
    /// `[（字段名, 值）; 50]`，按 [`FIELD_ORDER`] 的顺序。
    pub fields: Vec<(String, u8)>,
}

/// 把 `a_bogus` 拆开（[`structure_error`] 通过后才有意义）。
///
/// 这是证明了算法的那个函数：跑在浏览器产出的签名上，它还原出那个浏览器的
/// 屏幕尺寸、抖音自己的 `aid`、以及与旁边 `timestamp` 参数一致的时钟。
pub fn decode(value: &str) -> Result<Decoded, String> {
    decode_with(value, "s4")
}

/// [`decode`]，可选字母表。
pub fn decode_with(value: &str, alphabet: &str) -> Result<Decoded, String> {
    if let Some(problem) = structure_error_with(value, alphabet) {
        return Err(format!("not a well-formed a_bogus: {problem}"));
    }

    let payload = decode_base64(value, alphabet).expect("structure_error 已校验");
    let plain = rc4(&[PAYLOAD_KEY], &payload[4..]);
    let unpacked = collapse_noise(&plain[8..]);
    let mut fields = [0u32; 86];
    for (index, name) in FIELD_ORDER.iter().enumerate() {
        fields[slot_number(name)] = unpacked[index] as u32;
    }

    fn little_endian(fields: &[u32; 86], first: usize, count: usize) -> u64 {
        (0..count)
            .map(|index| (fields[first + index] as u64) << (8 * index))
            .sum()
    }

    let info_length = (fields[79] | (fields[80] << 8)) as usize;
    let tail_length = (fields[84] | (fields[85] << 8)) as usize;
    let start = FIELD_ORDER.len();
    let (version_low, version_high) = unmask_pair(&plain[..4]);
    let (version_low2, version_high2) = unmask_pair(&plain[4..8]);
    Ok(Decoded {
        header_magic: unmask_pair(&payload[..4]),
        sdk_version: [version_low, version_high, version_low2, version_high2],
        now_ms: little_endian(&fields, 29, 6),
        ink_ms: little_endian(&fields, 60, 6),
        page_id: little_endian(&fields, 67, 4) as u32,
        aid: little_endian(&fields, 71, 4) as u32,
        env_flags: field_byte(fields[35]),
        detect_flags: little_endian(&fields, 44, 4) as u32,
        call_bucket: field_byte(fields[27]),
        tripwire: field_byte(fields[66]),
        browser_info: String::from_utf8_lossy(&unpacked[start..start + info_length]).into_owned(),
        tail: String::from_utf8_lossy(
            &unpacked[start + info_length..start + info_length + tail_length],
        )
        .into_owned(),
        fields: FIELD_ORDER
            .iter()
            .zip(unpacked.iter())
            .map(|(name, &value)| ((*name).to_owned(), value))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use serde_json::Value;

    use super::*;
    use crate::sign::encode::{quote_strict, urlencode};
    use crate::sign::websign::{VERIFY_FP_PARAMS, pairs_from_query, sign};

    fn vectors() -> Value {
        serde_json::from_str(include_str!("../../sign-vectors.json")).expect("向量文件是合法 JSON")
    }

    fn pairs_of(value: &Value) -> Vec<(String, String)> {
        value
            .as_array()
            .expect("pairs 是数组")
            .iter()
            .map(|pair| {
                let items = pair.as_array().expect("pair 是二元组");
                (
                    items[0].as_str().expect("key 是字符串").to_owned(),
                    items[1].as_str().expect("value 是字符串").to_owned(),
                )
            })
            .collect()
    }

    fn stream_of(value: &Value) -> Vec<f64> {
        value
            .as_array()
            .expect("rng_stream 是数组")
            .iter()
            .map(|item| item.as_f64().expect("rng_stream 是浮点"))
            .collect()
    }

    /// 重放录好的 `rng_stream`（Python 侧 `random.Random(seed)` 逐次
    /// `random()` 的返回值）。返回的队列用来断言流被**恰好**用尽。
    fn replay_rng(stream: &[f64]) -> (Rc<RefCell<VecDeque<f64>>>, impl FnMut() -> f64) {
        let queue = Rc::new(RefCell::new(
            stream.iter().copied().collect::<VecDeque<_>>(),
        ));
        let inner = Rc::clone(&queue);
        let rng = move || {
            inner
                .borrow_mut()
                .pop_front()
                .expect("rng 流耗尽：移植代码比 Python 多抽了随机数")
        };
        (queue, rng)
    }

    /// 四条 a_bogus 向量逐字节对拍 Python。
    #[test]
    fn abogus_vectors_match_python() {
        let data = vectors();
        for case in data["abogus"].as_array().expect("abogus 数组") {
            let label = case["label"].as_str().unwrap();
            let stream = stream_of(&case["rng_stream"]);
            let (queue, mut rng) = replay_rng(&stream);
            let mut signer = ABogus::new(
                case["user_agent"].as_str().unwrap(),
                case["browser_info"].as_str().unwrap(),
                &mut rng,
            );
            let got = signer.get_value(
                case["query"].as_str().unwrap(),
                case["body"].as_str().unwrap(),
                case["content_type"].as_str().unwrap(),
                case["now_ms"].as_u64().unwrap(),
            );
            assert_eq!(got, case["want"].as_str().unwrap(), "abogus[{label}]");
            assert!(queue.borrow().is_empty(), "abogus[{label}] rng 流未用尽");
        }
    }

    /// 端到端 flow：urlencode → a_bogus（值 quote(safe='') 回填）→ 拆对
    /// 重序列化 + `verifyFp`/`fp` → websign.sign，四个 want 全对拍。
    #[test]
    fn flow_vector_matches_python() {
        let data = vectors();
        let case = &data["flow"].as_array().expect("flow 数组")[0];
        let params = pairs_of(&case["params"]);
        let query = urlencode(&params);
        let stream = stream_of(&case["rng_stream"]);
        let (queue, mut rng) = replay_rng(&stream);
        let mut signer = ABogus::new(
            case["user_agent"].as_str().unwrap(),
            DEFAULT_BROWSER_INFO,
            &mut rng,
        );
        let bogus = signer.get_value(&query, "", "", case["now_ms"].as_u64().unwrap());
        assert_eq!(
            bogus,
            case["want_a_bogus"].as_str().unwrap(),
            "flow a_bogus"
        );
        assert!(queue.borrow().is_empty(), "flow rng 流未用尽");

        let query2 = format!("{query}&a_bogus={}", quote_strict(&bogus));
        let mut pairs = pairs_from_query(&query2);
        let verify_fp = case["verify_fp"].as_str().unwrap();
        for name in VERIFY_FP_PARAMS {
            pairs.push(((*name).to_owned(), verify_fp.to_owned()));
        }
        let signed = sign(
            &pairs,
            case["uifid"].as_str().unwrap(),
            case["timestamp"].as_i64().unwrap(),
        );
        assert_eq!(
            signed.query,
            case["want_query"].as_str().unwrap(),
            "flow query"
        );
        assert_eq!(
            signed.signature,
            case["want_signature"].as_str().unwrap(),
            "flow signature"
        );
        let want_headers = case["want_headers"].as_object().expect("headers 是对象");
        assert_eq!(signed.headers.len(), want_headers.len(), "flow headers");
        for (name, value) in &signed.headers {
            assert_eq!(
                value,
                want_headers.get(name).and_then(Value::as_str).unwrap_or(""),
                "flow header {name}"
            );
        }
    }

    /// 向量里的每个签名都必须通过结构校验，且解码回来时钟 / `page_id` /
    /// `aid` / 几何串与生成输入一致（`ink` 恰好慢一毫秒）。
    #[test]
    fn vector_signatures_pass_structural_checks() {
        let data = vectors();
        for case in data["abogus"].as_array().expect("abogus 数组") {
            let label = case["label"].as_str().unwrap();
            let want = case["want"].as_str().unwrap();
            assert_eq!(structure_error(want), None, "structure_error[{label}]");
            let decoded = decode(want).expect("want 可解码");
            assert_eq!(
                decoded.now_ms,
                case["now_ms"].as_u64().unwrap(),
                "now_ms[{label}]"
            );
            assert_eq!(
                decoded.ink_ms,
                case["now_ms"].as_u64().unwrap() - 1,
                "ink_ms[{label}]"
            );
            assert_eq!(decoded.page_id, PAGE_ID, "page_id[{label}]");
            assert_eq!(decoded.aid, AID, "aid[{label}]");
            assert_eq!(
                decoded.browser_info,
                case["browser_info"].as_str().unwrap(),
                "browser_info[{label}]"
            );
        }
    }

    /// 噪声层可逆：`unmask_pair(mask_pair(p)) == p`、`collapse(expand(b)) == b`。
    #[test]
    fn noise_layers_round_trip() {
        let mut rng = || 0.5f64;
        let pair = mask_pair(HEADER_MAGIC, &mut rng, None, None);
        assert_eq!(unmask_pair(&pair), HEADER_MAGIC);
        let pair = mask_pair(
            (SDK_VERSION[2], SDK_VERSION[3]),
            &mut rng,
            Some(0x12),
            Some(0x34),
        );
        assert_eq!(unmask_pair(&pair), (SDK_VERSION[2], SDK_VERSION[3]));

        // 97 字节 = 32 组 + 1 字节尾巴，走 SDK 收尾分支。
        let body: Vec<u8> = (0u8..=96).collect();
        let frame = expand_noise(&body, &mut rng);
        assert_eq!(frame.len(), 129);
        assert_eq!(collapse_noise(&frame), body);
    }

    /// `multipart/form-data`（大小写不敏感）时 body 哈希前被置空——与
    /// 「空体签名」逐字节相同。
    #[test]
    fn multipart_body_is_blanked_before_hashing() {
        let data = vectors();
        let case = &data["abogus"].as_array().expect("abogus 数组")[0];
        let query = case["query"].as_str().unwrap();
        let now_ms = case["now_ms"].as_u64().unwrap();
        let ua = case["user_agent"].as_str().unwrap();
        let info = case["browser_info"].as_str().unwrap();
        let stream = stream_of(&case["rng_stream"]);

        let (queue_a, mut rng_a) = replay_rng(&stream);
        let mut with_body = ABogus::new(ua, info, &mut rng_a);
        let signed_form = with_body.get_value(
            query,
            "ignored=body",
            "Multipart/Form-Data; boundary=x",
            now_ms,
        );
        assert!(queue_a.borrow().is_empty(), "multipart rng 流未用尽");

        let (queue_b, mut rng_b) = replay_rng(&stream);
        let mut no_body = ABogus::new(ua, info, &mut rng_b);
        let signed_empty = no_body.get_value(query, "", "", now_ms);
        assert!(queue_b.borrow().is_empty(), "empty rng 流未用尽");

        assert_eq!(signed_form, signed_empty);
        assert_eq!(signed_form, case["want"].as_str().unwrap());
    }

    /// 时钟早于双周期纪元直接拒绝（Python `ValueError` 语义）。
    #[test]
    #[should_panic(expected = "clock is before the a_bogus epoch")]
    fn rejects_a_clock_before_the_epoch() {
        let mut rng = || 0.5f64;
        let mut signer = ABogus::new("ua", DEFAULT_BROWSER_INFO, &mut rng);
        let _ = signer.get_value("", "", "", FORTNIGHT_EPOCH_MS - 1);
    }
}
