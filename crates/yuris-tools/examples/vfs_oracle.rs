//! 只读探针:**VFS 契约符合性实测**(成果 136)。
//!
//! 引擎的 VFS 契约有三条,全部有二进制取证(`旁线仓库/docs/format-ybn-loader.md`
//! §5 + `docs/architecture.md` §4.1):
//!
//! 1. **CP932 名字空间**:包内条目名是 CP932(SJIS)字节;脚本侧字符串是 UTF-8。
//! 2. **大小写不敏感**:`FUN_00468d40`(208B)是"不区分大小写的字符串比较",
//!    用逐字节折叠实现 —— **引擎按名字查文件就是大小写不敏感的**(二进制实证)。
//! 3. **路径分隔符等价**:`/` 与 `\` 等价(Windows 语义)。
//!
//! 本探针对每个名字实测上述三条,输出 `OK` / `FAIL`,作为重构的验收基线。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example vfs_oracle -- <游戏目录>

use yuris_vm::host::PacFileIndex;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: vfs_oracle <dir>"));
    let idx = PacFileIndex::scan_game_dir_auto(&dir).expect("扫描封包");

    // (说明, 探针路径)
    let cases: &[(&str, &str)] = &[
        // 契约 1:CP932 名字(日文名资源)
        ("CP932 日文名", "cg/ev/サークル"),
        ("CP932 日文名(显式扩展名)", "cg/ev/サークル.jpg"),
        ("CP932 中文名资源", "cg/bg/部室"),
        // 契约 2:大小写不敏感
        ("全大写(真实存在的名字)", "CG/EV/8_4E"),
        ("全大写(带扩展名)", "CG/EV/8_4E.JPG"),
        ("大小写混合", "Cg/Ev/8_4e.Jpg"),
        // 契约 3:分隔符等价
        ("反斜杠", r"cg\ev\8_4e"),
        ("正斜杠", "cg/ev/8_4e"),
        // 对照:纯 ASCII 小写(基线,必通)
        ("ASCII 基线", "cg/ev/8_4e"),
        // 对照:确定不存在的资源(必败 —— 防"什么都返回 true"的假阳性)
        ("【阴性对照】不存在的名字", "cg/ev/__NO_SUCH_ASSET__"),
    ];

    let mut pass = 0usize;
    let mut fail = 0usize;
    for (desc, path) in cases {
        let got = idx.read_image_bytes(path).is_some();
        let want = !desc.starts_with("【阴性对照】");
        let ok = got == want;
        if ok {
            pass += 1;
        } else {
            fail += 1;
        }
        println!(
            "  [{}] {desc:28} {path:32} → {}",
            if ok { "OK  " } else { "FAIL" },
            if got { "命中" } else { "未命中" }
        );
    }
    println!("\n== VFS 契约: {pass} 通过 / {fail} 失败 ==");
    if fail > 0 {
        println!("⇒ 有契约不满足,见上(每个 FAIL 即一条待修项)。");
    }
}
