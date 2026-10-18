//! 只读探针:**某变量的逐组出现点**(读写形态 + pc + 组内角色)。
//!
//! 用途(成果 137):精灵队列状态数组(`@0x0d42` = @3394)在运行期恒为 0,
//! 而 `es.SP.DRAW` 的调用条件是 `@3394[@48] == 1|2` —— 必须知道
//! **谁在什么时候写它、写成什么**,才能判定是「我方 VM 语义错」还是
//! 「脚本依赖某个我们没实现的副作用」。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example varloc -- <游戏目录> <变量id十进制> [脚本号]
//!
//! 输出每个命中:脚本号 / 组号 / 命令名 / 该变量在窗内是「读值(0x48)」
//! 「引用(0x56/0x76)」还是「带下标(后随 ArrayLoad 0x29)」。

use yuris_vm::host::{ScriptHost, YpfScriptHost};

fn cmd_name(host: &mut YpfScriptHost, sid: u16) -> Vec<String> {
    // 从游戏自己的 ysc.ybn 取命令名(不写死序号)
    let n = host
        .entry_names()
        .into_iter()
        .find(|e| e.to_ascii_lowercase().ends_with(r"ysbin\ysc.ybn"));
    let Some(n) = n else { return Vec::new() };
    let Ok(blob) = host.read_entry(&n) else { return Vec::new() };
    let Ok(t) = yuris_format::yscm::YscmTable::from_bytes(&blob) else { return Vec::new() };
    let _ = sid;
    t.commands().iter().map(|c| c.name.clone()).collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: varloc <dir> <varid_dec> [sid] [space: @|$|#|`]"));
    let want_id: u16 = args.next().and_then(|s| s.parse().ok()).expect("变量 id");
    let only_sid: Option<u16> = args.next().and_then(|s| s.parse().ok());
    /// 变量空间前缀字符(`@`=0x40 数值 / `$`=0x24 字符串 / `#`=0x23 标签 /
    /// `` ` ``=0x60 其它)。**旧版写死 `@`** ⇒ 查字符串变量(`$3655` 这类)
    /// 恒 0 命中,会得出"没人写它"的假结论(实测踩到)。
    let space: u8 = args
        .next()
        .and_then(|s| s.chars().next())
        .map(|c| c as u8)
        .unwrap_or(b'@');

    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("ypf")).unwrap_or(false) {
                packs.push(p);
            }
        }
    }
    packs.sort();
    let pack = packs
        .iter()
        .find(|p| {
            let Ok(mut h) = YpfScriptHost::open(p) else { return false };
            h.entry_names().iter().any(|e| e.to_ascii_lowercase().ends_with(r"ysbin\ysl.ybn"))
        })
        .cloned()
        .expect("脚本包");

    let mut host = YpfScriptHost::open(&pack).expect("打开包");
    let names = cmd_name(&mut host, 0);

    let ids = host.script_ids().expect("脚本清单");
    let [lo, hi] = want_id.to_le_bytes();
    let mut total = 0usize;
    for sid in ids {
        if let Some(o) = only_sid {
            if sid != o {
                continue;
            }
        }
        let Ok(ctx) = host.load(sid) else { continue };
        let Ok(groups) = ctx.script.groups() else { continue };
        let Ok(firsts) = Ok::<_, yuris_core::Error>(ctx.script.group_first_slots(&groups)) else {
            continue;
        };
        for (gi, g) in groups.iter().enumerate() {
            for wi in 0..g.window_count as usize {
                let Some(w) = ctx.script.slots().get(firsts[gi] + wi) else { continue };
                let Some(b) = ctx.script.window_bytes_pooled_copy(w) else { continue };
                // 在窗内找 `[40]['@'][id_lo][id_hi]`(变量操作数的 space+id 部分)
                let mut i = 0usize;
                while i + 5 < b.len() {
                    let op = b[i];
                    if (op == 0x48 || op == 0x56 || op == 0x76)
                        && u16::from_le_bytes([b[i + 1], b[i + 2]]) == 3
                        && b[i + 3] == space
                        && b[i + 4] == lo
                        && b[i + 5] == hi
                    {
                        // 带下标判定:往后扫到 ArrayLoad(0x29),中间只允许
                        // 值类指令(var ref / 整数 / 移位算符)—— 近似足够定位。
                        let mut j = i + 6;
                        let mut indexed = false;
                        let mut steps = 0;
                        while j + 2 < b.len() && steps < 6 {
                            if b[j] == 0x29 {
                                indexed = true;
                                break;
                            }
                            if b[j] == 0x48 || b[j] == 0x56 || b[j] == 0x76 {
                                j += 6;
                                steps += 1;
                                continue;
                            }
                            if b[j] == 0x42 && u16::from_le_bytes([b[j + 1], b[j + 2]]) == 4 {
                                j += 7;
                                steps += 1;
                                continue;
                            }
                            break;
                        }
                        let role = match op {
                            0x48 => "读值",
                            0x56 => "引用(左值)",
                            _ => "下标形式",
                        };
                        let cname = names
                            .get(g.command_type as usize)
                            .map(String::as_str)
                            .unwrap_or("?");
                        println!(
                            "s{sid:<4} g{gi:<5} {cname:<12} w{wi}  {}{role}{}",
                            space as char,
                            if indexed { " + 带下标" } else { "" }
                        );
                        total += 1;
                    }
                    i += 1;
                }
            }
        }
    }
    println!("== 共 {total} 处 ==");
}
