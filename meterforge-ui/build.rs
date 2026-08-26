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

    embed_windows_icon();
}

// `winres` 只作为 `cfg(windows)` 的 build-dependency 引入（见 Cargo.toml），
// 所以这里也必须用 `#[cfg(windows)]` 而不是运行时判断，
// 否则在非 Windows 主机上编译 build.rs 本身就会因为找不到 `winres` crate 而失败。
#[cfg(windows)]
fn embed_windows_icon() {
    // 把软件图标(assets/icon/app.ico)嵌入到 Windows 可执行文件中，
    // 这样任务栏、Explorer 文件图标、Alt-Tab 切换器等系统层面显示的
    // 都是这个图标，而不是 Rust 默认的图标。
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/icon/app.ico");
    res.set("ProductName", "MeterForge");
    res.set("FileDescription", "MeterForge");
    res.set("InternalName", "MeterForge");
    res.set("OriginalFilename", "MeterForge.exe");
    if let Err(err) = res.compile() {
        println!("cargo:warning=未能嵌入应用图标: {err}");
    }
}

#[cfg(not(windows))]
fn embed_windows_icon() {}
