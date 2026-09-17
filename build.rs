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
}
