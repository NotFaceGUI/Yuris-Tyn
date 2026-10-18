//! scenario 解析器单测。
//!
//! 注:原 `tests.rs` 在环境迁移(2026-09-04 会话 → 2026-09-05 会话)中丢失,
//! 本文件按 lib.rs 模块文档记载的语料形态**重建最小回归集**(P7.2 补录)。
//! 断言全部以解析器**实测行为**为准(2026-09-05 逐例试验)。

use super::*;

/// 语料主流对话形态:`(ID:n)\VO(x)\LE("…")\LT("…")` → 四元素。
#[test]
fn vo_le_lt_mainstream() {
    let src = b"(ID:123)\\VO(x)\\LE(\"\x83\x65\x83\x58\x83\x67\")\\LT(\"\x83\x65\x83\x58\x83\x67\")\r\n";
    let sc = parse_scenario(src).unwrap();
    let kinds: Vec<&str> = sc
        .elements
        .iter()
        .map(|e| match e {
            Element::LineId { .. } => "lineid",
            Element::Command { name: _, .. } => "cmd",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["lineid", "cmd", "cmd", "cmd"]);
    for e in &sc.elements {
        match e {
            Element::LineId { line, .. } => assert_eq!(*line, 1),
            Element::Command { line, .. } => assert_eq!(*line, 1),
            _ => panic!("unexpected element"),
        }
    }
}

/// 命令形态:修饰符链、首槽跳位、引号串/裸词/负整数。
#[test]
fn command_modifiers_and_params() {
    let sc =
        parse_scenario(b"\\BG(white,200,0)\r\n\\BGM(,800)\r\n\\GO.G.IF(1,-2)\r\n").unwrap();
    let cmds: Vec<&Element> = sc
        .elements
        .iter()
        .filter(|e| matches!(e, Element::Command { .. }))
        .collect();
    assert_eq!(cmds.len(), 3);
    match cmds[0] {
        Element::Command { name, modifiers, params, .. } => {
            assert_eq!(name, "BG");
            assert!(modifiers.is_empty());
            assert_eq!(
                params,
                &[Param::Str("white".into()), Param::Int(200), Param::Int(0)]
            );
        }
        _ => unreachable!(),
    }
    match cmds[1] {
        Element::Command { name, params, .. } => {
            assert_eq!(name, "BGM");
            // 首槽跳位 → Empty(parser.rs §参数文档「(,800) = [Empty, Int]」)
            assert_eq!(params, &[Param::Empty, Param::Int(800)]);
        }
        _ => unreachable!(),
    }
    match cmds[2] {
        Element::Command { name, modifiers, params, .. } => {
            assert_eq!(name, "GO");
            assert_eq!(modifiers, &["G".to_string(), "IF".to_string()]);
            assert_eq!(params, &[Param::Int(1), Param::Int(-2)]);
        }
        _ => unreachable!(),
    }
}

/// `#标签` 段定义 + 标签表指向对应元素下标;`//` 注释/空行不产出元素。
#[test]
fn labels_and_comment_lines() {
    let src = b"// comment\r\n\r\n#SCENARIO_MAIN\r\n\\CMG()\r\n#NEXT\r\n";
    let sc = parse_scenario(src).unwrap();
    assert_eq!(sc.elements.len(), 3); // Label, Command, Label
    assert!(matches!(sc.elements[0], Element::Label { line: 3, .. }));
    assert_eq!(sc.labels.get("SCENARIO_MAIN"), Some(&0));
    assert_eq!(sc.labels.get("NEXT"), Some(&2));
}

/// 块注释跨行;字符串内 `//` 不剥离。
#[test]
fn block_comment_and_string_guard() {
    let src = b"/* head\r\n span */\\BGM(\"a//b\")\r\n";
    let sc = parse_scenario(src).unwrap();
    assert_eq!(sc.elements.len(), 1);
    match &sc.elements[0] {
        Element::Command { name, params, line, .. } => {
            assert_eq!(name, "BGM");
            assert_eq!(*line, 2);
            assert_eq!(params, &[Param::Str("a//b".into())]);
        }
        _ => panic!("unexpected element"),
    }
}

/// 对话裸文本(可带 `(ID:n)` 前缀)。
#[test]
fn dialogue_with_id() {
    let sc = parse_scenario(b"(ID:5)text\r\n").unwrap();
    assert_eq!(
        sc.elements,
        vec![Element::Dialogue {
            line: 1,
            id: Some(5),
            text: "text".into(),
        }]
    );
}

/// 非法字节序列:报 Decode 且携带 1 基行号(不做替换静默吞)。
/// (0x81 = SJIS 双字节首字节,0x20 非合法尾字节)
#[test]
fn invalid_bytes_report_line() {
    let src = b"ok\r\n\x81\x20\r\n";
    let err = parse_scenario(src).unwrap_err();
    match err {
        ScenarioError::Decode { line, encoding } => {
            assert_eq!(line, 2);
            assert_eq!(encoding, "Shift_JIS");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// T1 + T21:跨层文本编码策略(数据侧判定)
// ---------------------------------------------------------------------------

/// **数据侧判定**:CP932 语料判为 SJIS,Big5 语料判为 Big5。
#[test]
fn detect_encoding_picks_the_valid_one() {
    // CP932 `テスト`(SJIS 字节)
    let sjis = b"\x83\x65\x83\x58\x83\x67";
    assert_eq!(detect_encoding(sjis).unwrap().name(), "Shift_JIS");
    // Big5 `測試`(0xB4 0xFA 0xB8 0xD5):SJIS 下 0xFA 之后 0xB8 非法
    let big5 = b"\xb4\xfa\xb8\xd5";
    assert_eq!(detect_encoding(big5).unwrap().name(), "Big5");
    // 纯 ASCII:两个方向都合法 → 取引擎默认 SJIS(字节层结果相同)
    assert_eq!(detect_encoding(b"\\BG(white,200)\r\n").unwrap().name(), "Shift_JIS");
}

/// **负向**:两种候选都非法 → 显式报错(不静默替换、不猜)。
#[test]
fn detect_encoding_rejects_undecodable() {
    // 0x81 需合法 SJIS 尾字节;0xFE/0xFF 在两个编码族里都非法
    let bad = b"\x81\xfe\xff\x81";
    assert_eq!(detect_encoding(bad), Err(UndecodableText));
    assert_eq!(decode_text(bad), Err(UndecodableText));
    // parse_scenario_auto 同口径:报 Decode
    assert!(matches!(
        parse_scenario_auto(b"\x81\xfe\xff\r\n"),
        Err(ScenarioError::Decode { .. })
    ));
}

/// **`decode_text` 跨层入口**:SJIS 台词与 Big5 台词各自正确解出。
#[test]
fn decode_text_handles_both_families() {
    assert_eq!(decode_text(b"\x83\x65\x83\x58\x83\x67").unwrap(), "テスト");
    assert_eq!(decode_text(b"\xb4\xfa\xb8\xd5").unwrap(), "測試");
}

/// **歧义票**(见模块文档):Big5 汉字串恰为 SJIS 合法序列时,
/// 取半角片假名更少的一方 —— 实测 `開始`(b6 7d a9 6c)在 SJIS 下是
/// 半角片假名串 `ｶ}ｩl`(3 个半角字),Big5 下是正常汉字 `開始`。
#[test]
fn ambiguous_pair_resolved_by_halfwidth_vote() {
    let ambiguous = b"\xb6\x7d\xa9\x6c"; // Big5 `開始`,SJIS 亦合法
    assert_eq!(detect_encoding(ambiguous).unwrap().name(), "Big5");
    assert_eq!(decode_text(ambiguous).unwrap(), "開始");
    // 反向:日文全角假名经 CP932 无半角片假名,Big5 侧非法 → 判 SJIS
    let jp = b"\x82\xb1\x82\xf1\x82\xc9\x82\xbf\x82\xcd"; // こんにちは
    assert_eq!(detect_encoding(jp).unwrap().name(), "Shift_JIS");
}

/// **`parse_scenario_auto`**:CP932 剧本按其真实编码解出、不再乱码。
///
/// 旧行为:`player-core` 写死 Big5 → CP932 台词整篇乱码(<game-b> 潜伏点)。
#[test]
fn parse_scenario_auto_decodes_cp932_text() {
    // `\LT("テスト")` 的 CP932 形态
    let src = b"\\LT(\"\x83\x65\x83\x58\x83\x67\")\r\n";
    let sc = parse_scenario_auto(src).unwrap();
    match &sc.elements[..] {
        [Element::Command { params, .. }] => {
            assert_eq!(params, &[Param::Str("テスト".into())]);
        }
        other => panic!("unexpected: {other:?}"),
    }
}
