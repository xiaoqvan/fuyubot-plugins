use super::*;

const KEY: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
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
