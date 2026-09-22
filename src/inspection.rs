use wasmparser::{ComponentExternalKind, Parser, Payload};

/// Read root exported instance names without compiling or executing the component.
pub fn exported_interfaces(bytes: &[u8]) -> Result<Vec<String>, wasmparser::BinaryReaderError> {
    let mut depth = 0;
    let mut interfaces = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => depth += 1,
            Payload::End(_) if depth > 0 => depth -= 1,
            Payload::ComponentExportSection(exports) if depth == 0 => {
                for export in exports {
                    let export = export?;
                    if export.kind == ComponentExternalKind::Instance {
                        interfaces.push(export.name.name.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(interfaces)
}
