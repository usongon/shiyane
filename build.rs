fn main() {
    tauri_build::build();

    // Swift 运行时 rpath：所有 macOS 内置 Swift 运行时（dyld shared cache，
    // 路径 /usr/lib/swift）。Xcode 构建的 app 自动带该 rpath；Rust 二进制
    // 不会，链接了 Swift 依赖（如 screencapturekit）后 dyld 启动即 abort
    // （"no LC_RPATH's found"）。补上平台默认值，用户机无需装任何东西。
    // 用 CARGO_CFG_TARGET_OS（目标平台）而非 cfg!（宿主平台），交叉编译安全。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }

    // pipeline/realtime 持有 tauri::AppHandle，引用它的集成测试会把
    // tauri→muda 整个 GUI 栈链进测试可执行文件；muda import 了
    // TaskDialogIndirect（comctl32 v6 独有导出）。应用本体的清单由
    // tauri-build 生成（内含 Common-Controls v6 依赖），但测试可执行文件
    // 没有 SxS 清单 → 加载器绑定 v5 comctl32 → 0xC0000139 入口点缺失，
    // cargo test 直接崩溃。给测试目标补嵌入 v6 依赖清单。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTDEPENDENCY:type='win32' \
             name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
             publicKeyToken='6595b64144ccf1df' language='*' \
             processorArchitecture='*'"
        );
    }
}
