//! 只读探针:**CP932 文件名的 VFS 解析实测**(成果 135)。
//!
//! 背景:包内条目名是 **CP932(SJIS)原始字节**,而脚本把 FILE 参数当
//! **字符串**求值 → 本内核内部一律 UTF-8。任何含日文/中文的路径
//! (`cg\ev\サークル.jpg`)在 `resolve_entry` 里以 `.as_bytes()`(UTF-8)
//! 拼查询键,**永远命不中** —— 这就是「片头 LOGO 画不出来」的候选根因:
//! LOGO 素材恰恰是全包少数几个日文名资源之一。
//!
//! 本探针逐个实测:给若干 CP932 名(日文/ASCII),分别用
//!   (a) UTF-8 字节键、(b) CP932 字节键
//! 查询索引,判定哪条路径能命中。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example cp932_lookup -- <游戏目录> <名字…>

use yuris_vm::host::PacFileIndex;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: cp932_lookup <dir> <name…>"));
    let names: Vec<String> = args.collect();

    let idx = PacFileIndex::scan_game_dir_auto(&dir).expect("扫描封包");

    for n in &names {
        println!("===== {n:?} =====");
        let norm = n.replace('/', "\\");
        // (a) 现状路径:resolve_entry(内部用 UTF-8 字节拼键)
        let via_utf8 = idx.read_image_bytes(&norm).is_some();
        let dims_utf8 = idx.image_dims(&norm);
        println!("  read_image_bytes(现状, UTF-8 键) = {via_utf8}; image_dims = {dims_utf8:?}");
        // (b) CP932 编码后再查
        let (cp932, _, had_err) = encoding_rs::SHIFT_JIS.encode(&norm);
        println!(
            "  CP932 字节 = {:02x?} (替换字符出现 = {had_err})",
            cp932.as_ref()
        );
        let utf8b = norm.as_bytes();
        println!("  UTF-8  字节 = {utf8b:02x?}");
        println!(
            "  两者{}",
            if utf8b == cp932.as_ref() {
                "相同(纯 ASCII ⇒ 无此问题)"
            } else {
                "**不同** ⇒ 现状路径必然命不中"
            }
        );
    }
}
