//! 一次性探针:把指定资源原始字节落盘(查看图片内容用,不入库逻辑)。
//!
//! 用法:`cargo run -p yuris-tools --release --example dump_cg -- <游戏目录> <资源名> <输出文件>`

use yuris_vm::host::PacFileIndex;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: dump_cg <dir> <name> <out>"));
    let name = args.next().expect("<name>");
    let out = args.next().expect("<out>");
    let index = std::sync::Arc::new(PacFileIndex::scan_game_dir_auto(&dir).expect("扫描封包"));
    let bytes = index
        .read_image_bytes_raw(name.as_bytes())
        .expect("读取失败");
    std::fs::write(&out, &bytes).expect("写出失败");
    println!("written {} bytes → {}", bytes.len(), out);
}
