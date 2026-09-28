fn main() {
    // 嵌入 Windows exe 图标（gscrcpy.ico）
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/gscrcpy.ico");
        res.compile().unwrap();
    }
}
