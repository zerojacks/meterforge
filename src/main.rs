// Workspace 根目录 - 使用 `cargo run -p meter-ui` 启动界面
// 或使用 `cd meter-core && cargo test` 测试核心引擎

fn main() {
    eprintln!("此项目使用 workspace 结构:");
    eprintln!("  - meter-core: 核心虚拟表引擎");
    eprintln!("  - meter-ui: GPUI 图形界面");
    eprintln!();
    eprintln!("请使用以下命令:");
    eprintln!("  cargo run -p meter-ui          # 启动图形界面");
    eprintln!("  cargo test -p meter-core       # 测试核心引擎");
    eprintln!("  cargo build --workspace        # 编译所有包");
}
