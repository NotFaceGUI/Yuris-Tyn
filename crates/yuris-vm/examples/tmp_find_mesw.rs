//! 找含指定字符串的组号(按窗口内容字节匹配)。
use yuris_vm::host::{ScriptHost, YpfScriptHost};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let sid: u16 = a[2].parse().unwrap();
    let needle = a[3].as_bytes();
    let pack = std::path::Path::new(dir).join("data.ypf");
    let mut host = YpfScriptHost::open(&pack).unwrap();
    let ctx = host.load(sid).unwrap();
    println!("s{sid}: {} 组", ctx.groups.len());
    for (gi, g) in ctx.groups.iter().enumerate() {
        let first = ctx.first_slots[gi];
        let wins = &ctx.script.slots()[first..first + g.window_count as usize];
        for w in wins {
            if let Some(b) = ctx.script.window_bytes_pooled(w) {
                if b.windows(needle.len()).any(|x| x == needle) {
                    println!("  g{gi} cmd={} 命中", g.command_type);
                    break;
                }
            }
        }
    }
}
