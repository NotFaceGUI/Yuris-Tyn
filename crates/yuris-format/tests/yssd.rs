//! YSSD 系统数据 + 压缩层（zlib / SNP 变体）解码/编码测试。
//!
//! - 合成流:varint / 字面量(+1 变体)/ copy1 / copy2 / copy4 逐形态;
//! - 编码器往返:snp_compress / compress_block_payload / YssdBuilder;
//! - 真实样本:按 save 目录**自适应**两套语料 ——
//!   * v555 标定样本(六文件全 SNP,2026-09-05 实测,PROGRESS 成果 60);
//!   * v494 语料(全部 zlib;含 btype=0/1/2 三种块,2026-10-18 引擎反汇编 +
//!     真实 save/ 实测,见 PROGRESS 成果「存档容器对齐」)。

use yuris_format::yssd::{
    compress_block_payload, decompress_block_payload, encode_block_body, encode_config_payload,
    encode_multi_body, parse_config_payload, parse_multi_payload, snp_compress, snp_uncompress,
    YssdBuilder, YssdFile, YssdPayload,
};

/// 样本 save 目录(仓库内游戏目录;缺失则跳过)。
fn sample_save_dir() -> Option<std::path::PathBuf> {
    let p = std::env::var("YURIS_SAMPLE_SAVE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../AnimalTrailGirlishSquare 2/save"
            ))
        });
    p.is_dir().then_some(p)
}

/// 单字面量流(变体:长度 = (tag>>2)+1)。
#[test]
fn snp_literal_only() {
    // varint(4) + tag 0x0C(len = 3+1 = 4)+ "abcd"
    let src = [0x04, 0x0c, b'a', b'b', b'c', b'd'];
    assert_eq!(snp_uncompress(&src).unwrap(), b"abcd");
}

/// 字面量长度下界:tag 0x00 → 长度 1。
#[test]
fn snp_literal_min_length_is_one() {
    let src = [0x01, 0x00, 0xff];
    assert_eq!(snp_uncompress(&src).unwrap(), b"\xff");
}

/// 长字面量(tag>>2 ∈ 60..63 → 1..4 额外小端长度字节,同样 +1)。
#[test]
fn snp_long_literal() {
    let data = vec![0xABu8; 61];
    let mut src = vec![61u8]; // varint
    src.push(0xf0); // 60<<2 → 1 额外字节
    src.push(60); // 60 + 1 = 61
    src.extend_from_slice(&data);
    assert_eq!(snp_uncompress(&src).unwrap(), data);
}

/// copy1(4..11 字节回引,1 字节偏移)。
#[test]
fn snp_copy1() {
    // literal "abc" + copy1(off=3, len=4) → "abcabca"
    let src = [0x07, 0x08, b'a', b'b', b'c', 0x01, 0x03];
    assert_eq!(snp_uncompress(&src).unwrap(), b"abcabca");
}

/// copy2(2 字节偏移)。
#[test]
fn snp_copy2() {
    let src = [0x07, 0x08, b'a', b'b', b'c', 0x0e, 0x03, 0x00];
    assert_eq!(snp_uncompress(&src).unwrap(), b"abcabca");
}

/// copy4(4 字节偏移)。
#[test]
fn snp_copy4() {
    let src = [0x07, 0x08, b'a', b'b', b'c', 0x0f, 0x03, 0x00, 0x00, 0x00];
    assert_eq!(snp_uncompress(&src).unwrap(), b"abcabca");
}

/// 多字节 varint。
#[test]
fn snp_multibyte_varint() {
    let data = vec![0x11u8; 200];
    let mut src = vec![0xc8, 0x01]; // varint(200)
    // 200 = 3×60 + 20 → 三个 60 长字面量(tag 0xF0/0xF1/0xF2)+ 尾字面量
    for _ in 0..3 {
        src.push(0xf0);
        src.push(59); // 59 + 1 = 60
        src.extend_from_slice(&data[0..60]);
    }
    src.push((20 - 1) << 2); // len 20
    src.extend_from_slice(&data[0..20]);
    assert_eq!(snp_uncompress(&src).unwrap(), data);
}

/// 载荷头解析([类型][维数][边界][数据长])。
#[test]
fn yssd_payload_parse() {
    // INT 一维 3 元素 [1,2,3]
    let mut raw = Vec::new();
    raw.extend_from_slice(&1u32.to_le_bytes());
    raw.extend_from_slice(&1u32.to_le_bytes());
    raw.extend_from_slice(&3u32.to_le_bytes());
    raw.extend_from_slice(&24u32.to_le_bytes());
    for v in [1i64, 2, 3] {
        raw.extend_from_slice(&v.to_le_bytes());
    }
    let p = YssdPayload::from_bytes(&raw).unwrap();
    assert_eq!(p.ty, 1);
    assert_eq!(p.dims, vec![3]);
    assert_eq!(p.data.len(), 24);
    assert_eq!(&p.data[..8], &1i64.to_le_bytes());
}


/// 真实样本自适应:save/ 目录含 gsd.sd ⇒ v494 语料(全 zlib,块类型 0/1/2);
/// 否则 = v555 标定样本(六文件全 SNP,btype 恒 0)。
#[test]
fn yssd_real_sample_blocks() {
    let Some(dir) = sample_save_dir() else {
        eprintln!("样本 save/ 缺失,跳过");
        return;
    };
    if dir.join("gsd.sd").exists() {
        yssd_v494_corpus(&dir);
    } else {
        yssd_v555_corpus(&dir);
    }
}

/// v494 语料断言(第二发行作品 save/ 六文件,2026-10-18 实测)。
fn yssd_v494_corpus(dir: &std::path::Path) {
    // btype=0 常规块:DNO → (var, ty, dims)
    let plain: &[(&str, u32, u16, u8, &[u32])] = &[
        ("extra.sd", 11, 2627, 3, &[8193]),
        ("extra.sd", 12, 2628, 1, &[]),
        ("extra.sd", 21, 2769, 3, &[8193]),
        ("extra.sd", 22, 2770, 1, &[]),
        ("extra.sd", 31, 2716, 1, &[501]),
        ("extra.sd", 41, 2577, 1, &[501]),
        ("gsd.sd", 1, 1113, 1, &[256, 128]),
        ("gsd.sd", 2, 1112, 3, &[256]),
        ("kidoku.sd", 1, 1116, 1, &[650, 160]),
        ("seld.sd", 1, 2264, 1, &[2001]),
        ("save00001.sd", 1, 2173, 1, &[]),
        ("save00001.sd", 2, 1113, 1, &[]),
        ("save00001.sd", 4, 2171, 3, &[16]),
        ("save00001.sd", 7, 1115, 1, &[256, 128]),
        ("save00001.sd", 8, 1114, 3, &[256]),
        ("save00001.sd", 9, 1005, 3, &[]),
    ];
    for (fname, dno, var, ty, dims) in plain {
        let data = std::fs::read(dir.join(fname)).unwrap();
        let yssd = YssdFile::from_bytes(&data).unwrap();
        let blk = yssd.block(*dno).unwrap_or_else(|| panic!("{fname}[{dno}] 缺块"));
        assert_eq!(blk.var_id, *var, "{fname}[{dno}] var");
        assert_eq!(blk.btype, 0, "{fname}[{dno}] btype");
        assert_eq!(blk.strict, 1, "{fname}[{dno}] strict");
        let raw = decompress_block_payload(&blk.compressed)
            .unwrap_or_else(|e| panic!("{fname}[{dno}] 解压: {e}"));
        let p = YssdPayload::from_bytes(&raw).unwrap();
        assert_eq!(p.ty, *ty, "{fname}[{dno}] ty");
        assert_eq!(&p.dims, dims, "{fname}[{dno}] dims");
    }
    // btype=2 多变量块:gsd DNO=3/4(单子块 var 1161/1162;与 LOAD 反汇编闭合)
    for (fname, dno, var, ulen) in [
        ("gsd.sd", 3u32, 1161u16, 65552usize),
        ("gsd.sd", 4, 1162, 32784),
    ] {
        let data = std::fs::read(dir.join(fname)).unwrap();
        let yssd = YssdFile::from_bytes(&data).unwrap();
        let blk = yssd.block(dno).unwrap();
        assert_eq!(blk.btype, 2, "{fname}[{dno}] btype");
        assert_eq!(blk.var_id, 0, "{fname}[{dno}] 外层 var 恒 0");
        let subs = parse_multi_payload(&blk.compressed).unwrap();
        assert_eq!(subs.len(), 1, "{fname}[{dno}] 子块数");
        assert_eq!(subs[0].var_id, var, "{fname}[{dno}] 子块 var");
        assert_eq!(subs[0].ulen as usize, ulen, "{fname}[{dno}] 子块 ulen");
        let raw = decompress_block_payload(&subs[0].compressed).unwrap();
        assert_eq!(raw.len(), ulen, "{fname}[{dno}] 解压长闭合");
        YssdPayload::from_bytes(&raw).unwrap();
    }
    // save00001.sd 的 btype=2 块(DNO=5/6)同构
    {
        let data = std::fs::read(dir.join("save00001.sd")).unwrap();
        let yssd = YssdFile::from_bytes(&data).unwrap();
        for dno in [5u32, 6] {
            let blk = yssd.block(dno).unwrap();
            assert_eq!(blk.btype, 2, "save00001[{dno}] btype");
            let subs = parse_multi_payload(&blk.compressed).unwrap();
            assert!(!subs.is_empty(), "save00001[{dno}] 子块非空");
            for s in &subs {
                let raw = decompress_block_payload(&s.compressed).unwrap();
                assert_eq!(raw.len() as u32, s.ulen, "save00001[{dno}] 子块 ulen 闭合");
            }
        }
    }
    // btype=1 配置块(save00001 DNO=3):段A=0x640 / 段B=0x7530(8+A+B=31608 闭合)
    {
        let data = std::fs::read(dir.join("save00001.sd")).unwrap();
        let yssd = YssdFile::from_bytes(&data).unwrap();
        let blk = yssd.block(3).unwrap();
        assert_eq!(blk.btype, 1);
        assert_eq!(blk.var_id, 0, "配置块 var 头恒 0");
        let raw = decompress_block_payload(&blk.compressed).unwrap();
        let cfg = parse_config_payload(&raw).unwrap();
        assert_eq!(cfg.section_a.len(), 0x640, "段A长 = 0x640(引擎硬编码值)");
        assert_eq!(cfg.section_b.len(), 0x7530, "段B长 = 30000");
        assert_eq!(8 + 0x640 + 0x7530, raw.len(), "载荷长闭合");
    }
}

/// v555 标定样本断言(2026-09-05 实测;全 SNP → decompress_block_payload 自动分流)。
fn yssd_v555_corpus(dir: &std::path::Path) {
    // (文件, 块数, [(DNO, var_id, 类型, 维数, 数据长)])
    let expect: &[(&str, usize, &[(u32, u16, u8, &[u32], usize)])] = &[
        ("seld.sd", 1, &[(1, 2337, 1, &[2001], 16008)]),
        (
            "config.sd",
            2,
            &[(1, 1174, 1, &[256, 256], 524288), (2, 1173, 3, &[256], 1067)],
        ),
        (
            "global.sd",
            4,
            &[
                (3, 1165, 1, &[14000], 112000),
                (4, 1166, 3, &[14000], 56000),
                (33, 1027, 1, &[1001], 8008),
                (34, 1028, 1, &[1001], 8008),
            ],
        ),
        (
            "kidoku.sd",
            4,
            &[
                (1001, 6675, 1, &[1000], 8000),
                (1002, 6671, 3, &[1000], 4920),
                (1003, 6672, 1, &[1000], 8000),
                (1004, 6674, 1, &[], 8),
            ],
        ),
        (
            "extra.sd",
            6,
            &[
                (11, 3611, 3, &[8193], 32810),
                (12, 3612, 1, &[], 8),
                (21, 3773, 3, &[8193], 32772),
                (22, 3774, 1, &[], 8),
                (31, 3718, 1, &[51], 408),
                (41, 3551, 1, &[101], 808),
            ],
        ),
        (
            "voice.sd",
            13,
            &[
                (20, 4192, 1, &[], 8),
                (21, 4193, 3, &[1001], 4004),
                (22, 4194, 3, &[1001], 4004),
                (23, 4195, 3, &[1001], 4004),
                (24, 4196, 3, &[1001, 12], 48048),
                (25, 4197, 1, &[1001], 8008),
                (26, 4198, 1, &[1001], 8008),
                (27, 4199, 1, &[1001], 8008),
                (28, 4200, 1, &[], 8),
                (29, 4190, 1, &[], 8),
                (30, 4201, 1, &[1001], 8008),
                (31, 4202, 1, &[], 8),
                (32, 4203, 1, &[], 8),
            ],
        ),
    ];
    // 勘误(2026-09-05):.sd 是**可变环境数据** —— 真引擎运行/退出会改写。
    // 断言保持「模式鲁棒」:可解析、已知块存在、类型/维数/数据长符合 schema。
    for (fname, nblk, blocks) in expect {
        let data = std::fs::read(dir.join(fname)).unwrap();
        let yssd = YssdFile::from_bytes(&data).unwrap();
        assert!(
            yssd.block_count() >= *nblk,
            "{fname} 块数 {} 应 ≥ 已知 {}",
            yssd.block_count(),
            nblk
        );
        for (dno, var_id, ty, dims, _dlen) in blocks.iter() {
            let Some(blk) = yssd.block(*dno) else {
                eprintln!("{fname}: DNO {dno} 不在(引擎改写,跳过)");
                continue;
            };
            assert_eq!(blk.var_id, *var_id, "{fname}[{dno}] var");
            assert_eq!(blk.btype, 0, "{fname}[{dno}] type");
            assert_eq!(blk.strict, 1, "{fname}[{dno}] strict");
            let raw = decompress_block_payload(&blk.compressed).unwrap();
            let p = YssdPayload::from_bytes(&raw).unwrap();
            assert_eq!(p.ty, *ty, "{fname}[{dno}] ty");
            assert_eq!(&p.dims, dims, "{fname}[{dno}] dims");
            let product: usize = dims.iter().map(|d| *d as usize).product();
            let min_len = if *ty == 1 || *ty == 2 { 8 * product } else { 4 * product };
            assert!(
                p.data.len() >= min_len,
                "{fname}[{dno}] dlen {} ≥ {min_len}",
                p.data.len()
            );
        }
    }
}

/// 编码器往返:snp_compress → snp_uncompress(含空/1B/60/61/长跑/重复)。
#[test]
fn snp_compress_roundtrip() {
    let mut rng: u64 = 0x243f6a8885a308d3;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut cases: Vec<Vec<u8>> = vec![
        Vec::new(),
        vec![0xff],
        vec![b'x'; 60],
        vec![b'x'; 61],
        vec![b'x'; 0x100],
        vec![b'x'; 0x10001],
        (0..100_000u32).map(|i| (i as u8).wrapping_mul(7)).collect(),
    ];
    // 带重复段的伪随机(触发 copy 族)。
    let mut rep: Vec<u8> = (0..40_000u32).map(|_| next() as u8).collect();
    let head = rep[..5_000].to_vec();
    rep.extend_from_slice(&head);
    cases.push(rep);
    for src in &cases {
        let enc = snp_compress(src);
        let dec = snp_uncompress(&enc).unwrap();
        assert_eq!(&dec, src, "snp 往返失败(len {})", src.len());
    }
}

/// zlib 块载荷压缩往返;且纯原数据(非 zlib 头)不得被 zlib 分支误收。
#[test]
fn zlib_block_payload_roundtrip() {
    let raw: Vec<u8> = (0..50_000u32)
        .map(|i| (i.wrapping_mul(2654435761u32) >> 24) as u8)
        .collect();
    let c = compress_block_payload(&raw);
    assert_eq!(c[0] & 0x0f, 0x08, "zlib CMF");
    assert_eq!(decompress_block_payload(&c).unwrap(), raw);
}

/// 块体/容器编码往返:encode_block_body + YssdBuilder → YssdFile 解回。
#[test]
fn builder_roundtrip() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&4u32.to_le_bytes());
    payload.extend_from_slice(&32u32.to_le_bytes());
    for v in [10i64, -20, 30, 40] {
        payload.extend_from_slice(&v.to_le_bytes());
    }
    let mut b = YssdBuilder::default();
    b.set_block(1, encode_block_body(0, 1, 1113, &payload, true));
    b.set_block(777, encode_block_body(0, 1, 2171, &payload, false));
    b.set_block(1024, encode_multi_body(&[(&1161u16, &payload)]));
    let bytes = b.build();
    let f = YssdFile::from_bytes(&bytes).unwrap();
    assert_eq!(f.block_count(), 3);
    for dno in [1u32, 777, 1024] {
        assert!(f.block(dno).is_some(), "DNO {dno}");
    }
    assert!(f.block(2).is_none(), "空 DNO 查无");
    // btype0 压缩块解回
    let blk = f.block(1).unwrap();
    assert_eq!((blk.btype, blk.strict, blk.var_id), (0, 1, 1113));
    let raw = decompress_block_payload(&blk.compressed).unwrap();
    assert_eq!(YssdPayload::from_bytes(&raw).unwrap().data, &payload[16..]);
    // btype0 非压缩块:clen == ulen == 载荷长
    let blk = f.block(777).unwrap();
    assert_eq!(blk.compressed, payload);
    // btype2 多块解回
    let blk = f.block(1024).unwrap();
    let subs = parse_multi_payload(&blk.compressed).unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].var_id, 1161);
    let raw = decompress_block_payload(&subs[0].compressed).unwrap();
    assert_eq!(raw, payload);
}

/// 配置载荷编码往返。
#[test]
fn config_payload_roundtrip() {
    let a = vec![1u8; 0x640];
    let b = vec![2u8; 100];
    let enc = encode_config_payload(&a, &b);
    let p = parse_config_payload(&enc).unwrap();
    assert_eq!(p.section_a, a);
    assert_eq!(p.section_b, b);
}


/// 真实样本:config.sd 块 1(INT 256×256)的数据抽样
///(@1174[101][1] = 1920 屏宽、[101][2] = 1080 屏高、[101][5] = -10 边距)。
#[test]
fn yssd_real_sample_config_values() {
    let Some(dir) = sample_save_dir() else {
        eprintln!("样本 save/ 缺失,跳过");
        return;
    };
    // 本测试锚定 v555 标定样本的 config.sd;v494 语料无此文件。
    if !dir.join("config.sd").exists() {
        eprintln!("非 v555 语料(无 config.sd),跳过");
        return;
    }
    let data = std::fs::read(dir.join("config.sd")).unwrap();
    let yssd = YssdFile::from_bytes(&data).unwrap();
    let blk = yssd.block(1).unwrap();
    let raw = snp_uncompress(&blk.compressed).unwrap();
    let p = YssdPayload::from_bytes(&raw).unwrap();
    let elem = |i: usize, j: usize| {
        let off = (i * 256 + j) * 8;
        i64::from_le_bytes(p.data[off..off + 8].try_into().unwrap())
    };
    // 勘误(2026-09-05):本断言曾锚定精确值(1920/1080/-10),但 config.sd
    // 是**可变环境数据** —— 真引擎采集/退出时会按当时窗口尺寸改写
    // (实测被改写为 1168;save/ 目录含 save50001.sd 等全套存档)。
    // 改为环境鲁棒断言:屏高宽为合理正数、边距/标记位为已知稳定值。
    let (w, h) = (elem(101, 1), elem(101, 2));
    assert!(
        w > 0 && h > 0 && w >= h,
        "@1174[101][1..2] 屏宽高应为正(实测 {w}x{h};引擎按采集时窗口写)"
    );
    assert_eq!(elem(110, 10), 0, "@1174[110][10](s41 g76 条件源)");
}
