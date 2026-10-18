//! 临时取证工具(config 页点亮链):静态反汇编指定脚本的组区间。
//!
//! 用法: cargo run -p yuris-vm --example tmp_s0_disasm -- <游戏目录> <script号> <起组> <止组>
//!
//! 输出每组的命令名(ysc.ybn 名表)+ 各窗口指令(YSTB 表达式机)。

use yuris_format::ystb::YstbFile;
use yuris_vm::host::YpfScriptHost;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let sid: u16 = args[2].parse().unwrap();
    let lo: usize = args[3].parse().unwrap();
    let hi: usize = args[4].parse().unwrap();

    let pack = std::path::Path::new(dir).join("data.ypf");
    let mut host = YpfScriptHost::open(&pack).unwrap();

    // 命令名表(ysbin\ysc.ybn;后缀匹配,根前缀逐包不同)。
    let ysc_name = host
        .entry_names()
        .into_iter()
        .find(|n| n.ends_with("ysbin\\ysc.ybn") || n.ends_with("ysbin/ysc.ybn"))
        .expect("ysc.ybn 不在封包内");
    let ysc_bytes = host.read_entry(&ysc_name).unwrap();
    let ysc = yuris_format::yscm::YscmTable::from_bytes(&ysc_bytes).ok();
    let cmd_name = |idx: u8| -> String {
        match ysc.as_ref().and_then(|t| t.command_at(idx as usize)) {
            Some(c) => format!("{}", c.name),
            None => format!("cmd{idx}"),
        }
    };

    let entry = host
        .entry_names()
        .into_iter()
        .find(|n| n.ends_with(&format!("yst{sid:05}.ybn")))
        .unwrap_or_else(|| panic!("yst{sid:05}.ybn 不在封包内"));
    let blob = host.read_entry(&entry).unwrap();
    let (key, _) = yuris_format::ystb::guess_key(&blob).unwrap();
    let y = YstbFile::from_bytes(&blob, key).unwrap();
    let groups = y.groups().unwrap();
    let firsts = y.group_first_slots(&groups);
    println!(
        "// {entry}: {} 组;dump {}..={}",
        groups.len(),
        lo,
        hi
    );
    for gi in lo..=hi.min(groups.len().saturating_sub(1)) {
        let g = &groups[gi];
        let wins: Vec<String> = y
            .group_windows(firsts[gi], g)
            .iter()
            .map(|w| {
                let bytes = y.window_bytes_pooled_copy(w).unwrap_or_default();
                match yuris_script::decode_window(&bytes) {
                    Ok(ins) => ins
                        .iter()
                        .map(|i| render_insn(i, &bytes))
                        .collect::<Vec<_>>()
                        .join(" "),
                    Err(_) => format!("(解码失败) {}", hex(&bytes)),
                }
            })
            .collect();
        println!("组{gi} {} (ct={}) n={}: {}", cmd_name(g.command_type), g.command_type, g.window_count, wins.join(" | "));
    }
}

fn render_insn(i: &yuris_script::insn::Instruction, _raw: &[u8]) -> String {
    use yuris_script::insn::Insn;
    match &i.kind {
        Insn::PushInt(v) => format!("pushint({v})"),
        Insn::PushFloat(v) => format!("pushfloat({v})"),
        Insn::PushStr(s) => {
            let txt: String = s
                .iter()
                .map(|&c| if (0x20..=0x7e).contains(&c) { c as char } else { '?' })
                .collect();
            format!("pushstr({txt:?})")
        }
        Insn::PushVar(r) => format!("pushvar({})", r.display()),
        Insn::PushVarRef(r) => format!("pushvarref({})", r.display()),
        Insn::PushVarIndexed(r) => format!("pushvaridx({})", r.display()),
        Insn::ArrayLoad { .. } => "aload".into(),
        other => other.mnemonic().into(),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}
