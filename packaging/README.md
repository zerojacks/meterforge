# 应用图标 / 打包说明

图标源文件统一放在 `meterforge-ui/assets/icon/`（`app.ico` / `app.icns` / `app.png`），
每份文件内部本身就是多分辨率容器，不需要额外拆分尺寸：

- `app.ico` — 内含 16/20/24/32/40/48/64/96/128/256 十档，Windows 会按需自动选用。
- `app.icns` — 内含 16 到 1024（含 @2x）多档，macOS 会按需自动选用。
- `app.png` — 512×512 源图，应用内 titlebar logo 用它，缩小到任意尺寸都清晰。

`app.ico` / `app.png` / Linux 的 `hicolor` 系列用的是同一份紧裁过的方形图（图形
撑满画布，四周只留一圈很窄的边），`app.icns` 保留了原始素材自带的大留白方形底板。
这不是疏漏：macOS 图标本来就设计成"图形只占画布中间一部分、留白让系统加阴影/圆角"
的样子，Dock 里这样显示才正常；但同一张图直接喂给 Windows 的 `.ico`，画面里那圈留白
会被当成图标本体的一部分一起显示，图标在任务栏里就会比隔壁应用小一圈、悬在中间。

## Windows

已在 `meterforge-ui/build.rs` 里用 `winres` 把 `app.ico` 嵌入可执行文件资源，
正常 `cargo build -p meter-ui` 即可，任务栏/资源管理器/Alt-Tab 会显示这个图标。

## macOS

cargo 只产出裸的可执行文件，Dock 图标需要一层 `.app` 包装。构建完成后运行：

```sh
cargo build -p meter-ui --release
sh packaging/macos/build-app-bundle.sh
```

会在 `target/release/MeterForge.app` 生成标准应用包（`Info.plist` 模板见
`packaging/macos/Info.plist.in`），双击即可运行，Dock 显示的就是 `app.icns`。

## Linux

`packaging/linux/hicolor/` 是标准 freedesktop 图标主题目录，`meterforge.desktop`
是应用入口。安装到当前用户:

```sh
cd packaging/linux
sh install.sh                          # 装到 ~/.local/share
# 或
sudo PREFIX=/usr/share sh install.sh   # 全局安装
```

`install.sh` 会把图标和 `.desktop` 复制到位并刷新图标/desktop 缓存。
`.desktop` 里的 `Exec=MeterForge` 假设该二进制已在 `PATH` 上，
按实际安装路径调整即可。

## CI 发布（GitHub Actions）

`.github/workflows/release.yml` 在推送 `v*` tag 时触发，四个环境并行编译
release 并自动创建 GitHub Release（changelog 按约定式提交 feat/fix/... 归类生成）：

- `MeterForge-windows-x86_64-<版本>.zip` — MeterForge.exe（图标已内嵌；release
  构建为 GUI 子系统，启动不弹控制台窗口）
- `MeterForge-linux-x86_64-<版本>.tar.gz` — 二进制 + `.desktop` + hicolor 图标 +
  `install.sh`
- `MeterForge-darwin-x86_64-<版本>.dmg` / `MeterForge-darwin-aarch64-<版本>.dmg`
  — `.app` 由 `packaging/macos/build-app-bundle.sh` 打包（CI 通过 `TARGET_TRIPLE` /
  `APP_VERSION` 环境变量驱动交叉编译和版本注入）

发布新版本：

```sh
git tag v0.2.0
git push origin v0.2.0
```

tag 中的版本号会写入 `Cargo.toml`（workspace.package.version）和 macOS
`Info.plist`，产物文件名也带同一版本号。
