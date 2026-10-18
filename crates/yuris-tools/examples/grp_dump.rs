//! 只读探针:**dump 指定脚本的指定组**(命令名 + 窗口槽名 + 解码指令)。
//!
//! 用途:状态机取证需要读脚本侧控制流(如 s66 精灵状态码推进链、
//! 精灵清理函数的守卫),varloc 只给「哪里出现」,本探针给「整组在干什么」。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example grp_dump -- <游戏目录> <脚本号> <起组> <止组)

use yuris_format::yscm::YscmTable;
use yuris_vm::host::{ScriptHost, YpfScriptHost};
use yuris_script::Insn;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(
        args.next().expect("usage: grp_dump <dir> <sid> <lo> <hi>"),
    );
    let sid: u16 = args.next().and_then(|s| s.parse().ok()).expect("脚本号");
    let lo: usize = args.next().and_then(|s| s.parse().ok()).expect("起组");
    let hi: usize = args.next().and_then(|s| s.parse().ok()).expect("止组");

    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension()
                .map(|x| x.eq_ignore_ascii_case("ypf"))
                .unwrap_or(false)
            {
                packs.push(p);
            }
        }
    }
    packs.sort();
    let pack = packs
        .iter()
        .find(|p| {
            let Ok(mut h) = YpfScriptHost::open(p) else { return false };
            h.entry_names()
                .iter()
                .any(|e| e.to_ascii_lowercase().ends_with(r"ysbin\ysl.ybn"))
        })
        .cloned()
        .expect("脚本包");

    let mut host = YpfScriptHost::open(&pack).expect("打开包");
    let yscm_name = host
        .entry_names()
        .into_iter()
        .find(|e| e.to_ascii_lowercase().ends_with(r"ysbin\ysc.ybn"))
        .expect("ysc.ybn");
    let blob = host.read_entry(&yscm_name).expect("读 ysc.ybn");
    let yscm = YscmTable::from_bytes(&blob).expect("解析 ysc.ybn");

    let ctx = host.load(sid).expect("载入脚本");
    let groups = ctx.script.groups().expect("part1");
    let firsts = ctx.script.group_first_slots(&groups);
    for gi in lo..hi.min(groups.len()) {
        let g = &groups[gi];
        let yc = yscm.command_at(g.command_type as usize);
        let cname = yc.map(|c| c.name.as_str()).unwrap_or("?");
        print!("g{gi:<5} {cname}");
        if g.param != 0 {
            print!(" param=0x{:04x}", g.param);
        }
        println!();
        for wi in 0..g.window_count as usize {
            let Some(w) = ctx.script.slots().get(firsts[gi] + wi) else { continue };
            let Some(b) = ctx.script.window_bytes_pooled_copy(w) else { continue };
            let slot = (w.tag & 0xff) as usize;
            let pname = yc
                .and_then(|c| c.params.get(slot))
                .map(|p| p.name.as_str())
                .unwrap_or("?");
            let insns = yuris_script::decode_window(&b).unwrap_or_default();
            let mut text = String::new();
            for insn in &insns {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&fmt_insn(insn));
            }
            println!("  w{wi} slot{slot:<3}({pname:<10}) tag=0x{:06x} len={:<4} {}",
                w.tag, w.len, text);
        }
    }
}

fn fmt_insn(ins: &yuris_script::Instruction) -> String {
    use yuris_script::Insn::*;
    match &ins.kind {
        PushInt(v) => format!("push {v}"),
        PushFloat(v) => format!("push {v}"),
        PushStr(s) => {
            let t = String::from_utf8_lossy(s);
            format!("pushstr {:?} ", t.trim_matches('"'))
        }
        PushVar(v) => format!("push {}", v.display()),
        PushVarRef(v) => format!("ref {}", v.display()),
        PushVarIndexed(v) => format!("idx {}", v.display()),
        ArrayLoad { .. } => "aload".into(),
        Binary(op) => format!("{op:?}"),
        Unary(op) => format!("{op:?}"),
        Cast(c) => format!("{c:?}"),
        GroupSep => ";".into(),
        Unknown { raw_op, operand } => format!("unk(0x{raw_op:02x} {operand:02x?})"),
    }
}
