#[test]
fn probe_baked_invoke_target() {
    let data =
        std::fs::read(r"C:\Users\mrmal\AppData\Local\Temp\opencode\bench\commons-io.jar").unwrap();
    let entries = vinylite_core::parse_jar(&data);
    for e in &entries {
        if e.name.ends_with("IOUtils.class") && !e.name.contains('$') {
            let class = e.class.as_ref().unwrap();
            let decl = vinylite_core::build_class_decl(class);
            for m in &decl.methods {
                if m.name == "close" {
                    for s in &m.statements {
                        let sd = format!("{s:?}");
                        if sd.contains("disconnect") {
                            println!("STMT: {sd}");
                        }
                    }
                }
            }
        }
    }
}
