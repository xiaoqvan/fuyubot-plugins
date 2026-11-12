//! 汽水的两层解密：`play_auth`（spade）还原密钥 + MP4/CENC 音频解密。
//!
//! ## 第一层：play_auth → AES 密钥
//!
//! 汽水下发的流如果是加密的，`play_auth` 是一段 base64：先按 spade 的异或/取模
//! 规则还原出一串十六进制，再解码成 16 字节 AES-128 密钥。
//!
//! ## 第二层：CENC 样本解密
//!
//! 加密的 MP4 把每个样本的 IV 放在 `senc` box 里，用 AES-128-**CTR** 逐样本解密
//! `mdat`；`stsd` 里样本格式是 `enca`，解密后要按 `frma` 改回原始编码（如 `mp4a` /
//! `fLaC`），否则播放器不认。
//!
//! 主路径是**流式**解密（[`decrypt_file_cancellable`]）：只把 `mdat` 之前的前导 box
//! 读进内存，`mdat` 按样本逐块读、解、写，峰值内存与音频大小无关；`moov` 在 `mdat`
//! 之后的少数文件才退回整份解密。
//!
//! 这是 CPU + 磁盘密集的**阻塞**操作，调用方要放进 `tokio::task::spawn_blocking`；
//! 循环里逐样本检查取消标志，插件 `stop` 之后最坏只多解一个样本。样本表来自文件头，
//! 全部做了上限钳制，损坏文件不会触发巨量分配。

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

/// `senc` 里每个样本最多允许的条数。正常音频几万条就顶天了。
const MAX_SAMPLES: usize = 4 * 1024 * 1024;

/// 流式解密允许缓存在内存里的「mdat 之前」字节上限（正常只有几十 KB）。
const MAX_PREFIX_BYTES: usize = 8 * 1024 * 1024;

/// MP4 box 的最小视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mp4Box {
    /// box 起始偏移。
    offset: usize,
    /// 整个 box 的长度（含头）。
    size: usize,
    /// 内容起始偏移。
    data_start: usize,
}

impl Mp4Box {
    fn data<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[self.data_start..self.offset + self.size]
    }
}

/// 一个 `senc` 样本：IV + 可选的 subsample 描述。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sample {
    iv: Vec<u8>,
    /// `(明文长度, 密文长度)`。
    subsamples: Vec<(u16, u32)>,
}

/// 从 `play_auth` 还原 16 字节 AES 密钥。
pub fn key_from_play_auth(play_auth: &str) -> Result<[u8; 16], String> {
    let hex_key = extract_key(play_auth)?;
    let key =
        hex_decode(&hex_key).ok_or_else(|| "play_auth 里的密钥不是合法十六进制".to_owned())?;
    key.try_into().map_err(|_| {
        format!(
            "play_auth 解出的密钥长度是 {} 字节，不是 16",
            hex_key.len() / 2
        )
    })
}

/// 用 `play_auth` 解一整份加密音频，返回明文 MP4。
pub fn decrypt_audio(data: &[u8], play_auth: &str) -> Result<Vec<u8>, String> {
    let key = key_from_play_auth(play_auth)?;
    decrypt_audio_with_key(data, &key)
}

/// 用现成的 AES 密钥解一整份加密音频（输入只读，内部复制一份）。
pub fn decrypt_audio_with_key(data: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, String> {
    let mut owned = data.to_vec();
    decrypt_in_place(&mut owned, key)?;
    Ok(owned)
}

/// 流式解密到文件：读一点、解一点、写一点，峰值内存与音频大小无关。
///
/// 无损整曲 30~80MB、加密 MV 可能更大，`decrypt_audio` 那种「整份进内存」在
/// 手机 / 小内存机器上不合适。这里只把 **`mdat` 之前的前导 box**（通常几十 KB）
/// 读进内存，`mdat` 按样本大小逐块读、解、写。
///
/// `moov` 排在 `mdat` 之后的少数文件退回整份解密（会 log 一条 warn）。
///
/// 这是**阻塞**操作（CPU + 磁盘），调用方要放进 `tokio::task::spawn_blocking`；
/// 需要「`stop` 后尽快退出」就用 [`decrypt_file_cancellable`]。
pub fn decrypt_file(input: &Path, output: &Path, play_auth: &str) -> Result<(), String> {
    decrypt_file_cancellable(input, output, play_auth, &AtomicBool::new(false))
}

/// 同 [`decrypt_file`]，但每一步检查取消标志（插件 `stop` 后尽快返回）。
///
/// 取消时删掉半成品输出文件，并返回可读错误——调用方按「解密失败」处理即可。
pub fn decrypt_file_cancellable(
    input: &Path,
    output: &Path,
    play_auth: &str,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let key = key_from_play_auth(play_auth)?;
    let file = File::open(input)
        .map_err(|error| format!("打不开加密音频 {}：{error}", input.display()))?;
    let mut reader = BufReader::new(file);

    // 1) 读到 mdat 为止的前导 box。
    let mut prefix = Vec::new();
    let mut mdat_header = Vec::new();
    let mdat_payload_len: u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(output);
            return Err("插件正在停止，解密已取消".to_owned());
        }
        let mut head = [0u8; 8];
        reader
            .read_exact(&mut head)
            .map_err(|error| format!("读 box 头失败：{error}"))?;
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let kind = &head[4..8];
        let (size, header_len, ext) = if size32 == 1 {
            let mut ext = [0u8; 8];
            reader
                .read_exact(&mut ext)
                .map_err(|error| format!("读 box 扩展头失败：{error}"))?;
            (u64::from_be_bytes(ext), 16usize, Some(ext))
        } else if size32 == 0 {
            // size=0：一直到文件末尾，只可能是最后一个 box（mdat 常见这种写法）。
            let remaining = std::fs::metadata(input)
                .map(|meta| meta.len())
                .unwrap_or(0)
                .saturating_sub(prefix.len() as u64 + 8);
            (remaining, 8usize, None)
        } else {
            (size32 as u64, 8usize, None)
        };
        if size < header_len as u64 {
            return Err("box 长度非法".to_owned());
        }

        if kind == b"mdat" {
            mdat_header.extend_from_slice(&head);
            if let Some(ext) = ext {
                mdat_header.extend_from_slice(&ext);
            }
            mdat_payload_len = size - header_len as u64;
            break;
        }

        if size > MAX_PREFIX_BYTES as u64 - prefix.len() as u64 {
            return Err("mdat 之前的前导 box 太大".to_owned());
        }
        let payload_len = (size - header_len as u64) as usize;
        let mut payload = vec![0u8; payload_len];
        reader
            .read_exact(&mut payload)
            .map_err(|error| format!("读 box 内容失败：{error}"))?;
        prefix.extend_from_slice(&head);
        if let Some(ext) = ext {
            prefix.extend_from_slice(&ext);
        }
        prefix.extend_from_slice(&payload);
    }

    // 2) `moov` 在 mdat 之后：退回整份解密（少见，但别直接判失败）。
    if find_box(&prefix, b"moov", 0, prefix.len()).is_none() {
        if cancel.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(output);
            return Err("插件正在停止，解密已取消".to_owned());
        }
        crate::log::warn("加密音频的 moov 在 mdat 之后，退回整份解密");
        let mut rest = Vec::new();
        reader
            .read_to_end(&mut rest)
            .map_err(|error| format!("读剩余数据失败：{error}"))?;
        let mut full = prefix;
        full.extend_from_slice(&mdat_header);
        full.extend_from_slice(&rest);
        decrypt_in_place(&mut full, &key)?;
        std::fs::write(output, &full).map_err(|error| format!("写解密结果失败：{error}"))?;
        return Ok(());
    }

    // 3) 从前导 box 里解出样本表，并把 stsd 的 `enca` 改回原始编码。
    let moov = find_box(&prefix, b"moov", 0, prefix.len())
        .ok_or_else(|| "加密音频缺少 moov".to_owned())?;
    let stbl = find_box_deep(&prefix, b"stbl", moov.data_start, moov.offset + moov.size)
        .ok_or_else(|| "加密音频缺少 stbl".to_owned())?;
    let stbl_end = stbl.offset + stbl.size;
    let stsz = find_box(&prefix, b"stsz", stbl.data_start, stbl_end)
        .ok_or_else(|| "加密音频缺少 stsz（样本表）".to_owned())?;
    let sizes = parse_stsz(stsz.data(&prefix))?;
    let senc = find_box(&prefix, b"senc", stbl.data_start, stbl_end)
        .or_else(|| find_box_deep(&prefix, b"senc", moov.data_start, moov.offset + moov.size))
        .ok_or_else(|| "加密音频缺少 senc（样本 IV）".to_owned())?;
    let iv_size = per_sample_iv_size(&prefix, stbl.data_start, stbl_end);
    let samples = parse_senc(senc.data(&prefix), iv_size)?;
    if samples.len() < sizes.len() {
        return Err(format!(
            "senc 只有 {} 个样本，stsz 声明了 {} 个——文件不完整",
            samples.len(),
            sizes.len()
        ));
    }

    if let Some(stsd) = find_box(&prefix, b"stsd", stbl.data_start, stbl_end) {
        let start = stsd.data_start;
        let end = stsd.offset + stsd.size;
        let patch = find_subslice(&prefix[start..end], b"enca").map(|relative| {
            (
                start + relative,
                encrypted_sample_original_format(&prefix[start..end]),
            )
        });
        if let Some((target, original)) = patch
            && target + 4 <= prefix.len()
        {
            prefix[target..target + 4].copy_from_slice(&original);
        }
    }

    // 4) 先写前导 box + mdat 头，再逐样本读、解、写。
    let out = File::create(output)
        .map_err(|error| format!("建不了解密输出 {}：{error}", output.display()))?;
    let mut writer = BufWriter::new(out);
    writer
        .write_all(&prefix)
        .and_then(|()| writer.write_all(&mdat_header))
        .map_err(|error| format!("写解密结果失败：{error}"))?;

    let mut remaining = mdat_payload_len;
    for (index, size) in sizes.iter().enumerate() {
        // 每样本检查一次取消：`stop` 之后最坏只多解一个样本。
        if cancel.load(Ordering::SeqCst) {
            drop(writer);
            let _ = std::fs::remove_file(output);
            return Err("插件正在停止，解密已取消".to_owned());
        }
        let size = *size as u64;
        if size > remaining {
            return Err("样本表超出 mdat——文件不完整".to_owned());
        }
        let mut chunk = vec![0u8; size as usize];
        reader
            .read_exact(&mut chunk)
            .map_err(|error| format!("读 mdat 失败：{error}"))?;
        remaining -= size;
        if let Some(sample) = samples.get(index) {
            decrypt_sample(&key, &mut chunk, sample);
        }
        writer
            .write_all(&chunk)
            .map_err(|error| format!("写解密结果失败：{error}"))?;
    }

    // 样本没覆盖满的部分（对齐填充 / mdat 之后的 box）原样透传。
    std::io::copy(&mut reader, &mut writer).map_err(|error| format!("写剩余数据失败：{error}"))?;
    writer
        .flush()
        .map_err(|error| format!("刷盘失败：{error}"))?;
    Ok(())
}

/// 原地解密：`mdat` 的每个样本按 `senc` 描述做 AES-128-CTR，再把 `enca` 换回原始编码。
fn decrypt_in_place(data: &mut [u8], key: &[u8; 16]) -> Result<(), String> {
    let moov = find_box(data, b"moov", 0, data.len())
        .ok_or_else(|| "不是可解密的音频：找不到 moov（可能已经是明文流）".to_owned())?;
    let stbl = find_box_deep(data, b"stbl", moov.data_start, moov.offset + moov.size)
        .ok_or_else(|| "加密音频缺少 stbl".to_owned())?;
    let stbl_end = stbl.offset + stbl.size;

    let stsz = find_box(data, b"stsz", stbl.data_start, stbl_end)
        .ok_or_else(|| "加密音频缺少 stsz（样本表）".to_owned())?;
    let sizes = parse_stsz(stsz.data(data))?;

    // senc 常在 stbl 里，个别封装放在 moov 下。
    let senc = find_box(data, b"senc", stbl.data_start, stbl_end)
        .or_else(|| find_box_deep(data, b"senc", moov.data_start, moov.offset + moov.size))
        .ok_or_else(|| "加密音频缺少 senc（样本 IV）".to_owned())?;
    let iv_size = per_sample_iv_size(data, stbl.data_start, stbl_end);
    let samples = parse_senc(senc.data(data), iv_size)?;
    if samples.len() < sizes.len() {
        return Err(format!(
            "senc 只有 {} 个样本，stsz 声明了 {} 个——文件不完整",
            samples.len(),
            sizes.len()
        ));
    }

    let mdat =
        find_box(data, b"mdat", 0, data.len()).ok_or_else(|| "加密音频缺少 mdat".to_owned())?;

    // 逐样本解密；CTR 的计数器在「同一个样本的密文段」之间连续，跨样本重置。
    let mut reader = mdat.data_start;
    let mdat_end = mdat.offset + mdat.size;
    for (index, size) in sizes.iter().enumerate() {
        let size = *size as usize;
        if reader + size > mdat_end {
            return Err("样本表超出 mdat——文件不完整".to_owned());
        }
        let chunk = &mut data[reader..reader + size];
        if let Some(sample) = samples.get(index) {
            decrypt_sample(key, chunk, sample);
        }
        reader += size;
    }
    if reader != mdat_end {
        return Err("样本没有覆盖满 mdat——文件不完整或不是 CENC".to_owned());
    }

    // `enca` → `frma` 指向的原始编码。
    if let Some(stsd) = find_box(data, b"stsd", stbl.data_start, stbl_end) {
        let start = stsd.data_start;
        let end = stsd.offset + stsd.size;
        let patch = find_subslice(&data[start..end], b"enca").map(|relative| {
            (
                start + relative,
                encrypted_sample_original_format(&data[start..end]),
            )
        });
        if let Some((target, original)) = patch
            && target + 4 <= data.len()
        {
            data[target..target + 4].copy_from_slice(&original);
        }
    }

    Ok(())
}

// ── spade ───────────────────────────────────────────────────────────

/// 从 `play_auth` 还原十六进制密钥串。
pub fn extract_key(play_auth: &str) -> Result<String, String> {
    let raw = BASE64
        .decode(play_auth.trim())
        .map_err(|error| format!("play_auth 不是合法 base64：{error}"))?;
    if raw.len() < 3 {
        return Err("play_auth 太短".to_owned());
    }
    let padding = (raw[0] ^ raw[1] ^ raw[2]) as i32 - 48;
    if padding < 0 {
        return Err("play_auth 的填充长度非法".to_owned());
    }
    let padding = padding as usize;
    if raw.len() < padding + 2 {
        return Err("play_auth 的填充长度超出数据".to_owned());
    }

    let inner = &raw[1..raw.len() - padding];
    let decoded = spade_inner(inner);
    if decoded.is_empty() {
        return Err("play_auth 还原失败".to_owned());
    }

    let skip =
        base36(decoded[0]).ok_or_else(|| "play_auth 的跳过字符不是 base36".to_owned())? as usize;
    // `skip` 来自不可信数据：用 checked_sub，别让 usize 下溢在 debug 下 panic
    // （`spawn_blocking` 里的 panic 会变成 JoinError，但没必要让它发生）。
    let end = (1 + (raw.len() - padding - 2))
        .checked_sub(skip)
        .ok_or_else(|| "play_auth 的索引越界".to_owned())?;
    if end < 1 || end > decoded.len() {
        return Err("play_auth 的索引越界".to_owned());
    }
    Ok(String::from_utf8_lossy(&decoded[1..end]).to_string())
}

/// 对 `inner` 逐字节做异或 / 减 popcount / 回卷。
fn spade_inner(inner: &[u8]) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(inner.len() + 2);
    buffer.push(0xFA);
    buffer.push(0x55);
    buffer.extend_from_slice(inner);

    let mut out = Vec::with_capacity(inner.len());
    for index in 0..inner.len() {
        let mut value = (inner[index] ^ buffer[index]) as i32 - index.count_ones() as i32 - 21;
        while value < 0 {
            value += 255;
        }
        out.push(value as u8);
    }
    out
}

/// base36 数字（大小写都认；非法字符返回 `None`，不要编一个 255 出来）。
fn base36(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'z' => Some(byte - b'a' + 10),
        b'A'..=b'Z' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push((high * 16 + low) as u8);
    }
    Some(out)
}

// ── MP4 结构 ────────────────────────────────────────────────────────

/// 同层查找：只在 `[start, end)` 这一层的 box 里找。
fn find_box(data: &[u8], kind: &[u8; 4], start: usize, end: usize) -> Option<Mp4Box> {
    let end = end.min(data.len());
    let mut pos = start;
    while pos + 8 <= end {
        let size32 = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        // `size == 0` 在 MP4 里表示「一直到文件末尾」（只允许出现在最后一个 box）。
        let mut size = if size32 == 0 { end - pos } else { size32 };
        let mut header = 8usize;
        if size32 == 1 {
            if pos + 16 > end {
                return None;
            }
            size = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().ok()?) as usize;
            header = 16;
        }
        if size < header || pos + size > end {
            return None;
        }
        if &data[pos + 4..pos + 8] == kind {
            return Some(Mp4Box {
                offset: pos,
                size,
                data_start: pos + header,
            });
        }
        pos += size;
    }
    None
}

/// 递归进入容器 box 查找。
fn find_box_deep(data: &[u8], kind: &[u8; 4], start: usize, end: usize) -> Option<Mp4Box> {
    find_box_deep_at(data, kind, start, end, 0)
}

/// 带深度上限的递归：待解密的 MP4 来自远端 CDN，深嵌套容器可以打爆线程栈
/// （cdylib 里栈溢出是进程级 abort，会带走整个宿主）。
fn find_box_deep_at(
    data: &[u8],
    kind: &[u8; 4],
    start: usize,
    end: usize,
    depth: usize,
) -> Option<Mp4Box> {
    const MAX_DEPTH: usize = 32;
    if depth >= MAX_DEPTH {
        return None;
    }
    let end = end.min(data.len());
    let mut pos = start;
    while pos + 8 <= end {
        let size32 = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let mut size = if size32 == 0 { end - pos } else { size32 };
        let mut header = 8usize;
        if size32 == 1 {
            if pos + 16 > end {
                return None;
            }
            size = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().ok()?) as usize;
            header = 16;
        }
        if size < header || pos + size > end {
            return None;
        }
        if &data[pos + 4..pos + 8] == kind {
            return Some(Mp4Box {
                offset: pos,
                size,
                data_start: pos + header,
            });
        }
        if let Some(child_start) = child_start(&data[pos + 4..pos + 8], pos, header)
            && child_start < pos + size
            && let Some(found) = find_box_deep_at(data, kind, child_start, pos + size, depth + 1)
        {
            return Some(found);
        }
        pos += size;
    }
    None
}

/// 哪些 box 带子 box、子 box 从哪里开始。
fn child_start(kind: &[u8], offset: usize, header: usize) -> Option<usize> {
    match kind {
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"sinf" | b"schi" => {
            Some(offset + header)
        }
        // stsd: version/flags(4) + entry_count(4)
        b"stsd" => Some(offset + header + 8),
        // enca / mp4a: 采样描述内部固定 28 字节后才是子 box
        b"enca" | b"mp4a" | b"alac" | b"fLaC" => Some(offset + header + 28),
        _ => None,
    }
}

fn parse_stsz(data: &[u8]) -> Result<Vec<u32>, String> {
    if data.len() < 12 {
        return Err("stsz 太短".to_owned());
    }
    let fixed = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
    let declared = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;
    let count = if fixed != 0 {
        declared.min(MAX_SAMPLES)
    } else {
        declared
            .min(data.len().saturating_sub(12) / 4)
            .min(MAX_SAMPLES)
    };
    if count == 0 {
        return Err("stsz 声明了 0 个样本".to_owned());
    }
    let mut sizes = vec![0u32; count];
    if fixed != 0 {
        sizes.fill(fixed as u32);
    } else {
        for (index, slot) in sizes.iter_mut().enumerate() {
            let start = 12 + index * 4;
            *slot = u32::from_be_bytes(data[start..start + 4].try_into().unwrap());
        }
    }
    Ok(sizes)
}

fn parse_senc(data: &[u8], iv_size: usize) -> Result<Vec<Sample>, String> {
    if data.len() < 8 {
        return Err("senc 太短".to_owned());
    }
    let iv_size = if iv_size == 8 || iv_size == 16 {
        iv_size
    } else {
        8
    };
    let flags = u32::from_be_bytes(data[0..4].try_into().unwrap()) & 0x00FF_FFFF;
    let has_subsamples = flags & 0x02 != 0;
    let declared = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
    let per_sample = if has_subsamples { iv_size + 2 } else { iv_size };
    let count = declared
        .min(data.len().saturating_sub(8) / per_sample.max(1))
        .min(MAX_SAMPLES);

    let mut samples = Vec::with_capacity(count);
    let mut pos = 8;
    for _ in 0..count {
        if pos + iv_size > data.len() {
            break;
        }
        let mut sample = Sample {
            iv: data[pos..pos + iv_size].to_vec(),
            subsamples: Vec::new(),
        };
        pos += iv_size;
        if has_subsamples {
            if pos + 2 > data.len() {
                break;
            }
            let count = u16::from_be_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
            pos += 2;
            if pos + count * 6 > data.len() {
                break;
            }
            for _ in 0..count {
                sample.subsamples.push((
                    u16::from_be_bytes(data[pos..pos + 2].try_into().unwrap()),
                    u32::from_be_bytes(data[pos + 2..pos + 6].try_into().unwrap()),
                ));
                pos += 6;
            }
        }
        samples.push(sample);
    }
    Ok(samples)
}

/// `tenc` 里声明的 IV 长度（8 或 16，缺省 8）。
fn per_sample_iv_size(data: &[u8], start: usize, end: usize) -> usize {
    match find_box_deep(data, b"tenc", start, end) {
        Some(tenc) if tenc.size >= tenc.data_start - tenc.offset + 8 => {
            let iv = data[tenc.data_start + 7];
            if iv == 8 || iv == 16 { iv as usize } else { 8 }
        }
        _ => 8,
    }
}

/// `stsd` 里的 `enca` 要换回 `frma` 指向的原始格式。
fn encrypted_sample_original_format(stsd: &[u8]) -> [u8; 4] {
    let Some(index) = find_subslice(stsd, b"frma") else {
        return *b"mp4a";
    };
    if index < 4 || index + 8 > stsd.len() {
        return *b"mp4a";
    }
    let size = u32::from_be_bytes(stsd[index - 4..index].try_into().unwrap()) as usize;
    if size < 12 || index - 4 + size > stsd.len() {
        return *b"mp4a";
    }
    stsd[index + 4..index + 8].try_into().unwrap_or(*b"mp4a")
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ── AES-CTR ─────────────────────────────────────────────────────────

/// AES-128-CTR（128 位大端计数器），对应 CENC 的 `cenc` scheme。
struct AesCtr {
    cipher: Aes128,
    counter: [u8; 16],
    keystream: [u8; 16],
    position: usize,
}

impl AesCtr {
    fn new(key: &[u8; 16], iv: &[u8; 16]) -> Self {
        Self {
            cipher: Aes128::new(GenericArray::from_slice(key)),
            counter: *iv,
            keystream: [0; 16],
            position: 16,
        }
    }

    fn next_block(&mut self) {
        let mut block = GenericArray::clone_from_slice(&self.counter);
        self.cipher.encrypt_block(&mut block);
        self.keystream.copy_from_slice(&block);
        self.position = 0;
        for byte in self.counter.iter_mut().rev() {
            let (value, carry) = byte.overflowing_add(1);
            *byte = value;
            if !carry {
                break;
            }
        }
    }

    fn apply(&mut self, data: &mut [u8]) {
        for byte in data {
            if self.position == 16 {
                self.next_block();
            }
            *byte ^= self.keystream[self.position];
            self.position += 1;
        }
    }
}

fn decrypt_sample(key: &[u8; 16], chunk: &mut [u8], sample: &Sample) {
    let mut iv = [0u8; 16];
    let len = sample.iv.len().min(16);
    iv[..len].copy_from_slice(&sample.iv[..len]);
    let mut cipher = AesCtr::new(key, &iv);

    if sample.subsamples.is_empty() {
        cipher.apply(chunk);
        return;
    }

    let mut position = 0usize;
    for (clear, encrypted) in &sample.subsamples {
        let clear = (*clear as usize).min(chunk.len().saturating_sub(position));
        position += clear;
        if position >= chunk.len() {
            break;
        }
        let encrypted = (*encrypted as usize).min(chunk.len().saturating_sub(position));
        cipher.apply(&mut chunk[position..position + encrypted]);
        position += encrypted;
        if position >= chunk.len() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    /// 用同一套 CTR 产生密文（解密 = 加密）。
    fn ctr(data: &[u8], iv8: &[u8; 8]) -> Vec<u8> {
        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(iv8);
        let mut cipher = AesCtr::new(&KEY, &iv);
        let mut out = data.to_vec();
        cipher.apply(&mut out);
        out
    }

    /// 一个最小的 CENC 文件：ftyp + moov(trak(mdia(minf(stbl(stsd, stsz, senc, sinf(tenc)))))) + mdat。
    fn build_mp4(sizes: &[u32], senc: &[u8], mdat: &[u8], iv_size: u8) -> Vec<u8> {
        let mut stsz = vec![0u8; 12];
        stsz[4..8].copy_from_slice(&0u32.to_be_bytes()); // 变长
        stsz[8..12].copy_from_slice(&(sizes.len() as u32).to_be_bytes());
        for size in sizes {
            stsz.extend_from_slice(&size.to_be_bytes());
        }

        let frma = mp4_box(b"frma", b"mp4a");
        let mut enca_payload = vec![0u8; 28];
        enca_payload.extend_from_slice(&frma);
        let enca = mp4_box(b"enca", &enca_payload);
        let mut stsd = vec![0u8; 8];
        stsd[4..8].copy_from_slice(&1u32.to_be_bytes());
        stsd.extend_from_slice(&enca);

        let mut tenc_payload = vec![0u8; 8];
        tenc_payload[7] = iv_size;
        let tenc = mp4_box(b"tenc", &tenc_payload);
        let sinf = mp4_box(b"sinf", &mp4_box(b"schi", &tenc));

        let mut stbl_payload = Vec::new();
        stbl_payload.extend_from_slice(&mp4_box(b"stsd", &stsd));
        stbl_payload.extend_from_slice(&mp4_box(b"stsz", &stsz));
        stbl_payload.extend_from_slice(&mp4_box(b"senc", senc));
        stbl_payload.extend_from_slice(&sinf);

        let stbl = mp4_box(b"stbl", &stbl_payload);
        let minf = mp4_box(b"minf", &stbl);
        let mdia = mp4_box(b"mdia", &minf);
        let trak = mp4_box(b"trak", &mdia);
        let moov = mp4_box(b"moov", &trak);

        let mut file = mp4_box(b"ftyp", b"isom\0\0\0\0isom");
        file.extend_from_slice(&moov);
        file.extend_from_slice(&mp4_box(b"mdat", mdat));
        file
    }

    /// 逆向 spade，构造一个能还原出 `key_hex` 的 play_auth。
    fn build_play_auth(key_hex: &str, skip: u8) -> String {
        let mut tmp: Vec<u8> = Vec::new();
        tmp.push(b'0' + skip);
        tmp.extend_from_slice(key_hex.as_bytes());
        tmp.resize(tmp.len() + skip as usize, 0);

        let desired = |index: usize| -> u8 {
            let value = tmp[index] as i32 + (index as u32).count_ones() as i32 + 21;
            (value % 255) as u8
        };

        let mut inner = vec![0u8; tmp.len()];
        inner[0] = desired(0) ^ 0xFA;
        if tmp.len() > 1 {
            inner[1] = desired(1) ^ 0x55;
        }
        for index in 2..tmp.len() {
            inner[index] = desired(index) ^ inner[index - 2];
        }

        // padding 必须为 0：要求 b0 ^ b1 ^ b2 == 48（'0'）
        let b1 = inner[0];
        let b2 = inner[1];
        let b0 = 0x30u8 ^ b1 ^ b2;
        let mut raw = vec![b0, b1, b2];
        raw.extend_from_slice(&inner[2..]);
        BASE64.encode(raw)
    }

    #[test]
    fn play_auth_round_trips_to_the_hex_key() {
        let key_hex = "00112233445566778899aabbccddeeff";
        for skip in [0u8, 1, 3] {
            let auth = build_play_auth(key_hex, skip);
            assert_eq!(extract_key(&auth).unwrap(), key_hex, "skip={skip}");
            assert_eq!(key_from_play_auth(&auth).unwrap(), KEY);
        }
    }

    /// 这条 play_auth 由独立的 Python 实现按 spade 规则生成（不是用本文件的
    /// `build_play_auth` 反推的），避免「自己编码、自己解码」掩盖同一个错误。
    #[test]
    fn play_auth_vector_from_an_independent_encoder() {
        let auth = "kbIT9FuzEvpZsxL4X7QR+kG3DvhfqCbRXagm01uvJdGlrbK6qg==";
        assert_eq!(
            extract_key(auth).unwrap(),
            "00112233445566778899aabbccddeeff"
        );
        assert_eq!(key_from_play_auth(auth).unwrap(), KEY);
    }

    #[test]
    fn base36_accepts_both_cases_and_rejects_junk() {
        assert_eq!(base36(b'0'), Some(0));
        assert_eq!(base36(b'9'), Some(9));
        assert_eq!(base36(b'a'), Some(10));
        assert_eq!(base36(b'Z'), Some(35));
        assert_eq!(base36(b'-'), None);
        assert_eq!(base36(0xFF), None);
    }

    /// 构造一个「跳过字符 35、总长却不够」的 play_auth：旧实现会在 debug 下
    /// usize 下溢 panic，现在必须是 Err。
    #[test]
    fn a_skip_longer_than_the_payload_is_an_error_not_a_panic() {
        let inner0 = (b'z' as i32 + 21) as u8 ^ 0xFA;
        let inner = [inner0, 0x11, 0x22];
        let b1 = inner[0];
        let b2 = inner[1];
        let b0 = 0x30u8 ^ b1 ^ b2;
        let mut raw = vec![b0];
        raw.extend_from_slice(&inner);
        let auth = BASE64.encode(raw);
        let error = extract_key(&auth).unwrap_err();
        assert!(error.contains("越界"), "{error}");
    }

    #[test]
    fn deeply_nested_containers_stop_at_the_depth_limit() {
        let mut data = mp4_box(b"mdat", &[0u8; 8]);
        for _ in 0..64 {
            data = mp4_box(b"moov", &data);
        }
        assert!(find_box_deep(&data, b"mdat", 0, data.len()).is_none());
    }

    #[test]
    fn malformed_play_auth_is_rejected() {
        assert!(extract_key("").is_err());
        assert!(extract_key("!!!").is_err());
        assert!(extract_key("AAAA").is_err());
    }

    #[test]
    fn ctr_round_trip_matches_encrypt_decrypt() {
        let plaintext: Vec<u8> = (0..100u8).collect();
        let iv = [7u8; 8];
        let ciphertext = ctr(&plaintext, &iv);
        assert_ne!(ciphertext, plaintext);
        assert_eq!(ctr(&ciphertext, &iv), plaintext);
    }

    #[test]
    fn sample_decryption_handles_subsamples_and_counter_continuity() {
        let iv = [3u8; 8];
        let clear = vec![0xAAu8; 16];
        let secret: Vec<u8> = (0..40u8).map(|value| value.wrapping_mul(3)).collect();
        let mut chunk = clear.clone();
        chunk.extend_from_slice(&ctr(&secret, &iv));

        let sample = Sample {
            iv: iv.to_vec(),
            subsamples: vec![(16, 40)],
        };
        decrypt_sample(&KEY, &mut chunk, &sample);
        assert_eq!(&chunk[..16], clear.as_slice(), "明文段不能被碰");
        assert_eq!(&chunk[16..], secret.as_slice(), "密文段要还原");
    }

    #[test]
    fn whole_file_decryption_restores_samples_and_the_original_format() {
        let ivs: [[u8; 8]; 3] = [[1; 8], [2; 8], [3; 8]];
        let plain: Vec<Vec<u8>> = vec![
            (0..64u8).collect(),
            (0..64u8).map(|value| value.wrapping_add(7)).collect(),
            (0..32u8).map(|value| value.wrapping_mul(5)).collect(),
        ];
        let mut mdat = Vec::new();
        let mut senc = vec![0u8; 8];
        senc[4..8].copy_from_slice(&(ivs.len() as u32).to_be_bytes());
        for (index, sample) in plain.iter().enumerate() {
            mdat.extend_from_slice(&ctr(sample, &ivs[index]));
            senc.extend_from_slice(&ivs[index]);
        }
        let sizes: Vec<u32> = plain.iter().map(|sample| sample.len() as u32).collect();
        let file = build_mp4(&sizes, &senc, &mdat, 8);

        let decrypted = decrypt_audio_with_key(&file, &KEY).expect("解密应当成功");
        let mdat = find_box(&decrypted, b"mdat", 0, decrypted.len()).unwrap();
        assert_eq!(mdat.data(&decrypted), plain.concat().as_slice());

        let stsd = find_box_deep(&decrypted, b"stsd", 0, decrypted.len()).unwrap();
        assert!(!stsd.data(&decrypted).windows(4).any(|w| w == b"enca"));
        assert!(stsd.data(&decrypted).windows(4).any(|w| w == b"mp4a"));
    }

    #[test]
    fn a_zero_sized_last_box_extends_to_the_end_of_the_file() {
        let mut data = mp4_box(b"moov", &[0u8; 8]);
        let payload = [0xA5u8; 32];
        let mut mdat = vec![0u8; 8];
        mdat[4..8].copy_from_slice(b"mdat"); // size = 0 → 到文件末尾
        mdat.extend_from_slice(&payload);
        data.extend_from_slice(&mdat);
        let found = find_box(&data, b"mdat", 0, data.len()).expect("size=0 的 mdat 要能认出来");
        assert_eq!(found.size, 8 + payload.len());
        assert_eq!(found.data(&data), payload.as_slice());
    }

    /// 造一份「3 个样本、其中一个带 subsample」的加密 MP4。
    fn encrypted_fixture() -> Vec<u8> {
        let ivs: [[u8; 8]; 3] = [[1; 8], [2; 8], [3; 8]];
        let clear = vec![0x11u8; 16];
        let secret: Vec<u8> = (0..48u8).map(|value| value.wrapping_mul(7)).collect();
        let plain: Vec<Vec<u8>> = vec![
            (0..128u8).collect(),
            {
                let mut sample = clear.clone();
                sample.extend_from_slice(&secret);
                sample
            },
            (0..96u8).map(|value| value.wrapping_add(3)).collect(),
        ];
        let mut mdat = Vec::new();
        let mut senc = vec![0u8; 8];
        senc[4..8].copy_from_slice(&(ivs.len() as u32).to_be_bytes());
        for (index, sample) in plain.iter().enumerate() {
            let iv = ivs[index];
            if index == 1 {
                mdat.extend_from_slice(&clear);
                mdat.extend_from_slice(&ctr(&secret, &iv));
                senc.extend_from_slice(&iv);
                senc.extend_from_slice(&1u16.to_be_bytes());
                senc.extend_from_slice(&(16u16).to_be_bytes());
                senc.extend_from_slice(&(48u32).to_be_bytes());
            } else {
                mdat.extend_from_slice(&ctr(sample, &iv));
                senc.extend_from_slice(&iv);
            }
        }
        let sizes: Vec<u32> = plain.iter().map(|sample| sample.len() as u32).collect();
        build_mp4(&sizes, &senc, &mdat, 8)
    }

    fn temp_paths() -> (std::path::PathBuf, std::path::PathBuf) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("fuyubot-music-dec-{stamp}"));
        (
            base.with_extension("enc.mp4"),
            base.with_extension("plain.mp4"),
        )
    }

    #[test]
    fn streaming_decryption_matches_the_in_memory_result() {
        let file = encrypted_fixture();
        let expected = decrypt_audio_with_key(&file, &KEY).expect("整份解密");
        let (input, output) = temp_paths();
        std::fs::write(&input, &file).unwrap();
        decrypt_file(
            &input,
            &output,
            &build_play_auth("00112233445566778899aabbccddeeff", 0),
        )
        .expect("流式解密");
        let actual = std::fs::read(&output).unwrap();
        assert_eq!(actual, expected);
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
    }

    #[test]
    fn streaming_decryption_handles_moov_after_mdat() {
        // 把 ftyp + moov + mdat 重排成 ftyp + mdat + moov：流式路径应退回整份解密。
        let file = encrypted_fixture();
        let moov = find_box(&file, b"moov", 0, file.len()).unwrap();
        let mdat = find_box(&file, b"mdat", 0, file.len()).unwrap();
        let ftyp_len = moov.offset;
        let mut reordered = file[..ftyp_len].to_vec();
        reordered.extend_from_slice(&file[mdat.offset..mdat.offset + mdat.size]);
        reordered.extend_from_slice(&file[moov.offset..moov.offset + moov.size]);
        let expected = decrypt_audio_with_key(&reordered, &KEY).expect("整份解密");
        let (input, output) = temp_paths();
        std::fs::write(&input, &reordered).unwrap();
        decrypt_file(
            &input,
            &output,
            &build_play_auth("00112233445566778899aabbccddeeff", 3),
        )
        .expect("回退解密");
        assert_eq!(std::fs::read(&output).unwrap(), expected);
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
    }

    #[test]
    fn decrypting_requires_a_play_auth_key_length_of_16() {
        let short = build_play_auth("00112233", 0);
        assert!(key_from_play_auth(&short).is_err());
    }

    #[test]
    fn truncated_files_fail_instead_of_allocating_absurdly() {
        let mut stsz = vec![0u8; 12];
        stsz[8..12].copy_from_slice(&4_000_000_000u32.to_be_bytes());
        let mut stbl_payload = Vec::new();
        stbl_payload.extend_from_slice(&mp4_box(b"stsz", &stsz));
        stbl_payload.extend_from_slice(&mp4_box(b"senc", &[0u8; 8]));
        let stbl = mp4_box(b"stbl", &stbl_payload);
        let mut file = mp4_box(
            b"moov",
            &mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"minf", &stbl))),
        );
        file.extend_from_slice(&mp4_box(b"mdat", &[0u8; 16]));

        let started = std::time::Instant::now();
        assert!(decrypt_audio_with_key(&file, &KEY).is_err());
        assert!(started.elapsed().as_secs() < 5, "损坏文件不应长时间循环");
    }
}
