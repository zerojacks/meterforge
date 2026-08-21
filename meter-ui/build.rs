fn main() {
    // Windows 下可执行文件的主线程栈默认只有 1MB（链接器历史默认值），
    // 而 gpui / gpui-component 的链式 builder 在 debug 构建下（无跨函数
    // 内联）会在调用方栈帧里保留较大的中间值，UI 树稍微复杂一点就容易
    // 在这个默认栈上溢出。Linux/macOS 主线程默认栈是 8MB，基本不会遇到
    // 这个问题；这里把 Windows 的主线程栈显式提到 16MB 对齐另外两个
    // 平台的量级，避免以后每加一个组件都要担心撞栈。
    //
    // 这是 Zed 自己在 Windows 上采用的同一类做法。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/STACK:16777216");
    }
}
